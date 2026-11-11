use super::*;

#[derive(Clone, Copy)]
pub(super) struct CaptureParameters {
    pub center: f64,
    pub bandwidth: f64,
    pub input_rate: f64,
    pub epoch: u64,
}

pub(super) struct HardwarePresentation {
    pipeline: sdr_pipeline::PipelinePresentation,
    output: sdr_pipeline::PipelineOutput,
    version: u64,
    rf_limits: (f64, f64),
    frequency_version: u32,
    center: f64,
    decimate: i64,
    listen: f64,
    demod: String,
    if_bw: f64,
    deemph: f64,
    errors: u64,
    last_error: Option<String>,
    timing: Timing,
    channel_configs: Vec<receiver_workers::ChannelConfig>,
}
pub(super) struct PresentedHardware {
    pub pan: Vec<Vec<u8>>,
    pub frames: IqFrames,
    actual: Value,
    version: u64,
    timing: Timing,
    peak: Option<(f64, f64, i32, u64)>,
    channel_power: Option<Vec<Option<(std::time::Instant, f64)>>>,
}
impl HardwarePresentation {
    pub fn finish(mut self) -> Result<PresentedHardware, String> {
        // Presentation never serializes diagnostic histories or worker metrics.
        if self.output.pan.is_empty() {
            return Ok(PresentedHardware { pan:Vec::new(), frames:self.output.frames,
                actual:Value::Null, version:self.version, timing:self.timing, peak:None, channel_power:None });
        }
        let snapshot = &self.pipeline.snapshot;
        let bandwidth = (2.0 * display_half_width(self.decimate as i32)).min(snapshot.usable_bandwidth);
        // The IQ window may extend past the RF limits the model reports; only bins inside those
        // limits are presented. The physical capture keeps its requested centre and bandwidth.
        let lo = (self.center - bandwidth / 2.0).max(self.rf_limits.0);
        let hi = (self.center + bandwidth / 2.0).min(self.rf_limits.1);
        let window = hi.max(lo) - lo;
        let mut actual = self.pipeline.encode_live();
        actual.as_object_mut().unwrap().extend(json!({
            "iq_rate":snapshot.input_rate,"ddc_rate":snapshot.rate,
            "bandwidth":bandwidth,"capture_bandwidth":snapshot.usable_bandwidth,
            "iq_center":self.center,"center":self.center,"decimate":self.decimate,
            "pfb_taps_per_phase":snapshot.taps,"capture_decimate":snapshot.device_decimate,
            "capture_center":snapshot.capture_center,"capture_start":snapshot.capture_center-snapshot.usable_bandwidth/2.0,
            "capture_stop":snapshot.capture_center+snapshot.usable_bandwidth/2.0,
            "start":lo,"stop":hi,
            "packet_samples":16240,"packet_bytes":64960,"pan_available":false,
            "listen":self.listen,"demod":self.demod,"if_bw":self.if_bw,
            "ddc_decimate":snapshot.registry.iter().find(|c|c.channel==1).map(|c| snapshot.input_rate/c.output_rate),"ddc_batch":snapshot.batch_packets,
            "audio_rate":null,"deemph_us":self.deemph,
            "timing_scope":"capture_lifetime_cumulative","timing_quantiles":"histogram_upper_bound_ns",
            "ddc_error_count":self.errors,"ddc_error_last":self.last_error,
        }).as_object().unwrap().clone());
        let mut pan = Vec::with_capacity(self.output.pan.len());
        let mut peak = None;
        let mut channel_power = None;
        let fft_size = snapshot.display_fft["size"].as_u64().unwrap_or(0) as usize;
        for (freq, power, warning, generation, seq, observation_size) in self.output.pan {
            if generation != snapshot.generation { continue; }
            // RF levels, auto gain and receiver power keep the original analysis
            // bandwidth even when the user requests finer display observations.
            if observation_size != 0 {
                if observation_size == fft_size { pan.push(hardware_pan(self.frequency_version, lo, window, &freq, &power)?); }
                continue;
            }
            peak = None;
            channel_power = Some(service_power::measure_channel_power(&freq, &power, &self.channel_configs));
            let started = std::time::Instant::now();
            if fft_size == 0 { pan.push(hardware_pan(self.frequency_version, lo, window, &freq, &power)?); }
            self.timing.observe(started.elapsed());
            let mut inside: Vec<f32> = freq.iter().zip(&power)
                .filter(|(f, p)| **f >= lo && **f <= hi && p.is_finite())
                .map(|(_, p)| *p).collect();
            if !inside.is_empty() {
                inside.sort_by(f32::total_cmp);
                peak = Some((*inside.last().unwrap() as f64, inside[(inside.len()-1)*3/10] as f64, warning, seq));
            }
        }
        if let Some(frame) = pan.last() {
            actual["pan_available"] = json!(true);
            actual["pan_points"] = json!(u32::from_le_bytes(frame[8..12].try_into().unwrap()));
        }
        actual["spectrum_encode"] = json!(self.timing.snapshot());
        Ok(PresentedHardware { pan, frames:self.output.frames, actual, version:self.version, timing:self.timing, peak, channel_power })
    }
}
impl DevState {
    pub(super) fn live_capture_parameters(&self) -> Option<CaptureParameters> {
        self.pipeline
            .as_ref()
            .map(|p| CaptureParameters {
                center: p.snapshot.capture_center,
                bandwidth: p.snapshot.usable_bandwidth,
                input_rate: p.snapshot.input_rate,
                epoch: p.snapshot.epoch,
            })
            .or_else(|| {
                self.ddc.as_ref().map(|d| CaptureParameters {
                    center: d.0.capture_center,
                    bandwidth: d.0.usable_bandwidth,
                    input_rate: d.0.stream.input_sample_rate(),
                    epoch: d.0.stream.epoch(),
                })
            })
    }
    fn remember_capture_parameters(&mut self) {
        if let Some(parameters) = self.live_capture_parameters() {
            self.last_capture = Some(parameters);
        }
    }
    pub(super) fn capture_snapshot(&self) -> Value {
        let live = self.live_capture_parameters();
        // Fixture lanes are independent baseband producers, without an RF/PFB
        // input clock. Their common window is simulated; per-lane clocks apply.
        let simulated = self.hardware_shim.is_none() && self.fixture_sources.is_some();
        let parameters = live
            .or_else(|| {
                simulated.then_some(CaptureParameters {
                    center: self.sdr_center,
                    bandwidth: 50e6,
                    input_rate: IQ_FS,
                    epoch: 0,
                })
            })
            .or(self.last_capture);
        let running = (live.is_some() || simulated)
            && self.ddc_error.is_none()
            && self.link_up
            && self.swp.mode == "sdr";
        json!({"source_id":"primary","center_hz":parameters.map_or(self.sdr_center,|p|p.center),
            "usable_bandwidth_hz":parameters.map(|p|p.bandwidth),"input_rate_hz":parameters.map(|p|p.input_rate),
            "epoch":parameters.map_or_else(||"0".to_owned(),|p|p.epoch.to_string()),
            "parameters_state":if running && simulated {"simulated"} else if running {"live"} else if parameters.is_some() {"last_known"} else {"unknown"},
            "source_kind":if self.hardware_shim.is_some() {"hardware"} else {"fixture"},
            "state":if self.ddc_error.is_some() || !self.link_up || self.swp.mode!="sdr" {"failed"}
                else if running {"running"} else {"idle"}})
    }
    pub(super) fn capture_bandwidth(&self) -> f64 {
        self.pipeline.as_ref().map(|p| p.snapshot.usable_bandwidth)
            .or_else(|| self.ddc.as_ref().map(|d| d.0.usable_bandwidth))
            .or(self.last_capture.map(|p|p.bandwidth)).unwrap_or(50e6)
    }
    pub(super) fn capture_input_rate(&self) -> f64 {
        self.live_capture_parameters().or(self.last_capture)
            .map_or(IQ_FS, |parameters| parameters.input_rate)
    }
    pub(super) fn capture_center(&self) -> f64 {
        self.pipeline.as_ref().map(|p| p.snapshot.capture_center)
            .or_else(|| self.ddc.as_ref().map(|d| d.0.capture_center))
            .or(self.last_capture.map(|p|p.center)).unwrap_or(self.sdr_center)
    }
    pub(super) fn ddc_channel_command(&mut self, command: impl FnOnce(&mut Ddc)->Result<IqFrames,String>+Send+'static)->Result<(),String> {
        self.ddc_command_inner(command,true)
    }
    pub(super) fn ddc_command(&mut self, command: impl FnOnce(&mut Ddc) -> Result<IqFrames, String> + Send + 'static) -> Result<(), String> {
        self.ddc_command_inner(command,false)
    }
    fn ddc_command_inner(&mut self, command:impl FnOnce(&mut Ddc)->Result<IqFrames,String>+Send+'static,local:bool)->Result<(),String> {
        let registry = self.stream_registry();
        let frames = if let Some(pipeline) = self.pipeline.as_mut() {
            match if local {pipeline.command_channel(command)} else {pipeline.command(command)} {
                Ok(frames) => frames,
                // boundary already retained drained frames and spectra; the next poll
                // delivers them, and a fatal pipeline reports itself there.
                Err(error) => return Err(error),
            }
        } else if let Some(ddc) = self.ddc.as_mut() { command(&mut ddc.0)? }
        else { Vec::new() };
        // Preserve immutable old configuration/identity at admission. A single
        // source control must not wait for synchronous inference on any lane;
        // changed generations explicitly cancel unfinished old processing.
        if !frames.is_empty() || self.receiver_workers.is_some() {
            let mut retiring = std::mem::take(&mut self.pending_iq);
            retiring.extend(frames);
            if let Some(scan)=&self.scan {scan.iq(&retiring);}
            // A temporary lease has no logical receiver or processor. Its
            // drained tail must stay outside formal decode/audio/record paths.
            retiring.retain(|frame|!self.scan_probe.as_ref().is_some_and(|probe|probe.matches(frame)));
            match self.process_control_receiver_frames(&retiring, &registry) {
                Ok(output)=>{
                    self.pending_receiver.append(output);
                }
                Err(error)=>self.latch_receiver_failure(error),
            }
            for input in retiring.iter().filter(|_|self.receiver_failure.is_none()) {
                let frame=input.iq_header();
                if frame.len()>=56 && matches!(frame.get(..4),Some(b"IQBF"|b"IQAX")) {
                    self.consumed_receiver_iq.insert((u32::from_le_bytes(frame[32..36].try_into().unwrap()),
                        u32::from_le_bytes(frame[36..40].try_into().unwrap()),u32::from_le_bytes(frame[8..12].try_into().unwrap())));
                }
            }
            let current=self.stream_registry();
            let changed:Vec<u32>=registry.iter().filter(|old| !current.iter().any(|new|
                new.channel==old.channel && new.channel_id==old.channel_id && new.epoch==old.epoch))
                .map(|old|old.channel).collect();
            if !changed.is_empty() {
                if let Err(error)=self.retire_receiver_channels(&changed,registry.clone()) {self.latch_receiver_failure(error);}
            }
            self.retiring_registry = registry;
            self.pending_iq.extend(retiring);
        }
        Ok(())
    }
    pub(super) fn stop_pipeline(&mut self) -> Result<(), String> {
        // Probe IQ has no formal receiver; drain it before finalizing receiver tails.
        self.retire_scan_probe()?;
        self.remember_capture_parameters();
        if let Some(mut pipeline) = self.pipeline.take() {
            let output = pipeline.stop()?;
            self.pending_iq.extend(output.frames);
        }
        Ok(())
    }
    fn prepare_hardware_output(&mut self, mut output:sdr_pipeline::PipelineOutput)->Result<sdr_pipeline::PipelineOutput,String> {
        let mut pending = std::mem::take(&mut self.pending_iq);
        pending.append(&mut output.frames);
        output.frames = pending;
        if let Some(scan)=&self.scan { scan.iq(&output.frames); }
        output.frames.retain(|frame| !self.scan_probe.as_ref().is_some_and(|probe| probe.matches(frame)));
        // A command may drain the next IQ batch while spectrum encoding runs
        // without the state lock. Queue this batch first so that command tails
        // cannot overtake it, and retain worker output for the ordered drain.
        if let Err(error)=self.submit_receiver_frames(&output.frames) {self.latch_receiver_failure(error);}
        Ok(output)
    }
    pub(super) fn take_hardware_presentation(&mut self, listeners: usize) -> Result<Option<HardwarePresentation>, String> {
        let blocked = self.ddc_error.is_some() && (!self.reception.capture_allowed() || !Self::retry_due(&self.ddc_retry_at));
        if self.pipeline.is_none() && listeners > 0 && !blocked {
            // Compile receiver models before arming capture; startup must not
            // stall the consumer while the hardware IQ queue is filling.
            if self.receiver_workers.is_none() && self.receiver_failure.is_none()
                && self.channel_configs().iter().any(|config|config.enabled&&config.processing_enabled) {
                match receiver_workers::ReceiverWorkers::new() {
                    Ok(workers)=>self.receiver_workers=Some(workers),
                    Err(error)=>self.latch_receiver_failure(error),
                }
            }
            let mut configs=self.channel_configs();
            for config in &mut configs {config.recovery_settle=self.ddc_error.is_some();if self.reception.source_stopped(config.channel) {config.enabled=false;}}
            let aux=configs.iter().filter(|c|c.channel>1 && c.enabled).map(|c|(c.channel,c.freq)).collect();
            if self.ddc_error.is_some() {self.reception.begin_capture();}
            self.pipeline = Some(Pipeline::start_service(PipelineConfig {
                center:self.sdr_center, listen:self.sdr_listen, fine:2, display_decimate:self.sdr_decimate as i32,
                front_end:self.front_end(self.sdr_ref), shim:self.hardware_shim.clone().unwrap(),
                taps:self.sdr_pfb_taps_per_phase, aux, channel_budget:MAX_CHANNELS,
                output_rate_budget:COARSE_RATE*MAX_CHANNELS as f64,
            },configs)?);
            self.device_detail = self.pipeline.as_ref().and_then(|pipeline| pipeline.device_info).map(|info| info.detail());
            self.remember_capture_parameters();
            let (refclk, refclk_src) = self.pipeline.as_ref().unwrap().clock();
            let front_end = self.pipeline.as_ref().unwrap().front_end();
            self.sdr_actual = json!({"refclk":refclk, "refclk_src":refclk_src,
                "ref":front_end.map(|f|f.ref_dbm), "atten":front_end.map(|f|f.atten),
                "preamp":front_end.map(|f|f.preamp), "ifgain":front_end.map(|f|f.ifgain)});
            self.ddc_error = None;
            self.ddc_retry_at = None;
            self.link_up = true;
        }
        let channel_configs = self.channel_configs();
        let Some(pipeline) = self.pipeline.as_mut() else { return Ok(None); };
        let output = pipeline.poll()?;
        self.status_warning = pipeline.warning();
        let presentation = pipeline.presentation();
        let (packets, warning) = pipeline.packet_observation();
        if packets > self.auto.iqs_packets {
            self.auto.iqs_packets = packets;
            self.note_auto_acquisition(warning);
        }
        let output = self.prepare_hardware_output(output)?;
        Ok(Some(HardwarePresentation {
            pipeline:presentation, output, version:self.config_version,
            frequency_version:self.freq_version as u32, center:self.sdr_center,
            rf_limits:self.freq_limits(),
            decimate:self.sdr_decimate, listen:self.sdr_listen, demod:self.sdr_demod.clone(),
            if_bw:self.sdr_if_bw, deemph:self.sdr_deemph, errors:self.ddc_error_count,
            last_error:self.ddc_error_last.clone(), timing:std::mem::take(&mut self.spectrum_timing),
            channel_configs,
        }))
    }
    pub(super) fn apply_hardware_presentation(&mut self, output: &mut PresentedHardware) {
        if self.config_version != output.version || self.swp.mode != "sdr" {
            output.pan.clear();
            return;
        }
        if !output.actual.is_null() { self.sdr_actual = std::mem::take(&mut output.actual); }
        self.spectrum_timing = std::mem::take(&mut output.timing);
        if let Some(channel_power) = output.channel_power.take() {
            self.channel_power = channel_power;
        }
        if let Some((peak, floor, warning, seq)) = output.peak {
            // A queued clean FFT cannot erase a newer overflow from this capture.
            if self.auto.observation_config == self.config_version
                && self.auto.observation_status == -12 && warning != -12 {
                return;
            }
            self.note_auto_acquisition(warning);
            if (warning == 0 || warning == -301)
                && self.auto.session.as_ref().is_none_or(|session| seq > session.packet_baseline)
            {
                self.auto.spectrum_observation = self.auto.observation;
                self.auto.last_peak = Some(peak);
                self.auto.last_floor = Some(floor);
            }
        }
    }
    pub(super) fn hardware_presentation_failed(&mut self, error: String) {
        self.reception_capture_failed(&error);
        self.remember_capture_parameters();
        // The source ended with a gap. Failed owners must discard their old
        // windows, not receive normal finish commands after their mailbox exits.
        self.pending_receiver=receiver_workers::ReceiverFrames::default();
        self.pending_iq.clear();
        self.consumed_receiver_iq.clear();
        self.note_auto_acquisition(i32::MIN);
        if let Some(pipeline) = &self.pipeline {
            let mut diagnostics = pipeline.failure_diagnostics();
            if diagnostics.get("error").is_none() {
                diagnostics = json!({"error":&error,"actual":diagnostics});
            }
            self.remember_capture_failure(diagnostics);
        }
        self.ddc_error_count += 1;
        self.ddc_error_last = Some(error.clone());
        self.ddc_error = Some(error);
        self.pipeline = None;
        self.ddc_retry_at = Some(std::time::Instant::now()+Duration::from_secs(1));
    }

    fn remember_capture_failure(&mut self, diagnostics: Value) {
        // A failed automatic retry must not erase the incident that ended the
        // original continuous stream. Retain one first and one latest snapshot;
        // these are only copied on failure and encoded by explicit diagnostics.
        self.ddc_first_failure_diagnostics.get_or_insert_with(|| diagnostics.clone());
        self.ddc_failure_diagnostics = Some(diagnostics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_retry_preserves_first_capture_failure_and_reports_latest() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        let first = json!({"error":"original queue overflow", "input_index":19_500_000_000u64});
        let retry = json!({"error":"retry overflow", "input_index":16_240});
        state.remember_capture_failure(first.clone());
        state.remember_capture_failure(retry.clone());
        assert_eq!(state.ddc_first_failure_diagnostics, Some(first));
        assert_eq!(state.ddc_failure_diagnostics, Some(retry));
    }

    #[test]
    fn independent_fixture_lanes_report_a_simulated_window() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.fixture_sources = Some(service_fixture::FixtureSources::new());
        let capture = state.capture_snapshot();
        assert_eq!(capture["state"], "running");
        assert_eq!(capture["parameters_state"], "simulated");
        assert_eq!(capture["source_kind"], "fixture");
        assert_eq!(capture["usable_bandwidth_hz"], 50e6);
        assert!(state.live_capture_parameters().is_none());
    }

    #[test]
    fn capture_parameters_are_unknown_until_a_source_exists() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.hardware_shim = Some("geometry-only".into());
        let capture = state.receiver_snapshot()["capture"].clone();
        assert_eq!(capture["state"], "idle");
        assert_eq!(capture["parameters_state"], "unknown");
        assert!(capture["usable_bandwidth_hz"].is_null());
        assert!(capture["input_rate_hz"].is_null());
        state.hardware_presentation_failed("open failed".into());
        let capture = state.receiver_snapshot()["capture"].clone();
        assert_eq!(capture["state"], "failed");
        assert_eq!(capture["parameters_state"], "unknown");
        assert!(capture["input_rate_hz"].is_null());
    }

    #[test]
    fn capture_failure_retains_geometry_and_recovery_has_a_new_dsp_epoch() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_center = 14_277_240.0;
        state.sdr_listen = 14_270_000.0;
        let fs = 3_906_250.0;
        let source = Ddc::configured_service(
            state.sdr_center,
            fs * 0.8,
            &mut None,
            16,
            fs,
            &state.channel_configs(),
        )
        .unwrap();
        state.ddc = Some(SendDdc(source));
        let before = state.receiver_snapshot();
        assert_eq!(before["capture"]["input_rate_hz"], fs);
        assert_ne!(before["capture"]["epoch"], "0");
        state.hardware_presentation_failed("SDK capture stall".into());
        state.ddc = None;
        state.sdr_center = 100e6; // A requested center cannot rewrite the failed source.
        let failed = state.receiver_snapshot();
        assert_eq!(failed["capture"]["parameters_state"], "last_known");
        assert_eq!(failed["capture"]["state"], "failed");
        assert_eq!(failed["capture"]["center_hz"], 14_277_240.0);
        assert_eq!(failed["capture"]["usable_bandwidth_hz"], 3_125_000.0);
        assert_eq!(failed["capture"]["epoch"], before["capture"]["epoch"]);
        state.sdr_center = 14_277_240.0;
        state.ddc = Some(SendDdc(
            Ddc::configured_service(
                state.sdr_center,
                fs * 0.8,
                &mut None,
                16,
                fs,
                &state.channel_configs(),
            )
            .unwrap(),
        ));
        state.ddc_error = None;
        let after = state.receiver_snapshot();
        assert_eq!(after["capture"]["parameters_state"], "live");
        assert_eq!(after["capture"]["state"], "running");
        assert_ne!(after["capture"]["epoch"], before["capture"]["epoch"]);
        assert_eq!(
            after["receivers"][0]["receiver_uid"],
            before["receivers"][0]["receiver_uid"]
        );
    }
    #[test]
    fn receiver_submit_failure_preserves_current_and_later_capture_products() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.swp.mode="sdr".into();
        let mut source=service_fixture::FixtureSources::new();
        source.configure(&state.channel_configs()).unwrap();
        let mut frames=source.step().unwrap();
        state.fixture_sources=Some(source);
        // Valid IQ transport and identity; deliberately inconsistent receiver rate.
        frames[0][16..24].copy_from_slice(&(-1.0_f64).to_le_bytes());
        let pan=vec![(vec![100e6],vec![-40.0],0,0,1,0)];
        let output=state.prepare_hardware_output(sdr_pipeline::PipelineOutput {frames:frames.clone(),pan:pan.clone()}).unwrap();
        assert_eq!(output.frames,frames,"receiver validation cannot consume raw IQ");
        assert_eq!(output.pan,pan,"receiver validation cannot discard queued spectra");
        assert_eq!(state.receiver_failure.as_deref(),Some("invalid receiver IQ frame metadata"));
        assert!(state.ddc_error.is_none());
        for sequence in 2..10 {
            let frames=state.fixture_sources.as_mut().unwrap().step().unwrap();
            let pan=vec![(vec![100e6],vec![-40.0],0,0,sequence,0)];
            let output=state.prepare_hardware_output(sdr_pipeline::PipelineOutput {frames:frames.clone(),pan:pan.clone()}).unwrap();
            assert_eq!(output.frames,frames);
            assert_eq!(output.pan,pan);
            assert!(state.drain_receiver_frames().unwrap().audio.is_empty());
        }
        assert_eq!(state.ddc_error_count,0);
    }

    #[test]
    fn detached_presentation_queues_iq_before_command_tail_and_keeps_new_epoch_start() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_demod = "am".into();
        state.sdr_if_bw = 6000.0;
        let configs = state.channel_configs();
        let mut source = service_fixture::FixtureSources::new();
        source.configure(&configs).unwrap();
        let first = source.step().unwrap();
        let tail = source.step().unwrap();
        let registry = source.registry();
        state.fixture_sources = Some(source);

        // An uninterrupted fixture is the reference for both PCM and decode
        // sequencing; the interleaved path must deliver the identical bytes.
        let mut reference = DevState::new(json!({"preset_defaults":{}}), json!({}));
        reference.sdr_demod = "am".into();
        reference.sdr_if_bw = 6000.0;
        let mut uninterrupted = first.clone();
        uninterrupted.extend(tail.clone());
        let expected = reference.process_receiver_frames_with_registry(&uninterrupted,&registry,true).unwrap();
        assert_eq!(u32::from_le_bytes(expected.audio[0][8..12].try_into().unwrap()),0);

        // The presentation remains detached while the command drains the later
        // tail. Its seq0 and first data packet must already be in the worker.
        let detached = state.prepare_hardware_output(sdr_pipeline::PipelineOutput {
            frames:first.clone(),pan:Vec::new(),
        }).unwrap();
        let retired = state.process_receiver_frames_with_registry(&tail,&registry,true).unwrap();
        assert_eq!(retired.audio,expected.audio);
        assert_eq!(retired.decode,expected.decode);
        assert_eq!(detached.frames,first);

        // Commands also retain retired IQ for raw clients. Polling it together
        // with the new identity must not feed the old tail a second time, and
        // must preserve the new identity's zero marker under its own registry.
        for frame in &tail {
            state.consumed_receiver_iq.insert((
                u32::from_le_bytes(frame[32..36].try_into().unwrap()),
                u32::from_le_bytes(frame[36..40].try_into().unwrap()),
                u32::from_le_bytes(frame[8..12].try_into().unwrap())));
        }
        state.pending_iq = tail.clone();
        let source = state.fixture_sources.as_mut().unwrap();
        source.restart(None).unwrap();
        let next = source.step().unwrap();
        let next_registry = source.registry();
        assert_ne!(registry[0].epoch,next_registry[0].epoch);
        let expected = reference.process_receiver_frames_with_registry(&next,&next_registry,true).unwrap();
        let detached_next = state.prepare_hardware_output(sdr_pipeline::PipelineOutput {
            frames:next.clone(),pan:Vec::new(),
        }).unwrap();
        state.ddc_command(|_|Ok(Vec::new())).unwrap();
        let mut actual = std::mem::take(&mut state.pending_receiver);
        // The control admits immutable work without waiting for inference.
        // This finite test explicitly waits before comparing the complete PCM.
        actual.append(state.receiver_workers.as_mut().unwrap().finish().unwrap());
        assert_eq!(actual.audio,expected.audio);
        assert_eq!(actual.decode,expected.decode);
        let mut raw = tail;
        raw.extend(next);
        assert_eq!(detached_next.frames,raw);
        assert!(state.consumed_receiver_iq.is_empty());
    }

    #[test]
    fn stale_presentation_expires_pan_but_preserves_iq_tail() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_actual = json!({"center":200e6});
        state.auto.last_peak = Some(-20.0);
        let mut output = PresentedHardware {
            pan:vec![vec![1]], frames:vec![vec![2].into(), vec![3].into()],
            actual:json!({"center":100e6}), version:state.config_version,
            timing:Timing::default(), peak:Some((-40.0, -80.0, 0, 1)),
            channel_power:None,
        };
        state.config_version += 1;
        state.apply_hardware_presentation(&mut output);
        assert!(output.pan.is_empty());
        assert_eq!(output.frames, vec![vec![2], vec![3]]);
        assert_eq!(state.sdr_actual, json!({"center":200e6}));
        assert_eq!(state.auto.last_peak, Some(-20.0));
    }

    #[test]
    fn failed_server_handoff_fences_detached_metadata_power_and_auto_observation() {
        if !crate::tests::isolated_handoff_test("sdr_hardware::tests::failed_server_handoff_fences_detached_metadata_power_and_auto_observation") { return; }
        crate::tests::register_stateless_handoff_library();
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();
        state.hardware_shim=Some("registered-stateless-library".into());
        state.link_up=true;
        state.actual=json!({"center":100e6});
        state.sdr_actual=json!({"center":100e6,"ref":-10.0});
        state.rta_actual=json!({"center":100e6});
        state.gnss_value=Some(json!({"lock":1}));
        state.channel_power=vec![Some((std::time::Instant::now(),-21.0));MAX_CHANNELS];
        state.note_auto_acquisition(-12);
        state.auto.last_peak=Some(-20.0);
        state.auto.last_floor=Some(-80.0);
        state.auto_scale(None,false);
        let old_version=state.config_version;
        let mut output=PresentedHardware {
            pan:vec![b"old-pan".to_vec()],frames:vec![b"retiring-iq".to_vec().into()],
            actual:json!({"center":200e6,"ref":3.0}),version:old_version,
            timing:Timing::default(),peak:Some((-2.0,-50.0,0,999)),
            channel_power:Some(vec![Some((std::time::Instant::now(),-1.0));MAX_CHANNELS]),
        };
        let error=dispatch(&mut state,"SET_MODE",&json!({"mode":"server"})).unwrap_err();
        assert_eq!(error.code,"device_error");
        assert!(error.message.contains("cannot release"));
        assert_eq!(state.swp.mode,"sdr");
        assert_eq!(state.config_version,old_version+1);
        assert!(!state.link_up);
        assert_eq!(state.actual,json!({}));
        assert_eq!(state.sdr_actual,json!({}));
        assert_eq!(state.rta_actual,json!({}));
        assert!(state.gnss_value.is_none());
        assert!(state.channel_power.iter().all(Option::is_none));
        assert!(state.auto.session.is_none());
        assert!(state.auto.last_peak.is_none() && state.auto.last_floor.is_none());
        let observation=(state.auto.observation,state.auto.observation_config,state.auto.observation_status,
            state.auto.spectrum_observation,state.auto.seq);
        state.apply_hardware_presentation(&mut output);
        assert!(output.pan.is_empty());
        assert_eq!(output.frames,vec![b"retiring-iq".to_vec()]);
        assert_eq!(state.sdr_actual,json!({}),"detached metadata cannot undo failed-handoff clearing");
        assert!(state.channel_power.iter().all(Option::is_none));
        assert_eq!((state.auto.observation,state.auto.observation_config,state.auto.observation_status,
            state.auto.spectrum_observation,state.auto.seq),observation);
        assert!(state.auto.last_peak.is_none() && state.auto.last_floor.is_none());
    }

    #[test]
    fn current_presentation_keeps_pan_and_updates_actual() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        let mut output = PresentedHardware {
            pan:vec![vec![1]], frames:vec![vec![2].into()],
            actual:json!({"center":100e6,"ref":-7.5,"atten":12,"preamp":1,"ifgain":2}), version:state.config_version,
            timing:Timing::default(), peak:Some((-40.0, -80.0, 0, 1)),
            channel_power:None,
        };
        state.apply_hardware_presentation(&mut output);
        assert_eq!(output.pan, vec![vec![1]]);
        assert_eq!(output.frames, vec![vec![2]]);
        assert_eq!(state.sdr_actual, json!({"center":100e6,"ref":-7.5,"atten":12,"preamp":1,"ifgain":2}));
        let status = state.status_snapshot().build();
        assert_eq!(status["amp"]["atten_actual"], 12);
        assert_eq!(status["amp"]["preamp_actual"], 1);
        assert_eq!(status["amp"]["ifgain_actual"], 2);
        assert_eq!(state.auto.last_peak, Some(-40.0));
        output.peak = Some((-10.0, -60.0, -12, 2));
        state.apply_hardware_presentation(&mut output);
        assert_eq!(output.pan, vec![vec![1]]);
        assert_eq!(state.auto.observation_status, -12);
        assert_ne!(state.auto.spectrum_observation, state.auto.observation);
        assert_eq!(state.auto.last_peak, None, "overflow invalidates the prior clean peak");
        state.auto.session = Some(AutoSession {
            mode:"sdr".into(), config:state.config_version, seen:state.auto.observation,
            deadline:std::time::Instant::now()+Duration::from_secs(20), remaining:24,
            display_only:false, display_ref:None, pending:None, previous_ref:None,
            recovering:false, fitted:false, packet_baseline:5,
        });
        // A clean FFT fetched before the click cannot prove the new session.
        output.peak = Some((-5.0, -50.0, 0, 1));
        state.apply_hardware_presentation(&mut output);
        assert_eq!(state.auto.last_peak, None);
        // The owner reports a new clean acquisition before its later FFT is
        // presented, as take_hardware_presentation does for fresh packet counts.
        state.note_auto_acquisition(0);
        output.peak = Some((-5.0, -50.0, 0, 6));
        state.apply_hardware_presentation(&mut output);
        assert_eq!(state.auto.last_peak, Some(-5.0));
    }
}
