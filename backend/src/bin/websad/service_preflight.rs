use super::*;

pub(super) struct Admission {
    revision: u64,
    command: String,
    request: Value,
    prepared: Option<receiver_workers::PreparedDecoders>,
    retired: Option<receiver_workers::PreparedDecoders>,
    released: Option<receiver_workers::PreparedDecoders>,
    consumed: bool,
    processing:Option<Value>,
    workers:Option<receiver_workers::ReceiverWorkers>,
}

impl Admission {
    fn empty(revision: u64, command: &str, request: &Value) -> Self {
        Self { revision, command: command.into(), request: request.clone(), prepared: None, retired: None, released: None, consumed: false,processing:None,workers:None }
    }

    pub(super) fn dispatch(&mut self, state: &mut DevState, command: &str, request: &Value) -> Result<bool, CommandError> {
        if self.consumed || state.config_version != self.revision || self.command != command || self.request != *request {
            return Err(CommandError::new("configuration_changed", "configuration changed during admission; retry command"));
        }
        self.consumed = true;
        prepare_receiver_recovery(state, command, request)?;
        if state.receiver_workers.is_none() {
            state.receiver_workers=self.workers.take();
        }
        if self.prepared.is_some() {
            if state.receiver_workers.is_none() {
                state.receiver_workers = Some(receiver_workers::ReceiverWorkers::new()
                    .map_err(|error| CommandError::new("device_error", &error))?);
            }
            self.retired = Some(state.receiver_workers.as_mut().unwrap().exchange_prepared(self.prepared.take().unwrap()));
        }
        let previous_auto = state.auto.clone();
        if matches!(command, "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC" | "SET_SDR" | "SET_MODE" | "CONNECT" | "SET_REF" | "SET_AMP" | "SET_REFCK" | "SET_REFCKOUT" | "SET_PRESET")
            && !(command == "SET_REF" && request["mode"] == "auto") {
            state.cancel_auto_session();
        }
        let result = dispatch_ready(state, command, request);
        if !matches!(result, Ok(true)) {
            if result.is_err() && state.config_version == self.revision { state.auto = previous_auto; }
            if let Some(previous) = self.retired.take() {
                if let Some(workers) = state.receiver_workers.as_mut() {
                    self.prepared = Some(workers.exchange_prepared(previous));
                }
            }
            // A recovery attempt may have created replacement workers after the
            // latched failure; retire them so the retry can reap.
            if state.receiver_failure.is_some() {
                if let Some(workers) = state.receiver_workers.as_mut() { workers.stop_failed(); }
            }
        } else {
            if let Some(report)=self.processing.as_ref() {state.receiver_admission=Some(report.clone());}
            let configs = if state.swp.mode == "sdr" { state.channel_configs() } else { Vec::new() };
            if let Some(workers) = state.receiver_workers.as_mut() {
                self.released = Some(workers.release_prepared(&configs));
            }
        }
        result
    }
}

/// Prepare kernels and decoder processes without holding device state or pausing capture.
pub(super) async fn preflight(app: &Arc<App>, cmd: &str, data: &Value) -> Result<Admission, String> {
    preflight_with(app, cmd, data, receiver_workers::PreparedDecoders::new).await
}

async fn preflight_with<F>(app: &Arc<App>, cmd: &str, data: &Value, prepare: F) -> Result<Admission, String>
where F: FnOnce(&[receiver_workers::ChannelConfig]) -> Result<receiver_workers::PreparedDecoders, String> + Send + 'static {
    static ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    static WAITING:tokio::sync::Semaphore=tokio::sync::Semaphore::const_new(64);
    {
        let state = app.state.lock_async().await.unwrap();
        if state.swp.mode == "server" && !matches!(cmd, "STATUS" | "SET_MODE" | "SET_SERVER") {
            return Err("leave server mode before changing hardware settings".into());
        }
    }
    let display_only = matches!(cmd, "AUTO_SCALE" | "SET_REF")
        && data["display_only"] == true
        && (cmd == "AUTO_SCALE" || data["mode"] == "auto");
    if display_only { return Ok(Admission::empty(app.state.lock_async().await.unwrap().config_version, cmd, data)); }
    if !matches!(cmd, "AUTO_SCALE" | "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC" | "SET_SDR" | "SET_MODE" | "CONNECT" | "SET_REF" | "SET_AMP" | "SET_REFCK" | "SET_REFCKOUT" | "SET_PRESET") { return Ok(Admission::empty(app.state.lock_async().await.unwrap().config_version, cmd, data)); }
    {
        let state = app.state.lock_async().await.unwrap();
        if !data.is_object() { return Err("JSON message must be an object".into()); }
        if matches!(cmd, "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC") {
            if state.swp.mode != "sdr" { return Err("channel commands require SDR mode".into()); }
            if matches!(cmd, "SET_SDR_AUX" | "SET_SDR_DDC") && !matches!(data["channel"].as_u64(), Some(2 | 3)) {
                return Err("aux channel must be 2 or 3".into());
            }
            if cmd == "SET_SDR_TUNE" && data.get("listen").is_none() && data.get("freq").is_none() {
                return Err("tune requires listen or freq".into());
            }
            let config = state.parse_channel_config(data)?;
            state.receivers.check_control(config.channel,data)?;
            if state.channel_recapture(&config)?.is_none() && config.enabled {
                let fs = state.capture_input_rate();
                let source = service_ddc::source_plan(fs, &config)?;
                service_ddc::validate_source_frequency(state.capture_center(), state.capture_bandwidth(),
                    fs, state.sdr_pfb_taps_per_phase, config.freq, &source)?;
            }
        } else {
            validate(&state.schema, cmd, data).map_err(|(_, message)| message)?;
            if cmd == "SET_SDR" { state.parse_service_capture(data)?; }
        }
    }
    // Wait for admission before snapshotting the configuration revision.
    let _queued=WAITING.try_acquire().map_err(|_|"admission_busy: command preparation queue full (64)")?;
    let permit = ADMISSION.acquire().await.map_err(|_| "admission_busy: service admission already running")?;
    let candidate = {
        let state = app.state.lock_async().await.unwrap();
        let revision = state.config_version;
        let channels = matches!(cmd, "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC");
        let front_end = matches!(cmd, "AUTO_SCALE" | "SET_REF" | "SET_AMP" | "SET_REFCK" | "SET_REFCKOUT");
        let preset = cmd == "SET_PRESET";
        let capture = cmd == "SET_SDR" || front_end || preset;
        let starting = (cmd == "SET_MODE" && data["mode"] == "sdr") || (cmd == "CONNECT" && state.swp.mode == "sdr");
        if !channels && !capture && !starting { return Ok(Admission::empty(revision, cmd, data)); }
        if !starting && state.swp.mode != "sdr" { return Ok(Admission::empty(revision, cmd, data)); }
        let mut configs = state.channel_configs();
        let mut live_channels = channels;
        let mut capture_restart = false;
        let (center, fs, bandwidth, taps, existing) = if capture || starting {
            let requested = if cmd == "SET_SDR" { data.clone() } else { json!({}) };
            let next = if preset {state.preset_service_capture()?} else {state.parse_service_capture(&requested)?};
            let previous_decimate = state.pipeline.as_ref().map(|p| p.snapshot.device_decimate)
                .or_else(|| state.ddc.as_ref().map(|d| d.0.device_decimate));
            capture_restart = preset || (front_end && cmd != "AUTO_SCALE" && !(cmd == "SET_REF" && data["mode"] == "auto"))
                || (cmd == "SET_SDR" && (next.center != state.sdr_center || next.taps != state.sdr_pfb_taps_per_phase
                    || next.configs != state.channel_configs()
                    || (state.hardware_shim.is_some() && previous_decimate != Some(next.device_decimate as u64))));
            configs = next.configs;
            (next.center, next.source_rate, next.bandwidth, next.taps, None)
        } else {
            let next = state.parse_channel_config(data)?;
            live_channels = !service_ddc::is_direct(&next);
            if let Some(capture) = state.channel_recapture(&next)? {
                live_channels = false;
                capture_restart = true;
                configs = capture.configs;
                (capture.center, capture.source_rate, capture.bandwidth, capture.taps, None)
            } else {
                let target=configs.iter_mut().find(|old|old.channel==next.channel).ok_or("channel not found")?;
                *target = next;
                let fs = state.capture_input_rate();
                let bins = state.pipeline.as_ref().map(|p| p.snapshot.selected_bins)
                    .or_else(|| state.ddc.as_ref().map(|d| d.0.stream.bins().len()));
                (state.capture_center(), fs, state.capture_bandwidth(), state.sdr_pfb_taps_per_phase, bins)
            }
        };
        let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
        let mut bins = Vec::new();
        for config in configs.iter().filter(|c| c.enabled) {
            let source = service_ddc::source_plan(fs, config)?;
            service_ddc::validate_source_frequency(center, bandwidth, fs, taps, config.freq, &source)?;
            // Direct channels consume capture IQ and do not select coarse bins.
            if source.is_direct() { continue; }
            let signed = signed_bin(config.freq, center);
            for neighbour in signed - PLAN_NEIGHBOURS..=signed + PLAN_NEIGHBOURS {
                if (-m / 2..m / 2).contains(&neighbour) {
                    let bin = neighbour.rem_euclid(m) as u32;
                    if !bins.contains(&bin) { bins.push(bin); }
                }
            }
        }
        // Admission also allows a new target bin before the drained selection is replaced.
        let recovery = state.receiver_failure.is_some()
            && channels && ["mode", "demod"].iter().any(|field| data.get(*field).is_some());
        let restart = starting || capture_restart || recovery || data["recover"]==true;
        // A reset needs an owned replacement: the publisher may consume an
        // already-staged decoder without advancing config_version during admission.
        let prepare_configs = if state.receiver_failure.is_some() && !recovery && !starting {
            Vec::new()
        } else if restart {
            let target=data["channel"].as_u64().unwrap_or(1) as u32;
            configs.iter().filter(|config| (data["recover"]!=true||starting||capture_restart||recovery||config.channel==target)
                && config.enabled && config.processing_enabled
                && websa_channelizer::catalog::is_external(&config.mode)).cloned().collect()
        } else {
            match state.receiver_workers.as_ref() {
                Some(workers) => workers.preparation_needed(&configs, false),
                None => configs.iter().filter(|config| config.enabled && config.processing_enabled
                    && websa_channelizer::catalog::is_external(&config.mode)).cloned().collect(),
            }
        };
        let validate_geometry = state.hardware_shim.is_some() || state.ddc.is_some() || state.pipeline.is_some();
        let needs_workers=(state.receiver_failure.is_none() || recovery || starting)
            && (state.receiver_workers.is_none() || recovery)
            && configs.iter().any(|c|c.enabled&&c.processing_enabled);
        (revision, configs, fs, taps, existing.unwrap_or(bins.len().max(1)), m as usize, live_channels, prepare_configs, validate_geometry,needs_workers)
    };
    let (revision, configs, fs, taps, selected, maximum, channels, prepare_configs, validate_geometry,needs_workers) = candidate;
    let needs_preparation = !prepare_configs.is_empty();
    let result = tokio::task::spawn_blocking(move || {
        if validate_geometry {service_cost::prepare_service(&configs,fs,taps,selected,channels && selected<maximum)?;}
        let processing=processing_admission::prepare(&configs,fs)?;
        let workers=if needs_workers {Some(receiver_workers::ReceiverWorkers::new()?)} else {None};
        Ok::<_,String>((prepare(&prepare_configs)?,processing,workers))
    }).await.map_err(|error| format!("service admission worker: {error}"))?;
    drop(permit);
    let mut ticket = Admission::empty(revision, cmd, data);
    let (prepared,processing,workers) = result?;
    ticket.processing=Some(processing);
    ticket.workers=workers;
    if needs_preparation { ticket.prepared = Some(prepared); }
    Ok(ticket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor="current_thread")]
    async fn first_processor_has_a_warm_pool_before_the_commit_lock() {
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();state.sdr_center=100e6;state.sdr_listen=100e6;
        for config in &mut state.sdr_channels {config.enabled=false;}
        let app=Arc::new(App {state:StateMutex::new(state),clients:PLMutex::new(Vec::new()),
            next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
            identities:channel_identities(),display_version:Arc::new(AtomicU64::new(0))});
        let request=json!({"channel":MAX_CHANNELS,"enabled":true,"freq":100e6,"mode":"am"});
        let mut ticket=preflight(&app,"SET_SDR_CHANNEL",&request).await.unwrap();
        assert!(ticket.workers.is_some(),"bounded mailboxes must be allocated before commit");
        let mut state=app.state.lock().unwrap();
        assert!(state.receiver_workers.is_none());
        ticket.dispatch(&mut state,"SET_SDR_CHANNEL",&request).unwrap();
        assert!(state.receiver_workers.is_some());
    }

    #[tokio::test(flavor="current_thread")]
    async fn radiofax_preflight_never_queues_page_break_and_rejects_atomically() {
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_demod="radiofax".into();
        state.sdr_if_bw=3_000.0;
        state.receiver_workers=Some(receiver_workers::ReceiverWorkers::new().unwrap());
        state.auto_scale(None,false);
        let sequence=state.auto.seq;
        let before=state.channel_configs();
        let app=Arc::new(App {state:StateMutex::new(state),clients:PLMutex::new(Vec::new()),
            next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
            identities:channel_identities(),
            display_version:Arc::new(AtomicU64::new(0))});
        for data in [json!({"radiofax_lpm":"180","radiofax_new_page":true}),
            json!({"radiofax_acquisition":"manual","radiofax_new_page":true})] {
            assert!(preflight(&app,"SET_SDR_CHANNEL",&data).await.is_err());
            let state=app.state.lock().unwrap();
            assert_eq!(state.channel_configs(),before);
            assert_eq!(state.auto.seq,sequence);
            assert!(state.auto.session.is_some());
            assert!(state.receiver_failure.is_none());
            assert!(!state.receiver_workers.as_ref().unwrap().radiofax_page_pending(1));
        }
        let page={let mut state=app.state.lock().unwrap();state.remember_receiver_streams();let context=state.receivers.control_metadata(1);
            json!({"radiofax_new_page":true,"action_id":"preflight-page","action_token":context["action_token"],
                "service_id":context["service_id"],"receiver_uid":context["receiver_uid"]})};
        preflight(&app,"SET_SDR_CHANNEL",&page).await.unwrap();
        let mut state=app.state.lock().unwrap();
        assert!(!state.receiver_workers.as_ref().unwrap().radiofax_page_pending(1));
        state.apply_channel_config(&page).unwrap();
        assert!(state.receiver_workers.as_ref().unwrap().radiofax_page_pending(1));
    }

    #[tokio::test(flavor="current_thread")]
    async fn rejected_request_preserves_running_auto_session() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.auto_scale(None, false);
        let sequence = state.auto.seq;
        let app = Arc::new(App {state:StateMutex::new(state),clients:PLMutex::new(Vec::new()),
            next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
            identities:channel_identities(),
            display_version:Arc::new(AtomicU64::new(0))});
        for (cmd, data) in [
            ("SET_SDR_CHANNEL", json!({"mode":"cw", "pitch":1601})),
            ("SET_SDR_TUNE", json!({})),
            ("SET_SDR_AUX", json!({"channel":1})),
            ("SET_REF", json!({"ref":"invalid"})),
            ("SET_SDR", json!({"pfb_taps_per_phase":3})),
        ] {
            assert!(preflight(&app, cmd, &data).await.is_err(), "{cmd}");
            let state = app.state.lock().unwrap();
            assert!(state.auto.session.is_some());
            assert_eq!(state.auto.seq, sequence);
            assert_eq!(state.auto.result, "adjusting");
        }
    }

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
            identities:channel_identities(),
            display_version:Arc::new(AtomicU64::new(0))})
    }

    async fn command(app:&Arc<App>,cmd:&str,data:Value)->Result<bool,CommandError> {
        let mut admission=preflight(app,cmd,&data).await.map_err(|e|CommandError::new("bad_parameter",&e))?;
        let mut state=app.state.lock().unwrap();
        admission.dispatch(&mut state,cmd,&data)
    }

    #[tokio::test(flavor="current_thread")]
    async fn failed_decoder_preparation_preserves_capture_and_auto_session() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let app=app();
        let before={
            let mut state=app.state.lock().unwrap();
            state.auto_scale(None,false);
            (state.channel_configs(),state.config_version,state.freq_version,
                state.ddc.as_ref().unwrap().0.generation,state.stream_registry(),state.auto.view())
        };
        for (cmd,data) in [
            ("SET_SDR_CHANNEL",json!({"freq":100.1e6})),
            ("SET_SDR",json!({"center":101e6})),
            ("SET_REF",json!({"ref":-30.0})),
            ("SET_PRESET",json!({})),
        ] {
            let preparing_app=app.clone();
            let result=preflight_with(&app,cmd,&data,move |_| {
                // Process startup can take seconds; capture state must remain available.
                assert_eq!(preparing_app.state.lock().unwrap().swp.mode,"sdr");
                Err("injected decoder process startup failure".into())
            }).await;
            assert!(matches!(result,Err(error) if error=="injected decoder process startup failure"));
            let mut state=app.state.lock().unwrap();
            assert_eq!((state.channel_configs(),state.config_version,state.freq_version,
                state.ddc.as_ref().unwrap().0.generation,state.stream_registry(),state.auto.view()),before);
            assert!(state.receiver_failure.is_none());
            // An unchanged owner generation and continued packets prove no SDK cutover.
            let packet=state.ddc.as_mut().unwrap().0.hardware.as_ref().unwrap().next().unwrap();
            assert_eq!(packet.generation,before.3);
        }
    }

    #[tokio::test(flavor="current_thread")]
    async fn admission_rejects_superseded_revision_and_different_request() {
        let (golden,schema)=load_golden();
        let app=Arc::new(App {state:StateMutex::new(DevState::new(golden,schema)),clients:PLMutex::new(Vec::new()),
            next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
            identities:channel_identities(),display_version:Arc::new(AtomicU64::new(0))});
        let request=json!({"mode":"sdr"});
        let mut ticket=preflight_with(&app,"SET_MODE",&request,|_|Ok(receiver_workers::PreparedDecoders::default())).await.unwrap();
        let mut state=app.state.lock().unwrap();
        assert_eq!(ticket.dispatch(&mut state,"SET_MODE",&json!({"mode":"rta"})).unwrap_err().code,"configuration_changed");
        state.config_version+=1;
        assert_eq!(ticket.dispatch(&mut state,"SET_MODE",&request).unwrap_err().code,"configuration_changed");
        assert_eq!(state.swp.mode,"std");
    }

    #[test]
    fn automatic_overflow_waits_for_external_preparation_before_reconfiguring() {
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();
        state.sdr_demod="wspr".into();
        state.sdr_center=14_095_600.0;
        state.sdr_listen=14_095_600.0;
        state.auto_scale(None,false);
        state.note_auto_acquisition(-12);
        let before=(state.sdr_ref,state.config_version,state.auto.view());
        assert!(state.auto_receiver_preparation_required());
        assert!(!state.advance_auto());
        assert_eq!((state.sdr_ref,state.config_version,state.auto.view()),before);
        assert!(state.ddc.is_none() && state.pipeline.is_none());
        // A display-only Auto reports overflow without preparing or changing capture.
        state.auto_scale(None,true);
        state.note_auto_acquisition(-12);
        assert!(!state.auto_receiver_preparation_required());
        assert!(state.advance_auto());
        assert_eq!(state.auto.view()["result"],"overflow");
    }

    #[tokio::test(flavor="current_thread")]
    async fn front_end_recapture_keeps_usb_audio_without_a_demodulator_change() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let app=app();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":1,"mode":"usb","if_bw":2400.0,"squelch":-160.0})).await.unwrap();
        let mut previous_epoch=None;
        for (cmd,data) in [("SET_REF",json!({"ref":-50.0})),("SET_REF",json!({"ref":-40.0})),
            ("SET_AMP",json!({"atten":10})),("SET_REFCK",json!({"mode":"external"}))] {
            {
                let mut state=app.state.lock().unwrap();
                let registry=state.stream_registry();
                let mut frames=Vec::new();
                for _ in 0..160 {frames.extend(state.ddc.as_mut().unwrap().0.packet().unwrap());}
                let output=state.process_receiver_frames_with_registry(&frames,&registry,true).unwrap();
                assert!(output.audio.iter().any(|frame|frame.len()>96),"audio before {cmd}");
                if let Some(epoch)=previous_epoch {assert_ne!(registry[0].epoch,epoch);}
                previous_epoch=Some(registry[0].epoch);
                // Leave source data to be admitted as old-stream tails during
                // the command, without waiting for receiver execution.
                let pending=state.ddc.as_mut().unwrap().0.packet().unwrap();
                state.pending_iq.extend(pending);
            }
            command(&app,cmd,data).await.unwrap();
            let mut state=app.state.lock().unwrap();
            let registry=state.stream_registry();
            let mut frames=std::mem::take(&mut state.pending_iq);
            for _ in 0..160 {frames.extend(state.ddc.as_mut().unwrap().0.packet().unwrap());}
            let output=state.process_receiver_frames_with_registry(&frames,&registry,false).unwrap();
            let mut output=output;
            output.append(state.receiver_workers.as_mut().unwrap().finish().unwrap());
            assert!(output.audio.iter().any(|frame|frame.len()>96
                && u32::from_le_bytes(frame[36..40].try_into().unwrap())==registry[0].epoch),"new stream audio after {cmd}");
            assert!(state.receiver_failure.is_none(),"{:?}",state.receiver_failure);
            assert!(state.receiver_workers.as_ref().unwrap().channel_failure(1).is_none());
            assert_eq!(state.channel_configs()[0].mode,"usb");
        }
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
    async fn manual_reference_and_overflow_preserve_sdr_gain_controls() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let _injection=AutoInjection;
        let app=app();
        std::fs::write(AUTO_CONTROL,"-15 inf").unwrap();
        for (atten,preamp,ifgain,gain_strategy) in [
            (-1,0,0,1),(-1,1,1,0),(17,0,2,1),(33,1,3,0),
        ] {
            command(&app,"SET_AMP",json!({"atten":atten,"preamp":preamp,
                "ifgain":ifgain,"gain_strategy":gain_strategy})).await.unwrap();
            for reference in [-70.0,-45.0] {
                command(&app,"SET_REF",json!({"ref":reference})).await.unwrap();
                let state=app.state.lock().unwrap();
                let status=state.status_snapshot().build();
                assert_eq!(status["req"]["sdr"]["ref"],reference);
                assert_eq!(status["amp"]["atten"],atten);
                assert_eq!(status["amp"]["preamp"],preamp);
                assert_eq!(status["amp"]["ifgain"],ifgain);
                assert_eq!(status["amp"]["gain_strategy"],gain_strategy);
                assert_eq!(status["amp"]["atten_actual"],18);
                assert_eq!(status["amp"]["preamp_actual"],1);
                assert_eq!(status["amp"]["ifgain_actual"],3);
            }
            let mut state=app.state.lock().unwrap();
            let amp=state.status_snapshot().build()["amp"].clone();
            let before=(state.config_version,state.ddc.as_ref().unwrap().0.generation);
            for _ in 0..6 {
                assert_eq!(fresh_iqs_observation(&mut state),-12);
                state.advance_auto();
                assert_eq!(state.sdr_ref,-45.0);
                assert_eq!(state.active_auto_ref(),Some(-47.5));
                assert_eq!(state.auto.view()["adjusting"],false);
                assert_eq!(state.status_snapshot().build()["amp"],amp);
                assert_eq!((state.config_version,state.ddc.as_ref().unwrap().0.generation),before);
            }
        }
    }

    #[tokio::test(flavor="current_thread")]
    async fn relevel_after_local_retunes_and_mode_change_uses_fresh_calibrated_geometry() {
        let _guard=hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let app=app();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":1,"freq":100e6+4.0*BIN_SPACING})).await.unwrap();
        command(&app,"SET_SDR_CHANNEL",json!({"channel":1,"mode":"nfm","if_bw":12000.0})).await.unwrap();
        let retained=app.state.lock().unwrap().ddc.as_ref().unwrap().0.stream.bins().len();
        assert_eq!(retained,1,"local tuning must already release unused coarse bins");
        for (cmd,data) in [("SET_REF",json!({"ref":-50.0})),("SET_AMP",json!({"atten":10})),
            ("SET_REFCK",json!({"mode":"external"})),("SET_REFCKOUT",json!({"on":true}))] {
            command(&app,cmd,data).await.unwrap();
            let mut state=app.state.lock().unwrap();
            let ddc=&mut state.ddc.as_mut().unwrap().0;
            assert_eq!(ddc.stream.bins().len(),retained);
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
            // Only already-produced PCM is retained across this boundary.
            // Queued old-generation input is intentionally cancelled on retirement.
            let output=state.process_receiver_frames_with_registry(&frames,&registry,true).unwrap();
            assert!(!output.audio.is_empty());
            state.pending_receiver.append(output);
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
