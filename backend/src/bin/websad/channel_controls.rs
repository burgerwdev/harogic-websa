use super::*;
use super::receiver_workers::ChannelConfig;

fn alias<'a>(data: &'a Value, names: &[&str]) -> Result<Option<&'a Value>, String> {
    let mut found = None;
    for name in names {
        if let Some(value) = data.get(*name) {
            if found.is_some_and(|previous| previous != value) {
                return Err(format!("conflicting {} aliases", names[0]));
            }
            found = Some(value);
        }
    }
    Ok(found)
}
fn number(data: &Value, names: &[&str], current: f64) -> Result<f64, String> {
    match alias(data, names)? {
        None => Ok(current),
        Some(value) => value.as_f64().filter(|v| v.is_finite())
            .ok_or_else(|| format!("{} must be a finite number", names[0])),
    }
}
fn boolean(data: &Value, name: &str, current: bool) -> Result<bool, String> {
    match data.get(name) {
        None => Ok(current),
        Some(value) => value.as_bool().ok_or_else(|| format!("{name} must be boolean")),
    }
}
fn text(data: &Value, names: &[&str], current: &str) -> Result<String, String> {
    match alias(data, names)? {
        None => Ok(current.to_owned()),
        Some(value) => value.as_str().map(str::to_owned)
            .ok_or_else(|| format!("{} must be a string", names[0])),
    }
}
fn analog_filter_min_hz(mode: &str) -> Option<f64> {
    match mode {
        "usb" | "lsb" => Some(101.0),
        "am" | "dsb" | "cw" | "nfm" | "wfm" | "pm" => Some(50.0),
        _ => None,
    }
}

/// Listenable fallback for a band-limited external decoder: its audio companion
/// when the catalog defines one, otherwise AM. Mirrors catalog.rs companions.
pub(super) fn audio_companion_or_am(mode: &str) -> String {
    match mode {
        "aprs" => "nfm".into(),
        "ft4" | "wspr" => "usb".into(),
        _ => "am".into(),
    }
}

/// A band-limited external decoder cannot follow every frequency. Reconcile the
/// request instead of committing a mode/frequency pair the decoder rejects:
/// - selecting the decoder retunes to its canonical center;
/// - tuning or panning out of the band demotes the channel to the listenable
///   companion mode;
/// - an explicit mode and frequency that conflict still fail validation.
/// Returns true when the configuration changed.
pub(super) fn reconcile_external_band(config: &mut receiver_workers::ChannelConfig,
    freq_requested: bool, mode_requested: bool) -> bool {
    use websa_channelizer::external::onair;
    if !onair::band_limited(&config.mode) { return false; }
    let center = config.freq.max(0.0).round() as u64;
    if onair::frequency_supported(&config.mode, center) { return false; }
    let retune = mode_requested && !freq_requested
        && onair::default_frequency(&config.mode).is_some_and(|hz| hz != center);
    if retune {
        config.freq = onair::default_frequency(&config.mode).unwrap() as f64;
        return true;
    }
    if !mode_requested {
        let companion = audio_companion_or_am(&config.mode);
        if let Some(descriptor) = websa_channelizer::catalog::mode(&companion) {
            config.mode = companion;
            config.if_bw = descriptor.default_bandwidth_hz;
            return true;
        }
    }
    false
}

impl DevState {
    /// The receiver definition remains enabled while its execution is paused outside capture.
    pub(super) fn channel_intents(&self) -> Vec<ChannelConfig> {
        let mut configs=self.channel_configs();
        for config in &mut configs {config.enabled |= self.receivers.band_paused.contains(&config.channel);}
        configs
    }
    pub(super) fn channel_configs(&self) -> Vec<ChannelConfig> {
        let mut configs: Vec<_> = (1..=MAX_CHANNELS as u32).map(|slot| self.sdr_channels.iter()
            .find(|config| config.channel == slot).cloned()
            .unwrap_or_else(|| ChannelConfig::for_slot(slot))).collect();
        let primary = &mut configs[0];
        primary.freq = self.sdr_listen;
        primary.mode = self.sdr_demod.clone();
        primary.if_bw = self.sdr_if_bw;
        primary.squelch = self.sdr_squelch;
        primary.volume = self.sdr_volume;
        primary.agc = self.sdr_agc;
        primary.pitch = self.sdr_pitch;
        primary.deemph_us = self.sdr_deemph;
        configs
    }
    pub(super) fn channel_config(&self, channel:u32) -> Option<ChannelConfig> {
        self.channel_configs().into_iter().find(|config|config.channel==channel)
    }
    pub(super) fn filter_limits(&self) -> Value {
        let mut primary = self.channel_configs().into_iter().next()
            .unwrap_or_else(ChannelConfig::default);
        let mode = primary.mode.clone();
        let Some(minimum) = analog_filter_min_hz(&mode) else {
            return json!({"mode": mode, "min_hz": 0, "max_hz": Value::Null});
        };
        if self.swp.mode != "sdr" || !primary.enabled {
            return json!({"mode": mode, "min_hz": minimum, "max_hz": Value::Null});
        }
        // The main panel sends mode + ifbw, which returns to a mode-derived
        // baseband in parse_channel_config_base. Probe that same candidate;
        // retaining a workbench/mode-memory IQ width falsely locks out widening.
        primary.baseband = None;
        primary.processing_enabled = true;
        let center = self.capture_center();
        let bandwidth = self.capture_bandwidth();
        let fs = self.capture_input_rate();
        let taps = if let Some(pipeline) = self.pipeline.as_ref() {
            pipeline.snapshot.taps
        } else if let Some(ddc) = self.ddc.as_ref() {
            ddc.0.stream.taps_per_phase()
        } else {
            self.sdr_pfb_taps_per_phase
        };
        let feasible = |width: f64, primary: &mut ChannelConfig| {
            primary.if_bw = width;
            service_ddc::source_plan(fs, primary).and_then(|source| {
                service_ddc::validate_source_frequency(center, bandwidth, fs, taps, primary.freq, &source)
            }).is_ok()
        };
        if !feasible(minimum, &mut primary) {
            return json!({"mode": mode, "min_hz": minimum, "max_hz": Value::Null});
        }
        let mut feasible_hz = minimum as i64;
        let mut infeasible_hz = COARSE_RATE.ceil() as i64 + 1;
        // Feasibility narrows monotonically with bandwidth; twenty halvings resolve the
        // ~1 MHz hard channel-plan ceiling to an integer Hz without caching a stale range.
        for _ in 0..20 {
            let candidate = feasible_hz + (infeasible_hz - feasible_hz) / 2;
            if feasible(candidate as f64, &mut primary) {
                feasible_hz = candidate;
            } else {
                infeasible_hz = candidate;
            }
        }
        json!({"mode": mode, "min_hz": minimum, "max_hz": feasible_hz})
    }

    #[cfg(test)]
    pub(super) fn channel_status(&self) -> Value {
        self.channel_status_detail(true)
    }
    pub(super) fn channel_status_detail(&self,diagnostics:bool) -> Value {
        let registry = self.stream_registry();
        let processing = self.processing_registry();
        let power = self.fresh_channel_power();
        Value::Array(self.channel_configs().into_iter().map(|config| {
            let entry = registry.iter().find(|entry| entry.channel == config.channel);
            let mut value = json!(config);
            value["control"]=self.receivers.control_metadata(config.channel);
            value["confirmed"]=multichannel::config_value(&config,None);
            value["reception"]=self.reception.snapshot(config.channel);
            value["processing_runtime"]=self.receiver_uid_for_slot(config.channel).and_then(|uid|
                self.receiver_workers.as_ref().map(|workers|if diagnostics {workers.runtime_status(config.channel,uid)}
                    else {workers.live_runtime_status(config.channel,uid)})).unwrap_or(Value::Null);
            value["control_receipts"]=self.receiver_uid_for_slot(config.channel).and_then(|uid|
                self.receiver_workers.as_ref().map(|workers|workers.control_receipts(config.channel,uid))).unwrap_or_else(||json!([]));
            value["actual_iq_rate"] = entry.map_or(Value::Null, |entry| json!(entry.output_rate));
            value["channel_id"] = entry.map_or(Value::Null, |entry| json!(entry.channel_id));
            value["epoch"] = entry.map_or(Value::Null, |entry| json!(entry.epoch));
            value["level_dbfs"] = json!(entry.filter(|entry| self.swp.mode == "sdr" && config.enabled
                    && entry.frequency_hz == config.freq)
                .and_then(|entry| self.receiver_workers.as_ref()
                    .and_then(|workers| workers.level_dbfs(entry, std::time::Instant::now()))));
            let active = entry.filter(|entry| self.swp.mode == "sdr" && config.enabled
                && entry.frequency_hz == config.freq);
            value["channel_dbm"] = json!(active.and_then(|_| power[(config.channel - 1) as usize]));
            value["squelch_open"] = json!(active.and_then(|entry| processing.iter().find(|p|p.channel==entry.channel))
                .and_then(|entry| self.receiver_workers.as_ref()
                .and_then(|workers| workers.squelch_status(entry, std::time::Instant::now()))
                .map(|(open, _)| open)));
            value
        }).collect())
    }
    pub(super) fn parse_channel_config(&self, data: &Value) -> Result<ChannelConfig, String> {
        self.parse_channel_config_base(data,None)
    }
    pub(super) fn parse_channel_config_base(&self,data:&Value,baseline:Option<ChannelConfig>)->Result<ChannelConfig,String> {
        if !data.is_object() { return Err("channel configuration must be an object".into()); }
        let channel = match data.get("channel") {
            None => 1,
            Some(value) => value.as_u64().filter(|slot| (1..=MAX_CHANNELS as u64).contains(slot))
                .ok_or("channel exceeds service capacity")? as u32,
        };
        let mut config = baseline.unwrap_or(self.channel_config(channel).ok_or("channel not found")?);
        let previous_mode=config.mode.clone();
        if data["enabled"] == true && (data.get("freq").is_some() || data.get("listen").is_some())
            && data.get("baseband").is_none() && data.get("processing_enabled").is_none() {
            self.restore_retired_listener(&mut config);
        }
        // An explicit legacy mode selection returns to its mode-derived plan.
        // Frequency-only edits preserve an independently configured IQ plan.
        if (data.get("mode").is_some() || data.get("demod").is_some())
            && data.get("baseband").is_none() && data.get("processing_enabled").is_none() {
            config.baseband=None;config.processing_enabled=true;
        }
        config.enabled = boolean(data, "enabled", config.enabled)?;
        config.processing_enabled = boolean(data, "processing_enabled", config.processing_enabled)?;
        if let Some(value) = data.get("baseband") {
            config.baseband = if value.is_null() { None } else {
                Some(serde_json::from_value(value.clone()).map_err(|error| format!("baseband: {error}"))?)
            };
        }
        config.freq = number(data, &["freq", "listen"], config.freq)?;
        config.mode = text(data, &["mode", "demod"], &config.mode)?;
        if config.mode != previous_mode
            && websa_channelizer::catalog::is_external(&config.mode)
            && alias(data, &["ifbw", "if_bw"])?.is_none() {
            config.if_bw = websa_channelizer::catalog::mode(&config.mode)
                .ok_or("unsupported receiver mode")?.default_bandwidth_hz;
        }
        let am_sync = boolean(data, "am_sync", config.am_sync)?;
        if am_sync != config.am_sync {
            config.am_sync_generation = config.am_sync_generation.checked_add(1)
                .ok_or("AM sync generation exhausted")?;
            config.am_sync = am_sync;
        }
        config.if_bw = number(data, &["ifbw", "if_bw"], config.if_bw)?;
        config.squelch = number(data, &["squelch"], config.squelch)?;
        config.volume = number(data, &["volume"], config.volume)?;
        config.agc = boolean(data, "agc", config.agc)?;
        config.pitch = number(data, &["pitch"], config.pitch)?;
        config.deemph_us = number(data, &["deemph_us", "deemph"], config.deemph_us)?;
        if let Some(minimum) = analog_filter_min_hz(&config.mode).filter(|_|config.processing_enabled) {
            if config.if_bw < minimum {
                return Err(format!("filter bandwidth must be at least {minimum:.0} Hz for {}", config.mode));
            }
        }

        config.nr = boolean(data, "nr", config.nr)?;
        config.nr_strength = number(data, &["nr_strength"], config.nr_strength)?;
        config.nr_algorithm = text(data, &["nr_algorithm"], &config.nr_algorithm)?;
        config.nr_atten_db = number(data, &["nr_atten_db"], config.nr_atten_db)?;
        config.stereo = boolean(data, "stereo", config.stereo)?;
        if let Some(value) = data.get("audio_rate") {
            config.audio_rate = value.as_u64().and_then(|rate| u32::try_from(rate).ok())
                .ok_or("audio_rate must be an integer")?;
        }
        let freq_requested = alias(data, &["freq", "listen"])?.is_some();
        let mode_requested = alias(data, &["mode", "demod"])?.is_some();
        if config.processing_enabled {reconcile_external_band(&mut config, freq_requested, mode_requested);}
        if let Some(value) = data.get("sstv_mode") {
            config.sstv_mode = if value.is_null() { None } else {
                Some(value.as_u64().and_then(|mode| usize::try_from(mode).ok())
                    .filter(|&mode| mode < websa_dsp::digital::sstv::SSTV_MODES.len())
                    .ok_or("sstv_mode must be null or a supported mode index")?)
            };
        }
        if let Some(value)=data.get("continuity") {config.continuity=serde_json::from_value(value.clone()).map_err(|_|"continuity must be continuous or strict")?;}
        config.radiofax_acquisition = text(data, &["radiofax_acquisition"], &config.radiofax_acquisition)?;
        config.radiofax_ioc = text(data, &["radiofax_ioc"], &config.radiofax_ioc)?;
        config.radiofax_lpm = text(data, &["radiofax_lpm"], &config.radiofax_lpm)?;
        config.radiofax_modulation = text(data, &["radiofax_modulation"], &config.radiofax_modulation)?;
        config.radiofax_polarity = text(data, &["radiofax_polarity"], &config.radiofax_polarity)?;
        config.radiofax_clock_recovery = text(data, &["radiofax_clock_recovery"], &config.radiofax_clock_recovery)?;
        config.radiofax_phase_pixels = number(data, &["radiofax_phase_pixels"], f64::from(config.radiofax_phase_pixels))? as f32;
        config.radiofax_clock_ppm = number(data, &["radiofax_clock_ppm"], f64::from(config.radiofax_clock_ppm))? as f32;
        boolean(data, "radiofax_new_page", false)?;
        if boolean(data, "radiofax_reset_correction", false)? {
            config.radiofax_phase_pixels = 0.0;
            config.radiofax_clock_ppm = 0.0;
        }
        let (minimum, maximum) = self.freq_limits();
        if config.enabled && !(minimum..=maximum).contains(&config.freq) {
            return Err(format!("channel frequency outside device limits: channel {} requested {} Hz; valid RF range {minimum}..={maximum} Hz", config.channel, config.freq));
        }
        service_ddc::source_plan(IQ_FS, &config)?;
        if !(-200.0..=0.0).contains(&config.squelch) { return Err("squelch must be in -200..=0 dB".into()); }
        if !(0.0..=2.0).contains(&config.volume) { return Err("volume must be in 0..=2".into()); }
        if config.pitch <= 0.0 || config.pitch > 20_000.0 { return Err("pitch must be in (0, 20000] Hz".into()); }
        if config.mode == "cw" && !(100.0..=1600.0).contains(&config.pitch) {
            return Err("CW pitch must be in 100..=1600 Hz".into());
        }
        if !(-1.0..=1000.0).contains(&config.deemph_us) { return Err("deemph_us must be in -1..=1000".into()); }
        if !(0.0..=1.0).contains(&config.nr_strength) { return Err("nr_strength must be in 0..=1".into()); }
        if websa_channelizer::nr::NrAlgorithm::parse(&config.nr_algorithm).is_none() {
            return Err("nr_algorithm must be wiener, dfn, gtcrn, ul-unas or fastenhancer".into());
        }
        if !(0.0..=100.0).contains(&config.nr_atten_db) { return Err("nr_atten_db must be in 0..=100".into()); }
        if !(8_000..=192_000).contains(&config.audio_rate) { return Err("audio_rate must be in 8000..=192000".into()); }
        config.validate()?;
        Ok(config)
    }
    /// Tuning retains the capture when it fits; wider channels can widen the shared source.
    pub(super) fn channel_recapture(&self, config: &ChannelConfig) -> Result<Option<service_capture::CaptureConfig>, String> {
        if !config.enabled {
            return Ok(None);
        }
        if self.pipeline.is_none() && self.ddc.is_none() {
            // There is no live source to retain. A coarse-channel tune that sits
            // inside the stored window keeps the live-path passband guard: it must
            // reject a short-tap coarse-gap frequency instead of relocating the
            // not-yet-negotiated capture. Direct-rate decoders and out-of-window
            // tunes still produce the fitted candidate capture for admission.
            let fs = self.capture_input_rate();
            let center = self.capture_center();
            let bandwidth = self.capture_bandwidth();
            if let Ok(source) = service_ddc::source_plan(fs, config) {
                let edge = source.plan().stopband_hz;
                let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
                let inside = !source.is_direct() && (-m / 2..m / 2).contains(&signed_bin(config.freq, center))
                    && (config.freq - center).abs() + edge <= bandwidth / 2.0;
                if inside {
                    service_ddc::validate_source_frequency(center, bandwidth, fs,
                        self.sdr_pfb_taps_per_phase, config.freq, &source)?;
                    return Ok(None);
                }
            }
            let mut configs = self.channel_intents();
            *configs.iter_mut().find(|old|old.channel==config.channel).ok_or("channel not found")? = config.clone();
            let fit=self.fit_service_capture(self.sdr_center,self.sdr_decimate,
                self.sdr_pfb_taps_per_phase,configs.clone(),false);
            return match fit {
                Err(_) if config.channel==1 && config.freq!=self.sdr_listen =>
                    self.fit_service_capture(config.freq,self.sdr_decimate,
                        self.sdr_pfb_taps_per_phase,configs,false).map(Some),
                result => result.map(Some),
            };
        }
        let fs = self.capture_input_rate();
        let center = self.capture_center();
        let bandwidth = self.capture_bandwidth();
        if let Ok(source) = service_ddc::source_plan(fs, config) {
            let plan = source.plan();
            let edge = plan.stopband_hz;
            let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
            if (source.is_direct() || (-m / 2..m / 2).contains(&signed_bin(config.freq, center)))
                && (config.freq - center).abs() + edge <= bandwidth / 2.0 {
                return Ok(None);
            }
        }
        let mut configs = self.channel_intents();
        *configs.iter_mut().find(|old|old.channel==config.channel).ok_or("channel not found")? = config.clone();
        let requested_center = if config.channel == 1 && config.freq != self.sdr_listen { config.freq } else { center };
        self.fit_service_capture(requested_center,self.sdr_decimate,self.sdr_pfb_taps_per_phase,configs,false).map(Some)
    }

    pub(super) fn apply_channel_config(&mut self, data: &Value) -> Result<(), String> {
        let config = self.parse_channel_config(data)?;
        let channel = config.channel;
        if self.receivers.check_control(channel,data)? {return Ok(());}
        if self.receiver_workers.as_ref().is_some_and(|w|!w.controls_available(channel)) {return Err("receiver command mailbox full (64); retry after data advances".into());}
        if (data["radiofax_new_page"]==true||data["radiofax_reset_correction"]==true)
            && (!config.enabled||!config.processing_enabled||config.mode!="radiofax") {return Err("Radiofax action requires a running Radiofax receiver".into());}
        let mut configs = self.channel_configs();
        let old = configs.iter().find(|old|old.channel==channel).ok_or("channel not found")?.clone();
        if data["recover"]==true && self.reception.known(channel) {
            if config!=old {return Err("apply settings before recovering reception".into());}
            self.reception.manual(channel)?;
            self.config_version+=1;
            if data["defer_receipt"]!=true {self.track_receiver_control(channel,data)?;}
            return Ok(());
        }
        let source_recovery=data["recover"]==true && self.fine_faults().iter().any(|fault|fault.channel==channel);
        if data["recover"]==true {
            if let Some(workers)=self.receiver_workers.as_mut() {workers.restart_channel(channel)?;}
            else if !source_recovery {return Err("receiver owner unavailable".into());}
            self.processing_branches.restart_channel(channel);
            self.decode_snapshots.clear_channel(channel);
            self.config_version+=1;
        }
        if config == old && !source_recovery {
            if data.get("enabled").is_some() {self.receivers.band_paused.remove(&channel);}
            if data["defer_receipt"]!=true {self.track_receiver_control(channel,data)?;}
            return Ok(());
        }
        let processing_failed = self.receiver_workers.as_ref()
            .is_some_and(|workers| workers.channel_failure(channel).is_some());
        let repair_nr = config.enabled && config.processing_enabled && processing_failed
            && self.receiver_failure.is_none() && !old.resets_receiver(&config)
            && (old.nr != config.nr || old.nr_algorithm != config.nr_algorithm)
            && !self.reception.source_interrupted(channel)
            && !self.fine_faults().iter().any(|fault| fault.channel == channel);
        if config.enabled && config.processing_enabled && processing_failed
            && (old.resets_receiver(&config) || repair_nr)
            && self.receiver_workers.as_ref().is_some_and(|workers| !workers.restart_ready(channel)) {
            return Err(format!("channel {channel} is still retiring; retry after its execution owner exits"));
        }
        self.remember_receiver_streams();
        let retiring_iq = self.stream_registry().iter().find(|entry|entry.channel==channel).cloned();
        let geometry_changed=source_recovery || old.enabled!=config.enabled || old.freq!=config.freq || old.mode!=config.mode
            || old.baseband!=config.baseband || old.processing_enabled!=config.processing_enabled
            || old.if_bw!=config.if_bw || old.pitch!=config.pitch || old.audio_rate!=config.audio_rate
            || old.stereo!=config.stereo || old.sstv_mode!=config.sstv_mode
            || old.radiofax_acquisition!=config.radiofax_acquisition || old.radiofax_ioc!=config.radiofax_ioc
            || old.radiofax_lpm!=config.radiofax_lpm || old.radiofax_modulation!=config.radiofax_modulation
            || old.radiofax_polarity!=config.radiofax_polarity || old.radiofax_clock_recovery!=config.radiofax_clock_recovery;
        let recapture = self.channel_recapture(&config)?;
        let target_paused=recapture.as_ref().is_some_and(|capture|capture.band_paused.contains(&channel));
        let staged_fixtures = if geometry_changed && self.hardware_shim.is_none() && self.swp.mode == "sdr" {
            let mut candidate = recapture.as_ref().map_or_else(||configs.clone(),|capture|capture.configs.clone());
            if recapture.is_none() {*candidate.iter_mut().find(|old|old.channel==channel).ok_or("channel not found")? = config.clone();}
            Some(match self.fixture_sources.as_ref() {
                Some(current) => current.staged(&candidate)?,
                None => service_fixture::FixtureSources::new().staged(&candidate)?,
            })
        } else { None };
        if geometry_changed && config.enabled && recapture.is_none() {
            // Replay IQ clocks describe receiver output, not the shared RF capture grid.
            let fs = self.capture_input_rate();
            let source=service_ddc::source_plan(fs,&config)?;
            service_ddc::validate_source_frequency(self.capture_center(),self.capture_bandwidth(),fs,
                self.sdr_pfb_taps_per_phase,config.freq,&source)?;
        }
        if geometry_changed && self.pipeline.is_none() && self.ddc.is_none() && self.receiver_failure.is_none() {
            if let Some(workers)=self.receiver_workers.as_mut() {
                match workers.retire_channels(&[channel]).and_then(|output| {
                    self.decode_snapshots.observe(&output.decode)?;
                    Ok(output)
                }) {
                    Ok(output)=>{
                        self.pending_receiver.append(output);
                        self.retiring_registry=self.stream_registry();
                    }
                    Err(error)=>self.latch_receiver_failure(error),
                }
            }
        }
        let processing_only=geometry_changed && (self.pipeline.is_some() || self.ddc.is_some())
            && service_ddc::same_baseband(self.capture_input_rate(),&old,&config);
        if geometry_changed {
            let next = config.clone();
            if let Some(capture) = recapture.as_ref() {
                let center = capture.center;
                let display = capture.display as i32;
                let taps = capture.taps;
                let next = capture.configs.clone();
                let front_end = self.front_end(self.sdr_ref);
                self.ddc_command(move |ddc| ddc.recapture_service_configs(center, display, front_end, taps, &next))?;
            } else {
                self.ddc_channel_command(move |ddc| if source_recovery {ddc.recover_service_channel(next)} else {ddc.configure_service_channel(next)})?;
            }
            // The DDC boundary first consumes all old queued IQ with its old
            // processor. Close that branch only after this drain completes.
            if processing_only {
                if let Err(error)=self.retire_receiver_channels(&[channel],self.stream_registry()) {self.latch_receiver_failure(error);}
            }
            self.decode_snapshots.clear_channel(channel);
        }
        if repair_nr {
            // Healthy NR edits stay hot. A failed processor cannot consume that
            // edit, so fence its old work and renew only the processing identity.
            self.receiver_workers.as_mut().ok_or("receiver owner unavailable")?.restart_channel(channel)?;
            self.processing_branches.restart_channel(channel);
            self.decode_snapshots.clear_channel(channel);
        }
        if !geometry_changed {
            if let Some(ddc)=self.ddc.as_mut() {
                if let Some(current)=ddc.0.service_configs.as_mut() {*current.iter_mut().find(|old|old.channel==channel).ok_or("channel not found")?=config.clone();}
            }
        }
        if let Some(capture) = recapture {
            self.sdr_center = capture.center;
            configs=capture.configs;
            self.receivers.band_paused=capture.band_paused;
            self.invalidate_observation();
            self.channel_power = vec![None; MAX_CHANNELS];
            for slot in 1..=MAX_CHANNELS as u32 {
                if !self.receivers.band_paused.contains(&slot) {self.decode_snapshots.clear_channel(slot);}
            }
            self.freq_version += 1;
        }
        if geometry_changed {
            self.channel_power[(channel-1) as usize]=None;
        }
        let mut effective=config.clone();effective.enabled &= !target_paused;
        *configs.iter_mut().find(|old|old.channel==channel).ok_or("channel not found")? = effective.clone();
        if channel == 1 {
            self.sdr_listen = config.freq;
            self.sdr_demod = config.mode.clone();
            self.sdr_if_bw = config.if_bw;
            self.sdr_squelch = config.squelch;
            self.sdr_volume = config.volume;
            self.sdr_agc = config.agc;
            self.sdr_pitch = config.pitch;
            self.sdr_deemph = config.deemph_us;
        }
        self.sdr_aux = json!({"2":configs[1], "3":configs[2]});
        self.sdr_channels = configs;
        if data.get("enabled").is_some()&&!target_paused {self.receivers.band_paused.remove(&channel);}
        if !effective.enabled || !effective.processing_enabled {self.processing_branches.retire_channel(channel);}
        if let Some(fixtures) = staged_fixtures { self.fixture_sources = Some(fixtures); }
        // Native and legacy stops share the same ordered drain boundary. Keep it
        // behind pending tails; only the UID-based transport exposes this marker.
        if old.enabled && !effective.enabled {
            if let Some(entry)=retiring_iq {
                let mut end=b"IQEN".to_vec();
                end.extend_from_slice(&channel.to_le_bytes());
                end.extend_from_slice(&entry.channel_id.to_le_bytes());
                end.extend_from_slice(&entry.epoch.to_le_bytes());
                self.pending_iq.push(end.into());
            }
        }
        if self.receiver_failure.is_none() {
            if let Some(workers) = self.receiver_workers.as_mut() {
                if let Err(error)=workers.configure(&self.sdr_channels) {self.latch_receiver_failure(error);}
            }
        }
        // Commit the new processing generation before checking its failure.
        // A retired generation's fault cannot block repair; actual new input
        // must still confirm recovery before the supervisor reports receiving.
        if (geometry_changed || repair_nr) && effective.enabled && self.receiver_failure.is_none()
            && !self.fine_faults().iter().any(|f|f.channel==channel)
            && !self.receiver_workers.as_ref().is_some_and(|w|w.channel_failure(channel).is_some()) {
            self.reception.configured(channel, config.processing_enabled);
        }
        // Keep the bounded healthy pool across an empty processor set. Recreating
        // its thread-local models while the capture is live blocks publication
        // for about a second and overruns the physical device queue.
        self.config_version += 1;
        if data["defer_receipt"]!=true {self.track_receiver_control(channel,data)?;}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration_repair_state(failed: bool) -> DevState {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_demod = "am".into();
        state.sdr_if_bw = 6000.0;
        state.sdr_squelch = -160.0;
        state.sdr_channels[0].audio_rate = 24000;
        state.sdr_channels[0].nr = true;
        state.sdr_channels[0].nr_algorithm = "dfn".into();
        state.sdr_channels[0].baseband = Some(receiver_workers::BasebandConfig {
            passband_hz: 16000.0, transition_hz: 1000.0, minimum_rate_hz: 48000.0,
        });
        let configs = state.channel_configs();
        let mut fixtures = service_fixture::FixtureSources::new();
        fixtures.configure(&configs).unwrap();
        state.fixture_sources = Some(fixtures);
        state.remember_receiver_streams();
        let mut workers = receiver_workers::ReceiverWorkers::new().unwrap();
        workers.configure(&configs).unwrap();
        if failed {
            workers.fail_source(1, "injected failed DFN configuration");
        }
        state.receiver_workers = Some(workers);
        state.advance_reception_recovery();
        state
    }

    #[test]
    fn configuration_repair_nr_changes_restart_only_failed_processing() {
        for edit in [json!({"channel":1,"nr":false}), json!({"channel":1,"nr_algorithm":"wiener"})] {
            for failed in [false, true] {
                let mut state = configuration_repair_state(failed);
                let before_iq = state.stream_registry();
                let old_config = state.channel_config(1).unwrap();
                let before_processing = state.processing_branches.input(&[0;56], &before_iq[0], &old_config).unwrap().0;
                let before_generation = state.receiver_workers.as_ref().unwrap().runtime_status(1, "")["processing_generation"].clone();
                let before_reception = state.reception.snapshot(1);

                state.apply_channel_config(&edit).unwrap();
                assert_eq!(state.stream_registry(), before_iq, "NR repair must preserve IQ identity");
                let config = state.channel_config(1).unwrap();
                let next_processing = state.processing_branches.input(&[0;56], &before_iq[0], &config).unwrap().0;
                let workers = state.receiver_workers.as_ref().unwrap();
                let generation = workers.runtime_status(1, "")["processing_generation"].clone();
                assert!(workers.channel_failure(1).is_none(), "the old failure blocked {edit}");
                if failed {
                    assert_ne!(generation, before_generation);
                    assert_ne!(next_processing.epoch, before_processing.epoch);
                    let waiting = state.reception.snapshot(1);
                    assert_eq!(waiting["state"], "preparing");
                    assert_eq!(waiting["last_interruption"], before_reception["last_interruption"]);
                    assert!(workers.recovery_progress(1).is_none(), "configuration is not observed processing progress");
                    let frames = state.fixture_sources.as_mut().unwrap().step().unwrap();
                    let output = state.process_receiver_frames_with_registry(&frames, &before_iq, true).unwrap();
                    assert!(output.audio.iter().any(|frame| frame.len() > 96), "the repaired processor must produce actual PCM");
                    assert!(state.receiver_workers.as_ref().unwrap().recovery_progress(1).is_some_and(|(count, _)| count > 0));
                    assert!(state.receiver_workers.as_ref().unwrap().channel_failure(1).is_none());
                } else {
                    assert_eq!(generation, before_generation, "healthy NR remains a hot update");
                    assert_eq!(next_processing, before_processing);
                    assert!(state.reception.snapshot(1).is_null());
                }
            }
        }
    }

    #[test]
    fn configuration_repair_removing_processor_waits_for_iq_without_forging_processing_recovery() {
        let mut state = configuration_repair_state(true);
        state.hardware_shim = Some("configuration-repair-test".into());
        state.fixture_sources = None;
        state.ddc = Some(SendDdc(Ddc::configured_service(100e6, 50e6, &mut None, 16, IQ_FS, &state.channel_configs()).unwrap()));
        let before_iq = state.stream_registry();
        let before = state.reception.snapshot(1);
        state.apply_channel_config(&json!({"channel":1,"processing_enabled":false})).unwrap();
        assert_eq!(state.stream_registry(), before_iq);
        let waiting = state.reception.snapshot(1);
        assert_eq!(waiting["state"], "preparing");
        assert_eq!(waiting["last_interruption"], before["last_interruption"]);
        assert_eq!(state.receiver_workers.as_ref().unwrap().runtime_status(1, "")["state"], "disabled");

        let iq = &before_iq[0];
        state.reception.observe_iq(1, iq.channel_id, iq.epoch, 100, 200);
        let receiving = state.reception.snapshot(1);
        assert_eq!(receiving["state"], "receiving", "healthy IQ must finish an IQ-only transition");
        assert_eq!(receiving["interruptions"], before["interruptions"]);
        assert_eq!(receiving["history"], before["history"]);
        assert_eq!(receiving["last_interruption"]["scope"], "processing");
        assert_eq!(receiving["last_interruption"]["failed_identity"], before["last_interruption"]["failed_identity"]);
        assert!(receiving["last_interruption"]["recovered_at_ms"].is_u64());
        assert!(receiving["last_interruption"]["resumed_processing_stream"].is_null());
        assert!(receiving["last_interruption"]["missing_input_samples"].is_null());
        assert_eq!(receiving["last_interruption"]["precision"], "unknown_gap");
        assert!(state.receiver_workers.as_ref().unwrap().recovery_progress(1).is_none());
    }

    #[test]
    fn configuration_repair_nr_does_not_mask_a_shared_source_fault() {
        let mut state = configuration_repair_state(false);
        state.reception_capture_failed("SDK IQS batch status=-130 samples=0");
        let before = state.reception.snapshot(1);
        assert_eq!(before["last_interruption"]["scope"], "capture");
        let generation = state.receiver_workers.as_ref().unwrap().runtime_status(1, "")["processing_generation"].clone();
        state.apply_channel_config(&json!({"channel":1,"nr":false})).unwrap();
        let after = state.reception.snapshot(1);
        assert_eq!(after["state"], before["state"]);
        assert_eq!(after["last_interruption"], before["last_interruption"]);
        assert_eq!(after["interruptions"], before["interruptions"]);
        assert_eq!(state.receiver_workers.as_ref().unwrap().runtime_status(1, "")["processing_generation"], generation);
        assert!(state.receiver_workers.as_ref().unwrap().channel_failure(1).is_some());
        assert!(!state.reception.capture_allowed(), "NR settings cannot cancel capture recovery backoff");
    }

    #[test]
    fn repaired_processor_configuration_waits_for_new_progress_without_retaining_old_fault() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_demod = "am".into();
        state.sdr_if_bw = 6000.0;
        state.sdr_channels[0].audio_rate = 24000;
        state.remember_receiver_streams();
        let mut workers = receiver_workers::ReceiverWorkers::new().unwrap();
        workers.configure(&state.channel_configs()).unwrap();
        workers.fail_source(1, "injected old processing failure");
        state.receiver_workers = Some(workers);
        state.advance_reception_recovery();
        let stopped = state.reception.snapshot(1);
        assert_eq!(stopped["state"], "stopped");
        assert_eq!(stopped["manual_recovery_allowed"], false);

        state.apply_channel_config(&json!({"channel":1,"audio_rate":48000})).unwrap();
        let waiting = state.reception.snapshot(1);
        assert!(state.receiver_workers.as_ref().unwrap().channel_failure(1).is_none());
        assert_eq!(waiting["state"], "preparing");
        assert_eq!(waiting["interruptions"], stopped["interruptions"]);
        assert_eq!(waiting["last_interruption"], stopped["last_interruption"]);
        assert!(waiting["last_interruption"]["recovered_at_ms"].is_null());
        assert!(state.receiver_workers.as_ref().unwrap().recovery_progress(1).is_none());
    }

    #[test]
    fn stopping_last_processor_keeps_models_ready_for_live_iq() {
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();state.sdr_center=100e6;state.sdr_listen=100e6;
        state.receiver_workers=Some(receiver_workers::ReceiverWorkers::new().unwrap());
        state.apply_channel_config(&json!({"channel":1,"enabled":false})).unwrap();
        assert!(state.channel_configs().iter().all(|c|!c.enabled));
        assert!(state.receiver_workers.is_some(),"live capture must retain its prepared models");
        state.apply_channel_config(&json!({"channel":MAX_CHANNELS,"enabled":true,"freq":100e6,"mode":"am"})).unwrap();
        assert!(state.receiver_workers.is_some());
    }

    #[test]
    fn external_band_reconciliation_retunes_selections_and_demotes_tuning() {
        let mut config = ChannelConfig { mode: "adsb".into(), freq: 435e6,
            if_bw: 1_800_000.0, ..ChannelConfig::default() };
        // Selecting the decoder (mode only) retunes to its canonical center.
        assert!(reconcile_external_band(&mut config, false, true));
        assert_eq!((config.mode.as_str(), config.freq), ("adsb", 1.090e9));
        assert!(!reconcile_external_band(&mut config, false, true), "in-band selection is untouched");
        // Re-selecting from a legacy out-of-band state heals it the same way.
        config.freq = 435e6;
        assert!(reconcile_external_band(&mut config, false, true));
        assert_eq!(config.freq, 1.090e9);
        // Tuning out of the band (freq only) demotes to the listenable companion.
        for (mode, companion) in
            [("adsb", "am"), ("meteor", "am"), ("ais", "am"), ("aprs", "nfm"), ("ft4", "usb"), ("wspr", "usb")] {
            let mut config = ChannelConfig { mode: mode.into(), freq: 500e6,
                if_bw: 20_000.0, ..ChannelConfig::default() };
            assert!(reconcile_external_band(&mut config, true, false), "{mode}");
            assert_eq!(config.mode, companion, "{mode}");
            assert_eq!(config.if_bw,
                websa_channelizer::catalog::mode(companion).unwrap().default_bandwidth_hz, "{mode}");
        }
        // An explicit mode and frequency that conflict stay for validation to reject.
        let mut config = ChannelConfig { mode: "adsb".into(), freq: 435e6, ..ChannelConfig::default() };
        assert!(!reconcile_external_band(&mut config, true, true));
        assert_eq!((config.mode.as_str(), config.freq), ("adsb", 435e6));
        // Native modes never reconcile.
        let mut config = ChannelConfig::default();
        assert!(!reconcile_external_band(&mut config, true, false));
    }

    #[test]
    fn radiofax_parse_is_pure_and_invalid_cross_fields_reject_before_commit() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.swp.mode="sdr".into();
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_demod="radiofax".into();
        state.sdr_if_bw=3_000.0;
        state.receiver_workers=Some(receiver_workers::ReceiverWorkers::new().unwrap());
        let before=state.channel_configs();
        let version=state.config_version;
        state.remember_receiver_streams();
        let context=state.receivers.control_metadata(1);
        let page=json!({"radiofax_new_page":true,"action_id":"new-page-test","action_token":context["action_token"],
            "service_id":context["service_id"],"receiver_uid":context["receiver_uid"]});
        state.parse_channel_config(&page).unwrap();
        state.parse_channel_config(&page).unwrap();
        assert!(!state.receiver_workers.as_ref().unwrap().radiofax_page_pending(1));
        for patch in [
            json!({"radiofax_acquisition":"manual","radiofax_new_page":true}),
            json!({"radiofax_lpm":"180","radiofax_new_page":true}),
            json!({"volume":3,"radiofax_new_page":true}),
            json!({"radiofax_ioc":"invalid","radiofax_new_page":true}),
        ] {
            assert!(state.apply_channel_config(&patch).is_err());
            assert_eq!(state.channel_configs(),before);
            assert_eq!(state.config_version,version);
            assert!(state.receiver_failure.is_none());
            assert!(!state.receiver_workers.as_ref().unwrap().radiofax_page_pending(1));
        }
        state.apply_channel_config(&page).unwrap();
        assert!(state.receiver_workers.as_ref().unwrap().radiofax_page_pending(1));
        assert_eq!(state.channel_configs(),before);
        assert_eq!(state.config_version,version);
    }

    #[test]
    fn unsupported_cw_pitch_rejects_without_retiring_receivers() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        let before = state.channel_configs();
        let version = state.config_version;
        for pitch in [99.0, 1601.0, 20_000.0] {
            assert!(dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"mode":"cw","pitch":pitch})).is_err());
            assert_eq!(state.channel_configs(), before);
            assert_eq!(state.config_version, version);
            assert!(state.receiver_failure.is_none());
        }
        for pitch in [100.0, 1600.0] {
            assert_eq!(state.parse_channel_config(&json!({"mode":"cw","pitch":pitch})).unwrap().pitch, pitch);
        }
    }

    #[test]
    fn agc_commands_preserve_squelch_and_reject_non_boolean_updates() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"squelch":-112.3,"volume":0.4})).unwrap();
        let mut expected = state.channel_configs();
        for enabled in [false, true] {
            dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"agc":enabled})).unwrap();
            expected[0].agc = enabled;
            assert_eq!(state.channel_configs(), expected);
            let status = state.status_snapshot().build();
            assert_eq!(status["sdr"]["agc"], enabled);
            assert_eq!(status["sdr"]["channels"][0]["agc"], enabled);
            assert_eq!(status["sdr"]["squelch"], -112.3);
        }
        for invalid in [json!("false"), json!(0), Value::Null] {
            assert!(dispatch(&mut state, "SET_SDR_CHANNEL", &json!({"agc":invalid})).is_err());
            assert_eq!(state.channel_configs(), expected);
        }
    }

    #[test]
    fn disabled_partial_preserves_config_and_enabling_checks_device_limits() {
        let mut state = DevState::new(json!({"preset_defaults": {}}), json!({}));
        let initial = state.channel_configs()[1].clone();
        assert_eq!(initial.freq, 0.0);
        let wfm = state.parse_channel_config(&json!({"channel":2,"enabled":true,"freq":100e6,"mode":"wfm","ifbw":180_000})).unwrap();
        assert!(wfm.stereo);
        let mono = state.parse_channel_config(&json!({"channel":2,"enabled":true,"freq":100e6,"mode":"wfm","ifbw":180_000,"stereo":false})).unwrap();
        assert!(!mono.stereo);
        assert_eq!(state.parse_channel_config(&json!({"channel":2,"enabled":false})).unwrap(), initial);
        assert!(state.parse_channel_config(&json!({"channel":2,"enabled":true})).is_err());
        state.sdr_channels[1].freq = 100e6;
        state.sdr_channels[1].mode = "wfm".into();
        state.sdr_channels[1].if_bw = 180_000.0;
        state.sdr_channels[1].stereo = false;
        state.sdr_channels[1].volume = 0.4;
        let mut expected = state.channel_configs()[1].clone();
        expected.enabled = true;
        assert_eq!(state.parse_channel_config(&json!({"channel":2,"enabled":true})).unwrap(), expected);
        expected.enabled = false;
        assert_eq!(state.parse_channel_config(&json!({"channel":2,"enabled":false})).unwrap(), expected);
        let (minimum, maximum) = state.freq_limits();
        for freq in [minimum - 1.0, maximum + 1.0] {
            assert!(state.parse_channel_config(&json!({"channel":2,"enabled":true,"freq":freq})).is_err());
        }
        assert!(state.parse_channel_config(&json!({"channel":2,"enabled":false,"volume":3})).is_err());
    }

    #[test]
    fn primary_filter_limits_match_mode_edits_after_independent_baseband() {
        let mut state = DevState::new(json!({"preset_defaults": {}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_center = 9_500_000.0;
        state.sdr_listen = 9_635_100.0;
        let mut conflicts = Vec::new();
        for mode in ["am", "dsb", "usb", "lsb", "cw", "nfm", "wfm", "pm"] {
            let descriptor = websa_channelizer::catalog::mode(mode).unwrap();
            state.sdr_demod = mode.into();
            state.sdr_if_bw = descriptor.bandwidth_presets_hz[0];
            state.sdr_channels[0].baseband = None;
            state.sdr_channels[0].processing_enabled = true;
            let expected = state.filter_limits();
            let plan = service_ddc::channel_plan(COARSE_RATE, &state.channel_configs()[0]).unwrap();
            state.sdr_channels[0].baseband = Some(receiver_workers::BasebandConfig {
                passband_hz: plan.occupied_bandwidth_hz,
                transition_hz: plan.transition_hz,
                minimum_rate_hz: plan.output_rate,
            });
            let before = state.channel_configs();
            let revision = state.config_version;
            let actual = state.filter_limits();
            if actual != expected { conflicts.push(format!("{mode}: expected {expected}, got {actual}")); }
            // The same wider preset is already accepted by the main-panel command.
            let widest = *descriptor.bandwidth_presets_hz.last().unwrap();
            let edit = state.parse_channel_config(&json!({"channel":1,"enabled":true,"mode":mode,"ifbw":widest})).unwrap();
            assert!(edit.baseband.is_none());
            assert!(edit.processing_enabled);
            let fs = state.capture_bandwidth() / 0.8;
            let source = service_ddc::source_plan(fs, &edit).unwrap();
            service_ddc::validate_source_frequency(state.capture_center(), state.capture_bandwidth(),
                fs, state.sdr_pfb_taps_per_phase, edit.freq, &source).unwrap();
            assert_eq!(state.channel_configs(), before);
            assert_eq!(state.config_version, revision);
        }
        assert!(conflicts.is_empty(), "{}", conflicts.join("\n"));
    }

    #[test]
    fn primary_filter_limits_publish_real_bounds_and_reject_below_minimum() {
        let mut state = DevState::new(json!({"preset_defaults": {}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        for (mode, width) in [("am", 100_000.0), ("usb", 50_000.0), ("cw", 10_000.0)] {
            let parsed = state.parse_channel_config(&json!({
                "channel": 1, "mode": mode, "ifbw": width,
            })).unwrap();
            assert_eq!(parsed.if_bw, width);
        }
        for (mode, minimum) in [("am", 50.0), ("usb", 101.0), ("lsb", 101.0), ("cw", 50.0)] {
            assert!(state.parse_channel_config(&json!({
                "channel": 1, "mode": mode, "ifbw": minimum,
            })).is_ok());
            assert!(state.parse_channel_config(&json!({
                "channel": 1, "mode": mode, "ifbw": minimum - 1.0,
            })).is_err());
        }
        let before = state.channel_configs();
        let revision = state.config_version;
        assert!(state.apply_channel_config(&json!({
            "channel": 1, "mode": "usb", "ifbw": 100.0,
        })).is_err());
        assert_eq!(state.channel_configs(), before);
        assert_eq!(state.config_version, revision);

        state.sdr_demod = "am".into();
        state.sdr_if_bw = 6_000.0;
        let limits = state.filter_limits();
        assert_eq!(limits["mode"], "am");
        assert_eq!(limits["min_hz"], 50.0);
        let maximum = limits["max_hz"].as_f64().unwrap();
        assert!(maximum >= 100_000.0 && maximum < COARSE_RATE);
        for width in [maximum, maximum + 1.0] {
            let mut config = state.channel_configs()[0].clone();
            config.if_bw = width;
            let fs = state.capture_bandwidth() / 0.8;
            let result = service_ddc::source_plan(fs, &config).and_then(|source| {
                service_ddc::validate_source_frequency(state.capture_center(), state.capture_bandwidth(),
                    fs, state.sdr_pfb_taps_per_phase, config.freq, &source)
            });
            assert_eq!(result.is_ok(), width == maximum, "width={width}");
        }
        let status = state.status_snapshot().build();
        assert_eq!(status["sdr"]["filter_limits"], limits);

        state.sdr_demod = "ft8".into();
        assert_eq!(state.filter_limits(), json!({"mode":"ft8","min_hz":0,"max_hz":null}));
        state.sdr_demod = "am".into();
        state.swp.mode = "std".into();
        assert_eq!(state.filter_limits(), json!({"mode":"am","min_hz":50.0,"max_hz":null}));
    }

    #[test]
    fn primary_outside_capture_recaptures_without_moving_aux() {
        let mut state = DevState::new(json!({"preset_defaults": {}}), json!({}));
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_decimate = 1;
        let mut configs = state.channel_configs();
        configs[1].enabled = true;
        configs[1].freq = 120e6;
        state.sdr_channels = configs;
        let config = state.parse_channel_config(&json!({"channel": 1, "freq": 130e6, "mode": "am", "ifbw": 6000})).unwrap();
        let capture = state.channel_recapture(&config).unwrap().unwrap();
        assert_eq!(capture.center, 130e6);
        assert_eq!(capture.configs.len(), MAX_CHANNELS);
        assert_eq!(capture.configs[0].freq, 130e6);
        assert_eq!(capture.configs[1].freq, 120e6);
        assert!(capture.configs[1].enabled);
        let aux = state.parse_channel_config(&json!({"channel": 2, "freq": 130e6})).unwrap();
        let outside=state.channel_recapture(&aux).unwrap().unwrap();
        assert!(outside.band_paused.contains(&2));assert_eq!(outside.configs[1].freq,130e6);
        let inside = state.parse_channel_config(&json!({"channel": 1, "freq": 105e6, "mode": "am", "ifbw": 6000})).unwrap();
        assert!(state.channel_recapture(&inside).unwrap().is_none());
        state.sdr_channels[1].freq = 90e6;
        let outside=state.channel_recapture(&config).unwrap().unwrap();
        assert!(outside.band_paused.contains(&2));assert_eq!(outside.configs[1].freq,90e6);
    }

    #[test]
    fn processing_only_control_consumes_pending_iq_before_the_new_branch() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.swp.mode="sdr".into();state.sdr_center=100e6;state.sdr_listen=100e6;
        state.sdr_squelch=-160.0;state.sdr_demod="am".into();state.sdr_if_bw=6000.0;
        state.sdr_channels[0].baseband=Some(receiver_workers::BasebandConfig {
            passband_hz:16000.0,transition_hz:1000.0,minimum_rate_hz:48000.0});
        state.hardware_shim=Some("numerical-control-test".into());
        state.ddc=Some(SendDdc(Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&state.channel_configs()).unwrap()));
        let registry=state.stream_registry();let entry=registry[0].clone();
        let frame=|seq,index,iq:&[f32]|websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,
            seq,entry.output_rate,entry.frequency_hz,entry.channel_id as u64,entry.epoch as u64,index,0.0,iq).unwrap();
        let block=vec![0.25f32;512];let step=(entry.input_rate/entry.output_rate).round() as u64;
        let output=state.process_receiver_frames_with_registry(&[frame(0,0,&[]),frame(1,0,&block)],&registry,true).unwrap();
        assert!(!output.decode.is_empty());assert_eq!(state.channel_status()[0]["squelch_open"],true);
        let first_processing=state.processing_registry()[0].epoch;
        state.pending_iq.push(frame(2,256*step,&block).into());
        state.apply_channel_config(&json!({"channel":1,"mode":"usb","ifbw":2400,
            "baseband":state.sdr_channels[0].baseband,"processing_enabled":true})).unwrap();
        assert!(state.receiver_failure.is_none(),"{:?}",state.receiver_failure);
        assert_eq!(state.stream_registry()[0].epoch,entry.epoch);
        assert_eq!(state.stream_registry()[0].channel_id,entry.channel_id);
        assert_eq!(state.receiver_workers.as_ref().unwrap().stats()["submitted_samples"],512);
        let next=state.process_receiver_frames_with_registry(&[frame(3,512*step,&block)],&registry,true).unwrap();
        assert!(!next.decode.is_empty());assert_ne!(state.processing_registry()[0].epoch,first_processing);
        assert_eq!(state.receiver_workers.as_ref().unwrap().stats()["submitted_samples"],768);
        assert!(state.receiver_failure.is_none());
    }

    #[test]
    fn auxiliary_outside_narrow_capture_waits_without_moving_primary_or_widening_capture() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.sdr_decimate = 2048;
        let fs = IQ_FS / 8.0;
        let bandwidth = fs * 0.8;
        state.ddc = Some(SendDdc(Ddc::configured_service(100e6, bandwidth, &mut None,
            16, fs, &state.channel_configs()).unwrap()));
        state.hardware_shim = Some("geometry-only".into());
        let aux = state.parse_channel_config(&json!({
            "channel":2,"enabled":true,"freq":110e6,"mode":"am","ifbw":6000,
        })).unwrap();
        let capture = state.channel_recapture(&aux).unwrap().unwrap();
        assert_eq!(capture.center, 100e6);
        assert_eq!(capture.configs[0].freq, 100e6);
        let mut paused=aux.clone();paused.enabled=false;
        assert_eq!(capture.configs[1], paused);
        assert!(capture.band_paused.contains(&2));
        assert!(capture.source_rate <= fs);
        assert!(capture.bandwidth <= bandwidth);
        assert_eq!(state.capture_bandwidth(), bandwidth);
    }

    #[test]
    fn same_frequency_gps_mode_widens_source_capture() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.sdr_center = 1_575_420_000.0;
        state.sdr_listen = state.sdr_center;
        state.sdr_decimate = 2048;
        let fs = IQ_FS / 32.0;
        let bandwidth = fs * 0.8;
        state.ddc = Some(SendDdc(Ddc::configured_service(state.sdr_center, bandwidth, &mut None,
            16, fs, &state.channel_configs()).unwrap()));
        state.hardware_shim = Some("geometry-only".into());
        let mut config = state.channel_configs()[0].clone();
        config.mode = "gps".into();
        config.if_bw = 2_046_000.0;
        assert_eq!(config.freq, state.sdr_listen);
        assert!(service_ddc::source_plan(fs, &config).is_err());
        let capture = state.channel_recapture(&config).unwrap().unwrap();
        assert_eq!(capture.center, state.sdr_center);
        assert_eq!(capture.configs[0], config);
        assert!(capture.source_rate > fs);
        let source = service_ddc::source_plan(capture.source_rate, &config).unwrap();
        assert!(source.is_direct());
        service_ddc::validate_source_frequency(capture.center, capture.bandwidth,
            capture.source_rate, capture.taps, config.freq, &source).unwrap();
        assert_eq!(state.capture_bandwidth(), bandwidth);
    }

    #[test]
    fn cold_gps_enable_uses_candidate_capture_instead_of_status_fallback() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.hardware_shim=Some("geometry-only".into());
        state.sdr_center=434_750_000.0;
        state.sdr_listen=1_000_000_000.0;
        state.sdr_decimate=16;
        let mut configs=state.channel_configs();
        configs[0].enabled=false;
        configs[0].freq=state.sdr_listen;
        state.sdr_channels=configs.clone();
        assert!(state.pipeline.is_none() && state.ddc.is_none());
        assert_eq!(state.capture_bandwidth(),50_000_000.0);
        let mut gps=configs[0].clone();
        gps.enabled=true;
        gps.freq=435_000_000.0;
        gps.mode="gps".into();
        gps.if_bw=2_046_000.0;
        let capture=state.channel_recapture(&gps).unwrap().unwrap();
        assert_eq!(capture.center,434_750_000.0);
        assert_eq!(capture.source_rate,3_906_250.0);
        assert_eq!(capture.device_decimate,16);
        assert_eq!(capture.configs[0],gps);
        let source=service_ddc::source_plan(capture.source_rate,&gps).unwrap();
        service_ddc::validate_source_frequency(capture.center,capture.bandwidth,
            capture.source_rate,capture.taps,gps.freq,&source).unwrap();
        assert_eq!(state.sdr_listen,1_000_000_000.0);
    }

    #[test]
    fn cold_primary_far_retune_moves_capture_center() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.hardware_shim=Some("geometry-only".into());
        state.sdr_center=1_000_000_000.0;
        state.sdr_listen=state.sdr_center;
        state.sdr_decimate=16;
        let mut config=state.channel_configs()[0].clone();
        config.enabled=true;
        config.freq=14_000_000.0;
        let capture=state.channel_recapture(&config).unwrap().unwrap();
        assert_eq!(capture.center,14_000_000.0);
        assert_eq!(capture.configs[0],config);
        assert_eq!(state.sdr_center,1_000_000_000.0);
    }
}

pub(super) async fn plugins() -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    use websa_channelizer::catalog;
    use websa_dsp::digital::sstv::{mode_vis, SSTV_MODES};
    let manifest = tokio::task::spawn_blocking(catalog::manifest).await
        .map_err(|error| (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":format!("decoder catalog failed: {error}")}))))?;
    let modes: Vec<_> = SSTV_MODES.iter().enumerate().map(|(index, (name, mode))| {
        let (width, height) = mode.resolution();
        json!({"index":index,"name":name,"vis":mode_vis(*mode),"width":width,"height":height})
    }).collect();
    Ok(Json(json!({"plugins":manifest,"sstv_modes":modes})))
}
