use super::*;

pub(super) struct HardwarePresentation {
    pipeline: sdr_pipeline::PipelinePresentation,
    output: sdr_pipeline::PipelineOutput,
    version: u64,
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
    pub frames: Vec<Vec<u8>>,
    actual: Value,
    version: u64,
    timing: Timing,
    peak: Option<(f64, f64, i32, u64)>,
    channel_power: Option<[Option<(std::time::Instant, f64)>; 3]>,
}
impl HardwarePresentation {
    pub fn finish(mut self) -> Result<PresentedHardware, String> {
        // Diagnostic JSON follows the spectrum cadence, not the IQ polling loop.
        if self.output.pan.is_empty() {
            return Ok(PresentedHardware { pan:Vec::new(), frames:self.output.frames,
                actual:Value::Null, version:self.version, timing:self.timing, peak:None, channel_power:None });
        }
        let snapshot = &self.pipeline.snapshot;
        let bandwidth = (2.0 * display_half_width(self.decimate as i32)).min(snapshot.usable_bandwidth);
        let mut actual = self.pipeline.encode();
        actual.as_object_mut().unwrap().extend(json!({
            "iq_rate":snapshot.usable_bandwidth / 0.8,"ddc_rate":snapshot.rate,
            "bandwidth":bandwidth,"capture_bandwidth":snapshot.usable_bandwidth,
            "iq_center":self.center,"center":self.center,"decimate":self.decimate,
            "pfb_taps_per_phase":snapshot.taps,"capture_decimate":snapshot.device_decimate,
            "capture_center":snapshot.capture_center,"capture_start":snapshot.capture_center-snapshot.usable_bandwidth/2.0,
            "capture_stop":snapshot.capture_center+snapshot.usable_bandwidth/2.0,
            "start":self.center-bandwidth/2.0,"stop":self.center+bandwidth/2.0,
            "packet_samples":16240,"packet_bytes":64960,"pan_available":false,
            "listen":self.listen,"demod":self.demod,"if_bw":self.if_bw,
            "ddc_decimate":snapshot.registry.iter().find(|c|c.channel==1).map(|c| snapshot.usable_bandwidth/0.8/c.output_rate),"ddc_batch":1,
            "audio_rate":null,"deemph_us":self.deemph,
            "timing_scope":"capture_lifetime_cumulative","timing_quantiles":"histogram_upper_bound_ns",
            "ddc_error_count":self.errors,"ddc_error_last":self.last_error,
        }).as_object().unwrap().clone());
        let mut pan = Vec::with_capacity(self.output.pan.len());
        let mut peak = None;
        let mut channel_power = None;
        for (freq, power, warning, generation, seq) in self.output.pan {
            if generation != snapshot.generation { continue; }
            peak = None;
            channel_power = Some(service_power::measure_channel_power(&freq, &power, &self.channel_configs));
            let started = std::time::Instant::now();
            pan.push(hardware_pan(self.frequency_version, self.center, bandwidth, &freq, &power)?);
            self.timing.observe(started.elapsed());
            let mut inside: Vec<f32> = freq.iter().zip(&power)
                .filter(|(f, p)| (**f-self.center).abs() <= bandwidth/2.0 && p.is_finite())
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
    pub(super) fn capture_bandwidth(&self) -> f64 {
        self.pipeline.as_ref().map(|p| p.snapshot.usable_bandwidth)
            .or_else(|| self.ddc.as_ref().map(|d| d.0.usable_bandwidth)).unwrap_or(50e6)
    }
    pub(super) fn capture_center(&self) -> f64 {
        self.pipeline.as_ref().map(|p| p.snapshot.capture_center)
            .or_else(|| self.ddc.as_ref().map(|d| d.0.capture_center)).unwrap_or(self.sdr_center)
    }
    pub(super) fn ddc_command(&mut self, command: impl FnOnce(&mut Ddc) -> Result<Vec<Vec<u8>>, String> + Send + 'static) -> Result<(), String> {
        let registry = self.stream_registry();
        let frames = if let Some(pipeline) = self.pipeline.as_mut() {
            match pipeline.command(command) {
                Ok(frames) => frames,
                // boundary already retained drained frames and spectra; the next poll
                // delivers them, and a fatal pipeline reports itself there.
                Err(error) => return Err(error),
            }
        } else if let Some(ddc) = self.ddc.as_mut() { command(&mut ddc.0)? }
        else { Vec::new() };
        // Even an empty fine tail can follow a batch already queued by a
        // detached presentation. Finish it before configuration generations
        // advance, so that the worker cannot discard that retiring batch.
        if !frames.is_empty() || self.receiver_workers.is_some() {
            let mut retiring = std::mem::take(&mut self.pending_iq);
            retiring.extend(frames);
            let output = self.process_receiver_frames_with_registry(&retiring, &registry, true)?;
            self.pending_receiver.audio.extend(output.audio);
            self.pending_receiver.decode.extend(output.decode);
            for frame in &retiring {
                if frame.len()>=56 && matches!(frame.get(..4),Some(b"IQBF"|b"IQAX")) {
                    self.consumed_receiver_iq.insert((u32::from_le_bytes(frame[32..36].try_into().unwrap()),
                        u32::from_le_bytes(frame[36..40].try_into().unwrap()),u32::from_le_bytes(frame[8..12].try_into().unwrap())));
                }
            }
            self.retiring_registry = registry;
            self.pending_iq.extend(retiring);
        }
        Ok(())
    }
    pub(super) fn stop_pipeline(&mut self) -> Result<(), String> {
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
        // A command may drain the next IQ batch while spectrum encoding runs
        // without the state lock. Queue this batch first so that command tails
        // cannot overtake it, and retain worker output for the ordered drain.
        self.submit_receiver_frames(&output.frames)?;
        Ok(output)
    }
    pub(super) fn take_hardware_presentation(&mut self, listeners: usize) -> Result<Option<HardwarePresentation>, String> {
        if self.receiver_failure.is_some() {return Ok(None);}
        let blocked = self.ddc_error.is_some() && !Self::retry_due(&self.ddc_retry_at);
        if self.pipeline.is_none() && listeners > 0 && !blocked {
            // Compile receiver models before arming capture; startup must not
            // stall the consumer while the hardware IQ queue is filling.
            if self.receiver_workers.is_none() {
                self.receiver_workers = Some(receiver_workers::ReceiverWorkers::new()?);
            }
            let configs=self.channel_configs();
            let aux=configs.iter().filter(|c|c.channel>1 && c.enabled).map(|c|(c.channel,c.freq)).collect();
            self.pipeline = Some(Pipeline::start_service(PipelineConfig {
                center:self.sdr_center, listen:self.sdr_listen, fine:2, display_decimate:self.sdr_decimate as i32,
                front_end:self.front_end(self.sdr_ref), shim:self.hardware_shim.clone().unwrap(),
                taps:self.sdr_pfb_taps_per_phase, aux, channel_budget:3,
                output_rate_budget:COARSE_RATE*3.0,
            },configs)?);
            self.device_detail = self.pipeline.as_ref().and_then(|pipeline| pipeline.device_info).map(|info| info.detail());
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
        self.note_auto_acquisition(i32::MIN);
        if let Some(pipeline) = &self.pipeline {
            let mut diagnostics = pipeline.failure_diagnostics();
            if diagnostics.get("error").is_none() {
                diagnostics = json!({"error":&error,"actual":diagnostics});
            }
            self.ddc_failure_diagnostics = Some(diagnostics);
        }
        self.ddc_error_count += 1;
        self.ddc_error_last = Some(error.clone());
        self.ddc_error = Some(error);
        self.pipeline = None;
        self.ddc_retry_at = Some(std::time::Instant::now()+Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let actual = std::mem::take(&mut state.pending_receiver);
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
            pan:vec![vec![1]], frames:vec![vec![2], vec![3]],
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
    fn current_presentation_keeps_pan_and_updates_actual() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        let mut output = PresentedHardware {
            pan:vec![vec![1]], frames:vec![vec![2]],
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
