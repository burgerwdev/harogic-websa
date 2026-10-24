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
impl DevState {
    pub(super) fn channel_configs(&self) -> Vec<ChannelConfig> {
        let mut configs: Vec<_> = (1..=3).map(|slot| self.sdr_channels.iter()
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
    pub(super) fn channel_status(&self) -> Value {
        let registry = self.stream_registry();
        let power = self.fresh_channel_power();
        Value::Array(self.channel_configs().into_iter().map(|config| {
            let entry = registry.iter().find(|entry| entry.channel == config.channel);
            let mut value = json!(config);
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
            value["squelch_open"] = json!(active.and_then(|entry| self.receiver_workers.as_ref()
                .and_then(|workers| workers.squelch_status(entry, std::time::Instant::now()))
                .map(|(open, _)| open)));
            value
        }).collect())
    }
    pub(super) fn parse_channel_config(&self, data: &Value) -> Result<ChannelConfig, String> {
        if !data.is_object() { return Err("channel configuration must be an object".into()); }
        let channel = match data.get("channel") {
            None => 1,
            Some(value) => value.as_u64().filter(|slot| (1..=3).contains(slot))
                .ok_or("channel must be an integer in 1..=3")? as u32,
        };
        let mut config = self.channel_configs()[(channel - 1) as usize].clone();
        config.enabled = boolean(data, "enabled", config.enabled)?;
        config.freq = number(data, &["freq", "listen"], config.freq)?;
        config.mode = text(data, &["mode", "demod"], &config.mode)?;
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
        config.nr = boolean(data, "nr", config.nr)?;
        config.nr_strength = number(data, &["nr_strength"], config.nr_strength)?;
        config.nr_algorithm = text(data, &["nr_algorithm"], &config.nr_algorithm)?;
        config.nr_atten_db = number(data, &["nr_atten_db"], config.nr_atten_db)?;
        config.stereo = boolean(data, "stereo", config.stereo)?;
        if let Some(value) = data.get("audio_rate") {
            config.audio_rate = value.as_u64().and_then(|rate| u32::try_from(rate).ok())
                .ok_or("audio_rate must be an integer")?;
        }
        if let Some(value) = data.get("sstv_mode") {
            config.sstv_mode = if value.is_null() { None } else {
                Some(value.as_u64().and_then(|mode| usize::try_from(mode).ok())
                    .filter(|&mode| mode < websa_dsp::digital::sstv::SSTV_MODES.len())
                    .ok_or("sstv_mode must be null or a supported mode index")?)
            };
        }
        let (minimum, maximum) = self.freq_limits();
        if config.enabled && !(minimum..=maximum).contains(&config.freq) { return Err("channel frequency outside device limits".into()); }
        service_ddc::channel_plan(COARSE_RATE,&config)?;
        if !(-200.0..=0.0).contains(&config.squelch) { return Err("squelch must be in -200..=0 dB".into()); }
        if !(0.0..=2.0).contains(&config.volume) { return Err("volume must be in 0..=2".into()); }
        if config.pitch <= 0.0 || config.pitch > 20_000.0 { return Err("pitch must be in (0, 20000] Hz".into()); }
        if !(-1.0..=1000.0).contains(&config.deemph_us) { return Err("deemph_us must be in -1..=1000".into()); }
        if !(0.0..=1.0).contains(&config.nr_strength) { return Err("nr_strength must be in 0..=1".into()); }
        if !matches!(config.nr_algorithm.as_str(), "wiener" | "dfn") { return Err("nr_algorithm must be wiener or dfn".into()); }
        if !(0.0..=100.0).contains(&config.nr_atten_db) { return Err("nr_atten_db must be in 0..=100".into()); }
        if !(8_000..=192_000).contains(&config.audio_rate) { return Err("audio_rate must be in 8000..=192000".into()); }
        Ok(config)
    }

    /// Primary tuning may move the shared capture; auxiliary tuning remains capture-bound.
    pub(super) fn channel_recapture(&self, config: &ChannelConfig) -> Result<Option<service_capture::CaptureConfig>, String> {
        if config.channel != 1 || !config.enabled || config.freq == self.sdr_listen {
            return Ok(None);
        }
        let fs = self.capture_bandwidth() / 0.8;
        let center = self.capture_center();
        let bandwidth = self.capture_bandwidth();
        let plan = service_ddc::channel_plan(COARSE_RATE, config)?;
        let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
        if (-m / 2..m / 2).contains(&signed_bin(config.freq, center))
            && (config.freq - center).abs() + plan.occupied_bandwidth_hz / 2.0 <= bandwidth / 2.0 {
            return Ok(None);
        }
        let mut configs = self.channel_configs();
        configs[0] = config.clone();
        self.fit_service_capture(config.freq,self.sdr_decimate,self.sdr_pfb_taps_per_phase,configs,false).map(Some)
    }

    pub(super) fn apply_channel_config(&mut self, data: &Value) -> Result<(), String> {
        let config = self.parse_channel_config(data)?;
        let channel = config.channel;
        let mut configs = self.channel_configs();
        let old = configs[(channel - 1) as usize].clone();
        if config == old { return Ok(()); }
        let geometry_changed=old.enabled!=config.enabled || old.freq!=config.freq || old.mode!=config.mode
            || old.if_bw!=config.if_bw || old.pitch!=config.pitch || old.audio_rate!=config.audio_rate
            || old.stereo!=config.stereo || old.sstv_mode!=config.sstv_mode;
        let recapture = self.channel_recapture(&config)?;
        if geometry_changed && config.enabled && recapture.is_none() {
            // Replay IQ clocks describe receiver output, not the shared RF capture grid.
            let fs = self.capture_bandwidth() / 0.8;
            let plan=service_ddc::channel_plan(COARSE_RATE,&config)?;
            Ddc::validate_service_frequency(self.capture_center(),self.capture_bandwidth(),fs,
                self.sdr_pfb_taps_per_phase,config.freq,&plan)?;
        }
        if geometry_changed && self.pipeline.is_none() && self.ddc.is_none() {
            if let Some(workers)=self.receiver_workers.as_mut() {
                let output=workers.finish()?;
                self.pending_receiver.audio.extend(output.audio);
                self.decode_snapshots.observe(&output.decode)?;
                self.pending_receiver.decode.extend(output.decode);
                self.retiring_registry=self.stream_registry();
            }
        }
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
                self.ddc_command(move |ddc| ddc.configure_service_channel(next))?;
            }
            self.decode_snapshots.clear_channel(channel);
        }
        if !geometry_changed {
            if let Some(ddc)=self.ddc.as_mut() {
                if let Some(current)=ddc.0.service_configs.as_mut() {current[(channel-1) as usize]=config.clone();}
            }
        }
        if let Some(capture) = recapture {
            self.sdr_center = capture.center;
            self.invalidate_observation();
            self.channel_power = [None; 3];
            for slot in 1..=3 { self.decode_snapshots.clear_channel(slot); }
            self.freq_version += 1;
        }
        if geometry_changed { self.channel_power[(channel-1) as usize]=None; }
        configs[(channel - 1) as usize] = config.clone();
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
        if let Some(workers) = self.receiver_workers.as_mut() { workers.configure(&self.sdr_channels)?; }
        self.config_version += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(capture.configs.len(), 3);
        assert_eq!(capture.configs[0].freq, 130e6);
        assert_eq!(capture.configs[1].freq, 120e6);
        assert!(capture.configs[1].enabled);
        let aux = state.parse_channel_config(&json!({"channel": 2, "freq": 130e6})).unwrap();
        assert!(state.channel_recapture(&aux).unwrap().is_none());
        let inside = state.parse_channel_config(&json!({"channel": 1, "freq": 105e6, "mode": "am", "ifbw": 6000})).unwrap();
        assert!(state.channel_recapture(&inside).unwrap().is_none());
        state.sdr_channels[1].freq = 90e6;
        assert!(state.channel_recapture(&config).is_err());
    }
}

pub(super) async fn plugins() -> Json<Value> {
    use websa_dsp::plugin::{plugins, PluginKind};
    use websa_dsp::digital::sstv::{mode_vis, SSTV_MODES};
    let manifest: Vec<_> = [("analog", PluginKind::Analog), ("digital", PluginKind::Digital),
        ("audio", PluginKind::Audio), ("ddc", PluginKind::Ddc)].into_iter()
        .flat_map(|(kind, family)| plugins(family).iter().map(move |plugin| json!({
            "kind":kind, "id":plugin.id, "implemented":plugin.implemented,
            "audioEnhancement":plugin.audio_enhancement,
        }))).collect();
    let modes: Vec<_> = SSTV_MODES.iter().enumerate().map(|(index, (name, mode))| {
        let (width, height) = mode.resolution();
        json!({"index":index,"name":name,"vis":mode_vis(*mode),"width":width,"height":height})
    }).collect();
    Json(json!({"plugins":manifest,"sstv_modes":modes}))
}
