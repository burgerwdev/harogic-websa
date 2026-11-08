//! Service startup and ordered teardown own publisher, storage and SDK lifetimes.
use super::{
    channel_identities, hardware_swp, load_golden, publisher, receiver_presets, receiver_workers,
    recording, service_routes, App, DevState, StateMutex,
};
use parking_lot::Mutex as PLMutex;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

fn check_hardware_exclusive() -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(executable) = std::fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        let name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let native_probe = matches!(name, "san90_iqs" | "san90_iqs (deleted)")
            && std::fs::read(entry.path().join("cmdline")).is_ok_and(|args| {
                args.split(|byte| *byte == 0)
                    .any(|arg| arg == b"--hardware")
            });
        let hardware_service = matches!(name, "websad" | "websad (deleted)")
            && std::fs::read(entry.path().join("environ"))?
                .split(|byte| *byte == 0)
                .any(|item| {
                    item.starts_with(b"WEBSA_RUST_IQS_SHIM=")
                        && item.len() > b"WEBSA_RUST_IQS_SHIM=".len()
                });
        if native_probe || hardware_service {
            return Err("stop the native hardware owner before exclusive Rust IQS".into());
        }
    }
    Ok(())
}

pub(super) async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    // Resolve decoder dependencies before device state exists; command preflight uses cached
    // capabilities and must never run child probes while holding the device mutex.
    tokio::task::spawn_blocking(websa_channelizer::catalog::manifest).await?;
    let (golden, schema) = load_golden();
    let port: u16 = std::env::var("WEBSA_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8099);
    let host = std::env::var("WEBSA_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    // Reserve the endpoint before creating owners or opening persistent state.
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let mut state = DevState::new(golden, schema);
    if let Ok(shim) = std::env::var("WEBSA_RUST_IQS_SHIM") {
        if shim.is_empty() {
            return Err("WEBSA_RUST_IQS_SHIM is empty".into());
        }
        check_hardware_exclusive()?;
        state.hardware_shim = Some(shim);
    }
    let recording_root = std::env::var_os("WEBSA_RECORDING_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                        .join(".local/share")
                });
            data.join("harogic-websa/recordings").join(port.to_string())
        });
    let presets_root = recording_root.clone();
    // Probe once during startup, before snapshots take the acquisition lock.
    let _ = recording::opus::available();
    match tokio::task::spawn_blocking(move || recording::Manager::open(recording_root)).await? {
        Ok(manager) => state.recordings = Some(manager),
        Err(error) => {
            eprintln!("WebSA recording storage unavailable: {error}");
            state.recording_error = Some(error.to_string());
        }
    }
    // The recording manager already owns this instance's exclusive directory lock.
    if state.recordings.is_some() {
        match tokio::task::spawn_blocking(move || receiver_presets::Store::open(presets_root))
            .await?
        {
            Ok(store) => state.receiver_presets = Some(Arc::new(std::sync::Mutex::new(store))),
            Err(error) => eprintln!("WebSA receiver presets unavailable: {error}"),
        }
    }
    state.server.lock().refresh_dependencies();
    // Allocate bounded mailboxes. Each processing owner prepares its selected
    // model lazily when the first input starts that branch.
    state.receiver_workers = Some(receiver_workers::ReceiverWorkers::new()?);
    let app = Arc::new(App {
        state: StateMutex::new(state),
        clients: PLMutex::new(Vec::new()),
        next_client: AtomicU64::new(0),
        epoch: Arc::new(AtomicU64::new(0)),
        display_version: Arc::new(AtomicU64::new(0)),
        identities: channel_identities(),
    });
    let stopping = Arc::new(AtomicBool::new(false));
    println!("websad listening on http://{host}:{port}");
    let publisher_app = app.clone();
    let publisher_stopping = stopping.clone();
    serve_running(
        listener,
        app,
        stopping,
        move |fatal| publisher(publisher_app, publisher_stopping, Some(fatal)),
        shutdown_signal(),
    )
    .await
    .map_err(Into::into)
}

async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => (),
        _ = terminate.recv() => (),
    }
}

async fn serve_running<P, F, S>(
    listener: tokio::net::TcpListener,
    app: Arc<App>,
    stopping: Arc<AtomicBool>,
    make_publisher: P,
    shutdown: S,
) -> Result<(), String>
where
    P: FnOnce(tokio::sync::oneshot::Sender<String>) -> F,
    F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    S: std::future::Future<Output = ()> + Send + 'static,
{
    let router = service_routes::router(app.clone());
    let (fatal, mut fatal_rx) = tokio::sync::oneshot::channel();
    let mut publisher = tokio::spawn(make_publisher(fatal));
    let (shutdown_http, http_shutdown) = tokio::sync::oneshot::channel::<()>();
    let served = async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = http_shutdown.await;
            })
            .await
    };
    tokio::pin!(served);
    tokio::pin!(shutdown);
    let mut errors = Vec::new();
    let mut publisher_result = None;
    let mut server_finished = false;
    let mut fatal_open = true;
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            result = &mut served => {
                server_finished = true;
                if let Err(error) = result { errors.push(error.to_string()); }
                break;
            }
            result = &mut fatal_rx, if fatal_open => {
                match result {
                    Ok(error) => { errors.push(error); break; }
                    Err(_) => fatal_open = false,
                }
            }
            result = &mut publisher => {
                match &result {
                    Ok(Ok(())) => errors.push("publisher exited before shutdown".into()),
                    Ok(Err(error)) => errors.push(error.clone()),
                    Err(error) => errors.push(format!("publisher task failed: {error}")),
                }
                publisher_result = Some(result);
                break;
            }
        }
    }
    stopping.store(true, Ordering::Release);
    let _ = shutdown_http.send(());
    disconnect_clients(&app);
    let stop_app = app.clone();
    let stop_child =
        tokio::task::spawn_blocking(move || stop_app.state.lock().unwrap().server.lock().stop());
    // Poll HTTP shutdown immediately: the listener must close even while a
    // failed publisher waits for its in-flight model preparation to return.
    if !server_finished {
        if let Err(error) = served.await {
            errors.push(error.to_string());
        }
    }
    match stop_child.await {
        Ok(Ok(_)) => (),
        Ok(Err(error)) => errors.push(format!("server stop during shutdown: {error}")),
        Err(error) => errors.push(format!("server stop worker: {error}")),
    }
    // Cooperatively join the publisher: aborting its async wrapper can detach
    // a spawn_blocking capture step which would race owner teardown.
    let published = match publisher_result {
        Some(result) => result,
        None => publisher.await,
    };
    let failure = match published {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(error) => Some(format!("publisher task failed: {error}")),
    };
    if let Some(error) = failure {
        if !errors.contains(&error) {
            errors.push(error);
        }
    }
    match tokio::task::spawn_blocking(move || stop_owners(app)).await {
        Ok(Ok(())) => (),
        Ok(Err(error)) => errors.push(error),
        Err(error) => errors.push(format!("owner shutdown worker: {error}")),
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn disconnect_clients(app: &App) {
    // Drop every outbound sender and wake parked sockets before waiting for
    // axum's upgraded connections; otherwise an idle WS can hold shutdown.
    let mut clients = app.clients.lock();
    for client in clients.iter() {
        client.wake.notify_one();
    }
    clients.clear();
}

fn stop_owners(app: Arc<App>) -> Result<(), String> {
    stop_owners_with(
        app,
        |state| state.server.lock().stop().map(|_| ()),
        hardware_swp::finish_sdk_session,
    )
}

fn stop_owners_with(
    app: Arc<App>,
    stop_server: impl FnOnce(&mut DevState) -> Result<(), String>,
    finish_sdk: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut state = app.state.lock().unwrap();
    let registry = state.stream_registry();
    let server = stop_server(&mut state);
    let pipeline = state.stop_pipeline();
    let receivers = state.finish_radiofax_with_registry(registry);
    state.ddc = None;
    state.swp_hw = None;
    state.rta_hw = None;
    state.pnm_hw = None;
    state.receiver_workers = None;
    let recordings = state.recordings.take();
    drop(state);
    if let Some(mut recordings) = recordings {
        recordings.shutdown();
    }
    // Release every mode owner even if another stop failed. The SDK registry
    // rejects final shutdown if a timed-out vendor owner is still parked.
    let sdk = finish_sdk();
    let errors: Vec<_> = [server, pipeline, receivers, sdk]
        .into_iter()
        .filter_map(Result::err)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{net::SocketAddr, time::Duration};

    fn test_app(state: DevState) -> Arc<App> {
        Arc::new(App {
            state: StateMutex::new(state),
            clients: PLMutex::new(Vec::new()),
            next_client: AtomicU64::new(0),
            epoch: Arc::new(AtomicU64::new(0)),
            identities: channel_identities(),
            display_version: Arc::new(AtomicU64::new(0)),
        })
    }

    async fn http_state(address: SocketAddr) -> std::io::Result<String> {
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut socket =
                std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
            socket.set_read_timeout(Some(Duration::from_secs(1)))?;
            socket.set_write_timeout(Some(Duration::from_secs(1)))?;
            socket.write_all(
                b"GET /api/state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )?;
            let mut response = String::new();
            socket.read_to_string(&mut response)?;
            Ok(response)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    async fn assert_publisher_exit(reason: &'static str) {
        let (golden, schema) = load_golden();
        let app = test_app(DevState::new(golden, schema));
        let stopping = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (fail, failed) = tokio::sync::oneshot::channel::<()>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let mut service = tokio::spawn(serve_running(
            listener,
            app,
            stopping,
            move |_fatal| async move {
                let _ = failed.await;
                match reason {
                    "error" => Err("injected publisher failure".into()),
                    "panic" => panic!("injected publisher panic"),
                    _ => Ok(()),
                }
            },
            async move {
                let _ = stopped.await;
            },
        ));
        assert!(http_state(address)
            .await
            .unwrap()
            .starts_with("HTTP/1.1 200"));
        fail.send(()).unwrap();
        let early = tokio::time::timeout(Duration::from_secs(1), &mut service).await;
        let still_responding = if early.is_err() {
            http_state(address)
                .await
                .is_ok_and(|response| response.starts_with("HTTP/1.1 200"))
        } else {
            false
        };
        let _ = stop.send(());
        let (stopped_early, result) = match early {
            Ok(result) => (true, result.unwrap()),
            Err(_) => (
                false,
                tokio::time::timeout(Duration::from_secs(2), service)
                    .await
                    .expect("test shutdown did not release service")
                    .unwrap(),
            ),
        };
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        assert!(
            stopped_early,
            "publisher {reason} left /api/state responding={still_responding}"
        );
        let error = result.expect_err("unexpected publisher exit reported clean shutdown");
        let expected = match reason {
            "error" => "injected publisher failure",
            "panic" => "injected publisher panic",
            _ => "publisher exited",
        };
        assert!(error.contains(expected), "publisher cause lost: {error}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_error_stops_http() {
        assert_publisher_exit("error").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_panic_stops_http() {
        assert_publisher_exit("panic").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_unexpected_success_stops_http() {
        assert_publisher_exit("success").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_real_acquisition_panic_stops_http_and_finishes_recording() {
        let root = std::env::temp_dir().join(format!(
            "websa-publisher-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut recordings = recording::Manager::open(root.clone()).unwrap();
        recordings
            .start_configured(
                "uid",
                "service",
                "publisher-failure",
                "iq",
                "wav",
                "retained",
            )
            .unwrap()
            .wait()
            .unwrap();
        let task = recordings.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.recordings = Some(recordings);
        let app = test_app(state);
        let stopping = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (entered, entry) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::sync_channel::<()>(1);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let publisher_app = app.clone();
        let publisher_stopping = stopping.clone();
        let mut service = tokio::spawn(serve_running(
            listener,
            app,
            stopping,
            move |fatal| {
                super::super::publisher_loop(
                    publisher_app,
                    publisher_stopping,
                    Some(fatal),
                    super::super::PublisherTestHooks {
                        before_step: Some(Box::new(move || {
                            let _ = entered.send(());
                            blocked.recv().unwrap();
                            panic!("injected acquisition worker panic");
                        })),
                        ..Default::default()
                    },
                )
            },
            async move {
                let _ = stopped.await;
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), entry)
            .await
            .unwrap()
            .unwrap();
        assert!(http_state(address)
            .await
            .unwrap()
            .starts_with("HTTP/1.1 200"));
        release.send(()).unwrap();
        let early = tokio::time::timeout(Duration::from_secs(1), &mut service).await;
        let still_responding = if early.is_err() {
            http_state(address)
                .await
                .is_ok_and(|response| response.starts_with("HTTP/1.1 200"))
        } else {
            false
        };
        let _ = stop.send(());
        let (stopped_early, result) = match early {
            Ok(result) => (true, result.unwrap()),
            Err(_) => (
                false,
                tokio::time::timeout(Duration::from_secs(2), service)
                    .await
                    .expect("test shutdown did not release service")
                    .unwrap(),
            ),
        };
        let saved: Value =
            serde_json::from_slice(&std::fs::read(root.join(&task).join("task.json")).unwrap())
                .unwrap();
        let reopened = recording::Manager::open(root.clone()).unwrap();
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(saved["state"], "failed");
        assert_eq!(saved["fault"]["code"], "SERVICE_INTERRUPTED");
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        assert!(
            stopped_early,
            "acquisition JoinError left /api/state responding={still_responding}"
        );
        let error = result.expect_err("acquisition panic reported clean shutdown");
        assert!(
            error.contains("acquisition worker")
                && error.contains("injected acquisition worker panic"),
            "{error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_during_requested_shutdown_is_clean() {
        let (golden, schema) = load_golden();
        let app = test_app(DevState::new(golden, schema));
        let stopping = Arc::new(AtomicBool::new(false));
        let publisher_stopping = stopping.clone();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let service = tokio::spawn(serve_running(
            listener,
            app,
            stopping,
            move |_fatal| async move {
                while !publisher_stopping.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            },
            async move {
                let _ = stopped.await;
            },
        ));
        assert!(http_state(address)
            .await
            .unwrap()
            .starts_with("HTTP/1.1 200"));
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), service)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_fatal_notice_closes_http_before_waiting_for_owned_work() {
        let root = std::env::temp_dir().join(format!(
            "websa-publisher-drain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.recordings = Some(recording::Manager::open(root.clone()).unwrap());
        let app = test_app(state);
        let stopping = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (release, blocked) = std::sync::mpsc::sync_channel::<()>(1);
        let (fail, failed) = tokio::sync::oneshot::channel::<()>();
        let (entered, entry) = tokio::sync::oneshot::channel::<()>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let mut service = tokio::spawn(serve_running(
            listener,
            app.clone(),
            stopping,
            move |fatal| async move {
                let owned = tokio::task::spawn_blocking(move || {
                    let _ = entered.send(());
                    let _ = blocked.recv();
                });
                let _ = failed.await;
                let _ = fatal.send("injected fatal publisher error before drain".into());
                owned.await.unwrap();
                Ok(())
            },
            async move {
                let _ = stopped.await;
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), entry)
            .await
            .unwrap()
            .unwrap();
        assert!(http_state(address)
            .await
            .unwrap()
            .starts_with("HTTP/1.1 200"));
        fail.send(()).unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        let owners_retained = app.state.lock().unwrap().recordings.is_some();
        let returned_before_work = service.is_finished();
        // Release both controls before assertions, including on the unfixed path.
        let _ = release.send(());
        let _ = stop.send(());
        let result = tokio::time::timeout(Duration::from_secs(2), &mut service)
            .await
            .expect("released publisher work did not finish shutdown")
            .unwrap();
        let reopened = recording::Manager::open(root.clone()).unwrap();
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            closed,
            "fatal publisher notice left the listener accepting connections"
        );
        assert!(
            owners_retained && !returned_before_work,
            "lifecycle tore down owners before publisher work returned"
        );
        assert!(result
            .unwrap_err()
            .contains("injected fatal publisher error before drain"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publisher_exit_async_panic_retains_pending_preparation_until_drained() {
        let root = std::env::temp_dir().join(format!(
            "websa-publisher-panic-drain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.recordings = Some(recording::Manager::open(root.clone()).unwrap());
        let app = test_app(state);
        let stopping = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (release, blocked) = std::sync::mpsc::sync_channel::<()>(1);
        let (entered, entry) = tokio::sync::oneshot::channel::<()>();
        let preparation_done = Arc::new(AtomicBool::new(false));
        let worker_done = preparation_done.clone();
        let preparing: super::super::PublisherPreparation =
            tokio::task::spawn_blocking(move || {
                let _ = entered.send(());
                let _ = blocked.recv();
                worker_done.store(true, Ordering::Release);
                (0, Vec::new(), Err("unused test preparation".into()))
            });
        let (fail, failed) = tokio::sync::oneshot::channel::<()>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let mut stop = Some(stop);
        let publisher_app = app.clone();
        let publisher_stopping = stopping.clone();
        let mut service = tokio::spawn(serve_running(
            listener,
            app.clone(),
            stopping,
            move |fatal| {
                super::super::publisher_loop(
                    publisher_app,
                    publisher_stopping,
                    Some(fatal),
                    super::super::PublisherTestHooks {
                        before_loop: Some(Box::pin(async move {
                            let _ = failed.await;
                            panic!("injected async publisher panic");
                        })),
                        preparing: Some(preparing),
                        ..Default::default()
                    },
                )
            },
            async move {
                let _ = stopped.await;
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), entry)
            .await
            .unwrap()
            .unwrap();
        assert!(http_state(address)
            .await
            .unwrap()
            .starts_with("HTTP/1.1 200"));
        fail.send(()).unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        if !closed {
            // Exercise cleanup on the unsupervised implementation as well, so
            // this regression detects the lost preparation handle independently.
            let _ = stop.take().unwrap().send(());
        }
        let early = tokio::time::timeout(Duration::from_millis(100), &mut service).await;
        let returned_before_work = early.is_ok();
        let owners_retained = app.state.lock().unwrap().recordings.is_some();
        let _ = release.send(());
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
        let result = match early {
            Ok(result) => result.unwrap(),
            Err(_) => tokio::time::timeout(Duration::from_secs(2), service)
                .await
                .expect("released preparation did not finish shutdown")
                .unwrap(),
        };
        let reopened = recording::Manager::open(root.clone()).unwrap();
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            closed && owners_retained && !returned_before_work,
            "async panic cleanup: listener_closed={closed}, owners_retained={owners_retained}, returned_before_preparation={returned_before_work}"
        );
        assert!(preparation_done.load(Ordering::Acquire));
        assert!(result
            .unwrap_err()
            .contains("injected async publisher panic"));
    }

    #[test]
    fn failed_server_stop_still_finishes_recordings_and_attempts_sdk_shutdown() {
        let root = std::env::temp_dir().join(format!(
            "websa-shutdown-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut recordings = recording::Manager::open(root.clone()).unwrap();
        recordings
            .start_configured(
                "uid",
                "service",
                "shutdown-test",
                "iq",
                "wav",
                "Retained recording",
            )
            .unwrap()
            .wait()
            .unwrap();
        let task = recordings.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.recordings = Some(recordings);
        state.receiver_workers = Some(receiver_workers::ReceiverWorkers::new().unwrap());
        let app = Arc::new(App {
            state: StateMutex::new(state),
            clients: PLMutex::new(Vec::new()),
            next_client: AtomicU64::new(0),
            epoch: Arc::new(AtomicU64::new(0)),
            identities: channel_identities(),
            display_version: Arc::new(AtomicU64::new(0)),
        });
        let mut sdk_attempted = false;
        let error = stop_owners_with(
            app.clone(),
            |_| Err("injected server stop failure".into()),
            || {
                sdk_attempted = true;
                Err("injected SDK shutdown failure".into())
            },
        )
        .unwrap_err();
        assert!(sdk_attempted);
        assert!(error.contains("injected server stop failure"));
        assert!(error.contains("injected SDK shutdown failure"));
        let state = app.state.lock().unwrap();
        assert!(state.recordings.is_none());
        assert!(state.receiver_workers.is_none());
        drop(state);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(root.join(&task).join("task.json")).unwrap())
                .unwrap();
        assert_eq!(saved["task_id"], task);
        assert_eq!(saved["state"], "failed");
        assert_eq!(saved["fault"]["code"], "SERVICE_INTERRUPTED");
        // Reopening proves the worker joined and the directory lock was released.
        let reopened = recording::Manager::open(root.clone()).unwrap();
        assert_eq!(reopened.snapshots()[0]["task_id"], task);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}
