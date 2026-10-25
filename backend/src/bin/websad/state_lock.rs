use super::*;
use std::ops::{Deref, DerefMut};
use std::time::Instant;

// Assigned under the state guard, independently of command config_version changes.
static STATUS_REVISION: AtomicU64 = AtomicU64::new(0);

pub(super) struct StatusSnapshot {
    pub revision: u64,
    golden: Value,
    swp: Swp,
    rta_center: f64,
    rta_span: f64,
    rta_rbw_mode: String,
    rta_rbw: f64,
    rta_vbw_mode: String,
    rta_vbw: f64,
    rta_ref: f64,
    rta_sweep_time_mode: i64,
    rta_sweep_time: f64,
    rta_actual: Value,
    rta_trigger_actual: Value,
    sdr_center: f64,
    sdr_actual: Value,
    sdr_ref: f64,
    receiver_failure: Option<String>,
    channel_registry: Arc<[sdr_pipeline::ChannelRegistryEntry]>,
    channels: Value,
    actual: Value,
    link_up: bool,
    status_warning: i32,
    ref_clock: String,
    refclk_out: bool,
    refclk_ppm: f64,
    device_defaults: Option<SwpDefaults>,
    device_detail: Option<Value>,
    has_docxo: bool,
    has_premium_ref_clock: bool,
    trigger: RtaTrigger,
    gnss_value: Option<Value>,
    sweep_ms: f64,
    sweep_ms_meas: f64,
    config_version: u64,
    ddc_error: Option<String>,
    ddc_failure_diagnostics: Option<Value>,
    rta_error: Option<String>,
    swp_error: Option<String>,
    pnm_error: Option<String>,
    sdr_decimate: i64,
    sdr_listen: f64,
    sdr_demod: String,
    sdr_if_bw: f64,
    sdr_squelch: f64,
    sdr_volume: f64,
    sdr_agc: bool,
    sdr_pitch: f64,
    sdr_deemph: f64,
    sdr_aux: Value,
    sdr_filter_limits: Value,
    sdr_ref_set: bool,
    sdr_pfb_taps_per_phase: usize,
    hardware: bool,
    freq_limits: (f64, f64),
    actual_taps: usize,
    auto: AutoRef,
    state_lock_wait: websa_channelizer::timing::TimingSnapshot,
    state_lock_hold: websa_channelizer::timing::TimingSnapshot,
}
impl DevState {
    pub(super) fn has_docxo(&self) -> bool {
        if self.hardware_shim.is_some() {
            self.device_detail.as_ref().and_then(|detail| detail["has_docxo"].as_bool()).unwrap_or(false)
        } else {
            self.golden["has_docxo"].as_bool().unwrap_or(false)
        }
    }

    pub(super) fn has_premium_ref_clock(&self) -> bool {
        if self.hardware_shim.is_some() {
            self.device_detail.as_ref().and_then(|detail| detail["has_premium_ref_clock"].as_bool()).unwrap_or(false)
        } else {
            self.golden["has_premium_ref_clock"].as_bool().unwrap_or_else(|| self.has_docxo())
        }
    }

    pub(super) fn status_snapshot(&self) -> StatusSnapshot {
        let mut sdr_actual = self.sdr_actual.clone();
        if let Some(pipeline) = self.pipeline.as_ref().filter(|_| self.swp.mode == "sdr") {
            pipeline.snapshot.write_channel_status(&mut sdr_actual);
            let (refclk, refclk_src) = pipeline.clock();
            sdr_actual["refclk"] = json!(refclk);
            sdr_actual["refclk_src"] = json!(refclk_src);
            let front_end = pipeline.front_end();
            sdr_actual["ref"] = json!(front_end.map(|f| f.ref_dbm));
            sdr_actual["atten"] = json!(front_end.map(|f| f.atten));
            sdr_actual["preamp"] = json!(front_end.map(|f| f.preamp));
            sdr_actual["ifgain"] = json!(front_end.map(|f| f.ifgain));
        } else {
            sdr_actual["channel_clocks"] = json!([]);
            if let Some(object) = sdr_actual.as_object_mut() { object.remove("channel_clock_error"); }
            if self.swp.mode == "sdr" {
                if let Some(capture) = self.ddc.as_ref().and_then(|ddc| ddc.0.hardware.as_ref()) {
                    let (refclk, refclk_src) = capture.diagnostics().clock();
                    sdr_actual["refclk"] = json!(refclk);
                    sdr_actual["refclk_src"] = json!(refclk_src);
                    let front_end = capture.diagnostics().front_end();
                    sdr_actual["ref"] = json!(front_end.map(|f| f.ref_dbm));
                    sdr_actual["atten"] = json!(front_end.map(|f| f.atten));
                    sdr_actual["preamp"] = json!(front_end.map(|f| f.preamp));
                    sdr_actual["ifgain"] = json!(front_end.map(|f| f.ifgain));
                }
            }
        }
        StatusSnapshot {
            revision: STATUS_REVISION.fetch_add(1, Ordering::Relaxed),
            golden: self.golden.clone(),
            swp: self.swp.clone(),
            rta_center: self.rta_center.clone(),
            rta_span: self.rta_span.clone(),
            rta_rbw_mode: self.rta_rbw_mode.clone(),
            rta_rbw: self.rta_rbw.clone(),
            rta_vbw_mode: self.rta_vbw_mode.clone(),
            rta_vbw: self.rta_vbw.clone(),
            rta_ref: self.rta_ref.clone(),
            rta_sweep_time_mode: self.rta_sweep_time_mode.clone(),
            rta_sweep_time: self.rta_sweep_time.clone(),
            rta_actual: self.rta_actual.clone(),
            rta_trigger_actual: self.rta_trigger_actual.clone(),
            sdr_center: self.sdr_center.clone(),
            sdr_actual,
            sdr_ref: self.sdr_ref.clone(),
            receiver_failure: self.receiver_failure.clone(),
            channels: self.channel_status(),
            channel_registry: self.stream_registry(),
            actual: self.actual.clone(),
            link_up: self.link_up.clone(),
            status_warning: self.status_warning.clone(),
            ref_clock: self.ref_clock.clone(),
            refclk_out: self.refclk_out.clone(),
            refclk_ppm: self.refclk_ppm.clone(),
            device_defaults: self.device_defaults.clone(),
            device_detail: self.device_detail.clone(),
            has_docxo: self.has_docxo(),
            has_premium_ref_clock: self.has_premium_ref_clock(),
            trigger: self.trigger.clone(),
            gnss_value: self.gnss_value.clone(),
            sweep_ms: self.sweep_ms.clone(),
            sweep_ms_meas: self.sweep_ms_meas.clone(),
            config_version: self.config_version.clone(),
            ddc_error: self.ddc_error.clone(),
            ddc_failure_diagnostics: self.ddc_failure_diagnostics.clone(),
            rta_error: self.rta_error.clone(),
            swp_error: self.swp_error.clone(),
            pnm_error: self.pnm_error.clone(),
            sdr_decimate: self.sdr_decimate.clone(),
            sdr_listen: self.sdr_listen.clone(),
            sdr_demod: self.sdr_demod.clone(),
            sdr_if_bw: self.sdr_if_bw.clone(),
            sdr_squelch: self.sdr_squelch.clone(),
            sdr_volume: self.sdr_volume.clone(),
            sdr_agc: self.sdr_agc.clone(),
            sdr_pitch: self.sdr_pitch.clone(),
            sdr_deemph: self.sdr_deemph.clone(),
            sdr_aux: self.sdr_aux.clone(),
            sdr_ref_set: self.sdr_ref_set.clone(),
            sdr_pfb_taps_per_phase: self.sdr_pfb_taps_per_phase.clone(),
            sdr_filter_limits: self.filter_limits(),
            hardware:self.hardware_shim.is_some(), freq_limits:self.freq_limits(),
            actual_taps:self.pipeline.as_ref().map(|p| p.snapshot.taps)
                .or_else(|| self.ddc.as_ref().map(|d| d.0.stream.taps_per_phase()))
                .unwrap_or(self.sdr_pfb_taps_per_phase),
            auto:self.auto.clone(),
            state_lock_wait:self.state_lock_wait.snapshot(), state_lock_hold:self.state_lock_hold.snapshot(),
        }
    }
}
impl StatusSnapshot {
    pub fn build(&self) -> Value {
        let mut s = self.golden.clone();
        let p = &self.swp;
        let mode = p.mode.as_str();
        let (req_active, mut actual): (Value, Value) = match mode {
            "rta" => (
                json!({
                    "center": self.rta_center, "span": self.rta_span, "points": 3328,
                    "rbw_mode": self.rta_rbw_mode, "rbw": self.rta_rbw,
                    "vbw_mode": self.rta_vbw_mode, "vbw": self.rta_vbw,
                    "ref_mode": "manual", "ref": self.rta_ref,
                    "sweep_time_mode": self.rta_sweep_time_mode,
                    "sweep_time": self.rta_sweep_time,
                }),
                self.rta_actual.clone(),
            ),
            "sdr" => (
                json!({
                    "center": self.sdr_center, "span": self.sdr_actual["bandwidth"],
                    "points": self.sdr_actual["pan_points"],
                    "rbw_mode": "auto", "rbw": 0.0, "vbw_mode": "equal", "vbw": 0.0,
                    "ref_mode": "manual", "ref": self.sdr_ref,
                }),
                self.sdr_actual.clone(),
            ),
            _ => (
                json!({
                    "center": p.center, "span": p.span, "points": p.points,
                    "rbw_mode": p.rbw_mode, "rbw": p.rbw, "vbw_mode": p.vbw_mode,
                    "vbw": p.vbw, "ref_mode": "manual", "ref": p.ref_level,
                    "sweep_time_mode": p.sweep_time_mode, "sweep_time": p.sweep_time,
                    "spur": p.spur, "detector": p.detector,
                    "frame_detector": p.frame_detector, "trace_align": p.trace_align,
                    "points_strategy": p.points_strategy, "window": p.window,
                }),
                self.actual.clone(),
            ),
        };
        let registry = json!(&*self.channel_registry);
        actual["channel_registry"] = registry.clone();
        for key in [
            "center",
            "span",
            "points",
            "rbw_mode",
            "rbw",
            "vbw_mode",
            "vbw",
            "ref_mode",
            "ref",
            "sweep_time_mode",
            "sweep_time",
            "spur",
            "detector",
            "frame_detector",
            "trace_align",
            "points_strategy",
            "window",
        ] {
            if let Some(value) = req_active.get(key) {
                // Python's top level is the effective value: the hardware readback
                // when present, the request otherwise. `req` keeps the request.
                let effective = actual.get(key).unwrap_or(value);
                set_path(&mut s, &format!("/{key}"), effective.clone());
                set_path(&mut s, &format!("/req/{key}"), value.clone());
            }
        }
        for (path, value) in [
            (
                "/connected",
                json!(!self.hardware || self.link_up),
            ),
            ("/status_warning", json!(self.status_warning)),
            ("/sdr_last_failure", json!(self.ddc_failure_diagnostics)),
            ("/ref_clock", json!(self.ref_clock)),
            ("/refclk_out", json!(self.refclk_out)),
            ("/refclk_ppm", json!(self.refclk_ppm)),
            ("/swp_actual", self.actual.clone()),
            ("/caps/fmin", json!(self.freq_limits.0)),
            ("/caps/fmax", json!(self.freq_limits.1)),
            (
                "/preset_defaults",
                match &self.device_defaults {
                    Some(d) => json!({
                        "center": d.center, "span": d.span,
                        "fmin": d.center - d.span / 2.0, "fmax": d.center + d.span / 2.0,
                        "ref": d.ref_dbm, "rbw": d.rbw, "vbw": d.vbw, "points": d.points,
                        "atten": d.atten, "window": d.window, "rbw_mode": d.rbw_mode,
                        "vbw_mode": d.vbw_mode, "spur": d.spur, "detector": "auto",
                        "frame_detector": d.frame_detector, "trace_align": d.trace_align,
                        "points_strategy": d.points_strategy, "preamp": d.preamp,
                        "ifgain": d.ifgain, "gain_strategy": d.gain_strategy,
                        "sweep_time_mode": d.sweep_time_mode, "sweep_time": d.sweep_time,
                    }),
                    None => self.golden["preset_defaults"].clone(),
                },
            ),
            (
                "/req/rta",
                json!({
                    "center": self.rta_center, "span": self.rta_span, "points": 3328,
                    "rbw_mode": self.rta_rbw_mode, "rbw": self.rta_rbw,
                    "vbw_mode": self.rta_vbw_mode, "vbw": self.rta_vbw,
                    "ref_mode": "manual", "ref": self.rta_ref,
                    "sweep_time_mode": self.rta_sweep_time_mode,
                    "sweep_time": self.rta_sweep_time,
                    "trigger_source": self.trigger.source,
                    "trigger_edge": self.trigger.edge,
                    "trigger_level": self.trigger.level_dbm,
                    "trigger_safetime": self.trigger.safe_time,
                    "trigger_delay": self.trigger.delay,
                    "trigger_pretime": self.trigger.pre_time,
                    "trigger_acqtime": self.trigger.acq_time,
                    "trigger_retrigger": self.trigger.retrigger_count,
                    "trigger_retriggerperiod": self.trigger.retrigger_period,
                    "trigger_out": self.trigger.out,
                    "trigger_outpolarity": self.trigger.out_polarity,
                    "trigger_actual": self.rta_trigger_actual,
                }),
            ),
            ("/req/rta_center", json!(self.rta_center)),
            (
                "/device",
                if self.hardware {
                    json!(self.device_detail.as_ref().map(|d| format!(
                        "{} UID:{} HW:{} MCU {} FPGA {}",
                        d["model_name"].as_str().unwrap_or(""), d["uid"].as_str().unwrap_or(""),
                        d["hw"], d["mfw_str"].as_str().unwrap_or(""), d["ffw_str"].as_str().unwrap_or("")
                    )).unwrap_or_else(|| "Device information unavailable".into()))
                } else {
                    json!("N90 R0 (FAKE)")
                },
            ),
            (
                "/device_detail",
                if self.hardware {
                    self.device_detail.clone().unwrap_or(Value::Null)
                } else {
                    self.golden["device_detail"].clone()
                },
            ),
            (
                "/gnss",
                if self.hardware {
                    self.gnss_value.clone().unwrap_or(Value::Null)
                } else {
                    self.golden["gnss"].clone()
                },
            ),
            ("/mode", json!(mode)),
            ("/sweep_ms", json!(self.sweep_ms)),
            ("/sweep_ms_meas", json!(self.sweep_ms_meas)),
            ("/config_version", json!(self.config_version)),
            ("/ref_range_db", json!(p.ref_range_db)),
            (
                "/last_error",
                json!(self
                    .ddc_error
                    .clone()
                    .or_else(|| self.rta_error.clone())
                    .or_else(|| self.swp_error.clone())
                    .or_else(|| self.pnm_error.clone())
                    .unwrap_or_default()),
            ),
            ("/actual", actual.clone()),
            ("/rta_actual", self.rta_actual.clone()),
            ("/auto_ref", self.auto.view()),
            (
                "/amp",
                json!({
                    "atten": p.atten, "preamp": p.preamp, "ifgain": p.ifgain,
                    "gain_strategy": p.gain_strategy,
                    "atten_actual": actual.get("atten").cloned().unwrap_or(Value::Null),
                    "preamp_actual": actual.get("preamp").cloned().unwrap_or(Value::Null),
                    "ifgain_actual": actual.get("ifgain").cloned().unwrap_or(Value::Null),
                }),
            ),
            (
                "/req/swp",
                json!({
                    "center": p.center, "span": p.span, "points": p.points,
                    "rbw_mode": p.rbw_mode, "rbw": p.rbw, "vbw_mode": p.vbw_mode,
                    "vbw": p.vbw, "ref_mode": "manual", "ref": p.ref_level,
                    "sweep_time_mode": p.sweep_time_mode, "sweep_time": p.sweep_time,
                    "spur": p.spur, "detector": p.detector,
                    "frame_detector": p.frame_detector, "trace_align": p.trace_align,
                    "points_strategy": p.points_strategy, "window": p.window,
                }),
            ),
            ("/req/sdr/center", json!(self.sdr_center)),
            ("/req/sdr/decimate", json!(self.sdr_decimate)),
            ("/req/sdr/ref", json!(self.sdr_ref)),
            ("/req/sdr/ref_set", json!(self.sdr_ref_set)),
            ("/req/sdr/listen", json!(self.sdr_listen)),
            ("/req/sdr/demod", json!(self.sdr_demod)),
            ("/req/sdr/if_bw", json!(self.sdr_if_bw)),
            ("/req/sdr/squelch", json!(self.sdr_squelch)),
            ("/req/sdr/volume", json!(self.sdr_volume)),
            ("/req/sdr/agc", json!(self.sdr_agc)),
            ("/req/sdr/pitch", json!(self.sdr_pitch)),
            ("/req/sdr/deemph_us", json!(self.sdr_deemph)),
            ("/req/sdr/aux", self.sdr_aux.clone()),
            ("/sdr/center", json!(self.sdr_center)),
            ("/sdr/decimate", json!(self.sdr_decimate)),
            ("/sdr/ref", json!(self.sdr_ref)),
            ("/sdr/ref_set", json!(self.sdr_ref_set)),
            ("/sdr/deemph_us", json!(self.sdr_deemph)),
            ("/sdr/listen", json!(self.sdr_listen)),
            ("/sdr/demod", json!(self.sdr_demod)),
            ("/sdr/if_bw", json!(self.sdr_if_bw)),
            ("/sdr/squelch", json!(self.sdr_squelch)),
            ("/sdr/volume", json!(self.sdr_volume)),
            ("/sdr/agc", json!(self.sdr_agc)),
            ("/sdr/pitch", json!(self.sdr_pitch)),
            ("/sdr/aux", self.sdr_aux.clone()),
            ("/sdr/actual", self.sdr_actual.clone()),
            ("/sdr/level_dbfs", self.channels.as_array().and_then(|channels| channels.iter()
                .find(|channel| channel["channel"] == 1)).map_or(Value::Null,
                    |channel| channel["level_dbfs"].clone())),
            ("/sdr/channel_dbm", self.channels.as_array().and_then(|channels| channels.iter()
                .find(|channel| channel["channel"] == 1)).map_or(Value::Null,
                    |channel| channel["channel_dbm"].clone())),
            ("/sdr/squelch_open", self.channels.as_array().and_then(|channels| channels.iter()
                .find(|channel| channel["channel"] == 1)).map_or(Value::Null,
                    |channel| channel["squelch_open"].clone())),
        ] {
            set_path(&mut s, path, value);
        }
        s["has_docxo"] = json!(self.has_docxo);
        s["has_premium_ref_clock"] = json!(self.has_premium_ref_clock);
        s["channel_registry"] = registry.clone();
        s["sdr"]["channels"] = self.channels.clone();
        s["sdr"]["filter_limits"] = self.sdr_filter_limits.clone();
        s["sdr"]["receiver_error"] = json!(self.receiver_failure);
        for name in ["am_sync", "am_sync_generation"] {
            s["sdr"][name] = self.channels[0][name].clone();
            s["req"]["sdr"][name] = self.channels[0][name].clone();
        }
        s["sdr"]["actual"]["channel_registry"] = registry;
        s["req"]["sdr"]["pfb_taps_per_phase"] = json!(self.sdr_pfb_taps_per_phase);
        s["sdr"]["actual"]["pfb_taps_per_phase"] = json!(self.actual_taps);
        s["sdr"]["actual"]["state_lock_wait"] = json!(self.state_lock_wait);
        s["sdr"]["actual"]["state_lock_hold"] = json!(self.state_lock_hold);
        s
    }
}

pub(super) struct StateMutex {
    state: PLMutex<DevState>,
    status: PLMutex<(u64, Value)>,
    revision: Arc<AtomicU64>,
}
impl StateMutex {
    pub fn new(state: DevState) -> Self {
        let snapshot = state.status_snapshot();
        let status = (snapshot.revision, snapshot.build());
        let revision = Arc::new(AtomicU64::new(snapshot.revision));
        Self { state:PLMutex::new(state), status:PLMutex::new(status), revision }
    }
    pub fn status_with_revision(&self) -> (u64, Value) { self.status.lock().clone() }
    pub fn revision(&self) -> Arc<AtomicU64> { self.revision.clone() }
    pub fn current_status(&self, revision: u64, status: Value) -> (u64, Value) {
        let cached = self.status.lock();
        if revision < cached.0 { cached.clone() } else { (revision, status) }
    }
    pub fn publish_status(&self, revision: u64, status: Value) -> bool {
        let mut cached = self.status.lock();
        if revision < cached.0 { return false; }
        *cached = (revision, status);
        self.revision.store(revision, Ordering::Release);
        true
    }
    pub fn lock(&self) -> Result<StateGuard<'_>, std::convert::Infallible> {
        let started = Instant::now();
        let mut guard = self.state.lock();
        guard.state_lock_wait.observe(started.elapsed());
        Ok(StateGuard { guard, started:Instant::now() })
    }
}
pub(super) struct StateGuard<'a> {
    guard: parking_lot::MutexGuard<'a, DevState>,
    started: Instant,
}
impl Deref for StateGuard<'_> {
    type Target = DevState;
    fn deref(&self) -> &DevState { &self.guard }
}
impl DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut DevState { &mut self.guard }
}
impl Drop for StateGuard<'_> {
    fn drop(&mut self) { self.guard.state_lock_hold.observe(self.started.elapsed()); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_gain_uses_each_modes_actual_and_keeps_unknown_null() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.atten = 30;
        state.swp.preamp = 0;
        state.swp.ifgain = 0;
        state.actual = json!({"ref":-12.5,"atten":11,"preamp":1,"ifgain":2});
        state.rta_actual = json!({"ref":-22.5,"atten":17,"preamp":0,"ifgain":3});
        state.sdr_actual = json!({"ref":-32.5,"atten":23,"preamp":1,"ifgain":1});
        for (mode, reference, atten, preamp, ifgain) in [
            ("std", -12.5, 11, 1, 2), ("rta", -22.5, 17, 0, 3), ("sdr", -32.5, 23, 1, 1),
        ] {
            state.swp.mode = mode.into();
            let status = state.status_snapshot().build();
            assert_eq!(status["actual"]["ref"], reference);
            assert_eq!(status["amp"]["atten_actual"], atten);
            assert_eq!(status["amp"]["preamp_actual"], preamp);
            assert_eq!(status["amp"]["ifgain_actual"], ifgain);
        }
        state.actual = json!({});
        state.rta_actual = json!({});
        state.sdr_actual = json!({});
        for mode in ["std", "rta", "sdr"] {
            state.swp.mode = mode.into();
            let status = state.status_snapshot().build();
            for key in ["atten_actual", "preamp_actual", "ifgain_actual"] {
                assert!(status["amp"][key].is_null(), "{mode}: {key}");
            }
        }
    }

    #[test]
    fn requested_sdr_reference_follows_confirmed_manual_changes_and_preset() {
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();
        for reference in [-50.0,-70.0,-40.0,0.0] {
            dispatch(&mut state,"SET_REF",&json!({"ref":reference})).unwrap();
            let status=state.status_snapshot().build();
            assert_eq!(status["req"]["sdr"]["ref"],reference);
            assert_eq!(status["req"]["sdr"]["ref_set"],true);
            assert_eq!(status["sdr"]["ref"],reference);
        }
        dispatch(&mut state,"SET_PRESET",&json!({})).unwrap();
        let status=state.status_snapshot().build();
        assert_eq!(status["req"]["sdr"]["ref"],0.0);
        assert_eq!(status["req"]["sdr"]["ref_set"],false);
    }

    #[test]
    fn sdr_status_reports_resolved_clock_before_spectrum_publication() {
        let _guard = hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let _ = std::fs::remove_file("/tmp/websa_fake_iqs_fail.txt");
        let _ = std::fs::remove_file("/tmp/websa_fake_iqs_open_fail.txt");
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
        state.swp.mode = "sdr".into();
        state.hardware_shim = Some(hardware_iqs::tests::compile_fake_iqs_shim());
        let mut capture = HardwareCapture::start(state.hardware_shim.clone().unwrap(), 100e6,
            hardware_iqs::tests::FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        let mut ddc = Ddc::new(100e6, 100e6).unwrap();
        ddc.hardware = Some(capture);
        state.ddc = Some(SendDdc(ddc));
        state.sdr_actual = json!({"refclk":10e6,"refclk_src":1});
        let before = state.status_snapshot().build();
        assert_eq!(before["sdr"]["actual"]["refclk_src"], 1);
        assert_eq!(before["actual"]["ref"], -7.5);
        assert_eq!(before["amp"]["atten_actual"], 12);
        assert_eq!(before["amp"]["preamp_actual"], 1);
        assert_eq!(before["amp"]["ifgain_actual"], 2);
        state.ref_clock = "external".into();
        let mut requested = hardware_iqs::tests::FRONT_END;
        requested.ref_clock = 1;
        state.ddc.as_ref().unwrap().0.hardware.as_ref().unwrap()
            .reconfigure(100e6, requested, 1).unwrap();
        // No IQ packet or spectrum presentation updates sdr_actual between the
        // successful configure and the acknowledgement's status snapshot.
        let acknowledged = state.status_snapshot().build();
        assert_eq!(acknowledged["ref_clock"], "external");
        assert_eq!(acknowledged["sdr"]["actual"]["refclk_src"], 0);
        assert_eq!(acknowledged["sdr"]["actual"]["refclk"], 100e6);
        assert_eq!(acknowledged["actual"]["ref"], -27.5);
        assert_eq!(acknowledged["amp"]["atten_actual"], 18);
        assert_eq!(acknowledged["amp"]["preamp_actual"], 1);
        assert_eq!(acknowledged["amp"]["ifgain_actual"], 3);
    }

    #[test]
    fn status_clocks_never_outlive_their_registry() {
        let mut state = DevState::new(json!({"preset_defaults":{},"sdr":{"actual":{}},"actual":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_actual = json!({"center":100e6,
            "channel_registry":[{"channel":1,"epoch":1}],
            "channel_clocks":[{"channel":1,"channel_id":1,"epoch":1,"input_samples":10,"output_samples":5}]});
        let status = state.status_snapshot().build();
        assert_eq!(status["sdr"]["actual"]["channel_registry"], json!([]));
        assert_eq!(status["sdr"]["actual"]["channel_clocks"], json!([]));
        state.swp.mode = "std".into();
        let status = state.status_snapshot().build();
        assert_eq!(status["sdr"]["actual"]["channel_clocks"], json!([]));
    }
}
