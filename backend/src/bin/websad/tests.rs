use super::*;

include!("auto_recovery_tests.rs");
include!("configuration_transaction_tests.rs");

// SDK session registration is process-global. Run handoff failures in a
// fresh test process so stateless registration cannot affect other tests.
pub(super) fn isolated_handoff_test(name: &str) -> bool {
    const KEY: &str = "WEBSA_ISOLATED_HANDOFF_TEST";
    if std::env::var(KEY).ok().as_deref() == Some(name) { return true; }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(KEY, name).status().expect("run isolated SDK handoff test");
    assert!(status.success(), "isolated handoff regression failed: {name}");
    false
}

pub(super) fn register_stateless_handoff_library() {
    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
    }
    // libc has none of the SDK session symbols. Register the actual loaded
    // library through the production ABI path, as an old stateless shim is.
    let library = unsafe { dlopen(c"libc.so.6".as_ptr(), hardware_swp::SDK_DLOPEN_FLAGS) };
    assert!(!library.is_null(), "load stateless library");
    unsafe { hardware_swp::register_sdk_session(library).unwrap(); }
    assert!(hardware_swp::release_sdk_session().unwrap_err().contains("cannot release"));
}

#[tokio::test(flavor="current_thread")]
async fn failed_server_handoff_retires_receiver_tails_and_publishes_http_and_ws_state() {
    if !isolated_handoff_test("tests::failed_server_handoff_retires_receiver_tails_and_publishes_http_and_ws_state") { return; }
    register_stateless_handoff_library();
    for websocket in [false, true] {
        let (app, url) = start_ws_server().await;
        let mut control = connect_ws(&url).await;
        wait_for_client(&app).await;
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden.clone(), schema.clone());
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_demod = "am".into();
        state.sdr_if_bw = 6000.0;
        let mut source = service_fixture::FixtureSources::new();
        source.configure(&state.channel_configs()).unwrap();
        let registry = source.registry();
        let initial_iq = source.step().unwrap();
        let tail_iq = source.step().unwrap();
        let initial = state.process_receiver_frames_with_registry(&initial_iq, &registry, true).unwrap();

        // Compute the uninterrupted receiver result from the very same IQ,
        // so a lost or repeated command tail changes the observed bytes.
        let mut reference = DevState::new(golden, schema);
        reference.sdr_center=state.sdr_center;
        reference.sdr_listen=state.sdr_listen;
        reference.sdr_demod=state.sdr_demod.clone();
        reference.sdr_if_bw=state.sdr_if_bw;
        reference.process_receiver_frames_with_registry(&initial_iq, &registry, true).unwrap();
        let tail = reference.process_receiver_frames_with_registry(&tail_iq, &registry, true).unwrap();
        assert!(tail.audio.iter().any(|frame|u32::from_le_bytes(frame[12..16].try_into().unwrap())>0));

        state.fixture_sources=Some(source);
        state.pending_iq=tail_iq;
        state.hardware_shim=Some("registered-stateless-library".into());
        state.sdr_actual=json!({"center":100e6,"ref":-20.0});
        state.actual=json!({"center":100e6});
        state.rta_actual=json!({"center":100e6});
        state.gnss_value=Some(json!({"lock":1}));
        state.auto.last_peak=Some(-30.0);
        state.last_freq=Some(b"FREQ-retired".to_vec());
        state.publish_identities(&app.identities);
        *app.state.lock().unwrap()=state;
        let mut receivers=Vec::new();
        for audio in [true, false] {
            let (tx, rx)=mpsc::channel(64);
            app.clients.lock().push(Client {
                id:if audio {100} else {101},tx,dropped:Arc::new(AtomicU64::new(0)),
                decode_pending_bytes:Arc::new(AtomicU64::new(0)),
                overflow:Arc::new(AtomicBool::new(false)),receiver_failed:Arc::new(AtomicBool::new(false)),
                wake:Arc::new(Notify::new()),epoch:app.epoch.clone(),status_revision:app.state.revision(),
                display_version:app.display_version.clone(),iq:false,aux:false,audio,decode:!audio,channel:Some(1),receiver_uid:None,
                identities:app.identities.clone(),latest:Arc::new(PLMutex::new((None,None))),
            });
            receivers.push((audio,rx));
        }
        let initial_audio=initial.audio.clone();
        let initial_decode=initial.decode.clone();
        {
            let state=app.state.lock().unwrap();
            service_stream::deliver_receiver(&state,&app,initial,false);
        }
        // Leave ordinary predecessors queued while the command admits its
        // retiring successors under the next capture epoch.
        let old_epoch=app.epoch.load(Ordering::Acquire);
        let request=json!({"cmd":"SET_MODE","mode":"server","request_id":"failed-handoff"});
        let error=if websocket {
            control.send(WsMessage::Text(request.to_string().into())).await.unwrap();
            let error=next_text(&mut control).await;
            assert_eq!(error["cmd"],"ERROR");
            assert!(error["msg"].as_str().unwrap().contains("cannot release"));
            error
        } else {
            let response=api_config(State(app.clone()),HeaderMap::new(),request.to_string()).await;
            assert_eq!(response.status(),StatusCode::BAD_REQUEST);
            let bytes=axum::body::to_bytes(response.into_body(),usize::MAX).await.unwrap();
            let error:Value=serde_json::from_slice(&bytes).unwrap();
            assert!(error["error"].as_str().unwrap().contains("cannot release"));
            error
        };
        assert_eq!(error["code"],"device_error");
        assert_eq!(error["response_to"],"SET_MODE");
        assert_eq!(error["request_id"],"failed-handoff");
        assert_eq!(app.epoch.load(Ordering::Acquire),old_epoch+1);
        assert_eq!(app.display_version.load(Ordering::Acquire),1);
        assert!(app.identities.iter().all(|id|id.load(Ordering::Acquire)==0));
        let published=app.state.status_with_revision().1;
        assert_eq!(published["mode"],"sdr");
        assert_eq!(published["config_version"],1);
        assert_eq!(published["connected"],false);
        assert!(published["sdr"]["actual"].get("center").is_none());
        assert!(published["gnss"].is_null());
        {
            let mut state=app.state.lock().unwrap();
            assert!(state.pending_iq.is_empty());
            assert!(state.pending_receiver.audio.is_empty() && state.pending_receiver.decode.is_empty());
            assert!(state.fixture_sources.is_none());
            assert!(state.auto.last_peak.is_none());
            // Acquisition may restart in the unchanged SDR mode only after
            // the error has delivered its retiring output.
            let mut next=service_fixture::FixtureSources::new();
            next.configure(&state.channel_configs()).unwrap();
            let next_registry=next.registry();
            assert_ne!(registry[0].epoch,next_registry[0].epoch);
            let iq=next.step().unwrap();
            state.fixture_sources=Some(next);
            state.publish_identities(&app.identities);
            let output=state.process_receiver_frames_with_registry(&iq,&next_registry,true).unwrap();
            service_stream::deliver_receiver(&state,&app,output,false);
        }
        for (audio,mut rx) in receivers {
            let mut subscriber=subscriber_stream::SubscriberStream::default();
            let initial=if audio {&initial_audio} else {&initial_decode};
            let mut packet=rx.try_recv().expect("queued predecessor");
            for (index,expected) in initial.iter().enumerate() {
                if audio && index>0 {packet=rx.try_recv().expect("queued audio predecessor");}
                assert_eq!(packet.epoch,old_epoch);
                assert!(packet.epoch_current(app.epoch.load(Ordering::Acquire)),"admitted receiver predecessor survives capture transition");
                let frame=packet.frames().nth(if audio {0} else {index}).unwrap();
                let frames=subscriber.frames(frame).expect("initial sequence remains contiguous");
                let Message::Binary(bytes)=&frames[0] else {panic!("expected receiver bytes")};
                assert_eq!(bytes.as_ref(),expected.as_slice());
            }
            let expected=if audio {&tail.audio} else {&tail.decode};
            let mut packet=rx.try_recv().expect("retiring tail cannot be lost on failed handoff");
            for (index,frame) in expected.iter().enumerate() {
                if audio && index>0 {packet=rx.try_recv().expect("retiring audio tail");}
                assert_eq!(packet.epoch,old_epoch+1);
                let message=packet.frames().nth(if audio {0} else {index}).expect("retiring tail remains admitted");
                assert!(packet.epoch_current(app.epoch.load(Ordering::Acquire)));
                let frames=subscriber.frames(message).expect("retiring successor follows queued predecessor");
                let message=frames.into_iter().next().unwrap();
                let Message::Binary(bytes)=message else {panic!("expected receiver bytes")};
                assert_eq!(bytes.as_ref(),frame.as_slice());
            }
            let packet=rx.try_recv().expect("fresh capture start follows every retiring tail");
            let message=packet.frames().next().expect("fresh start remains admitted");
            let Message::Binary(bytes)=message else {panic!("expected fresh receiver start")};
            if audio {
                assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()),0);
                assert_ne!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()),registry[0].epoch);
            } else {
                let event:Value=serde_json::from_slice(&bytes[8..]).unwrap();
                assert_eq!(event["seq"],0);assert_eq!(event["event"]["type"],"start");
                assert_ne!(event["epoch"],json!(registry[0].epoch));
            }
        }
        control.close(None).await.unwrap();
    }
}

#[test]
fn server_mode_guards_commands_without_changing_settings_or_opening_hardware() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "server".into();
    state.hardware_shim = Some("must-not-be-opened.so".into());
    state.link_up = false;
    let before = state.status();
    for (command, data) in [
        ("CONNECT", json!({})), ("SET_PRESET", json!({})),
        ("SET_REF", json!({"ref":-10})), ("SET_FREQ", json!({"center":100e6})),
        ("SET_AMP", json!({"atten":10})), ("SET_SDR", json!({"center":100e6})),
        ("SET_SDR_CHANNEL", json!({"mode":"am"})),
    ] {
        assert_eq!(dispatch(&mut state, command, &data).unwrap_err().code, "server_mode_active");
    }
    state.refresh_gnss();
    assert!(state.swp_hw.is_none() && state.rta_hw.is_none() && state.pnm_hw.is_none());
    assert!(state.pipeline.is_none() && state.ddc.is_none());
    let after = state.status();
    for key in ["req", "config_version", "mode", "server"] { assert_eq!(before[key], after[key], "{key}"); }
    assert!(dispatch(&mut state,"SET_MODE",&json!({"mode":"std"})).unwrap());
    assert_eq!(state.swp.mode,"std");
}

#[tokio::test]
async fn publisher_in_server_mode_never_captures_or_polls_the_sdk() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "server".into();
    state.hardware_shim = Some("must-not-be-opened.so".into());
    let app = Arc::new(App {state:StateMutex::new(state),clients:PLMutex::new(Vec::new()),
        next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
        identities:channel_identities(),
        display_version:Arc::new(AtomicU64::new(0))});
    let stopping = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(publisher(app.clone(), stopping.clone(), None));
    tokio::time::sleep(Duration::from_millis(250)).await;
    stopping.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(1), task).await.expect("publisher did not stop").unwrap().unwrap();
    let state=app.state.lock().unwrap();
    assert_eq!(state.tick,0);
    assert!(state.swp_hw.is_none() && state.rta_hw.is_none() && state.pnm_hw.is_none());
    assert!(state.pipeline.is_none() && state.ddc.is_none());
    assert!(state.swp_error.is_none() && state.rta_error.is_none() && state.ddc_error.is_none());
    assert!(state.gnss_value.is_none());
}

#[test]
fn premium_reference_requires_hardware_capability_not_gnss_state() {
    let (mut golden, schema) = load_golden();
    golden["has_docxo"] = json!(true);
    let mut state = DevState::new(golden, schema);
    state.hardware_shim = Some("unused-capability-test".into());
    state.device_detail = Some(hardware_swp::DeviceInfo::default().detail());
    state.gnss_value = Some(json!({"lock":1,"docxo":1,"docxo_mode":1}));
    state.ref_clock = "external".into();
    state.actual = json!({"refclk_src":0});
    let before = state.status();
    let error = dispatch(&mut state, "SET_REFCK", &json!({"mode":"premium"})).unwrap_err();
    assert_eq!(error.code, "unsupported");
    assert_eq!(state.ref_clock, "external");
    let hardware_configuration=|mut value:Value| {
        // Logical identity discovery and monotonic action tokens are
        // observations; all hardware and channel settings must remain equal.
        for channel in value["sdr"]["channels"].as_array_mut().unwrap() {channel.as_object_mut().unwrap().remove("control");}
        value
    };
    assert_eq!(hardware_configuration(state.status()), hardware_configuration(before.clone()));
    assert_eq!(before["has_docxo"], false);
    assert_eq!(before["has_premium_ref_clock"], false);
    assert_eq!(before["actual"]["refclk_src"], 0);
    for (ocxo_type, internal_ocxo, has_docxo, premium) in [
        (0, 0, false, false), (1, 0, false, true), (2, 0, true, true),
        (0, 1, false, true), (-1, -1, false, false),
    ] {
        state.device_detail = Some(hardware_swp::DeviceInfo {
            ocxo_type, internal_ocxo, ..hardware_swp::DeviceInfo::default()
        }.detail());
        let status = state.status();
        assert_eq!(status["has_docxo"], has_docxo);
        assert_eq!(status["has_premium_ref_clock"], premium);
    }
}

#[test]
fn standard_reference_sources_remain_selectable_without_docxo() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.golden["has_docxo"] = json!(false);
    state.golden["has_premium_ref_clock"] = json!(false);
    for source in ["internal", "external", "external_forced"] {
        dispatch(&mut state, "SET_REFCK", &json!({"mode":source})).unwrap();
        assert_eq!(state.ref_clock, source);
        assert_eq!(state.status()["ref_clock"], source);
    }
}

#[test]
fn capture_decimate_covers_tap_passband_and_usable_edges() {
    let center = 100e6;
    for taps in [4, 8, 16] {
        let flat = coarse_passband_hz(COARSE_RATE, taps).unwrap();
        for d in [1, 2, 4, 8, 16] {
            let edge = IQ_FS * 0.8 / d as f64 / 2.0;
            let frequency = center + edge - flat;
            let chosen = capture_decimate(center, frequency, &[], 2048, taps);
            assert!(chosen <= d);
            assert!(capture_decimate(center, frequency + 1.0, &[], 2048, taps) < d || d == 1);
            assert!(capture_decimate(center, center, &[frequency + 1.0], 2048, taps) < d || d == 1);
        }
    }
}
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[test]
fn short_taps_fit_center_and_reject_incompatible_channel_changes_atomically() {
    for streaming in [false, true] {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_decimate = 1;
        state.sdr_pfb_taps_per_phase = 4;
        dispatch(&mut state, "SET_SDR_DEMOD", &json!({"mode": "wfm", "ifbw": 200_000})).unwrap();
        if streaming {
            state.ddc = Some(SendDdc(Ddc::configured_service(100e6, 50e6, &mut None, 4, IQ_FS, &state.channel_configs()).unwrap()));
        }
        let boundary = 100e6 + BIN_SPACING * 0.49;
        dispatch(&mut state, "SET_SDR", &json!({"center": boundary})).unwrap();
        assert_eq!(state.sdr_listen, 100e6);
        assert!((state.sdr_center - boundary).abs() > 1.0);
        assert!((state.sdr_center - boundary).abs() < BIN_SPACING / 2.0);
        let plan = service_ddc::channel_plan(COARSE_RATE, &state.channel_configs()[0]).unwrap();
        Ddc::validate_service_frequency(state.sdr_center, 50e6, IQ_FS, 4, state.sdr_listen, &plan).unwrap();
        dispatch(&mut state, "SET_SDR", &json!({"center": 100e6})).unwrap();
        let old_stream = state.ddc.as_ref().map(|ddc| &*ddc.0.stream as *const PfbStream);
        assert!(dispatch(&mut state, "SET_SDR_TUNE", &json!({"listen": boundary})).is_err());
        assert_eq!(state.sdr_listen, 100e6);
        let old_aux = state.sdr_aux.clone();
        assert!(dispatch(&mut state, "SET_SDR_DDC", &json!({"channel": 2, "enabled": true, "freq": boundary, "mode": "wfm", "ifbw": 200_000})).is_err());
        assert_eq!(state.sdr_aux, old_aux);
        assert_eq!(state.sdr_center, 100e6);
        assert_eq!(state.ddc.as_ref().map(|ddc| &*ddc.0.stream as *const PfbStream), old_stream);
        if streaming {
            let mut candidate=state.channel_configs();
            candidate[0].mode="am".into();
            candidate[0].if_bw=6000.0;
            let selected=state.ddc.as_ref().unwrap().0.stream.bins().len();
            service_cost::admit_service(&candidate,IQ_FS,4,selected).unwrap();
        }
        dispatch(&mut state, "SET_SDR_DEMOD", &json!({"mode": "am", "ifbw": 6000})).unwrap();
        dispatch(&mut state, "SET_SDR_TUNE", &json!({"listen": boundary})).unwrap();
        assert!(dispatch(&mut state, "SET_SDR_DEMOD", &json!({"mode": "wfm", "ifbw": 200_000, "volume": 0.1})).is_err());
        assert_eq!(state.sdr_demod, "am");
        assert_eq!(state.sdr_if_bw, 6000.0);
        if streaming {
            service_cost::admit_service(&state.channel_configs(),IQ_FS,16,5).unwrap();
        }
        dispatch(&mut state, "SET_SDR", &json!({"pfb_taps_per_phase": 16})).unwrap();
        if streaming {
            let mut candidate=state.channel_configs();
            candidate[0].mode="wfm".into();
            candidate[0].if_bw=200_000.0;
            let selected=state.ddc.as_ref().unwrap().0.stream.bins().len();
            service_cost::admit_service(&candidate,IQ_FS,16,selected).unwrap();
        }
        dispatch(&mut state, "SET_SDR_DEMOD", &json!({"mode": "wfm", "ifbw": 200_000})).unwrap();
        assert!(dispatch(&mut state, "SET_SDR", &json!({"pfb_taps_per_phase": 4})).is_err());
        assert_eq!(state.sdr_pfb_taps_per_phase, 16);
        if let Some(ddc) = state.ddc.as_mut() {
            assert_eq!(ddc.0.listen, boundary);
            assert_eq!(ddc.0.stream.taps_per_phase(), 16);
            assert!(ddc.0.step().unwrap().iter().any(|frame| &frame[..4] == b"IQBF"));
        }
    }
}

#[test]
fn tap_replan_delivers_pending_primary_and_aux_tails() {
    let center = 100e6;
    let aux_freq = center + BIN_SPACING;
    let build = || {
        let mut ddc = Ddc::configured(center, center, 20, 50e6, &mut None, &[aux_freq], 16).unwrap();
        ddc.configure_aux(2, true, aux_freq).unwrap();
        let raw = vec![1200i16; 16240 * 2];
        for seq in 0..3 {
            ddc.stream.submit(seq, (seq + 1) * 16240, &raw).unwrap();
        }
        ddc
    };
    let mut reference = build();
    reference.stream.flush_pending().unwrap();
    let mut expected = reference.drain().unwrap();
    expected.push(reference.framer.finish_native().unwrap().unwrap());
    expected.push(reference.aux.get_mut(&2).unwrap().2.finish_native().unwrap().unwrap());
    let mut switched = build();
    let old_epoch = switched.stream.epoch();
    let frames = switched.replan_with_taps(center, 20, &[(2, aux_freq)], 8).unwrap();
    assert_ne!(reference.stream.epoch(), switched.stream.epoch());
    assert_eq!(frames.len(), expected.len());
    for (frame, expected) in frames.iter().zip(&expected) {
        assert_eq!(u32::from_le_bytes(frame[36..40].try_into().unwrap()) as u64, old_epoch);
        assert_eq!(u32::from_le_bytes(expected[36..40].try_into().unwrap()) as u64, reference.stream.epoch());
        assert_eq!(&frame[..36], &expected[..36]);
        assert_eq!(&frame[40..], &expected[40..]);
    }
    assert_eq!(switched.stream.taps_per_phase(), 8);
}

#[test]
fn pfb_taps_rebuild_preserves_aux_and_rejects_without_losing_plan() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    state.sdr_center = 100e6;
    state.sdr_listen = 100e6;
    state.sdr_decimate = 1;
    dispatch(&mut state, "SET_SDR", &json!({"pfb_taps_per_phase": 8})).unwrap();
    state.ddc = Some(SendDdc(Ddc::configured_service(100e6, 50e6, &mut None, state.sdr_pfb_taps_per_phase, IQ_FS, &state.channel_configs()).unwrap()));
    let aux_command = json!({"channel": 2, "enabled": true, "freq": 100e6 + BIN_SPACING});
    let mut configs = state.channel_configs();
    configs[1] = state.parse_channel_config(&aux_command).unwrap();
    let selected = state.ddc.as_ref().unwrap().0.stream.bins().len();
    service_cost::admit_service(&configs, IQ_FS, state.sdr_pfb_taps_per_phase, selected).unwrap();
    service_cost::admit_service(&configs, IQ_FS, state.sdr_pfb_taps_per_phase, selected + 1).unwrap();
    dispatch(&mut state, "SET_SDR_DDC", &aux_command).unwrap();
    for taps in [4, 16, 8] {
        service_cost::admit_service(&state.channel_configs(), IQ_FS, taps, 6).unwrap();
        dispatch(&mut state, "SET_SDR", &json!({"pfb_taps_per_phase": taps})).unwrap();
        let status = state.status();
        assert_eq!(status["req"]["sdr"]["pfb_taps_per_phase"], taps);
        assert_eq!(status["sdr"]["actual"]["pfb_taps_per_phase"], taps);
        let frames = {
            let ddc = &mut state.ddc.as_mut().unwrap().0;
            assert_eq!(ddc.stream.taps_per_phase(), taps);
            assert_eq!(ddc.aux[&2].1, 100e6 + BIN_SPACING);
            ddc.step().unwrap()
        };
        state.process_receiver_frames(&frames).unwrap();
        assert!(frames.iter().any(|frame| &frame[..4] == b"IQBF"));
    }
    for invalid in [json!(0), json!(12), json!(8.5), json!("8")] {
        assert!(dispatch(&mut state, "SET_SDR", &json!({"pfb_taps_per_phase": invalid})).is_err());
    }
    let old_stream = &*state.ddc.as_ref().unwrap().0.stream as *const PfbStream;
    state.sdr_if_bw = 1e6;
    assert!(dispatch(&mut state, "SET_SDR", &json!({"pfb_taps_per_phase": 4})).is_err());
    assert_eq!(state.sdr_pfb_taps_per_phase, 8);
    assert_eq!(&*state.ddc.as_ref().unwrap().0.stream as *const PfbStream, old_stream);
    state.sdr_if_bw = 6000.0;
    let front_end = state.front_end(state.sdr_ref);
    let ddc = &mut state.ddc.as_mut().unwrap().0;
    let old_input = ddc.input_pos;
    let old_stream = &*ddc.stream as *const PfbStream;
    let mut invalid = ddc.service_configs.clone().unwrap();
    invalid[1].enabled = true;
    invalid[1].freq = 200e6;
    assert!(ddc.recapture_service_configs(100e6, 1, front_end, 4, &invalid).is_err());
    assert_eq!(&*ddc.stream as *const PfbStream, old_stream);
    assert_eq!(ddc.input_pos, old_input);
    assert!(ddc.step().unwrap().iter().any(|frame| &frame[..4] == b"IQBF"));
    let configs = ddc.service_configs.clone().unwrap();
    ddc.recapture_service_configs(100e6, 1, front_end, 8, &configs).unwrap();
    assert_eq!(ddc.stream.taps_per_phase(), 8);
    assert!(ddc.step().unwrap().iter().any(|frame| &frame[..4] == b"IQBF"));
}

#[test]
fn low_rate_negative_bins_tune_and_aux_use_live_geometry() {
    let center = 100e6;
    let fs = IQ_FS / 8.0;
    let listen = center - BIN_SPACING;
    for taps in [4, 8, 16] {
        let mut ddc = Ddc::configured_at_rate(center, listen, 20, fs * 0.8, &mut None, &[center - 2.0 * BIN_SPACING], taps, fs).unwrap();
        assert_eq!(ddc.stream.bins()[0] as usize, ddc.stream.bin_count() - 1);
        ddc.configure_aux(2, true, center - 2.0 * BIN_SPACING).unwrap();
        ddc.tune(center - 2.0 * BIN_SPACING, 20).unwrap();
        ddc.replan_with_taps(listen, 20, &[(2, center - 2.0 * BIN_SPACING)], taps).unwrap();
        assert_eq!(ddc.stream.input_sample_rate(), fs);
        let frames = ddc.step().unwrap();
        assert!(frames.iter().any(|frame| {
            &frame[..4] == b"IQBF"
                && f64::from_le_bytes(frame[24..32].try_into().unwrap()) == listen
        }));
    }
}

#[test]
fn channelizer_keeps_every_wire_frame_across_bounded_submissions() {
    let mut ddc = Ddc::new(100_000_000.0, 100_000_000.0).expect("PFB/fine unavailable");
    let mut frames = ddc.step().expect("first block");
    frames.extend(ddc.step().expect("second block"));
    assert!(frames.len() >= 4);
    let mut samples = 0;
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(&frame[..4], b"IQBF");
        let seq = u32::from_le_bytes(frame[8..12].try_into().unwrap());
        let count = u32::from_le_bytes(frame[12..16].try_into().unwrap()) as usize;
        assert_eq!(seq, index as u32);
        assert_eq!(frame.len(), 56 + count * 8);
        assert_eq!(
            f64::from_le_bytes(frame[16..24].try_into().unwrap()),
            ddc.channel_rate()
        );
        samples += count;
    }
    assert!(samples > 5_000);

    let listen = 100_000_000.0 - 62_500_000.0 / 128.0;
    let mut wrapped = Ddc::new(100_000_000.0, listen).expect("wrapped bin rejected");
    let frames = wrapped.step().expect("wrapped bin failed");
    assert_eq!(
        f64::from_le_bytes(frames[0][24..32].try_into().unwrap()),
        listen
    );
    assert!(Ddc::new(100_000_000.0, 130_000_000.0).is_none());
}

#[test]
fn same_bin_tune_command_keeps_capture_and_resets_iqbf_sequence() {
    let center = 100_000_000.0;
    let schema =
        serde_json::from_str(include_str!("../../../../tests/fixtures/contract/schema.json"))
            .unwrap();
    let mut state = DevState::new(json!({"preset_defaults": {}}), schema);
    state.swp.mode = "sdr".into();
    state.sdr_center = center;
    state.sdr_listen = center;
    state.sdr_decimate = 1; // display window = full 50 MHz capture
    state.ddc = Some(SendDdc(Ddc::configured_service(center, 50e6, &mut None, state.sdr_pfb_taps_per_phase, IQ_FS, &state.channel_configs()).unwrap()));
    let frames = state.ddc.as_mut().unwrap().0.step().unwrap();
    state.process_receiver_frames(&frames).unwrap();
    let ddc = &state.ddc.as_ref().unwrap().0;
    let old_input = ddc.input_pos;
    let old_identity = ddc.channel_registry()[0].channel_id;
    let old_stream = (&*ddc.stream) as *const PfbStream;
    let selected = ddc.stream.bins().len();
    service_cost::admit_service(&state.channel_configs(), IQ_FS, state.sdr_pfb_taps_per_phase, selected).unwrap();
    service_cost::admit_service(&state.channel_configs(), IQ_FS, state.sdr_pfb_taps_per_phase, selected + 1).unwrap();
    dispatch(
        &mut state,
        "SET_SDR_TUNE",
        &json!({"listen": center + 10_000.0}),
    )
    .unwrap();
    let ddc = &mut state.ddc.as_mut().unwrap().0;
    assert_eq!(ddc.input_pos, old_input);
    assert_ne!(ddc.channel_registry()[0].channel_id, old_identity);
    assert_eq!((&*ddc.stream) as *const PfbStream, old_stream);
    assert!(state.pending_iq.iter().all(|frame| &frame[..4] == b"IQBF"));
    let frames = ddc.step().unwrap();
    assert!(!frames.is_empty());
    assert_eq!(u32::from_le_bytes(frames[0][8..12].try_into().unwrap()), 0);
    assert_eq!(
        f64::from_le_bytes(frames[0][24..32].try_into().unwrap()),
        center + 10_000.0
    );
    assert_eq!(ddc.input_pos, old_input + 329 * 16240);
    state.process_receiver_frames(&frames).unwrap();
    // One step of 500 kHz stays inside the +/-2 provisioned bins: same PFB stream.
    dispatch(
        &mut state,
        "SET_SDR_TUNE",
        &json!({"listen": center + 500_000.0}),
    )
    .unwrap();
    let ddc = &state.ddc.as_ref().unwrap().0;
    assert_eq!((&*ddc.stream) as *const PfbStream, old_stream);
    // Far outside the plan: a new plan on the same input; the channel is not torn down.
    dispatch(
        &mut state,
        "SET_SDR_TUNE",
        &json!({"listen": center + 5_000_000.0}),
    )
    .unwrap();
    let ddc = &mut state.ddc.as_mut().unwrap().0;
    assert_eq!(ddc.listen, center + 5_000_000.0);
    let frames = ddc.step().unwrap();
    assert_eq!(u32::from_le_bytes(frames[0][8..12].try_into().unwrap()), 0);
    assert_eq!(
        f64::from_le_bytes(frames[0][24..32].try_into().unwrap()),
        center + 5_000_000.0
    );
}

#[test]
fn registry_tracks_aux_retune_replan_rejections_and_mode_exit() {
    let (golden, schema) = load_golden();
    let center = 100e6;
    let aux_frequency = center + BIN_SPACING;
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    state.sdr_center = center;
    state.sdr_listen = center;
    state.ddc = Some(SendDdc(Ddc::configured(center, center, 20, 50e6,
        &mut None, &[aux_frequency], 16).unwrap()));
    let ddc = &mut state.ddc.as_mut().unwrap().0;
    ddc.configure_aux(2, true, aux_frequency).unwrap();
    let initial = ddc.channel_registry();
    assert_eq!(initial.iter().map(|entry| entry.channel).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(initial[0].state, "active");
    assert_eq!(initial[0].input_rate, ddc.stream.input_sample_rate());
    assert_eq!(initial[0].group_delay_samples, ddc.bank.channel_metadata(1).unwrap().1);
    let old_epoch = initial[0].epoch;
    let old_aux_id = initial[1].channel_id;
    ddc.packet().unwrap();
    assert!(Arc::ptr_eq(&initial, &ddc.channel_registry()));
    assert!(ddc.configure_aux(2, true, center + 100e6).is_err());
    assert!(ddc.replan_with_taps(center, 20, &[], 3).is_err());
    assert!(Arc::ptr_eq(&initial, &ddc.channel_registry()));
    ddc.configure_aux(2, false, 0.0).unwrap();
    assert_eq!(ddc.channel_registry().len(), 1);
    ddc.configure_aux(2, true, aux_frequency).unwrap();
    let readded = ddc.channel_registry();
    assert!(readded[1].channel_id > old_aux_id);
    assert_eq!(readded[1].epoch, old_epoch);
    let old_primary_id = readded[0].channel_id;
    ddc.tune(center + 1000.0, 20).unwrap();
    let tuned = ddc.channel_registry();
    assert!(tuned[0].channel_id > readded[1].channel_id);
    assert!(tuned[0].channel_id > old_primary_id);
    assert_eq!(tuned[0].epoch, old_epoch);
    assert_eq!(tuned[0].frequency_hz, center + 1000.0);
    assert_eq!(tuned[1], readded[1]);
    let frames = ddc.step().unwrap();
    let marker = frames.iter().find(|frame| frame.starts_with(b"IQBF")
        && u32::from_le_bytes(frame[8..12].try_into().unwrap()) == 0).unwrap();
    assert_eq!(u32::from_le_bytes(marker[32..36].try_into().unwrap()), tuned[0].channel_id);
    assert_eq!(u32::from_le_bytes(marker[36..40].try_into().unwrap()), tuned[0].epoch);
    assert_eq!(f64::from_le_bytes(marker[48..56].try_into().unwrap()), tuned[0].group_delay_samples);
    let next_id = ddc.next_id;
    ddc.next_id = u32::MAX as u64;
    assert!(ddc.tune(center + 2000.0, 20).is_err());
    assert!(Arc::ptr_eq(&tuned, &ddc.channel_registry()));
    ddc.configure_aux(2, false, 0.0).unwrap();
    assert_eq!(ddc.channel_registry().len(), 1);
    ddc.next_id = next_id;
    ddc.replan_with_taps(center, 20, &[(2, aux_frequency)], 8).unwrap();
    assert_ne!(ddc.channel_registry()[0].epoch, old_epoch);
    let status = state.status();
    assert_eq!(status["channel_registry"], status["actual"]["channel_registry"]);
    assert_eq!(status["channel_registry"], status["sdr"]["actual"]["channel_registry"]);
    assert_eq!(status["channel_registry"][1]["frequency_hz"], aux_frequency);
    assert!(dispatch(&mut state, "SET_SDR_DDC", &json!({"channel":4294967298u64,"enabled":false})).is_err());
    assert_eq!(state.status()["channel_registry"], status["channel_registry"]);
    dispatch(&mut state, "SET_MODE", &json!({"mode":"std"})).unwrap();
    let status = state.status();
    assert_eq!(status["channel_registry"], json!([]));
    assert_eq!(status["actual"]["channel_registry"], json!([]));
    assert_eq!(status["sdr"]["actual"]["channel_registry"], json!([]));
}

#[test]
fn measured_iq_levels_reach_status_and_clear_on_mode_exit() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    for channel in state.sdr_channels.iter_mut() { channel.enabled = true; }
    let configs = state.channel_configs();
    let mut source = service_fixture::FixtureSources::new();
    source.configure(&configs).unwrap();
    let registry = source.registry();
    state.fixture_sources = Some(source);
    for (seq, amplitude) in [(1, 0.5_f32), (2, 0.05_f32)] {
        let frames: Vec<_> = registry.iter().flat_map(|row| {
            let magic = if row.channel == 1 { websa_channelizer::BasebandMagic::Primary }
                else { websa_channelizer::BasebandMagic::Aux };
            let encode = |seq, iq: &[f32]| websa_channelizer::encode_baseband(magic,
                seq, row.output_rate, row.frequency_hz, u64::from(row.channel_id), u64::from(row.epoch),
                if seq == 0 { 0 } else { (seq as u64 - 1) * 4096 },
                row.group_delay_samples, iq).unwrap();
            let iq = vec![amplitude / row.channel as f32, 0.0].repeat(4096);
            if seq == 1 { vec![encode(0, &[]), encode(seq, &iq)] }
            else { vec![encode(seq, &iq)] }
        }).collect();
        state.process_receiver_frames_with_registry(&frames, &registry, true).unwrap();
        let status = state.status_snapshot().build();
        let primary = status["sdr"]["level_dbfs"].as_f64().unwrap();
        assert!((primary - 20.0 * f64::from(amplitude).log10()).abs() < 1e-5);
        for channel in 1..=3 {
            let level = status["sdr"]["channels"][channel - 1]["level_dbfs"].as_f64().unwrap();
            assert!((level - primary + 20.0 * (channel as f64).log10()).abs() < 1e-5);
        }
    }
    state.swp.mode = "std".into();
    let status = state.status_snapshot().build();
    assert!(status["sdr"]["level_dbfs"].is_null());
    assert!(status["sdr"]["channels"].as_array().unwrap().iter()
        .all(|channel| channel["level_dbfs"].is_null()));
}

#[test]
fn mode_exit_discards_receiver_output_before_a_fresh_sdr_session() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    let configs = state.channel_configs();
    let mut source = service_fixture::FixtureSources::new();
    source.configure(&configs).unwrap();
    let old_registry = source.registry();
    let frames = source.step().unwrap();
    state.fixture_sources = Some(source);
    state.submit_receiver_frames(&frames).unwrap();
    state.receiver_workers.as_mut().unwrap().finish().unwrap();
    let frames = state.fixture_sources.as_mut().unwrap().step().unwrap();
    state.submit_receiver_frames(&frames).unwrap();

    dispatch(&mut state, "SET_MODE", &json!({"mode":"std"})).unwrap();
    assert!(state.receiver_workers.is_some());
    let retired = state.drain_receiver_frames().unwrap();
    assert!(retired.audio.is_empty() && retired.decode.is_empty());
    dispatch(&mut state, "SET_MODE", &json!({"mode":"sdr"})).unwrap();
    let mut source = service_fixture::FixtureSources::new();
    source.configure(&configs).unwrap();
    let registry = source.registry();
    assert_ne!(registry[0].epoch, old_registry[0].epoch);
    let frames = source.step().unwrap();
    let output = state.process_receiver_frames_with_registry(&frames, &registry, true).unwrap();
    assert!(output.decode.iter().any(|frame| {
        let event: Value = serde_json::from_slice(&frame[8..]).unwrap();
        event["epoch"] == registry[0].epoch && event["event"]["type"] == "start"
    }));
    assert!(output.audio.iter().all(|frame|
        u32::from_le_bytes(frame[36..40].try_into().unwrap()) == registry[0].epoch));
}

#[test]
fn synthetic_modes_emit_v2_identity_markers_and_exact_sample_clocks() {
    let mut sources = service_fixture::FixtureSources::new();
    let mut config = receiver_workers::ChannelConfig::for_slot(1);
    config.freq = 100.2e6;
    let mut previous_epoch = None;
    for (mode, rate) in [("cw", 48_000.0), ("ft8", 48_000.0), ("sstv", 8_000.0)] {
        config.mode = mode.into();
        sources.configure(&[config.clone()]).unwrap();
        let registry = sources.registry();
        let entry = &registry[0];
        assert_ne!(Some(entry.epoch), previous_epoch);
        previous_epoch = Some(entry.epoch);
        assert_eq!(entry.input_rate, rate);
        assert_eq!(entry.output_rate, rate);
        let frames = sources.step().unwrap();
        assert_eq!(frames.len(), 2);
        let count = (rate * 0.085) as usize;
        for (seq, frame) in frames.iter().enumerate() {
            assert_eq!(&frame[..4], b"IQBF");
            assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), 2);
            assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()), seq as u32);
            assert_eq!(u32::from_le_bytes(frame[32..36].try_into().unwrap()), entry.channel_id);
            assert_eq!(u32::from_le_bytes(frame[36..40].try_into().unwrap()), entry.epoch);
            assert_eq!(u64::from_le_bytes(frame[40..48].try_into().unwrap()), 0);
        }
        assert_eq!(frames[0].len(), 56);
        assert_eq!(frames[1].len(), 56 + count * 8);
        let next = sources.step().unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(u32::from_le_bytes(next[0][8..12].try_into().unwrap()), 2);
        assert_eq!(u64::from_le_bytes(next[0][40..48].try_into().unwrap()), count as u64);
        assert_eq!(u32::from_le_bytes(next[0][36..40].try_into().unwrap()), entry.epoch);
    }
}

#[test]
fn neural_nr_api_modes_keep_capture_settings_and_confirm_real_ids() {
    let (golden,schema)=load_golden();let mut state=DevState::new(golden,schema);
    let (_,schema)=load_golden();
    for command in ["SET_SDR_DEMOD","SET_SDR_CHANNEL"] {
        let params=schema["commands"][command]["params"].as_array().unwrap();
        let algorithm=params.iter().find(|p|p["name"]=="nr_algorithm").unwrap();
        assert_eq!(algorithm["choices"],json!(["wiener","dfn","gtcrn","ul-unas","fastenhancer"]));
    }
    state.swp.mode="sdr".into();
    let original=state.channel_configs();
    for id in ["dfn","gtcrn","ul-unas","fastenhancer"] {
        dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"channel":1,"nr":true,"nr_algorithm":id})).unwrap();
        let configs=state.channel_configs();
        assert_eq!(configs[0].nr_algorithm,id);assert!(configs[0].nr);
        let mut expected=original[0].clone();expected.nr=true;expected.nr_algorithm=id.into();
        assert_eq!(configs[0],expected);
        assert_eq!(state.status()["sdr"]["channels"][0]["nr_algorithm"],id);
    }
    let accepted=state.channel_configs();
    assert!(dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"channel":1,"nr_algorithm":"fake"})).is_err());
    assert_eq!(state.channel_configs(),accepted);
}

#[test]
fn am_sync_partial_configuration_is_strict_persistent_and_resettable() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    assert!(state.channel_configs().iter().all(|config| config.squelch == websa_dsp::audio::SQUELCH_OPEN_DBM));
    state.swp.mode = "sdr".into();
    dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"channel":1,"mode":"am",
        "volume":0.37,"agc":false,"squelch":-140.0,"nr":true,"nr_strength":0.2,
        "nr_algorithm":"wiener","nr_atten_db":7.0,"audio_rate":32000})).unwrap();
    let mut expected = state.channel_configs();
    let registry = state.stream_registry();
    let capture = (state.sdr_center, state.sdr_decimate, state.sdr_pfb_taps_per_phase);
    for enabled in [true, false, true] {
        dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"channel":1,"am_sync":enabled})).unwrap();
        expected[0].am_sync = enabled;
        expected[0].am_sync_generation += 1;
        assert_eq!(state.channel_configs(), expected);
        assert_eq!((state.sdr_center, state.sdr_decimate, state.sdr_pfb_taps_per_phase), capture);
        let current = state.stream_registry();
        assert_eq!((current[0].channel_id,current[0].epoch), (registry[0].channel_id,registry[0].epoch));
        let status = state.status();
        assert_eq!(status["sdr"]["channels"][0]["am_sync"], enabled);
        assert_eq!(status["sdr"]["am_sync"], enabled);
        assert_eq!(status["req"]["sdr"]["am_sync"], enabled);
        assert_eq!(status["sdr"]["am_sync_generation"], expected[0].am_sync_generation);
        assert_eq!(status["sdr"]["channels"][0]["am_sync_generation"], expected[0].am_sync_generation);
    }
    dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"am_sync":true,"am_sync_generation":999})).unwrap();
    assert_eq!(state.channel_configs(),expected,"duplicate/supplied tokens cannot advance or overwrite generation");
    for invalid in [json!("true"),json!(1),json!(null),json!([])] {
        assert!(dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"am_sync":invalid,"volume":1.0})).is_err());
        assert_eq!(state.channel_configs(),expected);
    }
    for mode in ["usb","am"] {
        dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"mode":mode})).unwrap();
        assert!(state.channel_configs()[0].am_sync);
    }
    dispatch(&mut state,"SET_PRESET",&json!({})).unwrap();
    assert!(state.channel_configs().iter().all(|config| !config.am_sync));
    assert!(state.channel_configs().iter().all(|config| config.squelch == websa_dsp::audio::SQUELCH_OPEN_DBM));
    let decoded: receiver_workers::ChannelConfig = serde_json::from_value(json!({})).unwrap();
    assert!(!decoded.am_sync);
    assert!(serde_json::from_value::<receiver_workers::ChannelConfig>(json!({"am_sync":"true"})).is_err());
}

#[test]
fn am_sync_live_toggle_keeps_aud2_clock_and_reports_real_carrier_lock() {
    use std::f64::consts::TAU;
    use websa_channelizer::{encode_baseband, BasebandMagic};
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"mode":"am","ifbw":6000,
        "agc":false,"volume":0.8,"squelch":-200.0})).unwrap();
    let registry = state.stream_registry();
    let row = &registry[0];
    let frame = |seq,index,iq:&[f32]| encode_baseband(BasebandMagic::Primary,seq,
        row.output_rate,row.frequency_hz,row.channel_id as u64,row.epoch as u64,
        index,0.0,iq).unwrap();
    state.process_receiver_frames_with_registry(&[frame(0,0,&[])],&registry,true).unwrap();
    let mut next_audio_seq = 0_u32;
    let mut next_audio_index = 0_u64;
    let mut locked = false;
    let mut fade_unlocked = false;
    let mut disabled_status = false;
    for block in 0..160_u32 {
        if block == 20 || block == 145 {
            dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"am_sync":block==20})).unwrap();
        }
        let first = block as u64 * 4096;
        let faded = (85..115).contains(&block);
        // Independent analytic IQ scenario: 40% AM at 700 Hz, +23 Hz carrier,
        // arbitrary initial phase, and a sustained RF fade while sync is requested.
        let iq: Vec<f32> = (first..first+4096).flat_map(|index| {
            let t=index as f64/row.output_rate;
            let amplitude=if faded {0.0} else {0.3*(1.0+0.4*(TAU*700.0*t).sin())};
            let phase=0.73+TAU*23.0*t;
            [(amplitude*phase.cos()) as f32,(amplitude*phase.sin()) as f32]
        }).collect();
        let output=state.process_receiver_frames_with_registry(&[frame(block+1,first,&iq)],&registry,true).unwrap();
        for audio in output.audio {
            assert_eq!(&audio[..4],b"AUD2");
            assert_eq!(u32::from_le_bytes(audio[8..12].try_into().unwrap()),next_audio_seq);
            assert_eq!(u64::from_le_bytes(audio[64..72].try_into().unwrap()),next_audio_index);
            assert_eq!(u32::from_le_bytes(audio[32..36].try_into().unwrap()),row.channel_id);
            assert_eq!(u32::from_le_bytes(audio[36..40].try_into().unwrap()),row.epoch);
            next_audio_seq+=1;
            next_audio_index+=u32::from_le_bytes(audio[12..16].try_into().unwrap()) as u64;
        }
        for decode in output.decode {
            let envelope: Value=serde_json::from_slice(&decode[8..]).unwrap();
            let event=&envelope["event"];
            if event["type"]!="am-info" {continue;}
            assert_eq!(event["syncEnabled"],(20..145).contains(&block));
            assert_eq!(event["amSyncGeneration"], if block<20 {0} else if block<145 {1} else {2});
            if (40..85).contains(&block) && event["syncLocked"]==true {
                locked=true;
                assert!((event["carrierOffsetHz"].as_f64().unwrap()-23.0).abs()<3.0);
                assert!((event["carrierHz"].as_f64().unwrap()
                    - (row.frequency_hz + event["carrierOffsetHz"].as_f64().unwrap())).abs() < 1e-6);
            }
            if (100..115).contains(&block) && event["syncLocked"]==false {
                fade_unlocked=true;
                assert!(event["carrierOffsetHz"].is_null());
                assert!(event["carrierHz"].is_null());
            }
            if block>=145 {
                disabled_status=true;
                assert_eq!(event["syncLocked"],false);
            }
        }
    }
    assert!(next_audio_index>48000,"real DSP PCM must cross the toggle boundaries");
    assert!(locked,"analytic AM carrier must acquire DSP lock");
    assert!(fade_unlocked,"requested sync must lose DSP lock during an RF fade");
    assert!(disabled_status,"disabled detector state must reach the consumer");
}

#[test]
fn am_sync_toggle_preserves_calibrated_power_and_closed_squelch_pcm() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"mode":"am","am_sync":false,
        "ifbw":6000,"agc":false,"squelch":-60.0,"volume":1.0})).unwrap();
    let registry=state.stream_registry();
    let mut next_sequence=0_u32;
    let mut next_audio_index=0_u64;
    // The source contains a real modulated AM carrier. Its calibrated RF
    // measurement is below the gate, so neither detector may emit it.
    for block in 0..14_u32 {
        state.observe_channel_power(&[state.sdr_listen-1000.0,state.sdr_listen,state.sdr_listen+1000.0],
            &[-120.0,-100.0,-120.0]);
        let measured=state.channel_power[0];
        if block>0 {
            dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"am_sync":block%2==1})).unwrap();
            assert_eq!(state.channel_power[0],measured);
        }
        let iq=state.fixture_sources.as_mut().unwrap().step().unwrap();
        let output=state.process_receiver_frames_with_registry(&iq,&registry,true).unwrap();
        for frame in output.audio {
            assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()),next_sequence);
            assert_eq!(u64::from_le_bytes(frame[64..72].try_into().unwrap()),next_audio_index);
            next_sequence+=1;
            next_audio_index+=u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64;
            assert!(frame[96..].chunks_exact(4).all(|sample|
                f32::from_le_bytes(sample.try_into().unwrap())==0.0),
                "a detector toggle must not open a calibrated closed squelch");
        }
    }
    assert!(next_audio_index>48000,"muted PCM must retain its real audio clock");
    dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"squelch":-140.0})).unwrap();
    let mut signal_energy=0.0_f64;
    for _ in 0..4 {
        let iq=state.fixture_sources.as_mut().unwrap().step().unwrap();
        let output=state.process_receiver_frames_with_registry(&iq,&registry,true).unwrap();
        signal_energy+=output.audio.iter().flat_map(|frame|frame[96..].chunks_exact(4))
            .map(|sample|f64::from(f32::from_le_bytes(sample.try_into().unwrap())).powi(2)).sum::<f64>();
    }
    assert!(signal_energy>1e-6,"opening the gate must expose the same real AM programme");
}

#[test]
fn aux_channel_uses_same_capture_without_leaking_into_primary_iq() {
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "sdr".into();
    state.sdr_center = 100e6;
    state.sdr_listen = 100e6;
    for channel in 1..=3 {
        dispatch(&mut state, "SET_SDR_CHANNEL", &json!({
            "channel": channel, "enabled": true,
            "freq": 100e6 + (channel - 1) as f64 * 100_000.0,
            "mode": "am", "ifbw": 6000
        })).unwrap();
    }
    let registry = state.stream_registry();
    assert_eq!(registry.len(), 3);
    assert_eq!(state.sdr_center, 100e6);
    for entry in registry.iter() {
        assert_eq!(entry.input_rate, 48_000.0);
        assert_eq!(entry.frequency_hz, 100e6 + (entry.channel - 1) as f64 * 100_000.0);
    }
    let frames = state.fixture_sources.as_mut().unwrap().step().unwrap();
    let (primary, aux): (Vec<_>, Vec<_>) = frames.into_iter().partition(|frame| frame.starts_with(b"IQBF"));
    assert!(primary.iter().any(|frame| frame.len() > 56));
    assert!(primary.iter().all(|frame| frame.starts_with(b"IQBF")
        && f64::from_le_bytes(frame[24..32].try_into().unwrap()) == 100e6));
    for channel in [2, 3] {
        let entry = registry.iter().find(|entry| entry.channel == channel).unwrap();
        assert!(aux.iter().any(|frame| frame.len() > 56 && frame.starts_with(b"IQAX")
            && u32::from_le_bytes(frame[32..36].try_into().unwrap()) == entry.channel_id
            && f64::from_le_bytes(frame[24..32].try_into().unwrap()) == entry.frequency_hz));
    }
    dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"channel": 2, "enabled": false})).unwrap();
    let registry = state.stream_registry();
    assert_eq!(registry.iter().map(|entry| entry.channel).collect::<Vec<_>>(), vec![1, 3]);
    let frames = state.fixture_sources.as_mut().unwrap().step().unwrap();
    let aux: Vec<_> = frames.into_iter().filter(|frame| frame.starts_with(b"IQAX")).collect();
    let remaining = registry.iter().find(|entry| entry.channel == 3).unwrap();
    assert!(aux.iter().any(|frame| frame.len() > 56));
    assert!(aux.iter().all(|frame| frame.starts_with(b"IQAX")
        && u32::from_le_bytes(frame[32..36].try_into().unwrap()) == remaining.channel_id));
}

#[test]
fn hardware_vendor_fft_retains_close_peaks_valley_and_true_frequency() {
    let freq: Vec<f64> = (0..16240).map(|i| 100e6 + i as f64 * 1000.0).collect();
    let mut dbm = vec![-100.0f32; freq.len()];
    // Adjacent peaks, a valley and a gap retain their native frequencies.
    dbm[8001] = -25.0;
    dbm[8002] = -95.0;
    dbm[8003] = -30.0;
    dbm[8004] = f32::NAN;
    let first = 3;
    let last = 16235;
    dbm[first] = -40.0;
    dbm[last] = -35.0;
    let lo = freq[first];
    let hi = freq[last];
    let frame = hardware_pan(7, lo, hi - lo, &freq, &dbm).unwrap();
    assert_eq!(&frame[..4], b"RTAF");
    assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), 7);
    let points = u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize;
    let power_at = 24 + points * 8;
    let decoded_freq: Vec<f64> = frame[24..power_at].chunks_exact(8)
        .map(|bytes| f64::from_le_bytes(bytes.try_into().unwrap())).collect();
    let decoded_power: Vec<f32> = frame[power_at..power_at + points * 4].chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect();
    assert_eq!(decoded_freq, freq[first..=last]);
    for source in [first, 8001, 8002, 8003, last] {
        let bin = decoded_freq.iter().position(|&hz| hz == freq[source]).unwrap();
        assert_eq!(decoded_power[bin], dbm[source]);
    }
    assert!(decoded_power[8004 - first].is_nan());
    let partial = hardware_pan(8, freq[0], 10_000.0, &freq, &dbm).unwrap();
    assert_eq!(f64::from_le_bytes(partial[24..32].try_into().unwrap()), freq[0]);
    assert!(hardware_pan(9, lo, hi - lo, &[2.0, 1.0], &[0.0, 0.0]).is_err());
}

#[tokio::test]
async fn slow_iq_consumer_gets_explicit_overflow_signal() {
    let (tx, _rx) = mpsc::channel::<Outbound>(1);
    let wake = Arc::new(Notify::new());
    let overflow = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let epoch = Arc::new(AtomicU64::new(0));
    let client = Client {
        id: 1,
        tx,
        decode_pending_bytes:Arc::new(AtomicU64::new(0)),
        dropped: dropped.clone(),
        overflow: overflow.clone(),
        receiver_failed: Arc::new(AtomicBool::new(false)),
        wake: wake.clone(),
        epoch,
        status_revision: Arc::new(AtomicU64::new(0)),
        display_version: Arc::new(AtomicU64::new(0)),
        iq: true,
        aux: false,
        audio: false,
        decode:false,
        channel:None,receiver_uid:None,
        identities:channel_identities(),
        latest: Arc::new(parking_lot::Mutex::new((None, None))),
    };
    send_or_count(&client, Message::Binary(vec![1].into()), 0);
    send_or_count(&client, Message::Binary(vec![2].into()), 0);
    assert!(overflow.load(Ordering::Acquire));
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    tokio::time::timeout(Duration::from_millis(100), wake.notified())
        .await
        .unwrap();
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start_ws_server() -> (Arc<App>, String) {
    let app = Arc::new(App {
        state: StateMutex::new(DevState::new(json!({"connected": false}), json!({}))),
        clients: PLMutex::new(Vec::new()),
        next_client: AtomicU64::new(0),
        epoch: Arc::new(AtomicU64::new(0)),
        display_version: Arc::new(AtomicU64::new(0)),
        identities:channel_identities(),
    });
    let router = Router::new()
        .route("/ws", get(ws_handler))
        .route("/ws/iq", get(multichannel::iq_ws))
    .route("/ws/audio", get(service_stream::audio_ws_handler))
        .route("/ws/decode", get(service_stream::decode_ws_handler))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind test listener");
    let address = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (app, format!("ws://{address}/ws"))
}

#[tokio::test(flavor = "current_thread")]
async fn command_errors_preserve_request_identity_across_http_and_websocket() {
    let (app, url) = start_ws_server().await;
    let (golden, schema) = load_golden();
    {
        let mut state = app.state.lock().unwrap();
        *state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
    }
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    let mut requests = [
        json!({"cmd":"SET_REF", "ref":"invalid"}),
        json!({"cmd":"SET_SDR", "pfb_taps_per_phase":3}),
        json!({"cmd":"BOGUS"}),
    ].to_vec();
    requests.push(json!({"cmd":"SET_REF", "ref":"invalid", "request_id":"acq-err"}));
    for request in requests {
        let cmd = request["cmd"].as_str().unwrap();
        let response = api_config(State(app.clone()), HeaderMap::new(), request.to_string()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let http: Value = serde_json::from_slice(&body).unwrap();
        ws.send(WsMessage::Text(request.to_string().into())).await.unwrap();
        let reply = next_text(&mut ws).await;
        if request.get("request_id").is_some() {
            assert_eq!(http["request_id"], "acq-err");
            assert_eq!(reply["request_id"], "acq-err");
        } else {
            assert!(http.get("request_id").is_none());
            assert!(reply.get("request_id").is_none());
        }
        assert_eq!(reply["cmd"], "ERROR");
        assert_eq!(reply["response_to"], cmd);
        assert_eq!(reply["code"], http["code"]);
        assert_eq!(reply["msg"], http["error"]);
        assert_eq!(reply.get("params"), http.get("params"));
        if cmd == "SET_SDR" {
            assert_eq!(reply["code"], "bad_parameter");
        }
    }
    let global_error = CommandError::new("device_error", "stream link failed").payload();
    {
        let clients = app.clients.lock();
        send_or_count(&clients[0], Message::Text(global_error.to_string().into()), app.epoch.load(Ordering::Acquire));
    }
    let reply = next_text(&mut ws).await;
    assert_eq!(reply, global_error);
    assert!(reply.get("response_to").is_none());
    ws.send(WsMessage::Text("invalid json".into())).await.unwrap();
    let malformed = next_text(&mut ws).await;
    assert_eq!(malformed["code"], "json_object_required");
    assert!(malformed.get("response_to").is_none());
}

#[test]
fn admission_error_reply_retains_details_without_attributing_global_errors() {
    let error = CommandError::new("configuration_changed", "configuration changed during admission; retry command")
        .param("cmd", "SET_AMP");
    let reply = error.reply_payload("SET_AMP");
    assert_eq!(reply["response_to"], "SET_AMP");
    assert_eq!(reply["params"]["cmd"], "SET_AMP");
    assert_eq!(reply["code"], "configuration_changed");
    assert_eq!(reply["msg"], "configuration changed during admission; retry command");
    assert!(error.payload().get("response_to").is_none());
    assert!(error.reply_payload("").get("response_to").is_none());
}

async fn connect_ws(url: &str) -> WsClient {
    tokio::time::timeout(Duration::from_secs(2), tokio_tungstenite::connect_async(url))
        .await
        .expect("WebSocket connect timed out")
        .expect("WebSocket connect failed")
        .0
}

async fn wait_for_client(app: &App) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.clients.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("no client registered");
}

fn fanout(app: &App, message: Message, epoch: u64) {
    let clients = app.clients.lock();
    assert!(!clients.is_empty(), "client disappeared");
    for client in clients.iter() {
        send_or_count(client, message.clone(), epoch);
    }
}

async fn next_ws(ws: &mut WsClient) -> WsMessage {
    tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("WebSocket read timed out")
        .expect("WebSocket closed")
        .expect("WebSocket read failed")
}

async fn next_text(ws: &mut WsClient) -> Value {
    match next_ws(ws).await {
        WsMessage::Text(text) => {
            serde_json::from_str(text.as_str()).expect("text reply is not JSON")
        }
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn next_binary(ws: &mut WsClient) -> Vec<u8> {
    match next_ws(ws).await {
        WsMessage::Binary(bytes) => bytes.to_vec(),
        other => panic!("expected binary frame, got {other:?}"),
    }
}

async fn expect_silence(ws: &mut WsClient, wait: Duration) {
    match tokio::time::timeout(wait, ws.next()).await {
        Err(_) => {}
        Ok(None) => panic!("socket closed while waiting for silence"),
        Ok(Some(Err(error))) => panic!("socket error while waiting for silence: {error}"),
        Ok(Some(Ok(frame))) => panic!("unexpected frame: {frame:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn publisher_keeps_spectrum_and_iq_live_through_receiver_failure_and_recovery() {
    // Exercise the production publisher, socket routing, controls and native
    // AM receiver together. Only the fault injection bypasses the wire API.
    struct PublisherTask(tokio::task::JoinHandle<Result<(), String>>);
    impl Drop for PublisherTask {
        fn drop(&mut self) { self.0.abort(); }
    }
    fn word(frame: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap())
    }
    fn identity(frame: &[u8]) -> (u32, u32) {
        (word(frame, 32), word(frame, 36))
    }
    fn check_status(value: &Value) {
        assert_ne!(value["cmd"], "ERROR", "unexpected service error: {value}");
        if value["cmd"] == "STATUS" {
            assert!(value["last_error"].is_null() || value["last_error"] == "",
                "receiver fault leaked into capture: {value}");
        }
    }
    // Native model preparation on a cold process can exceed the lightweight
    // socket helper's two-second timeout; retain a bounded integration wait.
    async fn next_ws(ws: &mut WsClient) -> WsMessage {
        tokio::time::timeout(Duration::from_secs(30), ws.next()).await
            .expect("publisher WebSocket read timed out")
            .expect("publisher WebSocket closed")
            .expect("publisher WebSocket read failed")
    }
    async fn next_binary(ws: &mut WsClient) -> Vec<u8> {
        match next_ws(ws).await {
            WsMessage::Binary(frame) => frame.to_vec(),
            other => panic!("expected publisher binary frame: {other:?}"),
        }
    }
    async fn command(ws: &mut WsClient, request: Value) -> Value {
        ws.send(WsMessage::Text(request.to_string().into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match next_ws(ws).await {
                    WsMessage::Text(text) => {
                        let value: Value = serde_json::from_str(&text).unwrap();
                        check_status(&value);
                        if value["response_to"] == request["cmd"] { return value; }
                    }
                    WsMessage::Binary(_) => {}
                    other => panic!("control socket stopped: {other:?}"),
                }
            }
        }).await.expect("control acknowledgement timed out")
    }
    async fn spectrum(ws: &mut WsClient) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match next_ws(ws).await {
                    WsMessage::Binary(frame) if frame.starts_with(b"RTAF") => return frame.to_vec(),
                    WsMessage::Binary(_) => {}
                    WsMessage::Text(text) => check_status(&serde_json::from_str::<Value>(&text).unwrap()),
                    other => panic!("display socket stopped: {other:?}"),
                }
            }
        }).await.expect("spectrum publication timed out")
    }
    async fn receiver_closed(ws: &mut WsClient) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match next_ws(ws).await {
                    WsMessage::Close(Some(close)) => {
                        assert_eq!(u16::from(close.code), 1011);
                        assert_eq!(close.reason, "receiver failed");
                        return;
                    }
                    WsMessage::Binary(_) => {} // Already-sent output may precede the close.
                    other => panic!("expected receiver failure close: {other:?}"),
                }
            }
        }).await.expect("failed receiver socket did not close")
    }
    async fn pcm(ws: &mut WsClient, expected: (u32, u32)) {
        let start = next_binary(ws).await;
        assert_eq!(&start[..4], b"AUD2");
        assert_eq!(identity(&start), expected);
        assert_eq!(word(&start, 8), 0);
        assert_eq!(word(&start, 12), 0);
        let samples = next_binary(ws).await;
        assert_eq!(&samples[..4], b"AUD2");
        assert_eq!(identity(&samples), expected);
        assert_eq!(word(&samples, 8), 1);
        assert!(word(&samples, 12) > 0, "AM must produce PCM, not only a start marker");
    }
    async fn decode(ws: &mut WsClient, expected: (u32, u32)) {
        let start = next_binary(ws).await;
        assert_eq!(&start[..4], b"SDE2");
        let start: Value = serde_json::from_slice(&start[8..]).unwrap();
        assert_eq!(start["channel_id"], expected.0);
        assert_eq!(start["epoch"], expected.1);
        assert_eq!(start["seq"], 0);
        assert_eq!(start["event"]["type"], "start");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let frame = next_binary(ws).await;
                assert_eq!(&frame[..4], b"SDE2");
                let event: Value = serde_json::from_slice(&frame[8..]).unwrap();
                assert_eq!(event["channel_id"], expected.0);
                assert_eq!(event["epoch"], expected.1);
                assert!(event["seq"].as_u64().unwrap() > 0);
                if event["event"]["type"] == "am-info" { return; }
            }
        }).await.expect("native AM decode telemetry timed out")
    }
    async fn new_iq_identity(ws: &mut WsClient, previous: (u32, u32)) -> (u32, u32) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let frame = next_binary(ws).await;
                assert_eq!(&frame[..4], b"IQBF");
                if identity(&frame) != previous {
                    assert_eq!(word(&frame, 8), 0, "replacement IQ identity must begin at zero");
                    assert_eq!(word(&frame, 12), 0);
                    return identity(&frame);
                }
            }
        }).await.expect("replacement IQ identity timed out")
    }

    let (app, url) = start_ws_server().await;
    {
        let (golden, schema) = load_golden();
        let mut state = app.state.lock().unwrap();
        *state = DevState::new(golden, schema);
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_decimate = 1;
    }
    let mut display = connect_ws(&url).await;
    command(&mut display, json!({"cmd":"SET_MODE","mode":"sdr"})).await;
    command(&mut display, json!({"cmd":"SET_SDR_CHANNEL","channel":1,
        "mode":"am","ifbw":6000.0,"squelch":-160.0})).await;
    let mut iq = connect_ws(&format!("{url}?iq")).await;
    let mut audio = connect_ws(&format!("{url}/audio?channel=1")).await;
    let mut events = connect_ws(&format!("{url}/decode?channel=1")).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.clients.lock().len() != 4 { tokio::task::yield_now().await; }
    }).await.expect("stream subscribers did not register");
    let _publisher = PublisherTask(tokio::spawn(publisher(app.clone(), Arc::new(AtomicBool::new(false)), None)));
    spectrum(&mut display).await;
    let first = next_binary(&mut iq).await;
    assert_eq!(&first[..4], b"IQBF");
    assert_eq!(word(&first, 8), 0);
    let original = identity(&first);
    pcm(&mut audio, original).await;
    decode(&mut events, original).await;

    let failure_tick = {
        let mut state = app.state.lock().unwrap();
        state.latch_receiver_failure("injected receiver failure".into());
        state.tick
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut notice, mut status) = (false, false);
        while !(notice && status) {
            match next_ws(&mut display).await {
                WsMessage::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    check_status(&value);
                    if value["cmd"] == "RECEIVER_ERROR" {
                        assert_eq!(value["error"], "injected receiver failure");
                        assert_eq!(value["scope"], "all_receivers");
                        assert_eq!(value["failed"], true);
                        notice = true;
                    }
                    if value["cmd"] == "STATUS" && value["sdr"]["receiver_error"].is_string() {
                        assert_eq!(value["sdr"]["receiver_error"], "injected receiver failure");
                        status = true;
                    }
                }
                WsMessage::Binary(_) => {}
                other => panic!("display closed after receiver failure: {other:?}"),
            }
        }
    }).await.expect("receiver failure was not published to the display");
    receiver_closed(&mut audio).await;
    receiver_closed(&mut events).await;

    // RTAF carries a configuration version, not a sequence. Observe two
    // changed spectra after the failure notice and check the acquisition tick.
    let failed_spectrum = spectrum(&mut display).await;
    let next_spectrum = spectrum(&mut display).await;
    assert_ne!(failed_spectrum, next_spectrum);
    assert!(app.state.lock().unwrap().tick > failure_tick + 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut previous = 0;
        loop {
            let frame = next_binary(&mut iq).await;
            assert_eq!(&frame[..4], b"IQBF");
            assert_eq!(identity(&frame), original, "failure must preserve capture identity");
            let sequence = word(&frame, 8);
            assert_eq!(sequence, previous + 1, "raw IQ must remain contiguous through failure");
            previous = sequence;
            // Each synthetic acquisition publishes one data packet. This
            // crosses the injection boundary even if earlier packets queued.
            if u64::from(sequence) > failure_tick + 2 { break; }
        }
    }).await.expect("raw IQ stopped after receiver failure");
    let tuned = command(&mut display, json!({"cmd":"SET_SDR_CHANNEL","channel":1,
        "freq":100_010_000.0})).await;
    assert_eq!(tuned["sdr"]["receiver_error"], "injected receiver failure");
    {
        let state = app.state.lock().unwrap();
        assert_eq!(state.sdr_listen, 100_010_000.0);
        assert_eq!(state.sdr_center, 100e6);
        assert_eq!(state.swp.mode, "sdr");
        assert!(state.link_up);
        assert!(state.ddc_error.is_none());
        assert_eq!(state.ddc_error_count, 0);
    }
    let tuned_identity = new_iq_identity(&mut iq, original).await;
    spectrum(&mut display).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if app.state.lock().unwrap().receiver_workers.is_none() { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("failed receiver workers did not retire");
    let recovered = command(&mut display, json!({"cmd":"SET_SDR_CHANNEL","channel":1,
        "mode":"am","ifbw":6000.0})).await;
    assert!(recovered["sdr"]["receiver_error"].is_null());
    let fresh = new_iq_identity(&mut iq, tuned_identity).await;
    assert_ne!(fresh, original);
    let mut audio = connect_ws(&format!("{url}/audio?channel=1")).await;
    let mut events = connect_ws(&format!("{url}/decode?channel=1")).await;
    pcm(&mut audio, fresh).await;
    decode(&mut events, fresh).await;
    spectrum(&mut display).await;
    let healthy = command(&mut display, json!({"cmd":"STATUS"})).await;
    assert!(healthy["sdr"]["receiver_error"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn latest_powr_flushes_on_wake_without_fifo_or_status() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    let epoch = app.epoch.load(Ordering::Acquire);
    for label in ["POWR-1", "POWR-2", "POWR-3"] {
        fanout(
            &app,
            Message::Binary(format!("{label}-body").into_bytes().into()),
            epoch,
        );
    }
    assert_eq!(next_binary(&mut ws).await, b"POWR-3-body".to_vec());
    expect_silence(&mut ws, Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn latest_slots_replace_freq_and_trace_then_send_freq_first() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    let epoch = app.epoch.load(Ordering::Acquire);
    for label in ["FREQ-old", "POWR-old", "FREQ-new", "POWR-new"] {
        fanout(
            &app,
            Message::Binary(label.as_bytes().to_vec().into()),
            epoch,
        );
    }
    assert_eq!(next_binary(&mut ws).await, b"FREQ-new".to_vec());
    assert_eq!(next_binary(&mut ws).await, b"POWR-new".to_vec());
    expect_silence(&mut ws, Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn initial_freq_is_sent_to_new_display_client() {
    let (app, url) = start_ws_server().await;
    app.state.lock().unwrap().last_freq = Some(b"FREQ-initial".to_vec());
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    assert_eq!(next_binary(&mut ws).await, b"FREQ-initial".to_vec());
}

#[tokio::test(flavor = "current_thread")]
async fn initial_decode_snapshot_uses_bounded_cumulative_application_ack() {
    let (app, url) = start_ws_server().await;
    let (channel_id, stream_epoch) = {
        let mut state = app.state.lock().unwrap();
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_demod = "radiofax".into();
        state.sdr_if_bw = 2400.0;
        let mut source = service_fixture::FixtureSources::new();
        source.configure(&state.channel_configs()).unwrap();
        let registry = source.registry();
        let identity = (registry[0].channel_id, registry[0].epoch);
        state.fixture_sources = Some(source);
        state.publish_identities(&app.identities);
        identity
    };
    let frame = |seq, event| websa_channelizer::encode_decode_event(&serde_json::to_vec(&json!({
        "version":1,"channel":1,"channel_id":channel_id,"epoch":stream_epoch,"seq":seq,
        "input_index":"0","input_rate":48_000,"input_fraction_num":"0","input_fraction_den":"1",
        "delay_seconds":0,"delay_known":true,"causal_presentation":false,"mode":"radiofax","event":event,
    })).unwrap()).unwrap();
    {
        let mut state = app.state.lock().unwrap();
        state.decode_snapshots.observe(&[frame(0,json!({"type":"start"}))]).unwrap();
        for (index,row) in (0..2400u32).step_by(64).enumerate() {
            let rows = (2400-row).min(64);
            let pixels = 1810*rows as usize;
            let padding = (3-pixels%3)%3;
            let gray = "A".repeat(pixels.div_ceil(3)*4-padding)+&"=".repeat(padding);
            state.decode_snapshots.observe(&[frame(index as u32+1,json!({
                "type":"radiofax-image","width":1810,"row":row,"rows":rows,"gray":gray,"revision":1,
                "meta":{"serial":"page","epoch":"paper","hz":100e6,"ioc":576,"lpm":120,
                    "lines":row+rows,"complete":false,"partial":false,"segmentIndex":0,
                    "transmissionId":"1","missingRows":[],"reason":null}
            }))]).unwrap();
        }
    }
    // Raw consumers retain their push transport and need no application acknowledgements.
    let mut raw = connect_ws(&format!("{url}/decode?channel=1")).await;
    let mut raw_bytes = 0;
    for seq in 0..=38 {
        let bytes = next_binary(&mut raw).await;
        raw_bytes += bytes.len();
        let envelope: Value = serde_json::from_slice(&bytes[8..]).unwrap();
        assert_eq!(envelope["seq"], seq);
        if seq == 0 { assert!(envelope["event"].get("snapshotAck").is_none()); }
    }
    assert!(raw_bytes > 4*1024*1024);
    let mut paced = connect_ws(&format!("{url}/decode?channel=1&ack=1")).await;
    let start = next_binary(&mut paced).await;
    let start: Value = serde_json::from_slice(&start[8..]).unwrap();
    assert_eq!(start["seq"], 0);
    assert_eq!(start["event"]["snapshotAck"], true);
    let mut total = 0;
    for seq in 1..DECODE_ACK_WINDOW as u32 {
        let bytes = next_binary(&mut paced).await;
        total += bytes.len();
        let envelope: Value = serde_json::from_slice(&bytes[8..]).unwrap();
        assert_eq!(envelope["seq"], seq);
    }
    expect_silence(&mut paced, Duration::from_millis(30)).await;
    // Wrong identities and unsent sequences cannot release the full window.
    for (channel,id,epoch,seq) in [(2,channel_id,stream_epoch,15),
        (1,channel_id+1,stream_epoch,15),(1,channel_id,stream_epoch+1,15),
        (1,channel_id,stream_epoch,16)] {
        paced.send(WsMessage::Text(json!({"type":"ack","channel":channel,"channel_id":id,
            "epoch":epoch,"seq":seq}).to_string().into())).await.unwrap();
        expect_silence(&mut paced, Duration::from_millis(30)).await;
    }
    paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"channel_id":channel_id,
        "epoch":stream_epoch,"seq":15}).to_string().into())).await.unwrap();
    for seq in DECODE_ACK_WINDOW as u32..=38 {
        let bytes = next_binary(&mut paced).await;
        assert!(bytes.len()-8 <= 2*1024*1024);
        total += bytes.len();
        let envelope: Value = serde_json::from_slice(&bytes[8..]).unwrap();
        assert_eq!(envelope["seq"], seq);
        assert_eq!(envelope["event"]["row"], (seq-1)*64);
        paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"channel_id":channel_id,
            "epoch":stream_epoch,"seq":seq}).to_string().into())).await.unwrap();
    }
    assert!(total > 4*1024*1024);
    let live = frame(39,json!({"type":"radiofax-state"}));
    fanout(&app, Message::Binary(live.into()), app.epoch.load(Ordering::Acquire));
    let bytes = next_binary(&mut paced).await;
    let envelope: Value = serde_json::from_slice(&bytes[8..]).unwrap();
    assert_eq!(envelope["seq"], 39);
    paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"channel_id":channel_id,"epoch":stream_epoch,"seq":39}).to_string().into())).await.unwrap();
    expect_silence(&mut paced, Duration::from_millis(30)).await;
    assert!(app.clients.lock().iter().all(|client| !client.overflow.load(Ordering::Acquire)));
}

#[test]
fn decode_pending_budget_counts_active_batches_and_releases_on_drop() {
    let pending=Arc::new(AtomicU64::new(0));
    let reservation=DecodeReservation::acquire(&pending,DECODE_PENDING_BYTE_LIMIT).unwrap();
    assert!(DecodeReservation::acquire(&pending,1).is_none());
    drop(reservation);
    assert_eq!(pending.load(Ordering::Acquire),0);
    assert!(DecodeReservation::acquire(&pending,DECODE_PENDING_BYTE_LIMIT).is_some());
}

#[test]
fn decode_credit_bounds_bytes_and_tracks_cumulative_identity_deadlines() {
    let mut credit=DecodeCredit::default();
    let first=DecodeApplied {channel:1,channel_id:11,epoch:7,seq:0};
    credit.sent(first,DECODE_ACK_BYTES/2);
    let oldest=credit.deadline();
    let second=DecodeApplied {seq:1,..first};
    credit.sent(second,DECODE_ACK_BYTES/2);
    assert!(!credit.can_send(1));
    assert!(!credit.acknowledge(DecodeApplied {seq:2,..first}));
    assert!(!credit.acknowledge(DecodeApplied {channel_id:12,..second}));
    assert_eq!(credit.deadline(),oldest);
    assert!(credit.acknowledge(second));
    assert_eq!(credit.bytes,0);
    assert_eq!(credit.deadline(),None);
    credit.sent(first,1);
    credit.sent(DecodeApplied {channel_id:12,..first},1);
    credit.sent(DecodeApplied {epoch:8,..first},1);
    assert!(credit.acknowledge(first));
    assert_eq!(credit.pending.len(),2);
    assert_eq!(credit.bytes,2);
    assert!(!credit.acknowledge(first));
    for seq in 1..DECODE_ACK_WINDOW as u32-1 {
        assert!(credit.can_send(1));
        credit.sent(DecodeApplied {epoch:8,seq,..first},1);
    }
    assert!(!credit.can_send(1));
}

#[tokio::test(flavor="current_thread")]
async fn decode_send_rejects_expired_oldest_credit_with_available_capacity() {
    let mut credit=DecodeCredit::default();
    credit.sent(DecodeApplied {channel:1,channel_id:11,epoch:7,seq:0},1);
    credit.pending.front_mut().unwrap().deadline=tokio::time::Instant::now()-Duration::from_secs(1);
    assert!(credit.can_send(1),"deadline must be enforced even below both window bounds");
    let mut polled=false;
    let result=send_with_decode_deadline(credit.deadline(),async {polled=true;true}).await;
    assert!(result.is_err());
    assert!(!polled,"an expired predecessor must prevent even an immediately ready send");
    assert!(send_with_decode_deadline(None,async {true}).await.unwrap());
    assert!(send_with_decode_deadline(Some(tokio::time::Instant::now()+DECODE_ACK_TIMEOUT),async {true}).await.unwrap());
    let deadline=tokio::time::Instant::now()+Duration::from_millis(1);
    assert!(send_with_decode_deadline(Some(deadline),std::future::pending()).await.is_err(),
        "a blocked send must end at the oldest credit deadline");
}

#[test]
fn capture_epoch_fences_raw_iq_and_display_but_keeps_admitted_receiver_frames() {
    for (magic,retained) in [(b"IQBF",false),(b"RTAF",false),(b"AUD2",true),(b"SDE2",true)] {
        let packet=Outbound {epoch:1,status_revision:None,display_version:None,identity:None,
            message:Message::Binary(magic.to_vec().into()).into()};
        assert!(packet.epoch_current(1));
        assert_eq!(packet.epoch_current(2),retained);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn live_decode_revision_batches_use_bounded_ack_without_losing_fifo() {
    let (app,url)=start_ws_server().await;
    for (index,identity) in app.identities.iter().enumerate() {
        identity.store(service_stream::identity_key(index as u32+11,7),Ordering::Release);
    }
    let mut paced=connect_ws(&format!("{url}/decode?ack=1")).await;
    let mut raw=connect_ws(&format!("{url}/decode")).await;
    let frame=|channel:u32,seq:u32,event:Value| websa_channelizer::encode_decode_event(&serde_json::to_vec(&json!({
        "version":1,"channel":channel,"channel_id":channel+10,"epoch":7,"seq":seq,
        "input_index":"0","input_rate":48_000,"input_fraction_num":"0","input_fraction_den":"1",
        "delay_seconds":0,"delay_known":true,"causal_presentation":false,"mode":"radiofax","event":event,
    })).unwrap()).unwrap();
    let mut output=receiver_workers::ReceiverFrames::default();
    // Almost 20 MiB of gray per channel: five retained segments, 184 image
    // chunks per channel. One synchronous delivery exceeds both old limits.
    for channel in 1..=MAX_CHANNELS as u32 {
        output.decode.push(frame(channel,0,json!({"type":"start"})));
        let mut seq=0;
        for (segment,lines) in [2400u32,2400,2400,2400,1986].into_iter().enumerate() {
            for row in (0..lines).step_by(64) {
                let rows=(lines-row).min(64);
                let pixels=1810*rows as usize;
                let padding=(3-pixels%3)%3;
                let gray="A".repeat(pixels.div_ceil(3)*4-padding)+&"=".repeat(padding);
                seq+=1;
                output.decode.push(frame(channel,seq,json!({
                    "type":"radiofax-image","width":1810,"row":row,"rows":rows,"gray":gray,"revision":2,
                    "meta":{"serial":format!("page-{segment}"),"segmentIndex":segment,"transmissionId":"fax",
                        "lines":row+rows,"complete":true,"partial":false,"missingRows":[],"reason":null}
                })));
            }
        }
        assert_eq!(seq,184);
    }
    let count=output.decode.len();
    assert!(count>64);
    let wire_bytes:usize=output.decode.iter().map(Vec::len).sum();
    assert!(wire_bytes>MAX_CHANNELS*DECODE_ACK_BYTES);
    assert!((wire_bytes as u64)<DECODE_PENDING_BYTE_LIMIT);
    {
        let state=app.state.lock().unwrap();
        service_stream::deliver_receiver(&state,&app,output,false);
        let mut following=receiver_workers::ReceiverFrames::default();
        for channel in 1..=MAX_CHANNELS as u32 {following.decode.push(frame(channel,185,json!({"type":"radiofax-state"})));}
        service_stream::deliver_receiver(&state,&app,following,false);
    }
    let mut sequences=[0u32;MAX_CHANNELS];
    let mut total=0;
    for index in 0..count+MAX_CHANNELS {
        let bytes=next_binary(&mut paced).await;
        let event:Value=serde_json::from_slice(&bytes[8..]).unwrap();
        let channel=event["channel"].as_u64().unwrap() as usize;
        let seq=event["seq"].as_u64().unwrap() as u32;
        assert_eq!(seq,sequences[channel-1]);
        sequences[channel-1]+=1;
        if index<count {assert!(seq<=184);} else {assert_eq!(seq,185);}
        if seq==0 {assert_eq!(event["event"]["snapshotAck"],true);}
        if seq>0 && seq<=184 {assert_eq!(event["event"]["revision"],2);}
        let raw_bytes=next_binary(&mut raw).await;
        let raw_event:Value=serde_json::from_slice(&raw_bytes[8..]).unwrap();
        assert_eq!(raw_event["channel"],event["channel"]);
        assert_eq!(raw_event["seq"],event["seq"]);
        assert!(raw_event["event"].get("snapshotAck").is_none());
        if seq>0 {assert_eq!(raw_bytes,bytes);}
        total+=bytes.len();
        paced.send(WsMessage::Text(json!({"type":"ack","channel":channel,"channel_id":channel+10,"epoch":7,"seq":seq}).to_string().into())).await.unwrap();
    }
    assert_eq!(sequences,[186;MAX_CHANNELS]);
    assert!(total>=wire_bytes);
    expect_silence(&mut paced,Duration::from_millis(20)).await;
    for client in app.clients.lock().iter() {
        assert!(!client.overflow.load(Ordering::Acquire));
        assert_eq!(client.dropped.load(Ordering::Relaxed),0);
        assert_eq!(client.decode_pending_bytes.load(Ordering::Acquire),0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn decode_channel_ack_stall_does_not_queue_unrelated_batches() {
    let (app,url)=start_ws_server().await;
    for (index,identity) in app.identities.iter().enumerate() {
        identity.store(service_stream::identity_key(index as u32+11,7),Ordering::Release);
    }
    let mut paced=connect_ws(&format!("{url}/decode?channel=1&ack=1")).await;
    let frame=|channel:u32,seq:u32,event:Value| websa_channelizer::encode_decode_event(&serde_json::to_vec(&json!({
        "version":1,"channel":channel,"channel_id":channel+10,"epoch":7,"seq":seq,
        "input_index":"0","input_rate":48_000,"input_fraction_num":"0","input_fraction_den":"1",
        "delay_seconds":0,"delay_known":true,"causal_presentation":false,"mode":"radiofax","event":event,
    })).unwrap()).unwrap();
    {
        let state=app.state.lock().unwrap();
        service_stream::deliver_receiver(&state,&app,receiver_workers::ReceiverFrames {
            audio_configs:Vec::new(),audio:Vec::new(),decode:vec![frame(1,0,json!({"type":"start"}))],
            ..Default::default()
        },false);
    }
    let start=next_binary(&mut paced).await;
    let start:Value=serde_json::from_slice(&start[8..]).unwrap();
    assert_eq!(start["event"]["snapshotAck"],true);
    let pending=app.clients.lock()[0].decode_pending_bytes.load(Ordering::Acquire);
    assert_eq!(pending,0,"sent frames use the separate bounded credit window");
    {
        let state=app.state.lock().unwrap();
        // No awaits: the client cannot drain a mistakenly enqueued batch.
        // Test both live filtering and retiring filtering with no matching
        // retiring identity for this channel.
        for retiring in [false,true] {
            for seq in 0..130 {
                service_stream::deliver_receiver(&state,&app,receiver_workers::ReceiverFrames {
                    audio_configs:Vec::new(),audio:Vec::new(),decode:vec![frame(2,seq,json!({"type":"radiofax-state"})),
                        frame(3,seq,json!({"type":"radiofax-state"}))],
                    ..Default::default()
                },retiring);
            }
        }
        let clients=app.clients.lock();
        let client=&clients[0];
        assert_eq!(client.tx.capacity(),64);
        assert_eq!(client.decode_pending_bytes.load(Ordering::Acquire),pending);
        assert_eq!(client.dropped.load(Ordering::Relaxed),0);
        assert!(!client.overflow.load(Ordering::Acquire));
    }
    {
        let state=app.state.lock().unwrap();
        service_stream::deliver_receiver(&state,&app,receiver_workers::ReceiverFrames {
            audio_configs:Vec::new(),audio:Vec::new(),decode:vec![frame(1,1,json!({"type":"radiofax-state"}))],
            ..Default::default()
        },false);
    }
    let following=next_binary(&mut paced).await;
    let following:Value=serde_json::from_slice(&following[8..]).unwrap();
    assert_eq!(following["channel"],1);
    assert_eq!(following["seq"],1);
    paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"channel_id":11,"epoch":7,"seq":1}).to_string().into())).await.unwrap();
    expect_silence(&mut paced,Duration::from_millis(20)).await;
    assert_eq!(app.clients.lock()[0].decode_pending_bytes.load(Ordering::Acquire),0);
}

#[tokio::test]
async fn pipelined_invalid_commands_reply_without_self_queue_deadlock() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        for seq in 0..128 {
            ws.send(WsMessage::Text(
                json!({"cmd": "BOGUS", "seq": seq}).to_string().into(),
            ))
            .await
            .expect("send command");
        }
        ws.send(WsMessage::Text(
            json!({"cmd": "STATUS"}).to_string().into(),
        ))
        .await
        .expect("send STATUS");
        for _ in 0..128 {
            let reply = next_text(&mut ws).await;
            assert_eq!(reply["cmd"], "ERROR");
            assert_eq!(reply["code"], "unknown_command");
        }
        let status = next_text(&mut ws).await;
        assert!(
            status.get("connected").is_some(),
            "expected STATUS after the errors, got {status}"
        );
    })
    .await
    .expect("pipelined commands and STATUS within deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn stale_generation_frames_are_dropped_before_send() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    fanout(&app, Message::Binary(b"POWR-stale".to_vec().into()), 0);
    fanout(&app, Message::Binary(b"IQBF-stale".to_vec().into()), 0);
    app.epoch.fetch_add(1, Ordering::AcqRel);
    fanout(&app, Message::Binary(b"POWR-fresh".to_vec().into()), 1);
    fanout(&app, Message::Binary(b"IQBF-fresh".to_vec().into()), 1);
    let mut frames = vec![next_binary(&mut ws).await, next_binary(&mut ws).await];
    frames.sort();
    assert_eq!(
        frames,
        vec![b"IQBF-fresh".to_vec(), b"POWR-fresh".to_vec()]
    );
    expect_silence(&mut ws, Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_status_cannot_regress_after_command_ack() {
    let (app, url) = start_ws_server().await;
    let (golden, schema) = load_golden();
    {
        let mut state = app.state.lock().unwrap();
        *state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
    }
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    let old = app.state.lock().unwrap().status_snapshot();
    let old_status = old.build();
    assert!(app.state.publish_status(old.revision, old_status.clone()));
    ws.send(WsMessage::Text(json!({"cmd": "SET_SDR_DEMOD", "mode": "am", "ifbw": 6000}).to_string().into()))
        .await.unwrap();
    let ack = next_text(&mut ws).await;
    assert_eq!(ack["response_to"], "SET_SDR_DEMOD");
    assert_eq!(ack["sdr"]["demod"], "am");
    assert!(!app.state.publish_status(old.revision, old_status.clone()));
    {
        let clients = app.clients.lock();
        // Model a packet accepted by the publisher before the acknowledgement,
        // but still pending in the socket's FIFO when the acknowledgement arrives.
        clients[0].tx.try_send(Outbound {
            epoch: app.epoch.load(Ordering::Acquire),
            status_revision: Some(old.revision), display_version: None,
            identity:None,
            message: Message::Text(old_status.to_string().into()).into(),
        }).unwrap();
        send_or_count(&clients[0], Message::Text(json!({"cmd": "MARKER"}).to_string().into()), 0);
    }
    assert_eq!(next_text(&mut ws).await["cmd"], "MARKER");
    ws.send(WsMessage::Text(json!({"cmd": "STATUS"}).to_string().into())).await.unwrap();
    assert_eq!(next_text(&mut ws).await["sdr"]["demod"], "am");
    expect_silence(&mut ws, Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_status_and_display_follow_new_configuration_without_dropping_iq() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    let (revision, mut fresh) = app.state.status_with_revision();
    {
        let clients = app.clients.lock();
        enqueue(&clients[0], Outbound {
            epoch: 0, status_revision: Some(revision), display_version: None,
            identity:None,
            message: Message::Text(json!({"cmd": "STATUS", "connected": false}).to_string().into()).into(),
        });
        for frame in [b"RTAF-old".to_vec(), b"FREQ-old".to_vec()] {
            enqueue(&clients[0], Outbound {
                epoch: 0, status_revision: None, display_version: Some(0),
                identity:None,
                message: Message::Binary(frame.into()).into(),
            });
        }
    }
    // No await between enqueue and the newer commit: the old packets are queued.
    fresh["connected"] = json!(true);
    assert!(app.state.publish_status(revision + 1, fresh));
    app.display_version.store(1, Ordering::Release);
    fanout(&app, Message::Binary(b"IQBF-tail".to_vec().into()), 0);
    assert_eq!(next_binary(&mut ws).await, b"IQBF-tail");
    ws.send(WsMessage::Text(json!({"cmd": "STATUS"}).to_string().into())).await.unwrap();
    assert_eq!(next_text(&mut ws).await["connected"], true);
    expect_silence(&mut ws, Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_sdr_mode_keeps_configuration_and_queued_iq() {
    let (app, url) = start_ws_server().await;
    let (golden, schema) = load_golden();
    {
        let mut state = app.state.lock().unwrap();
        *state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        assert!(!dispatch(&mut state, "SET_MODE", &json!({"mode": "sdr"})).unwrap());
        assert_eq!((state.config_version, state.freq_version), (0, 0));
    }
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    ws.send(WsMessage::Text(json!({"cmd": "SET_MODE", "mode": "sdr"}).to_string().into())).await.unwrap();
    ws.send(WsMessage::Text(json!({"cmd": "STATUS"}).to_string().into())).await.unwrap();
    let status = next_text(&mut ws).await;
    assert_eq!(status["response_to"], "SET_MODE");
    let polled = next_text(&mut ws).await;
    assert_eq!(polled["response_to"], "STATUS");
    let published = app.state.status_with_revision().1;
    assert!(published.get("response_to").is_none());
    assert_eq!(app.epoch.load(Ordering::Acquire), 0);
    {
        let state = app.state.lock().unwrap();
        assert_eq!((state.config_version, state.freq_version), (0, 0));
    }
    fanout(&app, Message::Binary(b"IQBF-next-sequence".to_vec().into()), 0);
    assert_eq!(next_binary(&mut ws).await, b"IQBF-next-sequence");
}

#[tokio::test]
async fn closing_socket_removes_client() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&url).await;
    wait_for_client(&app).await;
    ws.close(None).await.expect("send close");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !app.clients.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client registration survived socket close");
}

#[tokio::test(flavor = "current_thread")]
async fn slow_iq_transport_closes_with_1013_and_removes_client() {
    let (app, url) = start_ws_server().await;
    let mut ws = connect_ws(&format!("{url}?iq")).await;
    wait_for_client(&app).await;
    let epoch = app.epoch.load(Ordering::Acquire);
    for _ in 0..65 {
        fanout(&app, Message::Binary(b"IQBF".to_vec().into()), epoch);
    }
    // Frames already in flight may arrive before the close; the contract is the
    // explicit 1013 close and removal, not close-first ordering.
    let close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match next_ws(&mut ws).await {
                WsMessage::Close(Some(frame)) => break frame,
                WsMessage::Close(None) => panic!("slow-consumer close without a code"),
                _ => continue,
            }
        }
    })
    .await
    .expect("1013 close within deadline");
    assert_eq!(u16::from(close.code), 1013);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !app.clients.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client registration survived slow-consumer close");
}

#[tokio::test(flavor = "current_thread")]
async fn eighth_slow_audio_consumer_closes_without_retiring_healthy_connection() {
    let (app, url) = start_ws_server().await;
    let mut healthy = connect_ws(&url).await;
    wait_for_client(&app).await;
    let mut slow = connect_ws(&format!("{url}/audio?channel=8")).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.clients.lock().len() != 2 {tokio::task::yield_now().await;}
    }).await.expect("both consumers registered");
    {
        let clients = app.clients.lock();
        let audio = clients.iter().find(|client|client.audio && client.channel == Some(8)).unwrap();
        for _ in 0..65 {
            send_or_count(audio, Message::Binary(b"AUD2".to_vec().into()), app.epoch.load(Ordering::Acquire));
        }
    }
    let close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let WsMessage::Close(Some(frame)) = next_ws(&mut slow).await {break frame;}
        }
    }).await.expect("audio overflow closes within deadline");
    assert_eq!(u16::from(close.code), 1013);
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.clients.lock().len() != 1 {tokio::task::yield_now().await;}
    }).await.expect("only the slow audio consumer is removed");
    healthy.send(WsMessage::Ping(b"healthy".to_vec().into())).await.unwrap();
    loop {
        if let WsMessage::Pong(bytes) = next_ws(&mut healthy).await {
            assert_eq!(bytes.as_ref(), b"healthy");break;
        }
    }
}


const FAKE_SWP_SHIM: &str = r#"
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <thread>
struct FrontEnd { double ref_dbm; int atten, preamp, ifgain, gain, ref_clock, refclk_out; };
struct Request {
    double center_hz, span_hz; FrontEnd fe; int rbw_mode; double rbw_hz; int vbw_mode;
    double vbw_hz; int window, sweep_time_mode; double sweep_time; int trace_points,
    points_strategy, trace_align, spur, trace_detect_mode, trace_detector, frame_detector;
};
struct Actual {
    double start_hz, stop_hz; uint32_t points; double sweep_ms, ref_dbm, rbw_hz, vbw_hz,
    refclk_hz; int refclk_src;
};
struct FrontEndActual { double ref_dbm; int atten, preamp, ifgain; };
static_assert(sizeof(FrontEndActual) == 24, "front-end actual ABI");
struct FakeSwp { double start, stop, center; int points; std::thread::id owner; FrontEndActual front_end; };
static const double FAKE_CARRIER_OFFSET = 1234.5;
static int fake_opens = 0;
static int fake_closes = 0;
static int fake_reconfigs = 0;
// Unlike count-based failures, overflow persists until hardware readback clears it.
static double fake_overflow_threshold = -INFINITY;
static double fake_ref_ceiling = INFINITY;
static int fake_config_status = 0;
extern "C" void san90_swp_fake_auto_controls_v1(double threshold, double ceiling, int config_status) {
    fake_overflow_threshold = threshold;
    fake_ref_ceiling = ceiling;
    fake_config_status = config_status;
}
static const char* FAKE_FAIL_PATH = "/tmp/websa_fake_swp_fail.txt";
static int fail_next() {
    FILE* f = fopen(FAKE_FAIL_PATH, "r+");
    if (!f) return 0;
    int n = 0, status = -9;
    if (fscanf(f, "%d %d", &n, &status) < 1) n = 0;
    rewind(f);
    fprintf(f, "%d %d   ", n > 0 ? n - 1 : 0, status);
    fclose(f);
    return n > 0 ? status : 0;
}
static int configure(FakeSwp* dev, const Request* r, Actual* a) {
    if (!(r->span_hz > 0.0) || !(r->center_hz >= 9000.0) || !(r->center_hz <= 9e9)) return -7;
    dev->start = r->center_hz - r->span_hz / 2.0;
    dev->stop = r->center_hz + r->span_hz / 2.0;
    dev->center = r->center_hz;
    dev->points = 201;
    a->start_hz = dev->start; a->stop_hz = dev->stop; a->points = 201; a->sweep_ms = 12.5;
    dev->front_end = {std::fmin(r->fe.ref_dbm - 2.5, fake_ref_ceiling), 14, 1, 3};
    a->ref_dbm = dev->front_end.ref_dbm; a->rbw_hz = r->rbw_hz; a->vbw_hz = r->vbw_hz;
    a->refclk_hz = 100e6; a->refclk_src = r->fe.ref_clock;
    return 0;
}
extern "C" int san90_swp_open_v2(void** out, const Request* r, Actual* a) {
    if (!out || !r || !a) return -100;
    auto* dev = new FakeSwp{};
    dev->owner = std::this_thread::get_id();
    int status = configure(dev, r, a);
    if (status) { delete dev; return status; }
    fake_opens += 1;
    *out = dev;
    return 0;
}
extern "C" int san90_swp_reconfig_v1(void* p, const Request* r, Actual* a) {
    fake_reconfigs += 1;
    if (fake_config_status) return fake_config_status;
    return configure(static_cast<FakeSwp*>(p), r, a);
}
extern "C" int san90_swp_front_end_actual_v1(void* p, FrontEndActual* out) {
    auto* dev = static_cast<FakeSwp*>(p);
    if (!dev || !out || dev->owner != std::this_thread::get_id()) return -100;
    *out = dev->front_end;
    return 0;
}
extern "C" int san90_swp_next_v2(void* p, double* freq, float* power, uint32_t cap,
                                   uint32_t* points, double* ppm) {
    auto* dev = static_cast<FakeSwp*>(p);
    if (!dev || cap < static_cast<uint32_t>(dev->points)) return -100;
    *ppm = 0.0;
    if (int status = fail_next()) { *points = 0; return status; }
    if (dev->front_end.ref_dbm < fake_overflow_threshold) { *points = 0; return -12; }
    for (int i = 0; i < dev->points; ++i) {
        double f = dev->start + (dev->stop - dev->start) * i / (dev->points - 1);
        freq[i] = f;
        double x = (f - (dev->center + FAKE_CARRIER_OFFSET)) / ((dev->stop - dev->start) / 50.0);
        power[i] = static_cast<float>(-90.0 + 60.0 * std::exp(-0.5 * x * x));
    }
    *points = static_cast<uint32_t>(dev->points);
    return 0;
}
extern "C" int san90_swp_close_v1(void* p) {
    fake_closes += 1;
    delete static_cast<FakeSwp*>(p);
    return 0;
}
extern "C" void san90_swp_fake_stats_v1(int* opens, int* closes, int* reconfigs) {
    if (opens) *opens = fake_opens;
    if (closes) *closes = fake_closes;
    if (reconfigs) *reconfigs = fake_reconfigs;
}
struct DeviceInfo {
    uint64_t uid;
    uint32_t model, hw, mfw, ffw, bus_speed, bus_ver, api_ver, pmu, agu;
    int32_t warnings, errors;
};
extern "C" int san90_device_info_v1(void* p, DeviceInfo* info) {
    auto* dev = static_cast<FakeSwp*>(p);
    if (!dev || !info) return -100;
    if (dev->owner != std::this_thread::get_id()) return -110;
    *info = {UINT64_C(0xFEDCBA9876543210), 140, 7, 0x12345678, 0x3768,
             5000, 0x10203, 0x3764, 271, 261, 2, 1};
    return 0;
}
extern "C" int san90_swp_defaults_v1(void* s, double* center, double* span, double* ref,
        double* rbw, double* vbw, int* points, int* atten, int* window, int* rbw_mode,
        int* vbw_mode, int* spur, int* frame_detector, int* trace_align,
        int* points_strategy, int* preamp, int* ifgain, int* gain, int* swt, double* stime) {
    if (!s) return -101;
    *center = 4.5e9; *span = 9e9; *ref = -10.0; *rbw = 3e5; *vbw = 3e5; *points = 2001;
    *atten = -1; *window = 0; *rbw_mode = 1; *vbw_mode = 4; *spur = 0; *frame_detector = 1;
    *trace_align = 1; *points_strategy = 0; *preamp = 0; *ifgain = 2; *gain = 0; *swt = 0;
    *stime = 0.0;
    return 0;
}
// First member of FakeSwp is NOT the handle, but the gnss entry only needs a non-null
// token to return its canned snapshot; the real shim's contract is exercised on device.
extern "C" int san90_gnss_v1(void* state, int* lock, int* sats, int* docxo,
                               int* docxo_mode, int* antenna, float* latitude,
                               float* longitude, int* altitude, int* year, int* month,
                               int* day, int* hour, int* minute, int* second) {
    if (!state || !lock || !sats || !docxo || !docxo_mode || !antenna || !latitude
        || !longitude || !altitude || !year || !month || !day || !hour || !minute
        || !second) return -100;
    *lock = 1; *sats = 7; *docxo = 1; *docxo_mode = 2; *antenna = 0;
    *latitude = 39.9f; *longitude = 116.4f; *altitude = 43;
    *year = 2026; *month = 10; *day = 21; *hour = 12; *minute = 34; *second = 56;
    return 0;
}
"#;

fn compile_fake_swp_shim() -> String {
    compile_fake_swp_source("shim", FAKE_SWP_SHIM)
}

fn compile_fake_swp_source(name: &str, contents: &str) -> String {
    let dir = std::env::temp_dir().join(format!("websa_fake_swp_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join(format!("{name}.cpp"));
    let built = dir.join(format!("libfake_swp_{name}.so"));
    if !built.exists() {
        std::fs::write(&source, contents).unwrap();
        let status = std::process::Command::new("g++")
            .args([
                "-O1",
                "-fPIC",
                "-shared",
                source.to_str().unwrap(),
                "-o",
                built.to_str().unwrap(),
            ])
            .status()
            .expect("g++ required for the fake SWP shim contract test");
        assert!(status.success(), "fake SWP shim compile failed");
    }
    built.to_str().unwrap().to_owned()
}

fn hardware_state(shim: Option<String>) -> DevState {
    let schema =
        serde_json::from_str(include_str!("../../../../tests/fixtures/contract/schema.json"))
            .unwrap();
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/contract/state_fake_initial.json"
    ))
    .unwrap();
    let mut state = DevState::new(golden, schema);
    state.hardware_shim = shim;
    state
}

/// The fake SWP shim's failure injection is a fixed-path file: serialise the
/// fake-swp tests so an injected -9 can never leak into a sibling test.
static TEST_SWP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn decode_header(frame: &[u8]) -> (String, u32, u32, f32) {
    (
        String::from_utf8(frame[..4].to_vec()).unwrap(),
        u32::from_le_bytes(frame[4..8].try_into().unwrap()),
        u32::from_le_bytes(frame[8..12].try_into().unwrap()),
        f32::from_le_bytes(frame[12..16].try_into().unwrap()),
    )
}

#[test]
fn hardware_device_info_matches_boot_metadata() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    let mut state = hardware_state(Some(compile_fake_swp_shim()));
    state.swp.mode = "std".into();
    state.fetch_sweep();
    let status = state.status();
    assert_eq!(status["device"], "W200 R3 UID:FEDCBA9876543210 HW:7 MCU 4660.86.120 FPGA 0.55.104");
    dispatch(&mut state, "SET_FREQ", &json!({"center":100e6, "span":1e6})).unwrap();
    assert_eq!(state.status()["device_detail"], status["device_detail"]);
}

#[test]
fn swp_optional_gain_query_missing_or_failed_keeps_gain_unknown() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    for (name, source) in [
        ("no_gain", FAKE_SWP_SHIM.replace("san90_swp_front_end_actual_v1", "san90_test_no_gain")),
        ("failed_gain", FAKE_SWP_SHIM.replace("*out = dev->front_end;", "return -100;")),
    ] {
        let mut state = hardware_state(Some(compile_fake_swp_source(name, &source)));
        state.swp.mode = "std".into();
        state.fetch_sweep();
        let status = state.status();
        assert_eq!(status["connected"], true);
        assert_eq!(status["actual"]["ref"], state.swp.ref_level - 2.5);
        for key in ["atten_actual", "preamp_actual", "ifgain_actual"] {
            assert!(status["amp"][key].is_null(), "{name}: {key}");
        }
    }
}

#[test]
fn vendor_swp_publishes_real_freq_powr_contract() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    let mut state = hardware_state(Some(compile_fake_swp_shim()));
    state.swp.mode = "std".into();
    state.swp.center = 4_510_004_000.0;
    state.swp.span = 9_019_992_000.0;
    let (freq, powr) = state.fetch_sweep();
    // FREQ frame: tag, version, points, sweep_ms, f64 axis; carrier exactly at center.
    let (tag, version, points, sweep_ms) = decode_header(freq.as_ref().unwrap());
    assert_eq!(
        (tag.as_str(), version, points, sweep_ms),
        ("FREQ", state.freq_version as u32, 201, 12.5)
    );
    let freqs: Vec<f64> = freq.unwrap()[16..]
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(freqs.len(), 201);
    assert!(freqs.windows(2).all(|p| p[0] < p[1]));
    let (tag, _, points_p, sweep_ms_p) = decode_header(&powr);
    assert_eq!((tag.as_str(), points_p, sweep_ms_p), ("POWR", 201, 12.5));
    let powers: Vec<f32> = powr[16..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let (peak_index, peak) = powers
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    assert_eq!(*peak, -30.0); // the fake carrier amplitude
    assert!((freqs[peak_index] - (4_510_004_000.0 + 1234.5)).abs() < 901_999_200.0 / 200.0);
    assert_eq!(state.auto.last_peak, Some(-30.0));
    assert!((state.auto.last_peak_hz.unwrap() - (4_510_004_000.0 + 1234.5)).abs() < 45e6);
    // Vendor actual mirrors the configured profile.
    assert_eq!(state.actual["points"], 201);
    assert_eq!(state.actual["refclk"], 100e6);
    assert_eq!(state.actual["ref"], state.swp.ref_level - 2.5);
    let amp = state.status()["amp"].clone();
    assert_eq!(amp["atten_actual"], 14);
    assert_eq!(amp["preamp_actual"], 1);
    assert_eq!(amp["ifgain_actual"], 3);
    assert_eq!(state.status()["connected"], true);
    // Same axis -> no repeated FREQ frame, POWR still flows.
    let (freq2, powr2) = state.fetch_sweep();
    assert!(freq2.is_none());
    assert_eq!(decode_header(&powr2).0, "POWR");
    // A request change reconfigures with a new version, so FREQ is re-published.
    dispatch(
        &mut state,
        "SET_FREQ",
        &json!({"center": 4_510_004_000.0, "span": 1e8}),
    )
    .unwrap();
    let (freq3, _) = state.fetch_sweep();
    assert!(freq3.is_some());
}

#[test]
fn hardware_std_mode_guards_and_single_handle_transitions() {
    let mut state = hardware_state(Some("/does/not/exist.so".into()));
    state.swp.mode = "std".into();
    // Missing shim -> explicit error, empty frames, disconnected; no fake trace fallback.
    let (freq, powr) = state.fetch_sweep();
    assert!(freq.is_none() && powr.is_empty());
    assert!(state.swp_hw.is_none() && state.swp_error.is_some());
    assert_eq!(state.status()["connected"], false);
    assert!(state.status()["last_error"]
        .as_str()
        .unwrap()
        .contains("SDK"));
    // Python validate(): while the link is down, device commands are refused and
    // CONNECT makes the next tick retry the open at once.
    assert_eq!(
        dispatch(&mut state, "SET_POINTS", &json!({"points": 1001}))
            .unwrap_err()
            .code,
        "device_not_connected"
    );
    state.swp_retry_at = Some(std::time::Instant::now() + Duration::from_secs(60));
    assert!(dispatch(&mut state, "CONNECT", &json!({})).unwrap());
    assert!(state.swp_retry_at.is_none());
    state.link_up = true;
    // Modes outside the schema choices are refused before dispatch.
    for mode in ["nfm", "drm"] {
        assert!(dispatch(&mut state, "SET_MODE", &json!({"mode": mode})).is_err());
    }
    dispatch(&mut state, "SET_MODE", &json!({"mode": "harmonic"})).unwrap();
    assert_eq!(state.swp.mode, "harmonic");
    assert!(state.swp_hw.is_none() && state.harm_window.is_none());
    dispatch(&mut state, "SET_MODE", &json!({"mode": "pnm"})).unwrap();
    assert!(state.swp_hw.is_none() && state.pnm_hw.is_none());
    // std is accepted and a mode switch drops the SWP handle (never two handles).
    state.swp_hw = None;
    dispatch(&mut state, "SET_MODE", &json!({"mode": "std"})).unwrap();
    assert_eq!(state.swp.mode, "std");
    assert!(state.swp_hw.is_none());
    dispatch(&mut state, "SET_MODE", &json!({"mode": "rta"})).unwrap();
    assert!(state.swp_hw.is_none() && state.rta_hw.is_none());
    dispatch(&mut state, "SET_MODE", &json!({"mode": "sdr"})).unwrap();
    assert_eq!(state.swp.mode, "sdr");
}

const FAKE_PNM_SHIM: &str = r#"
#include <cmath>
#include <cstdint>
struct FakePnm { double center; uint32_t i; };
extern "C" int san90_pnm_open_v1(void** out, double center, float threshold,
        double rbw_ratio, double start, double stop, uint32_t average,
        uint32_t* points, uint32_t* segments, uint32_t* partial) {
    (void)threshold; (void)rbw_ratio; (void)average;
    if (!out || !points || !segments || !partial) return -100;
    if (!(center >= 9000.0) || !(start < stop)) return -7;
    auto* dev = new FakePnm{center, 0};
    *points = 64; *segments = 1; *partial = 4;
    *out = dev;
    return 0;
}
extern "C" int san90_pnm_next_v1(void* p, double* carrier, float* power,
        double* offset, float* pn, uint32_t cap, uint32_t* points, float* ref,
        uint32_t* progress, uint32_t* done) {
    auto* dev = static_cast<FakePnm*>(p);
    if (!dev || cap < 64 || !carrier || !power || !offset || !pn || !points
        || !ref || !progress || !done) return -100;
    dev->i += 1;
    if (dev->i > 4) dev->i = 1;  // a fresh cycle after done
    *carrier = dev->center; *power = -40.0f; *ref = -50.0f;
    for (int k = 0; k < 64; ++k) {
        offset[k] = 100.0 * (k + 1);
        pn[k] = -90.0f - k;
    }
    *points = 64;
    *progress = dev->i * 25;
    *done = dev->i >= 4 ? 1u : 0u;
    return 0;
}
extern "C" int san90_pnm_close_v1(void* p) { delete static_cast<FakePnm*>(p); return 0; }
"#;

fn compile_fake_pnm_shim() -> String {
    let dir = std::env::temp_dir().join(format!("websa_fake_pnm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("shim.cpp");
    let built = dir.join("libfake_pnm.so");
    if !built.exists() {
        std::fs::write(&source, FAKE_PNM_SHIM).unwrap();
        let status = std::process::Command::new("g++")
            .args([
                "-O1",
                "-fPIC",
                "-shared",
                source.to_str().unwrap(),
                "-o",
                built.to_str().unwrap(),
            ])
            .status()
            .expect("g++ required for the fake PNM shim contract test");
        assert!(status.success(), "fake PNM shim compile failed");
    }
    built.to_str().unwrap().to_owned()
}

#[test]
fn hardware_harmonic_retunes_vendor_swp_per_window() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    let mut state = hardware_state(Some(compile_fake_swp_shim()));
    state.swp.mode = "harmonic".into();
    dispatch(
        &mut state,
        "SET_HARM",
        &json!({"f0": 100e6, "count": 2, "span": 1e6}),
    )
    .unwrap();
    // Scan phase: one window, eight sweeps, one vendor open.
    let (_, _, reconfigs0) = fake_swp_stats();
    for _ in 0..8 {
        let (frames, msgs) = state.step_harm();
        assert!(msgs.is_empty());
        assert!(!frames.is_empty());
    }
    assert_eq!(
        state.swp_open_count, 1,
        "one vendor open for the whole scan window"
    );
    // Measure phase: one in-place reconfigure per harmonic, then the HARM message.
    let mut harm = None;
    for _ in 0..3 {
        let (_, msgs) = state.step_harm();
        if let Some(msg) = msgs.first() {
            harm = Some(msg.clone());
            break;
        }
    }
    assert_eq!(state.swp_open_count, 1, "window changes keep the handle");
    let (_, _, reconfigs1) = fake_swp_stats();
    assert_eq!(
        reconfigs1 - reconfigs0,
        2,
        "one reconfigure per harmonic window"
    );
    let harm = harm.expect("HARM message after the last harmonic");
    assert_eq!(harm["cmd"], "HARM");
    assert_eq!(harm["f0"], 100e6);
    let rows = harm["list"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!((rows[0]["f"].as_f64().unwrap() - (100e6 + 1234.5)).abs() < 1e6 / 200.0);
    assert!((rows[1]["f"].as_f64().unwrap() - (200e6 + 1234.5)).abs() < 1e6 / 200.0);
    assert!((rows[0]["amp"].as_f64().unwrap() + 30.0).abs() < 0.5);
    assert!(rows[1]["dbc"]
        .as_f64()
        .map(|d| d.abs() < 1.0)
        .unwrap_or(false));
}

#[test]
fn vendor_swp_link_error_abandons_handle_and_auto_reopens() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let shim = compile_fake_swp_shim();
    let fail = std::path::Path::new("/tmp/websa_fake_swp_fail.txt");
    let _ = std::fs::remove_file(fail);
    std::fs::write(fail, "1").unwrap();
    let mut state = hardware_state(Some(shim));
    state.swp.mode = "std".into();
    state.swp.center = 1e9;
    state.swp.span = 1e8;

    // First fetch: open succeeds, next() gets the injected -9 bus error.
    let (opens0, closes0, _) = fake_swp_stats();
    let (freq, powr) = state.fetch_sweep();
    assert!(freq.is_none() && powr.is_empty());
    let error = state.swp_error.clone().unwrap_or_default();
    assert!(error.contains("status=-9"), "{error}");
    assert!(state.swp_hw.is_none());
    assert_eq!(state.status()["connected"], false);

    // The stale handle was ABANDONED: the vendor close must never have run. The shim
    // counters are process-global across tests, so compare deltas.
    let (opens1, closes1, _) = fake_swp_stats();
    assert_eq!(opens1 - opens0, 1);
    assert_eq!(
        closes1 - closes0,
        0,
        "bus error must not reach the vendor close"
    );

    // The retry cooldown gates the reopen; force it due and fetch again.
    state.swp_retry_at = None;
    let (freq, powr) = state.fetch_sweep();
    assert!(freq.is_some() && !powr.is_empty());
    assert!(state.swp_error.is_none() && state.swp_retry_at.is_none());
    assert_eq!(state.status()["connected"], true);
    let (opens2, closes2, _) = fake_swp_stats();
    assert_eq!(opens2 - opens1, 1, "auto-reopen after the link error");
    assert_eq!(
        closes2 - closes1,
        0,
        "streaming keeps the healthy handle open"
    );
    drop(state); // Drop joins the owner: the vendor close runs synchronously
    let (_, closes3, _) = fake_swp_stats();
    assert_eq!(
        closes3 - closes2,
        1,
        "drop closes the healthy handle normally"
    );
    let _ = std::fs::remove_file(&fail);
}

#[test]
fn hardware_gnss_snapshot_publishes_python_parity_fields() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    let source = FAKE_SWP_SHIM.replace(
        "    *lock = 1; *sats = 7;",
        "    if (int status = fail_next()) return status;\n    *lock = 1; *sats = 7;",
    );
    let mut state = hardware_state(Some(compile_fake_swp_source("gnss_fail", &source)));
    state.swp.mode = "std".into();
    state.swp.center = 1e9;
    state.swp.span = 1e8;
    state.refresh_gnss();
    assert_eq!(state.status()["gnss"], Value::Null, "no owner has supplied telemetry yet");
    let _ = state.fetch_sweep(); // open the owner so gnss has a live handle
    state.refresh_gnss();
    let gnss = state.status()["gnss"].clone();
    assert_eq!(gnss["lock"], 1);
    assert_eq!(gnss["sats"], 7);
    assert_eq!(gnss["docxo"], 1);
    assert_eq!(gnss["docxo_mode"], 2);
    assert_eq!(gnss["antenna"], 0);
    assert!((gnss["latitude"].as_f64().unwrap() - 39.9).abs() < 1e-4);
    assert!((gnss["longitude"].as_f64().unwrap() - 116.4).abs() < 1e-4);
    assert_eq!(gnss["altitude"], 43);
    assert_eq!(gnss["time"], "2026-10-21 12:34:56");
    assert_eq!(gnss["year"], 2026);
    assert_eq!(gnss["stale"], false);
    // Query failure invalidates freshness in SWP too, and the next successful
    // owner query restores the same snapshot to fresh without a mode change.
    std::fs::write("/tmp/websa_fake_swp_fail.txt", "1 -7").unwrap();
    state.refresh_gnss();
    assert_eq!(state.status()["gnss"]["stale"], true);
    assert_eq!(state.status()["gnss"]["lock"], 1);
    state.refresh_gnss();
    assert_eq!(state.status()["gnss"], gnss);

    // Dispatch retires each owner. The last snapshot survives the gap, but
    // neither a mode change nor polling an absent owner can make it fresh.
    for mode in ["rta", "sdr", "pnm", "harmonic", "std"] {
        assert!(dispatch(&mut state, "SET_MODE", &json!({"mode":mode})).unwrap());
        let pending = state.status()["gnss"].clone();
        assert_eq!(pending["lock"], 1, "{mode}");
        assert_eq!(pending["sats"], 7, "{mode}");
        assert_eq!(pending["stale"], true, "{mode}");
        state.refresh_gnss();
        assert_eq!(state.status()["gnss"], pending, "{mode}");
    }
    let _ = state.fetch_sweep();
    state.refresh_gnss();
    assert_eq!(state.status()["gnss"], gnss, "new owner restores fresh telemetry");
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
}

#[test]
fn gnss_server_transition_discards_previous_hardware_session() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // No SDK is loaded or server process started for this dispatch test.
    let (golden, schema) = load_golden();
    let mut state = DevState::new(golden, schema);
    state.swp.mode = "std".into();
    state.gnss_value = Some(json!({"lock":1,"sats":7,"stale":false}));
    assert!(dispatch(&mut state, "SET_MODE", &json!({"mode":"rta"})).unwrap());
    assert_eq!(state.gnss_value.as_ref().unwrap()["stale"], true);
    assert!(dispatch(&mut state, "SET_MODE", &json!({"mode":"server"})).unwrap());
    assert!(state.gnss_value.is_none());
    state.refresh_gnss();
    assert_eq!(state.status()["gnss"], Value::Null);
    assert!(dispatch(&mut state, "SET_MODE", &json!({"mode":"std"})).unwrap());
    // Hardware STATUS must stay unavailable until the new session reports.
    state.hardware_shim = Some("must-not-be-opened.so".into());
    state.refresh_gnss();
    assert!(state.gnss_value.is_none());
    assert_eq!(state.status()["gnss"], Value::Null);
    for mode in ["rta", "sdr", "std"] {
        assert!(dispatch(&mut state, "SET_MODE", &json!({"mode":mode})).unwrap());
        state.refresh_gnss();
        assert_eq!(state.status()["gnss"], Value::Null, "{mode}");
    }
}

fn fake_swp_stats() -> (i32, i32, i32) {
    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
        fn dlsym(
            handle: *mut std::ffi::c_void,
            symbol: *const std::ffi::c_char,
        ) -> *mut std::ffi::c_void;
    }
    let shim = compile_fake_swp_shim();
    let path = format!("{}\0", shim);
    let handle = unsafe { dlopen(path.as_ptr().cast(), 2) };
    assert!(!handle.is_null());
    let stats: unsafe extern "C" fn(*mut i32, *mut i32, *mut i32) = unsafe {
        std::mem::transmute(dlsym(handle, b"san90_swp_fake_stats_v1\0".as_ptr().cast()))
    };
    let (mut opens, mut closes, mut reconfigs) = (0, 0, 0);
    unsafe { stats(&mut opens, &mut closes, &mut reconfigs) };
    (opens, closes, reconfigs)
}

#[test]
fn swp_settings_reconfigure_open_handle_and_keep_link_up() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    let mut state = hardware_state(Some(compile_fake_swp_shim()));
    state.swp.mode = "std".into();
    state.fetch_sweep();
    let (opens0, closes0, reconfigs0) = fake_swp_stats();
    for (cmd, data) in [
        ("SET_FREQ", json!({"center": 101e6, "span": 20e6})),
        ("SET_FREQ", json!({"center": 102e6, "span": 5e6})),
        ("SET_RBW", json!({"mode": "manual", "rbw": 1e4})),
        ("SET_POINTS", json!({"points": 2001})),
        ("SET_REF", json!({"ref": -20.0})),
        ("SET_AMP", json!({"atten": 10})),
    ] {
        dispatch(&mut state, cmd, &data).unwrap();
        // The command reply is built right after dispatch: it must not say disconnected.
        assert_eq!(state.status()["connected"], true, "{cmd}");
        assert!(state.swp_hw.is_some(), "{cmd} must keep the handle");
    }
    let (opens1, closes1, reconfigs1) = fake_swp_stats();
    assert_eq!((opens1 - opens0, closes1 - closes0), (0, 0));
    assert_eq!(reconfigs1 - reconfigs0, 6);
    assert_eq!(state.actual["start"], 102e6 - 2.5e6);
    assert_eq!(state.actual["ref"], -22.5);
    let status = state.status();
    assert_eq!(status["req"]["swp"]["ref"], -20.0);
    assert_eq!(status["ref"], -22.5);
}

#[test]
fn swp_warning_without_frame_keeps_handle_and_reports_warning() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fail = std::path::Path::new("/tmp/websa_fake_swp_fail.txt");
    let _ = std::fs::remove_file(fail);
    let mut state = hardware_state(Some(compile_fake_swp_shim()));
    state.swp.mode = "std".into();
    state.fetch_sweep();
    std::fs::write(fail, "3 -12").unwrap();
    for _ in 0..3 {
        let (_, powr) = state.fetch_sweep();
        assert!(powr.is_empty());
    }
    assert!(
        state.swp_hw.is_some(),
        "a vendor warning must not close the handle"
    );
    assert_eq!(state.status()["status_warning"], -12);
    assert_eq!(state.status()["connected"], true);
    let (_, powr) = state.fetch_sweep();
    assert!(!powr.is_empty());
    assert_eq!(state.status()["status_warning"], 0);
    let _ = std::fs::remove_file(fail);
}

#[test]
fn swp_valid_overflow_frames_publish_without_changing_manual_front_end() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fail = std::path::Path::new("/tmp/websa_fake_swp_fail.txt");
    let _ = std::fs::remove_file(fail);
    let source = FAKE_SWP_SHIM
        .replace(
            "if (int status = fail_next()) { *points = 0; return status; }",
            "const int status = fail_next(); if (status && status != -12) { *points = 0; return status; }",
        )
        .replace(
            "*points = static_cast<uint32_t>(dev->points);\n    return 0;",
            "*points = static_cast<uint32_t>(dev->points);\n    return status;",
        );
    let mut state = hardware_state(Some(compile_fake_swp_source("valid_overflow", &source)));
    state.swp.mode = "std".into();
    state.fetch_sweep();
    dispatch(&mut state, "SET_REF", &json!({"ref": -20.0})).unwrap();
    dispatch(&mut state, "SET_AMP", &json!({"atten": 10})).unwrap();
    let (_, clean_power) = state.fetch_sweep();
    let points = u32::from_le_bytes(clean_power[8..12].try_into().unwrap()) as usize;
    assert_eq!(clean_power.len(), 16 + points * 4);
    let version = state.config_version;
    let actual = state.actual.clone();
    let requested = state.status()["req"]["swp"].clone();
    let opens = state.swp_open_count;
    std::fs::write(fail, "3 -12").unwrap();
    for _ in 0..3 {
        let (_, power) = state.fetch_sweep();
        assert_eq!(power, clean_power, "valid SDK power must reach the wire during overflow");
        assert_eq!(state.status_warning, -12);
        assert_eq!(state.auto.observation_status, -12);
        assert!(state.auto.last_peak.is_none(), "saturated data cannot fit Auto reference");
        assert!(state.swp_error.as_deref().is_some_and(|error| error.starts_with("IF overflow")));
        state.advance_auto();
        assert_eq!(state.config_version, version);
        assert_eq!(state.actual, actual);
        assert_eq!(state.status()["req"]["swp"], requested);
        assert_eq!(state.swp_open_count, opens);
        assert_eq!(state.swp_fail_streak, 0);
        assert!(state.link_up && state.swp_hw.is_some());
    }
    let (_, recovered) = state.fetch_sweep();
    assert_eq!(recovered, clean_power);
    assert_eq!(state.status_warning, 0);
    assert_eq!(state.auto.observation_status, 0);
    assert!(state.auto.last_peak.is_some());
    assert!(state.swp_error.is_none());
    let _ = std::fs::remove_file(fail);
}

#[test]
fn preset_keeps_hardware_and_uses_device_defaults() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
    let mut state = hardware_state(Some(compile_fake_swp_shim()));
    state.swp.mode = "std".into();
    state.fetch_sweep();
    let version = state.config_version;
    dispatch(
        &mut state,
        "SET_FREQ",
        &json!({"center": 100e6, "span": 1e6}),
    )
    .unwrap();
    dispatch(&mut state, "SET_FRAMEDET", &json!({"mode":"max_power"})).unwrap();
    state.sdr_squelch = -90.0;
    dispatch(&mut state, "SET_PRESET", &json!({})).unwrap();
    assert_eq!(state.sdr_squelch, websa_dsp::audio::SQUELCH_OPEN_DBM);
    assert!(state.channel_configs().iter().all(|config| config.squelch == websa_dsp::audio::SQUELCH_OPEN_DBM));
    assert!(state.hardware_shim.is_some() && state.swp_hw.is_some());
    let status = state.status();
    assert!(!status["device"].as_str().unwrap().contains("FAKE"));
    // Fake device defaults: 4.5 GHz / 9 GHz span covers the range -> full span.
    assert_eq!(status["preset_defaults"]["points"], 2001);
    assert_eq!(state.swp.points, 2001);
    assert_eq!(state.swp.ref_level, -10.0);
    assert_eq!(status["req"]["swp"]["frame_detector"], "device_default");
    assert_eq!(status["frame_detector"], "device_default");
    // Device span 0 - 9 GHz widens the 9 kHz model floor (apply_caps_from_defaults).
    assert_eq!((state.swp.center, state.swp.span), (4.5e9, 9e9));
    assert_eq!(status["caps"]["fmin"], 0.0);
    assert!(state.config_version > version, "versions never go back");
    assert_eq!(state.swp.mode, "std");
}

#[test]
fn mode_guards_follow_python_contract() {
    let mut state = hardware_state(None);
    state.swp.mode = "sdr".into();
    let err = dispatch(&mut state, "SET_FREQ", &json!({"center": 1e8})).unwrap_err();
    assert_eq!(err.code, "sdr_unsupported");
    state.swp.mode = "rta".into();
    assert_eq!(
        dispatch(&mut state, "SET_POINTS", &json!({"points": 1001}))
            .unwrap_err()
            .code,
        "swp_only"
    );
    assert_eq!(
        dispatch(&mut state, "SET_SDR", &json!({"center": 1e8}))
            .unwrap_err()
            .code,
        "sdr_mode_required"
    );
    state.swp.mode = "std".into();
    assert_eq!(
        dispatch(&mut state, "SET_RTA", &json!({"center": 1e8}))
            .unwrap_err()
            .code,
        "rta_mode_required"
    );
    state.swp.mode = "pnm".into();
    assert_eq!(
        dispatch(&mut state, "SET_REF", &json!({"ref": 0.0}))
            .unwrap_err()
            .code,
        "cmd_unavailable_measurement"
    );
}

#[test]
fn rta_span_and_center_follow_python_rules() {
    let mut state = hardware_state(None);
    state.swp.mode = "rta".into();
    dispatch(
        &mut state,
        "SET_RTA",
        &json!({"center": 100e6, "span": 10e6}),
    )
    .unwrap();
    // 50.78125 MHz / 2^round(log2(5.078)) = 50.78125 / 4.
    assert_eq!(state.rta_decimate, 4);
    assert_eq!(state.rta_span, 50_781_250.0 / 4.0);
    assert_eq!(state.status()["req"]["rta"]["span"], 50_781_250.0 / 4.0);
    dispatch(&mut state, "SET_RTA", &json!({"center": 1e3})).unwrap();
    assert_eq!(state.rta_center, 9e3 + state.rta_span / 2.0);
}

#[test]
fn sdr_center_tune_and_wideband_demods() {
    let mut state = hardware_state(None);
    state.swp.mode = "sdr".into();
    dispatch(
        &mut state,
        "SET_SDR",
        &json!({"center": 98e6, "decimate": 2}),
    )
    .unwrap();
    assert_eq!((state.sdr_center, state.sdr_decimate), (98e6, 2));
    // A primary frequency beyond the capture moves the shared capture to the requested signal.
    dispatch(&mut state, "SET_SDR_TUNE", &json!({"listen": 150e6})).unwrap();
    assert_eq!((state.sdr_center, state.sdr_listen), (150e6, 150e6));
    for (mode, ifbw, decimate) in [
        ("am", 12000.0, 20),
        ("nfm", 12000.0, 20),
        ("wfm", 180000.0, 3),
        ("drmplus", 100000.0, 6),
    ] {
        dispatch(
            &mut state,
            "SET_SDR_DEMOD",
            &json!({"mode": mode, "ifbw": ifbw}),
        )
        .unwrap();
        assert_eq!(state.sdr_demod, mode);
        assert_eq!(fine_decimate(ifbw).unwrap(), decimate, "{mode}");
    }
    // Occupied bandwidth plus transition must fit the coarse IQ rate.
    assert!(dispatch(&mut state, "SET_SDR_DEMOD", &json!({"ifbw": 1_000_000.0})).is_err());
    assert_eq!(state.sdr_if_bw, 100000.0);
}

#[test]
fn wfm_channel_carries_its_bandwidth() {
    let mut ddc = Ddc::configured(100e6, 100.3e6, 3, 50e6, &mut None, &[], 16).unwrap();
    assert!((ddc.channel_rate() - 325_520.833).abs() < 1.0);
    let frames = ddc.step().unwrap();
    assert!(frames.iter().all(|f| &f[..4] == b"IQBF"));
    assert_eq!(
        f64::from_le_bytes(frames[0][16..24].try_into().unwrap()),
        ddc.channel_rate()
    );
    // Retune across a bin outside the plan: new plan, same input, seq restarts at 0.
    let tail = ddc.tune(110e6, 3).unwrap();
    assert!(tail.iter().all(|f| &f[..4] == b"IQBF"));
    let frames = ddc.step().unwrap();
    assert_eq!(u32::from_le_bytes(frames[0][8..12].try_into().unwrap()), 0);
    assert_eq!(
        f64::from_le_bytes(frames[0][24..32].try_into().unwrap()),
        110e6
    );
}

#[test]
fn hardware_pnm_streams_vendor_cycle_contract() {
    let mut state = hardware_state(Some(compile_fake_pnm_shim()));
    state.swp.mode = "pnm".into();
    dispatch(
        &mut state,
        "SET_PNM",
        &json!({"center": 100.02e6, "threshold": -50.0, "traceavg": 4,
                "start": 100.0, "stop": 10e6}),
    )
    .unwrap();
    let mut progress = Vec::new();
    for _ in 0..5 {
        for msg in state.step_pnm() {
            assert_eq!(msg["cmd"], "PNM");
            assert_eq!(msg["carrier_freq"], 100.02e6);
            assert_eq!(msg["offset"].as_array().unwrap().len(), 64);
            progress.push(msg["progress"].as_u64().unwrap());
            if msg["done"] == json!(true) {
                assert_eq!(state.pnm_last["progress"], 100);
            }
        }
    }
    assert_eq!(
        progress,
        vec![25, 50, 75, 100, 25],
        "cycle restarts after done"
    );
    assert_eq!(state.status()["connected"], true);
    assert_eq!(state.status()["last_error"], "");
    // The older PNM fixture has no metadata export: hardware status must not
    // substitute a model, version, UID, or the golden fake-device identity.
    assert_eq!(state.status()["device_detail"], Value::Null);
    assert_eq!(state.status()["device"], "Device information unavailable");
    // A parameter change waits for the running cycle; the completed cycle then
    // drops the handle so the next one opens with the new profile.
    dispatch(&mut state, "SET_PNM", &json!({"center": 200.0e6})).unwrap();
    assert!(state.pnm_dirty);
    for _ in 0..5 {
        if state.pnm_hw.is_none() {
            break;
        }
        state.step_pnm();
    }
    assert!(state.pnm_hw.is_none());
}

// Independent fake SDK: preserve the legacy Actual and versioned trigger ABI.
const FAKE_RTA_SHIM: &str = r#"
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <thread>
struct FrontEnd { double ref_dbm; int atten, preamp, ifgain, gain, ref_clock, refclk_out; };
struct Request {
    double center_hz; uint32_t decimate; FrontEnd fe; int rbw_manual; double rbw_hz;
    int vbw_mode; double vbw_hz; int sweep_time_mode; double sweep_time;
    int trigger_source, trigger_edge; double trigger_level_dbm, trigger_safe_time,
        trigger_delay, trigger_pre_time, trigger_acq_time; uint32_t retrigger_count;
    double retrigger_period; int trigger_out, trigger_out_polarity;
};
struct Actual {
    double start_hz, stop_hz; uint32_t points; double ref_dbm, rbw_hz, vbw_hz, refclk_hz;
    int refclk_src, atten, preamp, ifgain;
};
struct TriggerActual {
    int valid, source, edge, out, polarity; uint32_t retrigger;
    double level, safetime, delay, pretime, acqtime, retriggerperiod, poi;
    uint64_t generation, packet_index; uint32_t packet_count;
    int complete, last_sdk_status; uint64_t first_data_timestamp; uint32_t trigger_edges;
};
static_assert(sizeof(Actual) == 72, "legacy RTA Actual ABI");
static_assert(sizeof(TriggerActual) == 128, "RTA trigger actual ABI");
static_assert(offsetof(TriggerActual, first_data_timestamp) == 112, "timestamp ABI");
struct FakeRta { Actual actual; TriggerActual trigger; std::thread::id owner; };
static std::atomic<int> opens{0}, closes{0}, reconfigs{0}, wrong_thread{0};
static std::atomic<uint32_t> permits{0};
static std::atomic<bool> lost_continuity_lastpacket{false};
static std::atomic<uint64_t> live_handle{0};
static bool owned(FakeRta* dev) {
    if (!dev) return false;
    if (dev->owner == std::this_thread::get_id()) return true;
    ++wrong_thread; return false;
}
static void configure(FakeRta* dev, const Request* r, Actual* out) {
    double span = 50781250.0 / r->decimate;
    dev->actual = {r->center_hz - span / 2, r->center_hz + span / 2, 8,
        r->fe.ref_dbm, 7500.0, 7500.0, 100e6, r->fe.ref_clock, 12, 1, 2};
    uint64_t generation = dev->trigger.generation + 1;
    dev->trigger = {};
    auto& t = dev->trigger;
    t.valid = 1; t.source = (r->trigger_source + 1) % 5;
    t.edge = (r->trigger_edge + 1) % 3; t.out = (r->trigger_out + 1) % 4;
    t.polarity = 1 - r->trigger_out_polarity; t.retrigger = r->retrigger_count + 3;
    t.level = r->trigger_level_dbm - 2.5; t.safetime = r->trigger_safe_time + 0.25;
    t.delay = r->trigger_delay + 0.5; t.pretime = r->trigger_pre_time + 0.75;
    t.acqtime = r->trigger_acq_time + 1.0; t.retriggerperiod = r->retrigger_period + 1.5;
    t.poi = 0.03125; t.generation = generation; t.packet_count = 2; t.complete = -1;
    *out = dev->actual;
}
extern "C" int san90_rta_open_v2(void** out, const Request* r, Actual* a) {
    if (!out || !r || !a || r->decimate == 0) return -100;
    auto* dev = new FakeRta{}; dev->owner = std::this_thread::get_id();
    configure(dev, r, a); ++opens; live_handle = reinterpret_cast<uintptr_t>(dev); *out = dev;
    return 0;
}
extern "C" int san90_rta_reconfig_v2(void* p, const Request* r, Actual* a) {
    auto* dev = static_cast<FakeRta*>(p);
    if (!owned(dev) || !r || !a) return -100;
    ++reconfigs;
    // Rollback restarts the old capture, preserving its applied trigger settings.
    if (r->trigger_level_dbm == -123.0) {
        ++dev->trigger.generation; dev->trigger.packet_index = 0;
        dev->trigger.complete = -1; dev->trigger.last_sdk_status = -7;
        dev->trigger.first_data_timestamp = 0; dev->trigger.trigger_edges = 0;
        return -7;
    }
    configure(dev, r, a); return 0;
}
extern "C" int san90_rta_trigger_actual_v1(void* p, TriggerActual* out) {
    auto* dev = static_cast<FakeRta*>(p);
    if (!owned(dev) || !out) return -100;
    *out = dev->trigger; return 0;
}
extern "C" int san90_rta_next_v2(void* p, double* freq, float* power, uint32_t cap, uint32_t* points) {
    auto* dev = static_cast<FakeRta*>(p);
    if (!owned(dev) || !freq || !power || !points || cap < 8) return -100;
    auto& t = dev->trigger;
    if (permits.load() == 0 || t.packet_index >= t.packet_count) {
        *points = 0; t.last_sdk_status = -10; return -10;
    }
    --permits; ++t.packet_index; t.complete = t.packet_index == t.packet_count ? 1 : 0;
    t.last_sdk_status = -12; t.first_data_timestamp = 9007199254740993ULL;
    t.trigger_edges = static_cast<uint32_t>(t.packet_index + 10);
    if (lost_continuity_lastpacket.exchange(false)) {
        t.packet_index = 0; t.complete = 1; t.last_sdk_status = -301;
    }
    for (uint32_t i = 0; i < 8; ++i) {
        freq[i] = dev->actual.start_hz + (dev->actual.stop_hz - dev->actual.start_hz) * i / 7;
        power[i] = -80.0f + static_cast<float>(i + t.packet_index);
    }
    *points = 8; return 0;
}
extern "C" int san90_rta_close_v1(void* p) {
    auto* dev = static_cast<FakeRta*>(p);
    if (!owned(dev)) return -100;
    ++closes; live_handle = 0; delete dev; return 0;
}
extern "C" void san90_rta_fake_permits_v1(uint32_t count) { permits = count; }
extern "C" void san90_rta_fake_lost_continuity_lastpacket_v1() { lost_continuity_lastpacket = true; }
extern "C" void san90_rta_fake_stats_v1(int* o, int* c, int* r, int* wrong, uint64_t* handle) {
    *o = opens.load(); *c = closes.load(); *r = reconfigs.load();
    *wrong = wrong_thread.load(); *handle = live_handle.load();
}
"#;

fn with_fake_rta_symbol<T>(name: &[u8], use_symbol: impl FnOnce(*mut std::ffi::c_void) -> T) -> T {
    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
        fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
        fn dlclose(handle: *mut std::ffi::c_void) -> i32;
    }
    let path = std::ffi::CString::new(compile_fake_swp_source("rta_trigger", FAKE_RTA_SHIM)).unwrap();
    let handle = unsafe { dlopen(path.as_ptr(), 2) };
    assert!(!handle.is_null(), "load fake RTA shim");
    let symbol = unsafe { dlsym(handle, name.as_ptr().cast()) };
    assert!(!symbol.is_null(), "missing fake RTA test control");
    let result = use_symbol(symbol);
    assert_eq!(unsafe { dlclose(handle) }, 0);
    result
}

fn fake_rta_permits(count: u32) {
    with_fake_rta_symbol(b"san90_rta_fake_permits_v1\0", |symbol| {
        let permit: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(symbol) };
        unsafe { permit(count) };
    });
}

fn fake_rta_lost_continuity_lastpacket() {
    with_fake_rta_symbol(b"san90_rta_fake_lost_continuity_lastpacket_v1\0", |symbol| {
        let inject: unsafe extern "C" fn() = unsafe { std::mem::transmute(symbol) };
        unsafe { inject() };
    });
}

fn fake_rta_stats() -> (i32, i32, i32, i32, u64) {
    with_fake_rta_symbol(b"san90_rta_fake_stats_v1\0", |symbol| {
        let stats: unsafe extern "C" fn(*mut i32, *mut i32, *mut i32, *mut i32, *mut u64) =
            unsafe { std::mem::transmute(symbol) };
        let (mut opens, mut closes, mut reconfigs, mut wrong_thread, mut handle) = (0, 0, 0, 0, 0);
        unsafe { stats(&mut opens, &mut closes, &mut reconfigs, &mut wrong_thread, &mut handle) };
        (opens, closes, reconfigs, wrong_thread, handle)
    })
}

fn waiting_fake_rta_state() -> DevState {
    let mut state = hardware_state(Some(compile_fake_swp_source("rta_trigger", FAKE_RTA_SHIM)));
    state.swp.mode = "rta".into();
    assert!(state.step_rta().is_empty());
    assert!(state.rta_hw.is_some(), "fake RTA open failed: {:?}", state.rta_error);
    fake_rta_permits(0);
    state
}

fn decode_rta_trigger(frame: &[u8]) -> Value {
    assert_eq!(&frame[..4], b"RTA2");
    let points = u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize;
    let waterfall = u16::from_le_bytes(frame[12..14].try_into().unwrap()) as usize;
    let trailer = 32 + points * 12 + waterfall * 2;
    assert_eq!(&frame[trailer..trailer + 4], b"TRIG");
    let len = u32::from_le_bytes(frame[trailer + 4..trailer + 8].try_into().unwrap()) as usize;
    assert_eq!(frame.len(), trailer + 8 + len, "exact metadata trailer length");
    serde_json::from_slice(&frame[trailer + 8..]).unwrap()
}

#[test]
fn rta_trigger_reads_back_all_eleven_applied_parameters() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = waiting_fake_rta_state();
    let before = state.status()["req"]["rta"]["trigger_actual"]["generation"].as_u64().unwrap();
    let requested = json!({"source":"level", "edge":"falling", "level":-32.0,
        "safetime":0.125, "delay":0.25, "pretime":0.5, "acqtime":1.0,
        "retrigger":4, "retriggerperiod":2.0, "out":"per_hop", "outpolarity":"positive"});
    dispatch(&mut state, "SET_TRIGGER", &requested).unwrap();
    let status = state.status();
    let rta = &status["req"]["rta"];
    let applied = &rta["trigger_actual"];
    let expected = json!({"source":"timer", "edge":"double", "level":-34.5,
        "safetime":0.375, "delay":0.75, "pretime":1.25, "acqtime":2.0,
        "retrigger":7, "retriggerperiod":3.5, "out":"per_sweep", "outpolarity":"negative"});
    for (key, value) in requested.as_object().unwrap() {
        assert_eq!(&rta[format!("trigger_{key}")], value, "requested {key}");
        assert_eq!(applied.get(key), expected.get(key), "SDK-applied {key}");
        assert_ne!(applied.get(key), Some(value), "{key} must come from SDK readback");
    }
    assert_eq!(applied["valid"], true);
    assert_eq!(applied["supported"], true);
    assert_eq!(applied["simulated"], false);
    assert_eq!(applied["poi"], 0.03125);
    assert_eq!(applied["generation"], before + 1);
    assert_eq!(applied["frames"], 0);
    assert_eq!(fake_rta_stats().3, 0, "fake SDK calls retain thread affinity");
}

#[test]
fn rta_trigger_reconfigure_rejection_preserves_request_applied_and_live_handle() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = waiting_fake_rta_state();
    dispatch(&mut state, "SET_TRIGGER", &json!({"source":"external", "level":-25.0})).unwrap();
    fake_rta_permits(1);
    let first = decode_rta_trigger(&state.step_rta());
    assert_eq!(first["frames"], 1);
    let request = state.trigger.clone();
    let rta = state.status()["req"]["rta"].clone();
    let versions = (state.config_version, state.freq_version);
    let stats = fake_rta_stats();
    let error = dispatch(&mut state, "SET_TRIGGER", &json!({"source":"timer", "level":-123.0,
        "edge":"double", "delay":0.5, "out":"per_profile"})).unwrap_err();
    assert_eq!(error.code, "device_error");
    assert!(state.rta_error.as_ref().unwrap().contains("status=-7"));
    assert_eq!(state.trigger, request);
    let restored = state.status()["req"]["rta"].clone();
    for key in ["source", "edge", "level", "safetime", "delay", "pretime", "acqtime",
        "retrigger", "retriggerperiod", "out", "outpolarity"] {
        assert_eq!(restored[format!("trigger_{key}")], rta[format!("trigger_{key}")], "request {key}");
        assert_eq!(restored["trigger_actual"][key], rta["trigger_actual"][key], "restored applied {key}");
    }
    assert_eq!(restored["trigger_actual"]["generation"].as_u64().unwrap(),
        rta["trigger_actual"]["generation"].as_u64().unwrap() + 1);
    assert_eq!(restored["trigger_actual"]["frames"], 1);
    assert_eq!(restored["trigger_actual"]["packet_index"], Value::Null);
    assert_eq!(restored["trigger_actual"]["complete"], Value::Null);
    assert_eq!(restored["trigger_actual"]["last_sdk_status"], -7);
    assert_eq!((state.config_version, state.freq_version), versions);
    let after = fake_rta_stats();
    assert_eq!((after.0, after.1, after.3, after.4), (stats.0, stats.1, stats.3, stats.4));
    assert_ne!(after.4, 0, "same live SDK handle survives rejection");
    assert_eq!(after.2, stats.2 + 1, "rejection reached transactional SDK reconfigure");
    assert!(state.rta_hw.is_some());
    fake_rta_permits(1);
    let metadata = decode_rta_trigger(&state.step_rta());
    assert_eq!(metadata["source"], rta["trigger_actual"]["source"]);
    assert_eq!(metadata["level"], rta["trigger_actual"]["level"]);
    assert_eq!(metadata["generation"], restored["trigger_actual"]["generation"]);
    assert_eq!(metadata["frames"], 2, "rollback retains the lifetime capture counter");
    assert_eq!(metadata["packet_index"], 1, "rollback restarts the old capture");
    assert_eq!(state.status()["connected"], true);
}

#[test]
fn rta_packet_metadata_counts_captures_and_completes_only_final_packet() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = waiting_fake_rta_state();
    let initial = state.status()["req"]["rta"]["trigger_actual"].clone();
    assert_eq!(initial["frames"], 0);
    assert_eq!(initial["last_sdk_status"], -10);
    assert_eq!(initial["waiting"], Value::Null, "-10 does not prove trigger wait");
    assert_eq!(initial["complete"], Value::Null);
    let stats = fake_rta_stats();
    for _ in 0..3 {
        assert_eq!(state.status()["req"]["rta"]["trigger_actual"], initial,
            "STATUS polling cannot advance capture metadata");
    }
    fake_rta_permits(2);
    for packet in 1..=2u64 {
        let metadata = decode_rta_trigger(&state.step_rta());
        assert_eq!(metadata, state.status()["req"]["rta"]["trigger_actual"]);
        assert_eq!(metadata["frames"], packet);
        assert_eq!(metadata["packet_index"], packet);
        assert_eq!(metadata["packet_count"], 2);
        assert_eq!(metadata["complete"], packet == 2);
        assert_eq!(metadata["generation"], initial["generation"]);
        assert_eq!(metadata["last_sdk_status"], -12, "successful data preserves SDK warning");
        assert_eq!(metadata["first_ts"], 9_007_199_254_740_993u64);
        assert_eq!(metadata["edges"], packet + 10);
        assert_eq!(state.status()["req"]["rta"]["trigger_actual"]["frames"], packet);
    }
    let completed = state.rta_completed_frame.clone().expect("completed frame retained");
    let complete_metadata = state.status()["req"]["rta"]["trigger_actual"].clone();
    let observation = state.auto.observation;
    for _ in 0..3 {
        assert_eq!(state.step_rta(), completed, "late consumers receive the exact complete capture");
        assert_eq!(state.status()["req"]["rta"]["trigger_actual"], complete_metadata,
            "replaying a capture cannot advance frames or replace complete metadata");
    }
    assert_eq!(state.auto.observation, observation, "replays cannot become fresh Auto observations");
    assert_eq!(complete_metadata["frames"], 2);
    assert_eq!(complete_metadata["complete"], true);
    dispatch(&mut state, "SET_TRIGGER", &json!({"source":"bus"})).unwrap();
    assert!(state.rta_completed_frame.is_none(), "configuration releases the retained capture");
    fake_rta_permits(1);
    assert_eq!(decode_rta_trigger(&state.step_rta())["frames"], 3);
    assert_eq!(fake_rta_stats().0, stats.0, "releasing capture retains its owner/handle");
    fake_rta_permits(1);
    state.step_rta();
    assert!(state.rta_completed_frame.is_some());
    dispatch(&mut state, "SET_MODE", &json!({"mode":"std"})).unwrap();
    assert!(state.rta_completed_frame.is_none(), "mode exit retires the retained capture");
    assert_eq!(state.status()["req"]["rta"]["trigger_actual"]["valid"], false);
}

#[test]
fn rta_lastpacket_without_continuity_renders_data_without_claiming_completion() {
    let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = waiting_fake_rta_state();
    fake_rta_permits(1);
    let first = decode_rta_trigger(&state.step_rta());
    assert_eq!(first["frames"], 1);
    assert_eq!(first["packet_index"], 1);
    assert_eq!(first["complete"], false);

    fake_rta_lost_continuity_lastpacket();
    fake_rta_permits(1);
    let frame = state.step_rta();
    let metadata = decode_rta_trigger(&frame);
    assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()), 8);
    let power_at = 24 + 8 * 8;
    assert_eq!(f32::from_le_bytes(frame[power_at..power_at + 4].try_into().unwrap()), -80.0);
    assert_eq!(metadata["frames"], 2, "valid spectrum survives continuity loss");
    assert_eq!(metadata["packet_index"], Value::Null);
    assert_eq!(metadata["complete"], Value::Null, "lastpacket alone cannot prove completion");
    assert_eq!(metadata["last_sdk_status"], -301);
    assert_eq!(metadata, state.status()["req"]["rta"]["trigger_actual"]);
}

#[test]
fn rta2_service_trailer_matches_golden_without_changing_legacy_payload() {
    let manifest: Value = serde_json::from_str(include_str!("../../../../tests/fixtures/frames/manifest.json")).unwrap();
    let legacy = include_bytes!("../../../../tests/fixtures/frames/rta.bin");
    let golden = include_bytes!("../../../../tests/fixtures/frames/rta2.bin");
    let mut state = hardware_state(None);
    state.rta_trigger_actual = manifest["rta2"]["metadata"].clone();
    let frame = state.attach_rta_trigger(legacy.to_vec());
    assert_eq!(&frame[..4], b"RTA2");
    assert_eq!(&frame[4..legacy.len()], &legacy[4..]);
    assert_eq!(&frame[..legacy.len() + 4], &golden[..legacy.len() + 4]);
    assert_eq!(decode_rta_trigger(golden), manifest["rta2"]["metadata"]);
    assert_eq!(decode_rta_trigger(&frame), manifest["rta2"]["metadata"]);
    assert!(state.attach_rta_trigger(Vec::new()).is_empty());
}

#[test]
fn simulated_rta_never_claims_hardware_trigger_support() {
    let mut state = hardware_state(None);
    state.swp.mode = "rta".into();
    dispatch(&mut state, "SET_TRIGGER", &json!({"source":"external", "retrigger":3})).unwrap();
    let metadata = decode_rta_trigger(&state.step_rta());
    assert_eq!(metadata["simulated"], true);
    assert_eq!(metadata["supported"], false);
    assert_eq!(metadata["valid"], false);
    assert_eq!(metadata["source"], Value::Null);
    assert_eq!(metadata["waiting"], Value::Null);
    assert_eq!(metadata["complete"], Value::Null);
    assert_eq!(metadata["frames"], 1);
}
