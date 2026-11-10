//! Commit one admitted command and publish its state/stream boundaries.
//! HTTP and WebSocket keep their own response formats around this operation.
use super::{service_preflight::Admission, service_stream, App, CommandError};
use serde_json::Value;
use std::sync::{atomic::Ordering, Arc};

pub(super) struct Reply {
    pub revision: u64,
    pub status: Value,
    pub changed: bool,
}

pub(super) async fn execute(
    app: Arc<App>,
    mut admission: Admission,
    command: String,
    request: Value,
) -> Result<Result<Reply, CommandError>, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let mut state = app.state.lock().unwrap();
        // A stale ticket can fail without changing anything. Only changes made
        // by this dispatch may retire the current transport on an error.
        let revision = state.config_version;
        let same_capture = state.swp.mode == "sdr" && command != "SET_MODE" && command != "CONNECT";
        let result = admission.dispatch(&mut state, &command, &request);
        // Failed hardware transitions may still retire a stream. Publish those
        // boundaries before releasing the command's prepared/retired owners.
        let snapshot = if result.is_ok() || state.config_version != revision {
            if result
                .as_ref()
                .is_ok_and(|changed| *changed && !same_capture)
                || result.is_err()
            {
                app.epoch.fetch_add(1, Ordering::AcqRel);
            }
            service_stream::deliver_retiring(&mut state, &app);
            app.display_version
                .store(state.freq_version, Ordering::Release);
            state.publish_identities(&app.identities);
            Some(state.live_status_snapshot())
        } else {
            None
        };
        drop(state);
        let status = snapshot.map(|snapshot| {
            let status = snapshot.build();
            app.state.publish_status(snapshot.revision, status.clone());
            (snapshot.revision, status)
        });
        drop(admission);
        result.map(|changed| {
            let (revision, status) = status.expect("successful commands publish their snapshot");
            Reply {
                revision,
                status,
                changed,
            }
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{channel_identities, load_golden, service_preflight, DevState, StateMutex};
    use parking_lot::Mutex;
    use serde_json::json;
    use std::sync::atomic::AtomicU64;

    async fn run_command(app: &Arc<App>, request: Value) -> Value {
        let command = request["cmd"].as_str().unwrap().to_owned();
        let admission = service_preflight::preflight(app, &command, &request)
            .await
            .unwrap_or_else(|error| panic!("{command} admission: {error}"));
        execute(app.clone(), admission, command.clone(), request)
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("{command} execution: {}", error.message))
            .status
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preset_mode_entry_capture_acknowledges_the_relocated_receiver_revision() {
        for mode in ["std", "rta"] {
            let (mut golden, schema) = load_golden();
            // The SAN-90 sweep preset differs from the 1 GHz SDR preset. The
            // ordinary fixture uses 1 GHz for both and never moves the receiver.
            golden["preset_defaults"]["center"] = json!(4_510_004_000.0);
            let app = Arc::new(App {
                state: StateMutex::new(DevState::new(golden, schema)),
                clients: Mutex::new(Vec::new()),
                next_client: AtomicU64::new(0),
                epoch: Arc::new(AtomicU64::new(0)),
                identities: channel_identities(),
                display_version: Arc::new(AtomicU64::new(0)),
            });
            run_command(&app, json!({"cmd":"SET_MODE", "mode":mode})).await;
            let preset = run_command(&app, json!({"cmd":"SET_PRESET"})).await;
            let center = preset["req"]["swp"]["center"].as_f64().unwrap();
            assert!((center - 1e9).abs() > 1e9);
            let entered = run_command(&app, json!({"cmd":"SET_MODE", "mode":"sdr"})).await;
            let before = entered["sdr"]["channels"][0].clone();
            let capture = run_command(&app, json!({"cmd":"SET_SDR", "center":center, "decimate":16})).await;
            let channel = &capture["sdr"]["channels"][0];
            assert_ne!(channel["freq"], before["freq"], "capture must relocate C1");
            let control = &channel["control"];
            assert_ne!(control["revision"], before["control"]["revision"],
                "the capture acknowledgement must publish the new receiver revision");
            // Use precisely the acknowledgement's identity, with no directory
            // GET or polling tick that could refresh stale metadata in between.
            let restored = run_command(&app, json!({"cmd":"SET_SDR_CHANNEL", "channel":1,
                "service_id":control["service_id"], "receiver_uid":control["receiver_uid"],
                "expected_revision":control["revision"], "freq":center,
                "mode":"am", "ifbw":12000, "enabled":true})).await;
            assert_eq!(restored["sdr"]["channels"][0]["freq"], json!(center));
            assert_eq!(restored["sdr"]["channels"][0]["control"]["receiver_uid"], control["receiver_uid"]);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn superseded_admission_does_not_retire_current_transport_or_publish_state() {
        let (golden, schema) = load_golden();
        let app = Arc::new(App {
            state: StateMutex::new(DevState::new(golden, schema)),
            clients: Mutex::new(Vec::new()),
            next_client: AtomicU64::new(0),
            epoch: Arc::new(AtomicU64::new(0)),
            identities: channel_identities(),
            display_version: Arc::new(AtomicU64::new(0)),
        });
        let request = json!({"cmd":"SET_POINTS", "points":2000});
        let admission = service_preflight::preflight(&app, "SET_POINTS", &request)
            .await
            .unwrap();
        // A newer command commits while this ticket waits for the state owner.
        {
            let mut state = app.state.lock().unwrap();
            crate::dispatch(&mut state, "SET_POINTS", &json!({"points":3000})).unwrap();
            let snapshot = state.live_status_snapshot();
            app.state
                .publish_status(snapshot.revision, snapshot.build());
        }
        let before = app.state.status_with_revision();
        let result = execute(app.clone(), admission, "SET_POINTS".into(), request)
            .await
            .unwrap();
        assert!(matches!(result, Err(error) if error.code == "configuration_changed"));
        assert_eq!(
            app.epoch.load(Ordering::Acquire),
            0,
            "rejected stale tickets cannot invalidate queued IQ"
        );
        assert_eq!(app.state.status_with_revision(), before);
        assert_eq!(app.state.lock().unwrap().swp.points, 3000);
    }
}
