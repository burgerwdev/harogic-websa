use super::*;

fn state() -> DevState {
    let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
    state.swp.mode="sdr".into();state.link_up=true;state.sdr_center=100e6;state.sdr_listen=100e6;
    for config in &mut state.sdr_channels {config.processing_enabled=false;config.enabled=false;}
    state.sdr_channels[0].enabled=true;
    let saved=&mut state.sdr_channels[1];
    saved.enabled=true;saved.freq=100.7e6;
    saved.baseband=Some(receiver_workers::BasebandConfig {passband_hz:8_000.0,transition_hz:1_000.0,minimum_rate_hz:16_000.0});
    state.remember_receiver_streams();
    state
}

#[test]
fn capture_changes_pause_and_restore_saved_definitions_without_reusing_identity() {
    let mut state=state();
    let before=state.receiver_snapshot()["receivers"][1].clone();
    let away=state.parse_service_capture(&json!({"center":200e6,"listen":200e6,"decimate":16})).unwrap();
    assert!(away.band_paused.contains(&2));
    assert!(!away.configs[1].enabled);
    state.apply_service_capture_candidate(away,state.front_end(state.sdr_ref),false).unwrap();
    let paused=state.receiver_snapshot()["receivers"][1].clone();
    assert_eq!(paused["confirmed"],before["confirmed"]);
    assert_eq!(paused["receiver_uid"],before["receiver_uid"]);
    assert_eq!(paused["coverage"]["state"],"out_of_band");
    assert_eq!(paused["state"],"interrupted");
    let back=state.parse_service_capture(&json!({"center":100e6,"listen":100e6,"decimate":16})).unwrap();
    assert!(back.band_paused.is_empty());assert!(back.configs[1].enabled);
    state.apply_service_capture_candidate(back,state.front_end(state.sdr_ref),false).unwrap();
    let restored=state.receiver_snapshot()["receivers"][1].clone();
    assert_eq!(restored["confirmed"],before["confirmed"]);
    assert_eq!(restored["receiver_uid"],before["receiver_uid"]);
    assert_eq!(restored["coverage"]["state"],"in_band");
}

#[test]
fn narrowing_iq_pauses_partial_passbands_instead_of_widening_the_source() {
    let mut state=state();
    state.hardware_shim=Some("coverage-planning-only".into());
    state.sdr_channels[1].freq=102e6;
    let wide=state.parse_service_capture(&json!({"decimate":8})).unwrap();
    assert!(wide.configs[1].enabled);
    let narrow=state.parse_service_capture(&json!({"decimate":128})).unwrap();
    assert!(narrow.bandwidth<wide.bandwidth);
    assert!(narrow.band_paused.contains(&2));
    assert!(!narrow.configs[1].enabled);
    assert_eq!(narrow.configs[1].freq,102e6);
    state.sdr_channels=narrow.configs;state.receivers.band_paused=narrow.band_paused;
    let back=state.parse_service_capture(&json!({"decimate":8})).unwrap();
    assert!(back.configs[1].enabled);
    assert!(back.band_paused.is_empty());
}

#[test]
fn explicit_stop_while_out_of_band_removes_only_automatic_resume_intent() {
    let mut state=state();
    state.receivers.band_paused.insert(2);state.sdr_channels[1].enabled=false;
    state.apply_channel_config(&json!({"channel":2,"enabled":false})).unwrap();
    assert!(!state.receivers.band_paused.contains(&2));
    let back=state.parse_service_capture(&json!({"center":100e6,"listen":100e6})).unwrap();
    assert!(!back.configs[1].enabled);
    let saved=state.receiver_snapshot()["receivers"][1].clone();
    assert_eq!(saved["confirmed"]["enabled"],false);
    assert_eq!(saved["confirmed"]["baseband"]["frequency_hz"],100.7e6);
}
