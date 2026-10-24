use super::*;

/// Calibrate candidate kernels without holding the device state or pausing capture.
pub(super) async fn preflight(app: &Arc<App>, cmd: &str, data: &Value) -> Result<u64, String> {
    static ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    let display_only = matches!(cmd, "AUTO_SCALE" | "SET_REF")
        && data["display_only"] == true
        && (cmd == "AUTO_SCALE" || data["mode"] == "auto");
    if display_only { return Ok(app.state.lock().unwrap().config_version); }
    if !matches!(cmd, "AUTO_SCALE" | "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC" | "SET_SDR" | "SET_MODE" | "CONNECT" | "SET_REF" | "SET_AMP" | "SET_REFCK" | "SET_REFCKOUT" | "SET_PRESET") { return Ok(app.state.lock().unwrap().config_version); }
    let supersede = cmd != "AUTO_SCALE" && !(cmd == "SET_REF" && data["mode"] == "auto")
        && app.state.lock().unwrap().cancel_auto_session();
    if supersede {
        let snapshot = app.state.lock().unwrap().status_snapshot();
        let built = snapshot.build();
        app.state.publish_status(snapshot.revision, built);
    }
    // Wait for admission before snapshotting the configuration revision.
    let permit = ADMISSION.acquire().await.map_err(|_| "admission_busy: service admission already running")?;
    let candidate = {
        let state = app.state.lock().unwrap();
        let revision = state.config_version;
        let channels = matches!(cmd, "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC");
        let front_end = matches!(cmd, "AUTO_SCALE" | "SET_REF" | "SET_AMP" | "SET_REFCK" | "SET_REFCKOUT");
        let preset = cmd == "SET_PRESET";
        let capture = cmd == "SET_SDR" || front_end || preset;
        let starting = (cmd == "SET_MODE" && data["mode"] == "sdr") || (cmd == "CONNECT" && state.swp.mode == "sdr");
        if !channels && !capture && !starting { return Ok(revision); }
        if !starting && state.swp.mode != "sdr" { return Ok(revision); }
        if state.hardware_shim.is_none() && state.ddc.is_none() && state.pipeline.is_none() {
            if channels {state.parse_channel_config(data)?;}
            if capture {
                if preset {state.preset_service_capture()?;} else {state.parse_service_capture(data)?;}
            }
            return Ok(revision);
        }
        let mut configs = state.channel_configs();
        let mut live_channels = channels;
        let (center, fs, bandwidth, taps, existing) = if capture || starting {
            let requested = if cmd == "SET_SDR" { data.clone() } else { json!({}) };
            let next = if preset {state.preset_service_capture()?} else {state.parse_service_capture(&requested)?};
            configs = next.configs;
            (next.center, next.source_rate, next.bandwidth, next.taps, None)
        } else {
            let next = state.parse_channel_config(data)?;
            if let Some(capture) = state.channel_recapture(&next)? {
                live_channels = false;
                configs = capture.configs;
                (capture.center, capture.source_rate, capture.bandwidth, capture.taps, None)
            } else {
                let slot = next.channel as usize - 1;
                configs[slot] = next;
                let fs = state.capture_bandwidth() / 0.8;
                let bins = state.pipeline.as_ref().map(|p| p.snapshot.selected_bins)
                    .or_else(|| state.ddc.as_ref().map(|d| d.0.stream.bins().len()));
                (state.capture_center(), fs, state.capture_bandwidth(), state.sdr_pfb_taps_per_phase, bins)
            }
        };
        let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
        let mut bins = Vec::new();
        for config in configs.iter().filter(|c| c.enabled) {
            let plan = service_ddc::channel_plan(COARSE_RATE, config)?;
            Ddc::validate_service_frequency(center, bandwidth, fs, taps, config.freq, &plan)?;
            let signed = signed_bin(config.freq, center);
            for neighbour in signed - PLAN_NEIGHBOURS..=signed + PLAN_NEIGHBOURS {
                if (-m / 2..m / 2).contains(&neighbour) {
                    let bin = neighbour.rem_euclid(m) as u32;
                    if !bins.contains(&bin) { bins.push(bin); }
                }
            }
        }
        // Live channel changes retain old selected bins and can add one new bin.
        (revision, configs, fs, taps, existing.unwrap_or(bins.len().max(1)), m as usize, live_channels)
    };
    let (revision, configs, fs, taps, selected, maximum, channels) = candidate;
    let result = tokio::task::spawn_blocking(move || {
        if channels {
            service_cost::admit_service(&configs, fs, taps, selected)?;
            if selected < maximum { service_cost::admit_service(&configs, fs, taps, selected + 1)?; }
            Ok(())
        } else {
            service_cost::admit_service(&configs, fs, taps, selected)
        }
    }).await.map_err(|error| format!("service admission worker: {error}"))?;
    drop(permit);
    result?;
    Ok(revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECONFIG_FAIL: &str = "/tmp/websa_fake_iqs_reconfig_fail.txt";
    struct Injection;
    impl Drop for Injection {
        fn drop(&mut self) {let _=std::fs::remove_file(RECONFIG_FAIL);}
    }

    const AUTO_CONTROL: &str = "/tmp/websa_fake_iqs_auto.txt";
    struct AutoInjection;
    impl Drop for AutoInjection {
        fn drop(&mut self) { let _ = std::fs::remove_file(AUTO_CONTROL); }
    }

    fn fresh_iqs_observation(state: &mut DevState) -> i32 {
        // Consume actual owner packets, excluding every packet from the old
        // hardware generation after reconfiguration.
        let ddc = &state.ddc.as_ref().unwrap().0;
        let capture = ddc.hardware.as_ref().unwrap();
        let packet = loop {
            let packet = capture.next().unwrap();
            if packet.generation >= ddc.generation { break packet; }
        };
        state.status_warning = packet.warning;
        state.note_auto_acquisition(packet.warning);
        if packet.warning == 0 {
            if let Some((freq, power)) = packet.spectrum {
                state.observe_peak(&power, Some(&freq));
            }
        }
        packet.warning
    }

    #[tokio::test(flavor="current_thread")]
    async fn explicit_sdr_auto_recovers_persistent_iqs_overflow_using_hardware_readback() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let _injection=AutoInjection;
        let app=app();
        std::fs::write(AUTO_CONTROL,"-15 inf").unwrap();
        command(&app,"SET_REF",json!({"ref":-45.0})).await.unwrap();
        {
            let mut state=app.state.lock().unwrap();
            for _ in 0..6 {
                assert_eq!(fresh_iqs_observation(&mut state),-12);
                state.advance_auto();
                assert_eq!(state.sdr_ref,-45.0,"manual SDR reference must hold during overflow");
                assert_eq!(state.active_auto_ref(),Some(-47.5));
            }
        }
        command(&app,"AUTO_SCALE",json!({"current_ref":-80.0})).await.unwrap();
        let mut state=app.state.lock().unwrap();
        let mut previous=state.active_auto_ref().unwrap();
        let mut steps=0;
        let mut clean=false;
        for _ in 0..1024 {
            let status=fresh_iqs_observation(&mut state);
            let requested=state.sdr_ref;
            state.advance_auto();
            let actual=state.active_auto_ref().unwrap();
            if state.sdr_ref != requested {
                assert_eq!(state.sdr_ref,previous+10.0);
                assert!(actual>previous);
                previous=actual;
                steps+=1;
            }
            clean |= status==0;
            if state.auto.view()["adjusting"]==false { break; }
        }
        assert!(steps>=3,"fixture requires several actual IQS reconfigurations");
        assert!(clean,"success requires a fresh clean IQS spectrum");
        assert_eq!(state.auto.view()["adjusting"],false);
        assert_eq!(state.auto.view()["result"],"applied");
        assert!(state.active_auto_ref().unwrap()>=-15.0);
        assert_eq!(state.status_warning,0);
        assert!(state.auto.target.is_some(),"display placement is published after recovery");
    }

    #[tokio::test(flavor="current_thread")]
    async fn sdr_display_only_preflight_never_reconfigures_overflowing_iqs() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let _injection=AutoInjection;
        let app=app();
        std::fs::write(AUTO_CONTROL,"-15 inf").unwrap();
        command(&app,"SET_REF",json!({"ref":-45.0})).await.unwrap();
        let before={let state=app.state.lock().unwrap();
            (state.sdr_ref,state.config_version,state.ddc.as_ref().unwrap().0.generation,state.active_auto_ref())};
        command(&app,"AUTO_SCALE",json!({"display_only":true,"current_ref":-80.0})).await.unwrap();
        let mut state=app.state.lock().unwrap();
        assert_eq!(fresh_iqs_observation(&mut state),-12);
        state.advance_auto();
        assert_eq!(state.auto.view()["adjusting"],false);
        assert_eq!(state.auto.view()["result"],"overflow");
        assert_eq!((state.sdr_ref,state.config_version,state.ddc.as_ref().unwrap().0.generation,state.active_auto_ref()),before);
        assert_eq!(fresh_iqs_observation(&mut state),-12,"overflow must persist without a hardware correction");
    }

    fn app() -> Arc<App> {
        let _=std::fs::remove_file(RECONFIG_FAIL);
        let _=std::fs::remove_file("/tmp/websa_fake_iqs_fail.txt");
        let _=std::fs::remove_file("/tmp/websa_fake_iqs_open_fail.txt");
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_decimate=1;
        let shim=hardware_iqs::tests::compile_fake_iqs_shim();
        let mut hardware=Some(HardwareCapture::start(shim.clone(),100e6,state.front_end(0.0),1).unwrap());
        let mut ddc=Ddc::configured_service(100e6,50e6,&mut hardware,16,IQ_FS,&state.channel_configs()).unwrap();
        ddc.hardware.as_mut().unwrap().arm().unwrap();
        state.hardware_shim=Some(shim);
        state.ddc=Some(SendDdc(ddc));
        Arc::new(App {state:StateMutex::new(state),clients:PLMutex::new(Vec::new()),
            next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
            identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
            display_version:Arc::new(AtomicU64::new(0))})
    }

    async fn command(app:&Arc<App>,cmd:&str,data:Value)->Result<bool,CommandError> {
        let revision=preflight(app,cmd,&data).await.map_err(|e|CommandError::new("bad_parameter",&e))?;
        let mut state=app.state.lock().unwrap();
        assert_eq!(state.config_version,revision);
        dispatch(&mut state,cmd,&data)
    }

    #[tokio::test(flavor="current_thread")]
    async fn front_end_ack_reports_resolved_gain_before_next_spectrum() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let app=app();
        command(&app,"SET_REF",json!({"ref":-50.0})).await.unwrap();
        command(&app,"SET_AMP",json!({"atten":5,"preamp":0,"ifgain":0})).await.unwrap();
        let state=app.state.lock().unwrap();
        let status=state.status_snapshot().build();
        assert_eq!(status["req"]["sdr"]["ref"],-50.0);
        assert_eq!(status["amp"]["atten"],5);
        assert_eq!(status["actual"]["ref"],-27.5);
        assert_eq!(status["amp"]["atten_actual"],18);
        assert_eq!(status["amp"]["preamp_actual"],1);
        assert_eq!(status["amp"]["ifgain_actual"],3);
    }

    #[tokio::test(flavor="current_thread")]
    async fn relevel_after_retained_bins_and_mode_change_uses_fresh_calibrated_geometry() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let app=app();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":1,"freq":100e6+4.0*BIN_SPACING})).await.unwrap();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":1,"mode":"nfm","if_bw":12000.0})).await.unwrap();
        let retained=app.state.lock().unwrap().ddc.as_ref().unwrap().0.stream.bins().len();
        for (cmd,data) in [("SET_REF",json!({"ref":-50.0})),("SET_AMP",json!({"atten":10})),
            ("SET_REFCK",json!({"mode":"external"})),("SET_REFCKOUT",json!({"on":true}))] {
            command(&app,cmd,data).await.unwrap();
            let mut state=app.state.lock().unwrap();
            let ddc=&mut state.ddc.as_mut().unwrap().0;
            assert!(ddc.stream.bins().len()<retained);
            assert_eq!(ddc.service_configs.as_ref().unwrap()[0].mode,"nfm");
            ddc.packet().unwrap();ddc.packet().unwrap();
            assert_eq!(ddc.stamp_gaps,0);
        }
        let mut state=app.state.lock().unwrap();
        assert_eq!(state.sdr_ref,-50.0);
        let generation=state.ddc.as_ref().unwrap().0.generation;
        let versions=(state.config_version,state.freq_version);
        dispatch(&mut state,"AUTO_SCALE",&json!({"current_ref":-80.0,"display_only":true})).unwrap();
        assert_eq!(state.auto.view()["adjusting"],true);
        state.note_auto_acquisition(0);
        state.observe_peak(&[-105.0,-105.0,-105.0,-30.0],None);
        state.advance_auto();
        assert_eq!(state.auto.view()["pending"],-20.0);
        assert_eq!(state.auto.view()["hardware_adjusting"],false);
        assert_eq!(state.auto.view()["adjusting"],true);
        state.note_auto_acquisition(0);
        state.observe_peak(&[-105.0,-105.0,-105.0,-30.0],None);
        state.advance_auto();
        assert_eq!(state.auto.target,Some(-20.0));
        assert_eq!(state.auto.view()["adjusting"],false);
        assert_eq!(state.sdr_ref,-50.0);
        assert_eq!(state.ddc.as_ref().unwrap().0.generation,generation);
        assert_eq!((state.config_version,state.freq_version),versions);
    }

    #[tokio::test(flavor="current_thread")]
    async fn rejected_front_end_and_preset_preserve_confirmed_controls() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let _injection=Injection;
        for (cmd,data) in [("SET_REF",json!({"ref":-50.0,"range_db":80.0})),
            ("SET_AMP",json!({"atten":10,"preamp":1,"ifgain":3,"gain_strategy":1})),
            ("SET_REFCK",json!({"mode":"external"})),("SET_REFCKOUT",json!({"on":true})),
            ("SET_PRESET",json!({}))] {
            let app=app();
            let confirmed={let state=app.state.lock().unwrap();
                json!([state.sdr_ref,state.sdr_ref_set,
                    [state.swp.atten,state.swp.preamp,state.swp.ifgain,state.swp.gain_strategy],
                    state.swp.ref_range_db,state.ref_clock,state.refclk_out,
                    state.channel_configs(),state.sdr_center,state.sdr_decimate,state.auto.view(),
                    state.config_version,state.freq_version])};
            std::fs::write(RECONFIG_FAIL,"-100").unwrap();
            command(&app,cmd,data).await.unwrap_err();
            let state=app.state.lock().unwrap();
            assert_eq!(json!([state.sdr_ref,state.sdr_ref_set,
                [state.swp.atten,state.swp.preamp,state.swp.ifgain,state.swp.gain_strategy],
                state.swp.ref_range_db,state.ref_clock,state.refclk_out,
                state.channel_configs(),state.sdr_center,state.sdr_decimate,state.auto.view(),
                state.config_version,state.freq_version]),confirmed,"{cmd}");
        }
    }

    #[tokio::test(flavor="current_thread")]
    async fn preset_replaces_actual_service_slots_and_retires_old_audio_before_new_epoch() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let app=app();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":1,"mode":"nfm","if_bw":12000.0,"nr":true})).await.unwrap();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":2,"enabled":true,"freq":100e6+BIN_SPACING,"mode":"cw","if_bw":500.0})).await.unwrap();
        command(&app,"SET_REF",json!({"ref":-50.0})).await.unwrap();
        let old_registry={
            let mut state=app.state.lock().unwrap();
            let registry=state.stream_registry();
            let mut frames=Vec::new();
            for _ in 0..160 {frames.extend(state.ddc.as_mut().unwrap().0.packet().unwrap());}
            state.submit_receiver_frames(&frames).unwrap();
            registry
        };
        command(&app,"SET_PRESET",json!({})).await.unwrap();
        let mut state=app.state.lock().unwrap();
        assert_eq!(state.channel_configs(),DevState::preset_service_channels());
        assert_eq!((state.sdr_center,state.sdr_decimate,state.sdr_ref,state.sdr_ref_set),(1e9,16,0.0,false));
        assert_eq!(state.ref_clock,"internal");assert!(!state.refclk_out);
        let ddc=&state.ddc.as_ref().unwrap().0;
        assert_eq!(ddc.service_configs.as_ref().unwrap(),&DevState::preset_service_channels());
        assert_eq!(ddc.capture_center,1e9);assert_eq!(ddc.listen,1e9);
        assert_eq!(ddc.device_decimate,16);
        assert!(ddc.aux.is_empty());
        let registry=state.stream_registry();
        assert_eq!(registry.len(),1);
        assert_ne!(registry[0].epoch,old_registry[0].epoch);
        assert!(state.pending_receiver.audio.iter().any(|frame|
            old_registry.iter().any(|r|u32::from_le_bytes(frame[36..40].try_into().unwrap())==r.epoch)));
        state.pending_receiver=receiver_workers::ReceiverFrames::default();
        let mut frames=Vec::new();
        for _ in 0..16 {frames.extend(state.ddc.as_mut().unwrap().0.packet().unwrap());}
        let output=state.process_receiver_frames_with_registry(&frames,&registry,true).unwrap();
        assert!(output.audio.iter().any(|frame|u32::from_le_bytes(frame[36..40].try_into().unwrap())==registry[0].epoch));
        assert!(output.audio.iter().all(|frame|u32::from_le_bytes(frame[36..40].try_into().unwrap())==registry[0].epoch));
        assert_eq!(state.ddc.as_ref().unwrap().0.stamp_gaps,0);
    }
}
