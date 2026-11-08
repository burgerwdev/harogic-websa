//! Fake-IQS capture contracts exercise the production pipeline and controller.
use super::*;

fn configs() -> Vec<receiver_workers::ChannelConfig> {
    let mut configs: Vec<_> = (1..=MAX_CHANNELS as u32)
        .map(receiver_workers::ChannelConfig::for_slot)
        .collect();
    configs[0].freq = 100e6;
    configs
}

fn config(shim: String, display: i32) -> PipelineConfig {
    PipelineConfig {
        center: 100e6,
        listen: 100e6,
        fine: 2,
        display_decimate: display,
        front_end: hardware_iqs::tests::FRONT_END,
        shim,
        taps: 16,
        aux: Vec::new(),
        channel_budget: MAX_CHANNELS,
        output_rate_budget: COARSE_RATE * MAX_CHANNELS as f64,
    }
}

fn clear_injection_files() {
    for path in [
        "/tmp/websa_fake_iqs_fail.txt",
        "/tmp/websa_fake_iqs_open_fail.txt",
        "/tmp/websa_fake_iqs_reconfig_fail.txt",
        "/tmp/websa_fake_iqs_auto.txt",
    ] {
        let _ = std::fs::remove_file(path);
    }
}

fn narrow_passband_state() -> DevState {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    state.sdr_center = 100e6;
    state.sdr_listen = 100e6;
    state.sdr_decimate = 2048;
    state.hardware_shim = Some("clock-only; no SDK calls".into());
    state.sdr_channels[0].processing_enabled = false;
    state.sdr_channels[0].baseband = Some(receiver_workers::BasebandConfig {
        passband_hz: 5_000.0,
        transition_hz: 1_000.0,
        minimum_rate_hz: 950_000.0,
    });
    let fs = IQ_FS / 16.0;
    let bandwidth = 2_200_000.0;
    state.ddc = Some(SendDdc(
        Ddc::configured_service(
            state.sdr_center,
            bandwidth,
            &mut None,
            16,
            fs,
            &state.channel_configs(),
        )
        .unwrap(),
    ));
    state
}

fn app_with_state(state: DevState) -> Arc<App> {
    Arc::new(App {
        state: StateMutex::new(state),
        clients: PLMutex::new(Vec::new()),
        next_client: AtomicU64::new(0),
        epoch: Arc::new(AtomicU64::new(0)),
        display_version: Arc::new(AtomicU64::new(0)),
        identities: channel_identities(),
    })
}

#[tokio::test(flavor = "current_thread")]
async fn capture_clock_legacy_preflight_and_commit_use_source_rate() {
    let app = app_with_state(narrow_passband_state());
    let before = app.state.lock().unwrap().stream_registry();
    let request = json!({"channel":1,"freq":100_050_000.0});
    let mut admission = service_preflight::preflight(&app, "SET_SDR_CHANNEL", &request)
        .await
        .expect("compatible IQ-only tune rejected during preflight");
    let mut state = app.state.lock().unwrap();
    admission
        .dispatch(&mut state, "SET_SDR_CHANNEL", &request)
        .unwrap();
    let after = state.stream_registry();
    assert_eq!(
        after[0].epoch, before[0].epoch,
        "local tune restarted shared capture"
    );
    assert_ne!(after[0].channel_id, before[0].channel_id);
    assert_eq!(after[0].input_rate, IQ_FS / 16.0);
    assert_eq!(after[0].frequency_hz, 100_050_000.0);
}

#[tokio::test(flavor = "current_thread")]
async fn capture_clock_native_preflight_uses_source_rate_and_actual_identity() {
    let app = app_with_state(narrow_passband_state());
    let before = app.state.lock().unwrap().receiver_snapshot();
    let receiver = &before["receivers"][0];
    let mut config = receiver["confirmed"].clone();
    config["baseband"]["frequency_hz"] = json!(100_050_000.0);
    let request = json!({"type":"configure","request_id":"source-clock-preview",
        "service_id":before["service_id"],"receiver_uid":receiver["receiver_uid"],
        "expected_revision":receiver["revision"],"config":config});
    let response =
        multichannel::preflight(State(app.clone()), HeaderMap::new(), request.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["accepted"], true, "{body}");
    assert_eq!(body["capture_change"], false);
    let after = app.state.lock().unwrap().receiver_snapshot();
    assert_eq!(after["capture"]["epoch"], before["capture"]["epoch"]);
    assert_eq!(after["receivers"][0]["revision"], receiver["revision"]);
    assert_eq!(after["receivers"][0]["confirmed"], receiver["confirmed"]);
}

#[test]
fn capture_clock_live_and_last_known_channel_retune_keeps_compatible_source() {
    let mut state = narrow_passband_state();
    let fs = IQ_FS / 16.0;
    let bandwidth = 2_200_000.0;
    let before = state.capture_snapshot();
    let mut next = state.channel_configs()[0].clone();
    next.freq += 50_000.0;
    let reference = service_ddc::source_plan(fs, &next).unwrap();
    service_ddc::validate_source_frequency(
        state.sdr_center,
        bandwidth,
        fs,
        16,
        next.freq,
        &reference,
    )
    .unwrap();
    let live = state.channel_recapture(&next).unwrap();
    state.last_capture = state.live_capture_parameters();
    state.ddc = None;
    let last_known = state.capture_snapshot();
    let retired = state.channel_recapture(&next).unwrap();
    assert_eq!(before["input_rate_hz"], fs);
    assert_eq!(last_known["input_rate_hz"], fs);
    assert_eq!(last_known["epoch"], before["epoch"]);
    assert_eq!(last_known["parameters_state"], "last_known");
    assert!(
        live.is_none() && retired.is_none(),
        "compatible IQ tune requested recapture: live={}, last_known={}",
        live.is_some(),
        retired.is_some()
    );
}

#[test]
fn capture_clock_local_display_reads_the_actual_source_rate() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    state.sdr_center = 100e6;
    state.sdr_listen = 100e6;
    let fs = IQ_FS / 16.0;
    state.ddc = Some(SendDdc(
        Ddc::configured_service(
            state.sdr_center,
            2_200_000.0,
            &mut None,
            16,
            fs,
            &state.channel_configs(),
        )
        .unwrap(),
    ));
    state.step_sdr(0, 0, 0, 0);
    assert_eq!(state.sdr_actual["iq_rate"], fs);
}

#[test]
fn capture_contract_usable_bandwidth_does_not_rewrite_source_clock() {
    let _guard = hardware_iqs::tests::TEST_IQS_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_injection_files();
    let mut pipeline = Pipeline::start_service(
        config(
            hardware_iqs::tests::compile_fake_iqs_narrow_bandwidth_shim(),
            1,
        ),
        configs(),
    )
    .unwrap();
    let initial = pipeline.snapshot.clone();
    let mut frames = Vec::new();
    let mut error = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match pipeline.poll() {
            Ok(output) => frames.extend(output.frames),
            Err(failure) => {
                error = Some(failure);
                break;
            }
        }
        if frames.iter().any(|frame| frame.len() > 56) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let stopped = pipeline.stop();
    drop(pipeline);
    assert_eq!(initial.usable_bandwidth, 25_000_000.0);
    assert_eq!(
        initial.input_rate, IQ_FS,
        "usable passband changed source clock; downstream error={error:?}"
    );
    assert_eq!(initial.device_decimate, 1);
    assert!(initial.registry.iter().all(|row| row.input_rate == IQ_FS));
    assert!(error.is_none(), "{error:?}");
    assert!(stopped.is_ok(), "{}", stopped.err().unwrap_or_default());
    assert!(
        frames.iter().any(|frame| frame.len() > 56),
        "no captured IQ output"
    );
}

#[test]
fn capture_contract_failed_reconfigure_preserves_terminal_cause_and_reopens() {
    let _guard = hardware_iqs::tests::TEST_IQS_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_injection_files();
    let channels = configs();
    let mut pipeline = Pipeline::start_service(
        config(
            hardware_iqs::tests::compile_fake_iqs_reconfigure_failure_shim(),
            16,
        ),
        channels.clone(),
    )
    .unwrap();
    let old_epoch = pipeline.snapshot.epoch;
    let mut frames = Vec::new();
    let ready = Instant::now() + Duration::from_secs(3);
    while frames.iter().all(|frame: &IqFrame| frame.len() <= 56) && Instant::now() < ready {
        frames.extend(pipeline.poll().unwrap().frames);
        thread::sleep(Duration::from_millis(1));
    }
    let command = pipeline.command(move |ddc| {
        ddc.recapture_service_configs(100e6, 16, hardware_iqs::tests::FRONT_END, 16, &channels)
    });
    let mut terminal = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match pipeline.poll() {
            Ok(output) => frames.extend(output.frames),
            Err(error) => {
                terminal = Some(error);
                break;
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    let diagnostics = pipeline.terminal_failure();
    let stopped = pipeline.stop();
    drop(pipeline);
    let mut reopened = Pipeline::start_service(
        config(hardware_iqs::tests::compile_fake_iqs_shim(), 16),
        configs(),
    )
    .unwrap();
    let new_epoch = reopened.snapshot.epoch;
    let reopened_stop = reopened.stop();
    drop(reopened);
    let error = command.expect_err("injected SDK reconfiguration unexpectedly succeeded");
    assert!(error.contains("-110"), "{error}");
    assert!(
        frames.iter().any(|frame| frame.len() > 56),
        "no valid source prefix"
    );
    assert!(frames.iter().all(
        |frame| u32::from_le_bytes(frame.header()[36..40].try_into().unwrap()) as u64 == old_epoch
    ));
    assert_eq!(
        terminal.as_deref(),
        Some(error.as_str()),
        "publisher lost the actual SDK failure"
    );
    assert_eq!(
        diagnostics
            .as_ref()
            .and_then(|value| value["error"].as_str()),
        Some(error.as_str())
    );
    assert_eq!(stopped.err().as_deref(), Some(error.as_str()));
    assert_ne!(new_epoch, old_epoch);
    assert!(reopened_stop.is_ok());
}

#[test]
fn capture_contract_owner_panic_is_terminal_and_releases_for_reopen() {
    let _guard = hardware_iqs::tests::TEST_IQS_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_injection_files();
    let shim = hardware_iqs::tests::compile_fake_iqs_shim();
    let mut pipeline = Pipeline::start_service(config(shim.clone(), 16), configs()).unwrap();
    let old_epoch = pipeline.snapshot.epoch;
    let mut frames = Vec::new();
    let ready = Instant::now() + Duration::from_secs(3);
    while frames.iter().all(|frame: &IqFrame| frame.len() <= 56) && Instant::now() < ready {
        frames.extend(pipeline.poll().unwrap().frames);
        thread::sleep(Duration::from_millis(1));
    }
    let command = pipeline.command(|_| panic!("injected PFB control panic"));
    let mut terminal = None;
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        match pipeline.poll() {
            Ok(output) => frames.extend(output.frames),
            Err(error) => {
                terminal = Some(error);
                break;
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    let diagnostics = pipeline.terminal_failure();
    let stopped = pipeline.stop();
    drop(pipeline);
    let mut reopened = Pipeline::start_service(config(shim, 16), configs()).unwrap();
    let new_epoch = reopened.snapshot.epoch;
    let reopened_stop = reopened.stop();
    drop(reopened);
    let command_error = command.expect_err("PFB control panic unexpectedly succeeded");
    assert!(
        command_error.contains("injected PFB control panic"),
        "{command_error}"
    );
    assert!(
        frames.iter().any(|frame| frame.len() > 56),
        "valid source prefix disappeared"
    );
    assert!(frames.iter().all(
        |frame| u32::from_le_bytes(frame.header()[36..40].try_into().unwrap()) as u64 == old_epoch
    ));
    assert!(
        terminal
            .as_deref()
            .is_some_and(|error| error.contains("injected PFB control panic")),
        "owner panic left polling healthy or lost cause: {terminal:?}"
    );
    assert_eq!(terminal.as_deref(), Some(command_error.as_str()));
    assert!(diagnostics
        .as_ref()
        .and_then(|value| value["error"].as_str())
        .is_some_and(|error| error.contains("injected PFB control panic")));
    assert!(stopped
        .err()
        .as_deref()
        .is_some_and(|error| error.contains("injected PFB control panic")));
    assert_ne!(new_epoch, old_epoch);
    assert!(reopened_stop.is_ok());
}
