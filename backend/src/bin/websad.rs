//! WebSA-compatible service with captured golden-fixture defaults for STATUS and
//! schema contract fields.
#[path = "../hardware_iqs.rs"]
mod hardware_iqs;
#[path = "../hardware_pnm.rs"]
mod hardware_pnm;
#[path = "../hardware_rta.rs"]
mod hardware_rta;
#[path = "../hardware_swp.rs"]
mod hardware_swp;
#[path = "websad/sdr_pipeline.rs"]
mod sdr_pipeline;
#[path = "websad/sdr_hardware.rs"]
mod sdr_hardware;
#[path = "websad/state_lock.rs"]
mod state_lock;
#[path = "websad/pipeline_bench.rs"]
mod pipeline_bench;
#[path = "websad/receiver_workers.rs"]
mod receiver_workers;
#[path = "websad/channel_controls.rs"]
mod channel_controls;
#[path = "websad/service_ddc.rs"]
mod service_ddc;
#[path = "websad/service_fixture.rs"]
mod service_fixture;
#[path = "websad/service_stream.rs"]
mod service_stream;
#[path = "websad/service_capture.rs"]
mod service_capture;
#[path = "websad/service_cost.rs"]
mod service_cost;
#[path = "websad/service_power.rs"]
mod service_power;
#[path = "websad/subscriber_stream.rs"]
mod subscriber_stream;
#[path = "websad/service_preflight.rs"]
mod service_preflight;
#[path = "websad/server_mode.rs"]
mod server_mode;
use state_lock::StateMutex;
use sdr_pipeline::{Pipeline, PipelineConfig};
use websa_channelizer::timing::Timing;
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use hardware_iqs::{HardwareCapture, CONSUMER_BUDGET};
use hardware_pnm::{HardwarePnm, PnmParams};
use hardware_rta::{HardwareRta, RtaFrame, RtaParams, RtaTrigger};
use hardware_swp::{
    ref_clock_code, FrontEnd, HardwareSwp, SdkFailure, SwpDefaults, SwpParams, LINK_LOST_STATUS,
    WARN_STATUS,
};
use parking_lot::Mutex as PLMutex;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify};
use websa_channelizer::{coarse_passband_hz, encode_rta, BasebandFramer, FineBank, PfbStream};

const STATUS_PERIOD: Duration = Duration::from_secs(1);

static DSP_EPOCH: AtomicU64 = AtomicU64::new(0);

fn allocate_dsp_epoch() -> Result<u32, &'static str> {
    DSP_EPOCH.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |epoch| {
        (epoch < u32::MAX as u64).then_some(epoch + 1)
    }).map(|previous| (previous + 1) as u32).map_err(|_| "DSP epoch exhausted")
}

/// One queued payload tagged with the generation that produced it. Decode receiver
/// output occupies one FIFO slot, with shared bytes and a per-client byte reservation.
#[derive(Debug)]
struct Outbound {
    epoch: u64,
    status_revision: Option<u64>,
    display_version: Option<u64>,
    identity: Option<u64>,
    message: OutboundPayload,
}

#[derive(Debug)]
enum OutboundPayload {
    Single(Message),
    DecodeBatch {
        frames: Arc<[(Message, Option<u64>)]>,
        // Retiring identities have already passed the channel filter under state.
        retiring: Option<Vec<u64>>,
        _reservation: DecodeReservation,
    },
}

impl From<Message> for OutboundPayload {
    fn from(message: Message) -> Self { Self::Single(message) }
}

impl OutboundPayload {
    fn frame(&self, index: usize) -> Option<(Message, Option<u64>)> {
        match self {
            Self::Single(message) => (index == 0).then(|| (message.clone(), None)),
            Self::DecodeBatch { frames, retiring, .. } => frames.get(index).map(|(message, identity)| {
                let identity = if retiring.as_ref().is_some_and(|ids| identity.is_some_and(|id| ids.contains(&id))) {
                    None
                } else { *identity };
                (message.clone(), identity)
            }),
        }
    }
}

// Three retained 20 MiB channels, base64 encoded, plus envelope overhead. The
// reservation includes the packet being applied, so ACK stalls cannot accumulate
// 64 complete receiver revisions in memory. Frame count never consumes FIFO slots.
const DECODE_PENDING_BYTE_LIMIT: u64 = 3 * (20 * 1024 * 1024u64).div_ceil(3) * 4 + 4 * 1024 * 1024;

#[derive(Debug)]
struct DecodeReservation { pending: Arc<AtomicU64>, bytes: u64 }
impl DecodeReservation {
    fn acquire(pending: &Arc<AtomicU64>, bytes: u64) -> Option<Self> {
        pending.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used|
            used.checked_add(bytes).filter(|&total| total <= DECODE_PENDING_BYTE_LIMIT)).ok()?;
        Some(Self { pending: pending.clone(), bytes })
    }
}
impl Drop for DecodeReservation {
    fn drop(&mut self) { self.pending.fetch_sub(self.bytes, Ordering::AcqRel); }
}

struct Client {
    id: u64,
    tx: mpsc::Sender<Outbound>,
    decode_pending_bytes: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    overflow: Arc<AtomicBool>,
    receiver_failed: Arc<AtomicBool>,
    wake: Arc<Notify>,
    /// Shared with `App.epoch`; fencing is done at enqueue and again at send.
    epoch: Arc<AtomicU64>,
    status_revision: Arc<AtomicU64>,
    display_version: Arc<AtomicU64>,
    iq: bool,
    aux: bool,
    audio: bool,
    decode: bool,
    channel: Option<u32>,
    identities: Arc<[AtomicU64;3]>,
    /// Latest-wins slots for display frames (Python ClientStream FRAME_POLICY: POWR/RTAF
    /// newest wins, FREQ is retained). A slow display client drops stale traces instead of
    /// building a backlog that delays its control traffic.
    latest: Arc<PLMutex<(Option<Outbound>, Option<Outbound>)>>,
}

#[derive(Clone, Default)]
struct AutoRef {
    last_peak: Option<f64>,
    last_peak_hz: Option<f64>,
    last_floor: Option<f64>,
    target: Option<f64>,
    result: String,
    seq: u64,
    observation: u64,
    observation_config: u64,
    observation_status: i32,
    spectrum_observation: u64,
    iqs_packets: u64,
    session: Option<AutoSession>,
}

impl AutoRef {
    fn view(&self) -> Value {
        json!({
            "last_peak": self.last_peak, "last_noise_floor": self.last_floor,
            "target": self.target, "result": self.result, "seq": self.seq,
            "pending": self.session.as_ref().and_then(|s| s.pending.or_else(|| s.fitted.then_some(s.display_ref).flatten())),
            "adjusting": self.session.is_some(),
            "hardware_adjusting": self.session.as_ref().is_some_and(|s| s.recovering && s.pending.is_some()),
        })
    }
}

#[derive(Clone)]
struct AutoSession {
    mode: String,
    config: u64,
    seen: u64,
    deadline: std::time::Instant,
    remaining: u8,
    display_only: bool,
    display_ref: Option<f64>,
    pending: Option<f64>,
    previous_ref: Option<f64>,
    recovering: bool,
    fitted: bool,
    packet_baseline: u64,
}

struct SyntheticIq {
    mode: String,
    registry: Arc<[sdr_pipeline::ChannelRegistryEntry]>,
    seq: u32,
    input_index: u64,
}

struct DevState {
    golden: Value,
    schema: Value,
    rng: u64,
    freq_version: u64,
    config_version: u64,
    sweep_ms: f64,
    sweep_ms_meas: f64,
    sweep_timing_version: Option<u64>,
    actual: Value,
    last_freq: Option<Vec<u8>>,
    auto: AutoRef,
    swp: Swp,
    // RTA session (Python RtaParams + TriggerParams)
    rta_center: f64,
    rta_span: f64,
    rta_decimate: u32,
    rta_ref: f64,
    rta_rbw_mode: String,
    rta_rbw: f64,
    rta_vbw_mode: String,
    rta_vbw: f64,
    rta_sweep_time_mode: i64,
    rta_sweep_time: f64,
    trigger: RtaTrigger,
    rta_actual: Value,
    rta_trigger_actual: Value,
    rta_trigger_generation: u64,
    rta_frames: u64,
    rta_completed_frame: Option<Vec<u8>>,
    // Shared front end (Python DeviceState common fields)
    ref_clock: String,
    refclk_out: bool,
    refclk_ppm: f64,
    status_warning: i32,
    /// Device link state (Python `connected`): false only when an open fails or the
    /// handle reports a bus error. A reconfiguration does not change it.
    link_up: bool,
    /// Device power-on SWP profile, read once from the first open handle.
    device_defaults: Option<SwpDefaults>,
    device_detail: Option<Value>,
    swp_fail_streak: u32,
    // SDR session
    sdr_center: f64,
    sdr_decimate: i64,
    sdr_pfb_taps_per_phase: usize,
    sdr_listen: f64,
    sdr_demod: String,
    sdr_if_bw: f64,
    sdr_squelch: f64,
    sdr_volume: f64,
    sdr_agc: bool,
    sdr_pitch: f64,
    sdr_deemph: f64,
    sdr_ref: f64,
    sdr_ref_set: bool,
    sdr_aux: Value,
    sdr_channels: Vec<receiver_workers::ChannelConfig>,
    receiver_workers: Option<receiver_workers::ReceiverWorkers>,
    receiver_failure: Option<String>,
    receiver_failure_pending: bool,
    fixture_sources: Option<service_fixture::FixtureSources>,
    channel_power: [Option<(std::time::Instant,f64)>;3],
    sdr_actual: Value,
    harm_f0: f64,
    harm_count: i64,
    harm_span: f64,
    harm_session_id: Option<String>,
    harm_active_session_id: Option<String>,
    harm_scan: bool,
    harm_scan_cnt: u64,
    harm_seq: Vec<Value>,
    harm_n: i64,
    harm_base: Option<f64>,
    pnm_center: f64,
    pnm_threshold: f64,
    pnm_traceavg: i64,
    pnm_start: f64,
    pnm_stop: f64,
    pnm_session_id: Option<String>,
    pnm_active_session_id: Option<String>,
    pnm_active_traceavg: i64,
    pnm_i: u64,
    /// A parameter change waits for the cycle boundary instead of abandoning the
    /// running cycle (Python `set_params` dirty flag).
    pnm_dirty: bool,
    harm_window: Option<(f64, f64)>,
    /// SWP window saved when a measurement mode takes over the same hardware.
    swp_restore: Option<(f64, f64)>,
    pnm_last: Value,
    ddc: Option<SendDdc>,
    pipeline: Option<Pipeline>,
    spectrum_timing: Timing,
    state_lock_wait: Timing,
    state_lock_hold: Timing,
    pending_iq: Vec<Vec<u8>>,
    pending_receiver: receiver_workers::ReceiverFrames,
    retiring_registry: Arc<[sdr_pipeline::ChannelRegistryEntry]>,
    decode_snapshots: service_stream::DecodeSnapshots,
    consumed_receiver_iq: std::collections::BTreeSet<(u32,u32,u32)>,
    ddc_error: Option<String>,
    ddc_error_count: u64,
    ddc_error_last: Option<String>,
    ddc_failure_diagnostics: Option<Value>,
    ddc_retry_at: Option<std::time::Instant>,
    hardware_shim: Option<String>,
    server: PLMutex<server_mode::SoapyServer>,
    gnss_value: Option<Value>,
    swp_hw: Option<HardwareSwp>,
    swp_error: Option<String>,
    swp_retry_at: Option<std::time::Instant>,
    swp_open_count: u64,
    pnm_hw: Option<HardwarePnm>,
    pnm_error: Option<String>,
    pnm_retry_at: Option<std::time::Instant>,
    rta_hw: Option<HardwareRta>,
    rta_error: Option<String>,
    rta_retry_at: Option<std::time::Instant>,
    tick: u64,
    audio_pos: usize,
    audio_seq: u32,
    synthetic_iq: Option<SyntheticIq>,
}

#[derive(Clone)]
struct Swp {
    mode: String,
    center: f64,
    span: f64,
    ref_level: f64,
    rbw_mode: String,
    rbw: f64,
    vbw_mode: String,
    vbw: f64,
    points: i64,
    window: i64,
    spur: String,
    detector: String,
    frame_detector: String,
    trace_align: String,
    points_strategy: String,
    sweep_time_mode: i64,
    sweep_time: f64,
    ref_range_db: f64,
    atten: i64,
    preamp: i64,
    ifgain: i64,
    gain_strategy: i64,
}

/// Only requested settings participate in rollback; hardware owners and DSP stay put.
struct RequestedConfiguration {
    swp: Swp,
    rta: RtaParams,
    rta_span: f64,
    sdr_ref: f64,
    sdr_ref_set: bool,
    ref_clock: String,
    refclk_out: bool,
}

impl RequestedConfiguration {
    fn capture(state: &DevState) -> Self {
        Self { swp: state.swp.clone(), rta: state.rta_params(), rta_span: state.rta_span,
            sdr_ref: state.sdr_ref, sdr_ref_set: state.sdr_ref_set,
            ref_clock: state.ref_clock.clone(), refclk_out: state.refclk_out }
    }

    fn restore(self, state: &mut DevState) {
        state.swp = self.swp;
        state.rta_center = self.rta.center;
        state.rta_span = self.rta_span;
        state.rta_decimate = self.rta.decimate;
        state.rta_ref = self.rta.front_end.ref_dbm;
        state.rta_rbw_mode = if self.rta.rbw_manual { "manual" } else { "auto" }.into();
        state.rta_rbw = self.rta.rbw;
        state.rta_vbw_mode = self.rta.vbw_mode;
        state.rta_vbw = self.rta.vbw;
        state.rta_sweep_time_mode = self.rta.sweep_time_mode;
        state.rta_sweep_time = self.rta.sweep_time;
        state.trigger = self.rta.trigger;
        state.sdr_ref = self.sdr_ref;
        state.sdr_ref_set = self.sdr_ref_set;
        state.ref_clock = self.ref_clock;
        state.refclk_out = self.refclk_out;
    }
}

impl DevState {
    fn new(golden: Value, schema: Value) -> Self {
        let preset = golden["preset_defaults"].clone();
        let f = |k: &str, d: f64| preset[k].as_f64().unwrap_or(d);
        let s = |k: &str, d: &str| preset[k].as_str().unwrap_or(d).to_string();
        let swp = Swp {
            mode: "std".into(),
            center: f("center", 1e9),
            span: f("span", 1e8),
            ref_level: f("ref", 0.0),
            rbw_mode: s("rbw_mode", "manual"),
            rbw: f("rbw", 1e5),
            vbw_mode: s("vbw_mode", "manual"),
            vbw: f("vbw", 1e5),
            points: preset["points"].as_i64().unwrap_or(1000),
            window: preset["window"].as_i64().unwrap_or(1),
            spur: s("spur", "bypass"),
            detector: s("detector", "auto"),
            frame_detector: "device_default".into(),
            trace_align: "start".into(),
            points_strategy: "speed".into(),
            sweep_time_mode: preset["sweep_time_mode"].as_i64().unwrap_or(0),
            sweep_time: 0.0,
            ref_range_db: 100.0,
            atten: preset["atten"].as_i64().unwrap_or(-1),
            preamp: preset["preamp"].as_i64().unwrap_or(0),
            ifgain: preset["ifgain"].as_i64().unwrap_or(2),
            gain_strategy: preset["gain_strategy"].as_i64().unwrap_or(0),
        };
        Self {
            golden,
            schema,
            rng: 20260914,
            freq_version: 0,
            config_version: 0,
            sweep_ms: 0.0,
            sweep_ms_meas: 0.0,
            sweep_timing_version: None,
            actual: json!({}),
            last_freq: None,
            auto: AutoRef {
                result: "idle".into(),
                ..Default::default()
            },
            swp,
            rta_center: 1e9,
            rta_span: 50_781_250.0,
            rta_decimate: 1,
            rta_ref: 0.0,
            rta_rbw_mode: "auto".into(),
            rta_rbw: 0.0,
            rta_vbw_mode: "equal".into(),
            rta_vbw: 0.0,
            rta_sweep_time_mode: 2,
            rta_sweep_time: 0.0,
            trigger: RtaTrigger::default(),
            rta_actual: json!({}),
            rta_trigger_actual: Self::unknown_rta_trigger(true),
            rta_trigger_generation: 0,
            rta_frames: 0,
            rta_completed_frame: None,
            ref_clock: "internal".into(),
            refclk_out: false,
            refclk_ppm: 0.0,
            status_warning: 0,
            link_up: true,
            device_defaults: None,
            device_detail: None,
            swp_fail_streak: 0,
            sdr_center: 1e9,
            sdr_decimate: 16,
            sdr_pfb_taps_per_phase: 16,
            sdr_listen: 1e9,
            sdr_demod: "am".into(),
            sdr_if_bw: 6000.0,
            sdr_squelch: -110.0,
            sdr_volume: 0.8,
            sdr_agc: true,
            sdr_pitch: 700.0,
            sdr_deemph: -1.0,
            sdr_ref: 0.0,
            sdr_ref_set: false,
            sdr_aux: json!({}),
            sdr_channels: (1..=3).map(receiver_workers::ChannelConfig::for_slot).collect(),
            receiver_workers: None,
            receiver_failure: None,
            receiver_failure_pending: false,
            fixture_sources: None,
            channel_power: [None;3],
            sdr_actual: json!({}),
            harm_f0: 1e9,
            harm_count: 5,
            harm_span: 10e6,
            harm_session_id: None,
            harm_active_session_id: None,
            harm_scan: true,
            harm_scan_cnt: 0,
            harm_seq: Vec::new(),
            harm_n: 1,
            harm_base: None,
            pnm_center: 1e9,
            pnm_threshold: -50.0,
            pnm_traceavg: 4,
            pnm_start: 100.0,
            pnm_stop: 10e6,
            pnm_session_id: None,
            pnm_active_session_id: None,
            pnm_active_traceavg: 4,
            pnm_i: 0,
            pnm_dirty: false,
            pnm_last: Value::Null,
            harm_window: None,
            swp_restore: None,
            ddc: None,
            pipeline: None,
            spectrum_timing: Timing::default(),
            state_lock_wait: Timing::default(),
            state_lock_hold: Timing::default(),
            pending_iq: Vec::new(),
            pending_receiver: receiver_workers::ReceiverFrames::default(),
            retiring_registry: Arc::from([]),
            decode_snapshots: service_stream::DecodeSnapshots::default(),
            consumed_receiver_iq: std::collections::BTreeSet::new(),
            ddc_error: None,
            ddc_error_count: 0,
            ddc_error_last: None,
            ddc_failure_diagnostics: None,
            ddc_retry_at: None,
            hardware_shim: None,
            server: PLMutex::new(server_mode::SoapyServer::default()),
            gnss_value: None,
            swp_hw: None,
            swp_error: None,
            swp_retry_at: None,
            swp_open_count: 0,
            pnm_hw: None,
            pnm_error: None,
            pnm_retry_at: None,
            rta_hw: None,
            rta_error: None,
            rta_retry_at: None,
            tick: 0,
            audio_pos: 0,
            audio_seq: 0,
            synthetic_iq: None,
        }
    }

    fn caps(&self) -> &Value {
        &self.golden["caps"]
    }

    /// Auto-reopen gate: Python's link loop retries the open until the analyzer answers;
    /// a bounded cooldown keeps the retry visible through `last_error` without hammering.
    /// One GNSS snapshot from whichever hardware owner currently holds the device.
    /// A missing owner or failed query retains the last snapshot as stale until
    /// the new owner supplies fresh telemetry. Server mode ends that session.
    fn refresh_gnss(&mut self) {
        if self.swp.mode == "server" {
            self.gnss_value = None;
            return;
        }
        // SDR uses the explicit cached-only shim ABI on the SDK owner thread.
        // It never pauses fine processing or consumes an IQ packet for STATUS.
        let info = match self.swp.mode.as_str() {
            "rta" => self.rta_hw.as_ref().map(|h| h.gnss()),
            "pnm" => self.pnm_hw.as_ref().map(|h| h.gnss()),
            "sdr" => self.pipeline.as_ref().map(|pipeline| pipeline.gnss()).or_else(|| self
                .ddc.as_ref().and_then(|ddc| ddc.0.hardware.as_ref())
                .map(|capture| capture.gnss())),
            _ => self.swp_hw.as_ref().map(|h| h.gnss()),
        };
        if let Some(Ok(g)) = info {
            self.gnss_value = Some(json!({
                "lock": g.lock, "sats": g.sats, "docxo": g.docxo,
                "docxo_mode": g.docxo_mode, "antenna": g.antenna,
                "latitude": g.latitude, "longitude": g.longitude, "altitude": g.altitude,
                "year": g.year, "month": g.month, "day": g.day,
                "hour": g.hour, "minute": g.minute, "second": g.second,
                "time": format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                                g.year, g.month, g.day, g.hour, g.minute, g.second),
                "stale": false,
            }));
        } else if let Some(gnss) = self.gnss_value.as_mut() {
            gnss["stale"] = json!(true);
        }
    }

    fn retry_due(at: &Option<std::time::Instant>) -> bool {
        at.map_or(true, |t| std::time::Instant::now() >= t)
    }

    /// Front-end fields of the active mode (each mode keeps its own Ref, Python REF_FIELD).
    fn front_end(&self, ref_dbm: f64) -> FrontEnd {
        FrontEnd {
            ref_dbm,
            atten: self.swp.atten as i32,
            preamp: self.swp.preamp as i32,
            ifgain: self.swp.ifgain as i32,
            gain_strategy: self.swp.gain_strategy as i32,
            ref_clock: ref_clock_code(&self.ref_clock),
            refclk_out: i32::from(self.refclk_out),
        }
    }

    fn rta_params(&self) -> RtaParams {
        RtaParams {
            center: self.rta_center,
            decimate: self.rta_decimate,
            front_end: self.front_end(self.rta_ref),
            rbw_manual: self.rta_rbw_mode == "manual",
            rbw: self.rta_rbw,
            vbw_mode: self.rta_vbw_mode.clone(),
            vbw: self.rta_vbw,
            sweep_time_mode: self.rta_sweep_time_mode,
            sweep_time: self.rta_sweep_time,
            trigger: self.trigger.clone(),
        }
    }

    /// Re-applies the SWP request on the open handle (Python configure_swp). The handle
    /// closes only on a mode switch or a bus error.
    fn configure_swp(&mut self) -> Result<(), String> {
        if self.hardware_shim.is_none() {
            self.actual = json!({});
        } else {
            let params = self.swp_params();
            let swp = self.swp_hw.as_mut().ok_or("SWP hardware is not ready")?;
            match swp.reconfigure(params) {
                Ok(geometry) => {
                    self.swp_error = None;
                    self.apply_swp_geometry(geometry);
                }
                Err(failure) => {
                    let message = failure.message.clone();
                    // Parameter rejection keeps the restored profile and its owner.
                    // Only link loss enters the normal automatic reopen path.
                    if LINK_LOST_STATUS.contains(&failure.status) {
                        self.swp_failed(failure);
                    } else {
                        self.swp_error = Some(message.clone());
                    }
                    return Err(message);
                }
            }
        }
        self.invalidate_observation();
        self.config_version += 1;
        self.freq_version += 1;
        Ok(())
    }

    /// Re-applies the RTA request on the open handle (Python rta.py _configure_locked).
    fn configure_rta(&mut self) -> Result<(), String> {
        let params = self.rta_params();
        let Some(rta) = self.rta_hw.as_mut() else {
            if self.hardware_shim.is_some() { return Err("RTA hardware is not ready".into()); }
            self.invalidate_observation();
            self.config_version += 1;
            self.freq_version += 1;
            return Ok(());
        };
        match rta.reconfigure(params) {
            Ok(geometry) => {
                self.rta_error = None;
                self.invalidate_observation();
                self.config_version += 1;
                self.freq_version += 1;
                self.apply_rta_geometry(geometry);
            }
            Err(failure) => {
                if LINK_LOST_STATUS.contains(&failure.status) {
                    self.link_up = false;
                }
                self.rta_error = Some(failure.message.clone());
                let restored = rta.trigger_actual();
                if restored.as_ref().map_or(true, |a| a.valid == 0) {
                    self.clear_rta_trigger(false);
                } else if let Ok(actual) = restored {
                    self.rta_trigger_generation += 1;
                    self.apply_rta_trigger_actual(actual, false);
                }
                if LINK_LOST_STATUS.contains(&failure.status) {
                    self.rta_hw = None;
                    self.rta_retry_at = Some(std::time::Instant::now() + Duration::from_secs(1));
                    self.clear_rta_trigger(false);
                }
                return Err(failure.message);
            }
        }
        Ok(())
    }
    fn unknown_rta_trigger(simulated: bool) -> Value {
        json!({"source":null,"edge":null,"level":null,"safetime":null,"delay":null,
            "pretime":null,"acqtime":null,"retrigger":null,"retriggerperiod":null,
            "out":null,"outpolarity":null,"poi":null,"frames":0,"generation":0,
            "capture_id":null,"packet_index":null,"packet_count":null,"waiting":null,
            "complete":null,"last_sdk_status":null,"first_ts":null,"edges":null,
            "simulated":simulated,"supported":false,"valid":false})
    }
    fn clear_rta_trigger(&mut self, simulated: bool) {
        self.rta_completed_frame = None;
        self.rta_trigger_actual = Self::unknown_rta_trigger(simulated);
        self.rta_trigger_actual["frames"] = json!(self.rta_frames);
        self.rta_trigger_actual["generation"] = json!(self.rta_trigger_generation);
    }


    fn apply_rta_trigger_actual(&mut self, a: hardware_rta::RtaTriggerActual, has_frame: bool) {
        if !has_frame { self.rta_completed_frame = None; }
        if a.valid == 0 {
            self.clear_rta_trigger(false);
            return;
        }
        let source = match a.source {0=>Some("freerun"),1=>Some("external"),2=>Some("bus"),3=>Some("level"),4=>Some("timer"),_=>None};
        let edge = match a.edge {0=>Some("rising"),1=>Some("falling"),2=>Some("double"),_=>None};
        let out = match a.out {0=>Some("none"),1=>Some("per_hop"),2=>Some("per_sweep"),3=>Some("per_profile"),_=>None};
        let polarity = match a.polarity {0=>Some("positive"),1=>Some("negative"),_=>None};
        self.rta_trigger_actual = json!({"source":source,"edge":edge,"level":a.level,
            "safetime":a.safetime,"delay":a.delay,"pretime":a.pretime,"acqtime":a.acqtime,
            "retrigger":a.retrigger,"retriggerperiod":a.retriggerperiod,"out":out,"outpolarity":polarity,
            "poi":a.poi,"frames":self.rta_frames,"generation":self.rta_trigger_generation,
            "capture_id":null,"packet_index":if a.packet_index>0 {Some(a.packet_index)} else {None},
            "packet_count":if a.packet_count>0 {Some(a.packet_count)} else {None},
            "waiting":if has_frame {Some(false)} else {None},
            "complete":match a.complete {0=>Some(false),1=>Some(true),_=>None},
            "trigger_missed":a.last_sdk_status == -302 || a.last_sdk_status == -303,
            "last_sdk_status":a.last_sdk_status,"first_ts":if has_frame {Some(a.first_data_timestamp)} else {None},
            "edges":if has_frame {Some(a.trigger_edges)} else {None},"simulated":false,"supported":true,"valid":true});
    }

    fn configure_trigger(&mut self, candidate: RtaTrigger) -> Result<(), String> {
        if self.swp.mode == "rta" && self.hardware_shim.is_some() {
            let mut params = self.rta_params();
            params.trigger = candidate.clone();
            let rta = self.rta_hw.as_mut().ok_or("RTA hardware is not ready")?;
            match rta.reconfigure(params) {
                Ok(geometry) => {
                    self.trigger = candidate;
                    self.rta_error = None;
                    self.invalidate_observation();
                    self.config_version += 1;
                    self.freq_version += 1;
                    self.apply_rta_geometry(geometry);
                }
                Err(failure) => {
                    self.rta_error = Some(failure.message.clone());
                    let actual = rta.trigger_actual();
                    // A failed rollback or lost link makes the last applied snapshot unavailable.
                    if LINK_LOST_STATUS.contains(&failure.status) || actual.as_ref().map_or(true, |a| a.valid == 0) {
                        self.clear_rta_trigger(false);
                    }
                    else if let Ok(actual) = actual {
                        self.rta_trigger_generation += 1;
                        self.apply_rta_trigger_actual(actual, false);
                    }
                    return Err(failure.message);
                }
            }
        } else {
            self.trigger = candidate;
            self.config_version += 1;
            if self.swp.mode == "rta" {
                self.rta_trigger_generation += 1;
                self.clear_rta_trigger(true);
            }
        }
        Ok(())
    }

    fn attach_rta_trigger(&self, mut frame: Vec<u8>) -> Vec<u8> {
        if frame.is_empty() { return frame; }
        frame[..4].copy_from_slice(b"RTA2");
        let metadata = self.rta_trigger_actual.to_string();
        frame.extend_from_slice(b"TRIG");
        frame.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        frame.extend_from_slice(metadata.as_bytes());
        frame
    }


    /// Re-applies the IQS front end/center on the open capture (Python sdr.py _configure).
    fn configure_sdr_capture(&mut self) -> Result<(), String> {
        let candidate=self.parse_service_capture(&json!({}))?;
        self.apply_service_capture_candidate(candidate,self.front_end(self.sdr_ref),true)
    }

    /// Re-applies the active mode's device profile (Python configure_active).
    fn configure_active(&mut self) -> Result<(), String> {
        match self.swp.mode.as_str() {
            "server" => return Err("hardware configuration is unavailable in server mode".into()),
            "rta" => self.configure_rta()?,
            "sdr" => self.configure_sdr_capture()?,
            "pnm" => {
                self.pnm_hw = None;
                self.pnm_error = None;
                self.pnm_retry_at = None;
            }
            _ => self.configure_swp()?,
        }
        Ok(())
    }

    fn apply_swp_geometry(&mut self, g: hardware_swp::SwpGeometry) {
        self.sweep_ms = g.sweep_ms;
        self.actual = json!({
            "center": (g.start + g.stop) / 2.0,
            "span": g.stop - g.start,
            "start": g.start, "stop": g.stop,
            "ref": g.ref_dbm,
            "atten": g.front_end.map(|f| f.atten),
            "preamp": g.front_end.map(|f| f.preamp),
            "ifgain": g.front_end.map(|f| f.ifgain),
            "rbw": g.rbw, "vbw": g.vbw, "points": g.points,
            "est_min": g.sweep_ms / 1000.0, "refclk": g.refclk, "refclk_src": g.refclk_src,
        });
    }

    fn apply_rta_geometry(&mut self, g: hardware_rta::RtaGeometry) {
        self.rta_span = g.stop - g.start;
        self.rta_trigger_generation += 1;
        self.apply_rta_trigger_actual(g.trigger, false);
        self.rta_actual = json!({
            "center": self.rta_center,
            "span": g.stop - g.start,
            "start": g.start,
            "stop": g.stop,
            "ref": g.ref_dbm, "rbw": g.rbw, "vbw": g.vbw,
            "points": g.points, "frame_points": g.points,
            "refclk": g.refclk, "refclk_src": g.refclk_src, "refclk_out": self.refclk_out,
            "atten": g.atten, "preamp": g.preamp, "ifgain": g.ifgain,
            "pan_available": true,
        });
    }

    /// SWP fetch/configure failure handling (Python note_link_status + fetch_sweep):
    /// warnings keep the handle; bus errors mark the link lost; 8 other errors in a row reopen.
    fn swp_failed(&mut self, failure: SdkFailure) {
        if WARN_STATUS.contains(&failure.status) {
            self.status_warning = failure.status;
            if failure.status == -12 {
                self.swp_error = Some(
                    "IF overflow (-12): raise the reference level until this warning clears".into(),
                );
            }
            return;
        }
        self.swp_fail_streak += 1;
        let lost = LINK_LOST_STATUS.contains(&failure.status);
        if lost || self.swp_fail_streak >= 8 {
            if lost {
                self.link_up = false;
            }
            self.swp_hw = None;
            self.swp_retry_at = Some(std::time::Instant::now() + Duration::from_secs(1));
            self.swp_fail_streak = 0;
        }
        self.swp_error = Some(failure.message);
    }

    /// Reads the device power-on SWP profile once through whichever handle is open.
    fn learn_defaults(&mut self) {
        let info = match self.swp.mode.as_str() {
            "rta" => self.rta_hw.as_ref().and_then(|h| h.device_info),
            "sdr" => self.ddc.as_ref().and_then(|d| d.0.hardware.as_ref()).and_then(|h| h.device_info),
            "pnm" => self.pnm_hw.as_ref().and_then(|h| h.device_info),
            _ => self.swp_hw.as_ref().and_then(|h| h.device_info),
        };
        self.device_detail = info.map(|info| info.detail());
        if self.device_defaults.is_some() {
            return;
        }
        let read = match self.swp.mode.as_str() {
            "rta" => self.rta_hw.as_ref().map(|h| h.defaults()),
            "sdr" => self
                .ddc
                .as_ref()
                .and_then(|ddc| ddc.0.hardware.as_ref())
                .map(|capture| capture.defaults()),
            "pnm" => None,
            _ => self.swp_hw.as_ref().map(|h| h.defaults()),
        };
        if let Some(Ok(defaults)) = read {
            self.device_defaults = Some(defaults);
        }
    }

    /// SWP preset values: device defaults when known, else the golden contract values.
    fn preset_swp(&self) -> Swp {
        let golden = &self.golden["preset_defaults"];
        let (fmin, fmax) = self.freq_limits();
        let mut swp = self.swp.clone();
        let (center, span) = match &self.device_defaults {
            Some(d) => {
                swp.ref_level = d.ref_dbm;
                swp.rbw = d.rbw;
                swp.vbw = d.vbw;
                swp.points = d.points;
                swp.atten = d.atten;
                swp.window = d.window;
                swp.rbw_mode = d.rbw_mode.clone();
                swp.vbw_mode = d.vbw_mode.clone();
                swp.spur = d.spur.clone();
                swp.trace_align = d.trace_align.clone();
                swp.points_strategy = d.points_strategy.clone();
                swp.preamp = d.preamp;
                swp.ifgain = d.ifgain;
                swp.gain_strategy = d.gain_strategy;
                swp.sweep_time_mode = d.sweep_time_mode;
                swp.sweep_time = d.sweep_time;
                (d.center, d.span)
            }
            None => {
                let f = |k: &str, d: f64| golden[k].as_f64().unwrap_or(d);
                let s = |k: &str, d: &str| golden[k].as_str().unwrap_or(d).to_string();
                swp.ref_level = f("ref", 0.0);
                swp.rbw = f("rbw", 1e5);
                swp.vbw = f("vbw", 1e5);
                swp.points = golden["points"].as_i64().unwrap_or(1000);
                swp.atten = golden["atten"].as_i64().unwrap_or(-1);
                swp.window = golden["window"].as_i64().unwrap_or(1);
                swp.rbw_mode = s("rbw_mode", "auto");
                swp.vbw_mode = s("vbw_mode", "bypass");
                swp.spur = s("spur", "bypass");
                swp.trace_align = s("trace_align", "start");
                swp.points_strategy = s("points_strategy", "speed");
                swp.preamp = golden["preamp"].as_i64().unwrap_or(0);
                swp.ifgain = golden["ifgain"].as_i64().unwrap_or(2);
                swp.gain_strategy = golden["gain_strategy"].as_i64().unwrap_or(0);
                swp.sweep_time_mode = golden["sweep_time_mode"].as_i64().unwrap_or(0);
                swp.sweep_time = f("sweep_time", 0.0);
                (f("center", 1e9), f("span", 1e8))
            }
        };
        swp.detector = "auto".into();
        swp.frame_detector = "device_default".into();
        // Python preset_state: a default span that covers the device range is full span.
        if span >= fmax - fmin {
            swp.center = (fmin + fmax) / 2.0;
            swp.span = fmax - fmin;
        } else {
            (swp.center, swp.span) = fit_center_span(center, span, fmin, fmax);
        }
        swp
    }

    /// Python _h_set_preset: reset every parameter group, keep the mode, the device and the
    /// open handles, then re-apply the active mode once.
    fn preset(&mut self) -> Result<(), String> {
        let swp = self.preset_swp();
        if self.swp.mode == "sdr" {
            let candidate = self.preset_service_capture()?;
            let front_end = FrontEnd { ref_dbm:0.0, atten:swp.atten as i32,
                preamp:swp.preamp as i32, ifgain:swp.ifgain as i32,
                gain_strategy:swp.gain_strategy as i32, ref_clock:0, refclk_out:0 };
            // Retire the old service and configure the entire candidate before
            // publishing any preset settings. Admission uses this same candidate.
            self.apply_service_capture_candidate(candidate,front_end,true)?;
        }
        self.swp = swp;
        self.rta_center = 1e9;
        self.rta_span = 50_781_250.0;
        self.rta_decimate = 1;
        self.rta_ref = 0.0;
        self.rta_rbw_mode = "auto".into();
        self.rta_rbw = 0.0;
        self.rta_vbw_mode = "equal".into();
        self.rta_vbw = 0.0;
        self.rta_sweep_time_mode = 2;
        self.rta_sweep_time = 0.0;
        self.trigger = RtaTrigger::default();
        self.ref_clock = "internal".into();
        self.refclk_out = false;
        if !matches!(self.swp.mode.as_str(), "sdr" | "harmonic") {
            self.configure_active()?;
        }
        self.auto = AutoRef {
            result: "idle".into(),
            seq: self.auto.seq + 1,
            ..Default::default()
        };
        if self.swp.mode != "sdr" {
            self.sdr_center = 1e9;
            self.sdr_decimate = 16;
            self.sdr_pfb_taps_per_phase = 16;
            self.sdr_listen = 1e9;
            self.sdr_demod = "am".into();
            self.sdr_if_bw = 6000.0;
            self.sdr_squelch = -110.0;
            self.sdr_volume = 0.8;
            self.sdr_agc = true;
            self.sdr_pitch = 700.0;
            self.sdr_deemph = -1.0;
            self.sdr_channels = Self::preset_service_channels();
            self.sdr_aux = json!({"2":self.sdr_channels[1],"3":self.sdr_channels[2]});
        }
        self.sdr_ref = 0.0;
        self.sdr_ref_set = false;
        self.refclk_ppm = 0.0;
        self.swp_error = None;
        self.rta_error = None;
        self.status_warning = 0;
        if self.receiver_failure.is_none() {self.ddc_failure_diagnostics = None;}
        match self.swp.mode.as_str() {
            "sdr" => Ok(()),
            "harmonic" => {
                self.harm_window = None;
                self.harm_scan = true;
                self.harm_scan_cnt = 0;
                self.harm_seq.clear();
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Frequency range: the model table, widened to the device's own full span when known
    /// (Python apply_caps_from_defaults; the SAN-90 sweeps 8 kHz - 9.02 GHz).
    fn freq_limits(&self) -> (f64, f64) {
        let (mut lo, mut hi) = (
            self.caps()["fmin"].as_f64().unwrap_or(9e3),
            self.caps()["fmax"].as_f64().unwrap_or(9e9),
        );
        if let Some(d) = &self.device_defaults {
            let (dlo, dhi) = (d.center - d.span / 2.0, d.center + d.span / 2.0);
            if dhi > dlo {
                lo = lo.min(dlo);
                hi = hi.max(dhi);
            }
        }
        (lo, hi)
    }

    fn gauss(&mut self) -> f64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let u = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64;
        (-2.0 * u.max(1e-12).ln()).sqrt()
    }

    /// Shared synthetic spectrum (fake.py _FakeRtaBase._spectrum).
    fn spectrum(&mut self, center: f64, span: f64, points: usize) -> (Vec<f64>, Vec<f32>) {
        let freq: Vec<f64> = (0..points)
            .map(|i| center - span / 2.0 + span * i as f64 / (points - 1) as f64)
            .collect();
        let spec: Vec<f32> = (0..points)
            .map(|i| {
                let x = (i as f64 - points as f64 * 0.5) / 6.0;
                (-95.0 + self.gauss() * 0.5 + 70.0 * (-0.5 * x * x).exp()) as f32
            })
            .collect();
        (freq, spec)
    }

    fn wf_row(&mut self, width: usize) -> Vec<u16> {
        (0..width)
            .map(|_| (self.gauss().abs() * 4096.0) as u16)
            .collect()
    }

    fn encode_rta_frame(
        &self,
        freq: &[f64],
        spec: &[f32],
        wf: &[u16],
        start: f64,
        stop: f64,
    ) -> Vec<u8> {
        let mut out = b"RTAF".to_vec();
        out.extend_from_slice(&(self.freq_version as u32).to_le_bytes());
        out.extend_from_slice(&(spec.len() as u32).to_le_bytes());
        out.extend_from_slice(&(wf.len() as u16).to_le_bytes());
        out.extend_from_slice(&4095u16.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        for value in freq {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for value in spec {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for value in wf {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&stop.to_le_bytes());
        out
    }

    /// RTA step: vendor FPGA RTA in hardware mode, synthetic RTA otherwise.
    fn step_rta(&mut self) -> Vec<u8> {
        self.tick += 1;
        // Preserve the exact completed acquisition for late subscribers and ACK
        // races. Replays do not consume SDK packets or advance acquisition counters.
        if self.rta_hw.is_some() {
            if let Some(frame) = &self.rta_completed_frame { return frame.clone(); }
        }
        if let Some(shim) = self.hardware_shim.clone() {
            if self.rta_hw.is_none() && Self::retry_due(&self.rta_retry_at) {
                match HardwareRta::start(shim, self.rta_params()) {
                    Ok(rta) => {
                        let geometry = rta.geometry;
                        self.rta_hw = Some(rta);
                        self.rta_error = None;
                        self.rta_retry_at = None;
                        self.link_up = true;
                        self.apply_rta_geometry(geometry);
                        self.learn_defaults();
                    }
                    Err(error) => {
                        self.note_auto_acquisition(i32::MIN);
                        self.clear_rta_trigger(false);
                        self.link_up = false;
                        self.rta_error = Some(error);
                        self.rta_retry_at =
                            Some(std::time::Instant::now() + Duration::from_secs(1));
                    }
                }
            }
            if let Some(rta) = &self.rta_hw {
                match rta.next() {
                    Ok(frame) => {
                        self.status_warning = frame.trigger.last_sdk_status;
                        self.note_auto_acquisition(frame.trigger.last_sdk_status);
                        self.rta_frames += 1;
                        self.apply_rta_trigger_actual(frame.trigger, true);
                        self.observe_peak(&frame.power, None);
                        let complete = frame.trigger.complete == 1;
                        let encoded = self.attach_rta_trigger(Self::encode_hardware_rta(self.freq_version as u32, frame));
                        if complete { self.rta_completed_frame = Some(encoded.clone()); }
                        return encoded;
                    }
                    // Bus timeout is ambiguous (trigger wait or communication failure).
                    Err(failure) if WARN_STATUS.contains(&failure.status) || failure.status == -304 => {
                        self.status_warning = failure.status;
                        let actual = rta.trigger_actual();
                        self.note_auto_acquisition(failure.status);
                        if let Ok(actual) = actual {
                            self.apply_rta_trigger_actual(actual, false);
                        }
                    }
                    Err(failure) => {
                        self.note_auto_acquisition(failure.status);
                        if LINK_LOST_STATUS.contains(&failure.status) {
                            self.link_up = false;
                        }
                        self.rta_error = Some(failure.message);
                        self.clear_rta_trigger(false);
                        self.rta_hw = None;
                        self.rta_retry_at =
                            Some(std::time::Instant::now() + Duration::from_secs(1));
                    }
                }
            }
            return Vec::new();
        }
        let points = 1024;
        let (freq, spec) = self.spectrum(self.rta_center, self.rta_span, points);
        let mut sorted: Vec<f32> = spec.iter().copied().collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        self.note_auto_acquisition(0);
        self.auto.spectrum_observation = self.auto.observation;
        self.auto.last_peak = Some(*sorted.last().unwrap() as f64);
        self.auto.last_floor = Some(sorted[(sorted.len() - 1) * 3 / 10] as f64);
        self.rta_actual = json!({
            "center": self.rta_center, "span": self.rta_span,
            "start": self.rta_center - self.rta_span / 2.0,
            "stop": self.rta_center + self.rta_span / 2.0,
            "ref": self.rta_ref, "rbw": 7500.0, "vbw": 7500.0, "points": points,
            "frame_points": points, "refclk": 100e6, "refclk_src": 0, "refclk_out": false,
            "atten": self.swp.atten, "preamp": self.swp.preamp, "ifgain": self.swp.ifgain,
            "poi": 0.0005, "time_resolution": 5.12e-7, "packet_count": 1, "packet_frame": 2,
        });
        let wf = self.wf_row(224);
        self.rta_frames += 1;
        self.rta_trigger_actual = Self::unknown_rta_trigger(true);
        self.rta_trigger_actual["frames"] = json!(self.rta_frames);
        self.rta_trigger_actual["generation"] = json!(self.rta_trigger_generation);
        self.attach_rta_trigger(self.encode_rta_frame(
            &freq,
            &spec,
            &wf,
            self.rta_center - self.rta_span / 2.0,
            self.rta_center + self.rta_span / 2.0,
        ))
    }

    fn encode_hardware_rta(version: u32, frame: RtaFrame) -> Vec<u8> {
        let waterfall_len = 224;
        let mut waterfall = vec![0u16; waterfall_len];
        for (index, value) in waterfall.iter_mut().enumerate() {
            let from = index * frame.power.len() / waterfall_len;
            let to = ((index + 1) * frame.power.len() / waterfall_len).max(from + 1);
            let peak = frame.power[from..to]
                .iter()
                .copied()
                .fold(-120.0f32, f32::max);
            *value = ((peak + 120.0) * 40.95).clamp(0.0, 4095.0) as u16;
        }
        encode_rta(
            version,
            &frame.freq,
            &frame.power,
            &waterfall,
            4095,
            frame.geometry.start,
            frame.geometry.stop,
        )
        .unwrap_or_default()
    }

    /// FT8 slot replay (fake.py _ft8_baseband): the committed fixture is 48 kHz baseband.
    fn fixture_baseband(&mut self) -> Result<Vec<Vec<u8>>, &'static str> {
        const RATE: f64 = 48_000.0;
        const SAMPLES: usize = 4096;
        const SLOT_S: f64 = 15.0;
        static RAW: std::sync::LazyLock<Result<Arc<[f32]>, String>> =
            std::sync::LazyLock::new(|| service_fixture::fixture("ft8/ft8_cq_iq.bin"));
        let raw = RAW.as_ref().map_err(|error| error.as_str())?;
        let n_iq = raw.len() / 2;
        let slot = (RATE * SLOT_S) as usize;
        let mut block = Vec::with_capacity(SAMPLES * 8);
        for i in 0..SAMPLES {
            let pos = (self.audio_pos + i) % slot;
            let (mut re, mut im) = (0.0f32, 0.0f32);
            if pos < n_iq {
                re = raw[pos * 2];
                im = raw[pos * 2 + 1];
            }
            block.extend_from_slice(&re.to_le_bytes());
            block.extend_from_slice(&im.to_le_bytes());
        }
        self.audio_pos = (self.audio_pos + SAMPLES) % slot;
        self.synthetic_frames(RATE, self.sdr_center, &block)
    }

    /// Keyed CW carrier at the sidetone pitch (fake.py _cw_baseband + cw_keying).
    fn cw_baseband(&mut self, rate: f64, center: f64) -> Result<Vec<Vec<u8>>, &'static str> {
        const SAMPLES: usize = 4096;
        static KEY: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
        let key = KEY.get_or_init(|| cw_keying("TEST DE N0CALL", 20.0, rate));
        let mut block = Vec::with_capacity(SAMPLES * 8);
        for i in 0..SAMPLES {
            let pos = (self.audio_pos + i) % key.len();
            let t = (self.audio_pos + i) as f64 / rate;
            let amp = key[pos] as f64;
            let phase = 2.0 * std::f64::consts::PI * (700.0) * t;
            let re = (amp * phase.cos()) as f32;
            let im = (amp * phase.sin()) as f32;
            block.extend_from_slice(&re.to_le_bytes());
            block.extend_from_slice(&im.to_le_bytes());
        }
        self.audio_pos = (self.audio_pos + SAMPLES) % key.len();
        self.synthetic_frames(rate, center, &block)
    }

    /// Harmonic session (harmonic.py): full-band scan, then per-harmonic peak, then a HARM message.
    fn step_harm(&mut self) -> (Vec<Vec<u8>>, Vec<Value>) {
        self.tick += 1;
        let (fmin, fmax) = self.freq_limits();
        if self.harm_scan {
            if self.harm_scan_cnt == 0 {
                self.harm_active_session_id.clone_from(&self.harm_session_id);
            }
            let hi = fmax.min(self.harm_f0 * (self.harm_count as f64 + 1.0));
            let lo = fmin.max(self.harm_f0 / 4.0);
            if self.hardware_shim.is_some() {
                // Python's session reconfigures the vendor SWP once per window change;
                // the same window keeps streaming without a reopen (fit_span: config.py).
                let center = (lo + hi) / 2.0;
                let span = fit_span(center, (hi - lo).clamp(1.0, fmax - fmin), fmin, fmax);
                if !self.apply_harm_window(center, span) {
                    return (Vec::new(), Vec::new());
                }
            } else {
                self.swp.center = (lo + hi) / 2.0;
                self.swp.span = (hi - lo).clamp(1.0, fmax - fmin);
            }
            let (freq, powr) = self.fetch_sweep();
            self.harm_scan_cnt += 1;
            if self.harm_scan_cnt >= 8 {
                self.harm_scan_cnt = 0;
                self.harm_scan = false;
                self.harm_seq.clear();
                self.harm_n = 1;
                self.harm_base = None;
            }
            let mut frames = Vec::new();
            if let Some(frame) = freq {
                frames.push(frame);
            }
            frames.push(powr);
            return (frames, Vec::new());
        }
        let n = self.harm_n;
        let f = self.harm_f0 * n as f64;
        if n > self.harm_count || f > fmax {
            let list = Value::Array(self.harm_seq.clone());
            let msg = json!({"cmd": "HARM", "list": list, "f0": self.harm_f0,
                "session_id": self.harm_active_session_id});
            self.harm_scan = true;
            self.harm_scan_cnt = 0;
            self.harm_seq.clear();
            self.harm_n = 1;
            self.harm_base = None;
            return (Vec::new(), vec![msg]);
        }
        if self.hardware_shim.is_some() {
            let center = f.clamp(fmin, fmax);
            if !self.apply_harm_window(center, fit_span(center, self.harm_span, fmin, fmax)) {
                return (Vec::new(), Vec::new());
            }
        } else {
            self.swp.center = f.clamp(fmin, fmax);
            self.swp.span = self.harm_span.clamp(1.0, fmax - fmin);
        }
        // Each harmonic consumes only its own acquisition: a failed sweep must not
        // borrow the previous harmonic's peak (Python appends a null row instead).
        self.auto.last_peak = None;
        self.auto.last_peak_hz = None;
        let (freq, powr) = self.fetch_sweep();
        let mut frames = Vec::new();
        if let Some(frame) = freq {
            frames.push(frame);
        }
        let row = if let Some(peak) = self.auto.last_peak {
            let peak_f = self.auto.last_peak_hz.unwrap_or(self.swp.center);
            if n == 1 {
                self.harm_base = Some(peak);
            }
            let dbc = self
                .harm_base
                .map(|base| ((peak - base) * 100.0).round() / 100.0);
            json!({"n": n, "f": (peak_f * 10.0).round() / 10.0,
                   "amp": (peak * 100.0).round() / 100.0, "dbc": dbc})
        } else {
            json!({"n": n, "f": f, "amp": Value::Null, "dbc": Value::Null})
        };
        self.harm_seq.push(row);
        self.harm_n += 1;
        frames.push(powr);
        (frames, Vec::new())
    }

    /// Vendor SWP window for the harmonic session: reconfigure only when the window
    /// changes (the scan phase holds one window for 8 sweeps; each harmonic retunes).
    fn apply_harm_window(&mut self, center: f64, span: f64) -> bool {
        if self.harm_window != Some((center, span)) {
            let previous = (self.swp.center, self.swp.span);
            self.swp.center = center;
            self.swp.span = span;
            // With no owner, fetch_sweep opens the requested window. An existing
            // owner must accept it before any spectrum can belong to this step.
            if self.swp_hw.is_some() && self.configure_swp().is_err() {
                (self.swp.center, self.swp.span) = previous;
                return false;
            }
            self.harm_window = Some((center, span));
        }
        true
    }

    /// Phase-noise session on the vendor PNM engine (fake synthesises above otherwise).
    fn step_pnm_hw(&mut self) -> Vec<Value> {
        if let Some(shim) = self.hardware_shim.clone() {
            if self.pnm_hw.is_none() && Self::retry_due(&self.pnm_retry_at) {
                let params = PnmParams {
                    center: self.pnm_center,
                    threshold: self.pnm_threshold as f32,
                    rbw_ratio: 0.02,
                    start_offset: self.pnm_start,
                    stop_offset: self.pnm_stop,
                    trace_average: self.pnm_traceavg.clamp(1, 1000) as u32,
                };
                match HardwarePnm::start(shim, params) {
                    Ok(pnm) => {
                        self.pnm_hw = Some(pnm);
                        self.pnm_active_session_id.clone_from(&self.pnm_session_id);
                        self.pnm_active_traceavg = self.pnm_traceavg;
                        self.pnm_error = None;
                        self.pnm_retry_at = None;
                        self.pnm_dirty = false;
                        self.link_up = true;
                        self.learn_defaults();
                    }
                    Err(error) => {
                        self.pnm_error = Some(error);
                        self.link_up = false;
                        self.pnm_retry_at =
                            Some(std::time::Instant::now() + Duration::from_secs(1));
                    }
                }
            }
            let Some(pnm) = &self.pnm_hw else {
                return Vec::new();
            };
            match pnm.next() {
                Ok(Some(frame)) => {
                    let msg = json!({
                        "cmd": "PNM", "carrier_freq": frame.carrier_hz,
                        "carrier_power": frame.carrier_dbm,
                        "offset": frame.offset, "pn": frame.pn, "ref": frame.ref_level,
                        "traceavg": self.pnm_active_traceavg as f64,
                        "session_id": self.pnm_active_session_id,
                        "done": frame.done, "progress": frame.progress,
                    });
                    self.link_up = true;
                    if frame.done {
                        self.pnm_last = msg.clone();
                        if self.pnm_dirty {
                            // The new profile opens with the next cycle; the completed
                            // cycle's result has already been published.
                            self.pnm_hw = None;
                        }
                    }
                    vec![msg]
                }
                Ok(None) => Vec::new(),
                Err(error) => {
                    self.pnm_error = Some(error);
                    self.pnm_hw = None;
                    self.link_up = false;
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        }
    }

    fn step_pnm(&mut self) -> Vec<Value> {
        if self.hardware_shim.is_some() {
            return self.step_pnm_hw();
        }
        if self.pnm_i == 0 {
            self.pnm_active_session_id.clone_from(&self.pnm_session_id);
        }
        self.pnm_i += 1;
        let done = self.pnm_i >= 4;
        let n = 64;
        let mut offset = Vec::with_capacity(n);
        let mut pn = Vec::with_capacity(n);
        for i in 0..n {
            let frac = i as f64 / (n - 1) as f64;
            let f = self.pnm_start * (self.pnm_stop / self.pnm_start).powf(frac);
            offset.push((f * 100.0).round() / 100.0);
            pn.push(((-95.0 - 10.0 * (f / 1000.0).log10()) * 100.0).round() / 100.0);
        }
        let progress = if done {
            100
        } else {
            ((self.pnm_i as f64 / 4.0) * 100.0).round() as i64
        };
        let msg = json!({
            "cmd": "PNM", "carrier_freq": self.pnm_center,
            "carrier_power": self.swp.ref_level - 6.0,
            "offset": offset, "pn": pn, "ref": self.pnm_threshold,
            "traceavg": self.pnm_traceavg as f64, "done": done, "progress": progress,
            "session_id": self.pnm_active_session_id,
        });
        if done {
            self.pnm_last = msg.clone();
            self.pnm_i = 0;
        }
        vec![msg]
    }

    fn audio_frame(&mut self) -> Vec<u8> {
        const RATE: f64 = 48_000.0;
        const SAMPLES: usize = 960;
        let mut pcm = Vec::with_capacity(SAMPLES * 2);
        for i in 0..SAMPLES {
            let t = (self.audio_pos + i) as f64 / RATE;
            let value = (0.2 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 32767.0) as i16;
            pcm.extend_from_slice(&value.to_le_bytes());
        }
        self.audio_pos += SAMPLES;
        self.audio_seq = self.audio_seq.wrapping_add(1);
        let mut out = b"AUDF".to_vec();
        out.extend_from_slice(&self.audio_seq.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&(SAMPLES as u32).to_le_bytes());
        out.extend_from_slice(&pcm);
        out
    }

    /// Rebinds the synthetic fixture stream at a control boundary. Frames never
    /// change registry identity; a boundary that alters the fixture parameters
    /// allocates the next epoch here, whether or not a subscriber is attached.
    fn rebind_synthetic_iq(&mut self) -> Result<(), &'static str> {
        let applies = self.hardware_shim.is_none()
            && matches!(self.sdr_demod.as_str(), "ft8" | "cw" | "sstv" | "drm" | "drmplus");
        if !applies {
            self.synthetic_iq = None;
            return Ok(());
        }
        let center = self.sdr_center;
        let current = self.synthetic_iq.as_ref().is_some_and(|stream| {
            let entry = &stream.registry[0];
            stream.mode == self.sdr_demod && entry.output_rate == 48_000.0 && entry.frequency_hz == center
        });
        if !current {
            let epoch = allocate_dsp_epoch()?;
            self.synthetic_iq = Some(SyntheticIq { mode:self.sdr_demod.clone(), seq:0, input_index:0,
                registry:Arc::from([sdr_pipeline::ChannelRegistryEntry { channel:1, channel_id:1, epoch,
                    frequency_hz:center, output_rate:48_000.0, input_rate:48_000.0,
                    group_delay_samples:0.0, state:"active" }]) });
        }
        Ok(())
    }

    fn synthetic_frames(&mut self, rate: f64, center: f64, block: &[u8]) -> Result<Vec<Vec<u8>>, &'static str> {
        let stream = self.synthetic_iq.as_mut().ok_or("synthetic stream absent at data path")?;
        let entry = &stream.registry[0];
        if stream.mode != self.sdr_demod
            || entry.output_rate != rate
            || (center - entry.frequency_hz).abs() > 0.0
        {
            return Err("synthetic stream metadata changed outside a control boundary");
        }
        let (rate, center) = (entry.output_rate, entry.frequency_hz);
        let samples = u32::try_from(block.len() / 8).map_err(|_| "synthetic frame too large")?;
        let mut frames = Vec::with_capacity(2);
        if stream.seq == 0 {
            frames.push(websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,
                0, rate, center, entry.channel_id as u64, entry.epoch as u64, stream.input_index, 0.0, &[])?);
        }
        let seq = stream.seq.checked_add(1).ok_or("synthetic sequence exhausted")?;
        let next_index = stream.input_index.checked_add(samples as u64).ok_or("synthetic sample clock exhausted")?;
        let mut frame = websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,
            0, rate, center, entry.channel_id as u64, entry.epoch as u64, stream.input_index, 0.0, &[])?;
        frame[8..12].copy_from_slice(&seq.to_le_bytes());
        frame[12..16].copy_from_slice(&samples.to_le_bytes());
        frame.extend_from_slice(block);
        frames.push(frame);
        stream.seq = seq;
        stream.input_index = next_index;
        Ok(frames)
    }

    fn baseband_frame(&mut self, rate: f64, _center: f64) -> Result<Vec<Vec<u8>>, &'static str> {
        const SAMPLES: usize = 4096;
        const MOD_HZ: f64 = 1000.0;
        if self.sdr_demod == "ft8" {
            return self.fixture_baseband();
        }
        if self.sdr_demod == "cw" {
            return self.cw_baseband(rate, self.sdr_center);
        }
        let mut block = Vec::with_capacity(SAMPLES * 8);
        for i in 0..SAMPLES {
            let t = (self.tick * SAMPLES as u64 + i as u64) as f64 / rate;
            let envelope = 0.25 * (1.0 + 0.5 * (2.0 * std::f64::consts::PI * MOD_HZ * t).cos());
            block.extend_from_slice(&(envelope as f32).to_le_bytes());
            block.extend_from_slice(&0.0f32.to_le_bytes());
        }
        self.synthetic_frames(rate, self.sdr_center, &block)
    }

    /// SDR step (FakeSdrSession.step): pan RTAF + optional IQBF + four 20 ms AUDF frames.
    fn step_sdr(
        &mut self,
        iq_clients: usize,
        aux_clients: usize,
        display_clients: usize,
        audio_clients: usize,
    ) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<Vec<u8>>) {
        self.tick += 1;
        let center = self.sdr_center;
        let decimate = self.sdr_decimate.max(1);
        // Display window: usable capture bandwidth / requested decimate (Python
        // sdr_capture_geometry). The hardware capture stays 50 MHz; only the view narrows.
        let usable = self.ddc.as_ref().map_or(50e6, |ddc| ddc.0.usable_bandwidth);
        let bandwidth = usable / decimate as f64;
        let points = 512;
        let (freq, spec) = self.spectrum(center, bandwidth, points);
        if self.hardware_shim.is_none() {
            self.note_auto_acquisition(0);
            self.observe_peak(&spec, None);
            self.observe_channel_power(&freq,&spec);
        }
        let effective_if = self.effective_if_bw(usable);
        let fine = fine_decimate(effective_if).unwrap_or(2);
        let front_end = self.ddc.as_ref().and_then(|d| d.0.hardware.as_ref())
            .and_then(|capture| capture.diagnostics().front_end());
        let (atten, preamp, ifgain, ref_dbm) = if self.hardware_shim.is_some() {
            (json!(front_end.map(|f| f.atten)), json!(front_end.map(|f| f.preamp)),
             json!(front_end.map(|f| f.ifgain)), json!(front_end.map(|f| f.ref_dbm)))
        } else {
            (json!(self.swp.atten), json!(self.swp.preamp), json!(self.swp.ifgain), json!(self.sdr_ref))
        };
        self.sdr_actual = json!({
            "iq_rate": self.ddc.as_ref().map_or(IQ_FS, |d| {
                d.0.usable_bandwidth / 0.8
            }),
            "ddc_rate": COARSE_RATE / fine as f64, "bandwidth": bandwidth,
            "capture_bandwidth": self.ddc.as_ref().map_or(usable, |d| d.0.usable_bandwidth),
            "iq_center": center, "decimate": decimate,
            "pfb_taps_per_phase": self.ddc.as_ref().map_or(self.sdr_pfb_taps_per_phase, |d| d.0.stream.taps_per_phase()),
            "capture_decimate": self.ddc.as_ref().map_or(1, |d| d.0.device_decimate as i64),
            "packet_samples": 16240, "packet_bytes": 64960, "pan_points": points,
            "center": center,
            "capture_center": self.ddc.as_ref().map_or(center, |d| d.0.capture_center),
            "capture_start": self.ddc.as_ref().map_or(center - usable / 2.0, |d| {
                d.0.capture_center - d.0.usable_bandwidth / 2.0
            }),
            "capture_stop": self.ddc.as_ref().map_or(center + usable / 2.0, |d| {
                d.0.capture_center + d.0.usable_bandwidth / 2.0
            }),
            "start": center - bandwidth / 2.0, "stop": center + bandwidth / 2.0,
            "atten": atten, "preamp": preamp, "ifgain": ifgain,
            "ref": ref_dbm,
            "refclk_src": ref_clock_code(&self.ref_clock), "refclk_out": self.refclk_out,
            "listen": self.sdr_listen,
            "demod": self.sdr_demod, "if_bw": effective_if, "ddc_offset": 0.0,
            "mix_offset": 0.0, "ddc_decimate": 64 * fine, "ddc_delay": 102, "ddc_batch": 1,
            "audio_rate": 48_000.0, "deemph_us": self.sdr_deemph,
        });
        let wf = self.wf_row(224);
        let mut pan = if self.hardware_shim.is_some() {
            self.sdr_actual["pan_available"] = json!(false);
            Vec::new()
        } else {
            vec![self.encode_rta_frame(
                &freq,
                &spec,
                &wf,
                center - bandwidth / 2.0,
                center + bandwidth / 2.0,
            )]
        };
        // Without hardware, digital modes replay committed fixtures so the browser decoders
        // see real transmissions. With hardware every mode uses the real channel IQ.
        let fixture = self.hardware_shim.is_none()
            && matches!(
                self.sdr_demod.as_str(),
                "ft8" | "cw" | "sstv" | "drm" | "drmplus"
            );
        let mut real_rate = None;
        let mut pending = std::mem::take(&mut self.pending_iq);
        let ddc_blocked = self.ddc_error.is_some() && !Self::retry_due(&self.ddc_retry_at);
        let iq: Vec<Vec<u8>> = if (iq_clients + aux_clients == 0
            && (self.hardware_shim.is_none() || display_clients == 0))
            || ddc_blocked
        {
            Vec::new()
        } else if fixture {
            match self.baseband_frame(48_000.0, self.sdr_center) {
                Ok(frames) => frames,
                Err(error) => { self.ddc_error = Some(error.into()); Vec::new() }
            }
        } else {
            self.synthetic_iq = None;
            if self.ddc.is_none() && !ddc_blocked {
                let frequencies: Vec<f64> = self
                    .sdr_aux
                    .as_object()
                    .into_iter()
                    .flat_map(|aux| aux.values())
                    .filter(|config| config["enabled"] == true)
                    .filter_map(|config| config["freq"].as_f64())
                    .collect();
                let setup = Ddc::configured(
                    center, self.sdr_listen, fine, 50_000_000.0, &mut None,
                    &frequencies, self.sdr_pfb_taps_per_phase,
                ).map(SendDdc)
                .and_then(|mut ddc| {
                    if let Some(config) = self.sdr_aux.as_object() {
                        for (channel, value) in config {
                            if value["enabled"] == true {
                                ddc.0.configure_aux(
                                    channel.parse().map_err(|_| "invalid aux channel")?,
                                    true,
                                    value["freq"].as_f64().ok_or("invalid aux frequency")?,
                                )?;
                            }
                        }
                    }
                    Ok(ddc)
                });
                match setup {
                    Ok(ddc) => {
                        self.ddc = Some(ddc);
                        self.ddc_error = None;
                        self.ddc_retry_at = None;
                        self.link_up = true;
                        self.learn_defaults();
                    }
                    Err(error) => {
                        self.ddc_error_count += 1;
                        self.ddc_error_last = Some(error.clone());
                        eprintln!("DDC setup error #{}: {}", self.ddc_error_count, error);
                        if self.hardware_shim.is_some() && error.starts_with("SDK IQS open") {
                            self.link_up = false;
                        }
                        self.ddc_error = Some(error);
                        self.ddc_retry_at =
                            Some(std::time::Instant::now() + Duration::from_secs(1));
                    }
                }
            }
            match self.ddc.as_mut() {
                Some(ddc) => {
                    real_rate = Some(ddc.0.channel_rate());
                    let result = ddc.0.step();
                    let window = (self.sdr_center, bandwidth.min(ddc.0.usable_bandwidth));
                    let pan_raw = std::mem::take(&mut ddc.0.pan);
                    let real_pan = if result.is_ok() {
                        pan_raw
                            .iter()
                            .map(|(freq, power, _warning, _generation, _seq)| {
                                hardware_pan(
                                    self.freq_version as u32,
                                    window.0,
                                    window.1,
                                    freq,
                                    power,
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()
                    } else {
                        Ok(Vec::new())
                    };
                    match (result, real_pan) {
                        (Ok(frames), Ok(spectra)) => {
                            if let Some(frame) = spectra.first() {
                                self.sdr_actual["pan_points"] =
                                    json!(u32::from_le_bytes(frame[8..12].try_into().unwrap()));
                                self.sdr_actual["pan_available"] = json!(true);
                            }
                            if let Some((freq, power, warning, _generation, seq)) = pan_raw.last() {
                                self.note_auto_acquisition(*warning);
                                self.observe_channel_power(freq,power);
                                let (lo, hi) =
                                    (window.0 - window.1 / 2.0, window.0 + window.1 / 2.0);
                                let inside: Vec<f32> = freq
                                    .iter()
                                    .zip(power)
                                    .filter(|(f, _)| (lo..=hi).contains(*f))
                                    .map(|(_, p)| *p)
                                    .collect();
                                if (*warning == 0 || *warning == -301)
                                    && *seq > self.auto.session.as_ref().map_or(0, |session| session.packet_baseline)
                                {
                                    self.observe_peak(&inside, None);
                                }
                            }
                            pan.extend(spectra);
                            frames
                        }
                        (Err(error), _) | (_, Err(error)) => {
                            self.ddc_error_count += 1;
                            self.ddc_error_last = Some(error.clone());
                            eprintln!("DDC step error #{}: {}", self.ddc_error_count, error);
                            if let Err(failure)=self.finish_radiofax() {self.latch_receiver_failure(failure);}
                            if error.contains("status=-8") || error.contains("status=-9") {
                                self.link_up = false;
                            }
                            self.ddc_error = Some(error);
                            self.ddc = None;
                            self.ddc_retry_at =
                                Some(std::time::Instant::now() + Duration::from_secs(1));
                            Vec::new()
                        }
                    }
                }
                None => Vec::new(),
            }
        };
        if let Some(rate) = real_rate {
            self.sdr_actual["ddc_rate"] = json!(rate);
        }
        let audio = if audio_clients > 0 && self.hardware_shim.is_none() {
            (0..4).map(|_| self.audio_frame()).collect()
        } else {
            Vec::new()
        };
        pending.extend(iq);
        let mut primary = Vec::new();
        let mut aux = Vec::new();
        for frame in pending {
            if frame.starts_with(b"IQAX") {
                aux.push(frame);
            } else {
                primary.push(frame);
            }
        }
        (pan, primary, aux, audio)
    }

    /// One synthetic swept trace (ported from hardware/fake_device.py fetch_sweep).
    fn fetch_sweep(&mut self) -> (Option<Vec<u8>>, Vec<u8>) {
        if let Some(shim) = self.hardware_shim.clone() {
            return self.fetch_sweep_vendor(&shim);
        }
        let points = self.swp.points.clamp(51, 16_000) as usize;
        let (center, span) = (self.swp.center, self.swp.span);
        let start = center - span / 2.0;
        let step = if points > 1 {
            span / (points - 1) as f64
        } else {
            0.0
        };
        let width = (points as f64 / 200.0).max(1.0);
        let (floor_db, peak_db) = (-95.0, -25.0 + self.gauss() * 0.2);
        let mut powers = Vec::with_capacity(points);
        for i in 0..points {
            let x = (i as f64 - points as f64 / 2.0) / width;
            powers
                .push(floor_db + self.gauss() * 0.6 + (peak_db - floor_db) * (-0.5 * x * x).exp());
        }
        let mut sorted = powers.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let peak_obs = *sorted.last().unwrap_or(&-200.0);
        let floor_obs = sorted[(sorted.len() - 1) * 3 / 10];
        self.note_auto_acquisition(0);
        self.auto.spectrum_observation = self.auto.observation;
        self.auto.last_peak_hz = Some(
            powers
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| start + i as f64 * step)
                .unwrap_or(center),
        );
        self.auto.last_peak = Some(peak_obs);
        self.auto.last_floor = Some(floor_obs);
        self.sweep_ms = 5.0;
        self.actual = json!({
            "center": center, "span": span, "start": start, "stop": center + span / 2.0,
            "ref": self.swp.ref_level, "rbw": self.swp.rbw, "vbw": self.swp.vbw,
            "points": points, "est_min": 0.0, "refclk": 100e6, "refclk_src": 0,
        });
        let mut freq = b"FREQ".to_vec();
        freq.extend_from_slice(&(self.freq_version as u32).to_le_bytes());
        freq.extend_from_slice(&(points as u32).to_le_bytes());
        freq.extend_from_slice(&5.0f32.to_le_bytes());
        for i in 0..points {
            freq.extend_from_slice(&(start + i as f64 * step).to_le_bytes());
        }
        let mut powr = b"POWR".to_vec();
        powr.extend_from_slice(&(self.freq_version as u32).to_le_bytes());
        powr.extend_from_slice(&(points as u32).to_le_bytes());
        powr.extend_from_slice(&5.0f32.to_le_bytes());
        for value in &powers {
            powr.extend_from_slice(&(*value as f32).to_le_bytes());
        }
        let freq_changed = self.last_freq.as_deref() != Some(freq.as_slice());
        if freq_changed {
            self.last_freq = Some(freq.clone());
            (Some(freq), powr)
        } else {
            (None, powr)
        }
    }

    /// Vendor SWP request mapped field-by-field from the WebSA SWP wire state (mirrors the
    /// Python service `_profile`; the C shim owns the vendor enum values).
    fn swp_params(&self) -> SwpParams {
        let p = &self.swp;
        SwpParams {
            center: p.center,
            span: p.span,
            front_end: self.front_end(p.ref_level),
            rbw_mode: p.rbw_mode.clone(),
            rbw: p.rbw,
            vbw_mode: p.vbw_mode.clone(),
            vbw: p.vbw,
            window: p.window,
            sweep_time_mode: p.sweep_time_mode,
            sweep_time: p.sweep_time,
            points: p.points.clamp(51, 16_000),
            spur: p.spur.clone(),
            detector: p.detector.clone(),
            frame_detector: p.frame_detector.clone(),
            trace_align: p.trace_align.clone(),
            points_strategy: p.points_strategy.clone(),
        }
    }

    /// Peak, its frequency and the 30th-percentile floor of one trace (NaN bins ignored).
    fn observe_peak(&mut self, power: &[f32], freq: Option<&[f64]>) {
        let mut finite: Vec<(usize, f32)> = power
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .collect();
        if finite.is_empty() {
            return;
        }
        let (peak_index, peak) = *finite
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty");
        finite.sort_by(|a, b| a.1.total_cmp(&b.1));
        self.auto.spectrum_observation = self.auto.observation;
        self.auto.last_peak = Some(peak as f64);
        self.auto.last_floor = Some(finite[(finite.len() - 1) * 3 / 10].1 as f64);
        if let Some(freq) = freq {
            self.auto.last_peak_hz = Some(freq[peak_index]);
        }
    }

    /// Vendor SWP step: opens the handle once and publishes each sweep as FREQ/POWR.
    fn fetch_sweep_vendor(&mut self, shim: &str) -> (Option<Vec<u8>>, Vec<u8>) {
        if self.swp_hw.is_none() && Self::retry_due(&self.swp_retry_at) {
            match HardwareSwp::start(shim.to_owned(), self.swp_params()) {
                Ok(swp) => {
                    self.swp_open_count += 1;
                    self.swp_error = None;
                    self.swp_retry_at = None;
                    self.swp_fail_streak = 0;
                    self.link_up = true;
                    let geometry = swp.geometry;
                    self.swp_hw = Some(swp);
                    self.apply_swp_geometry(geometry);
                    self.learn_defaults();
                }
                Err(error) => {
                    self.note_auto_acquisition(i32::MIN);
                    self.link_up = false;
                    self.swp_error = Some(error);
                    self.swp_retry_at = Some(std::time::Instant::now() + Duration::from_secs(1));
                }
            }
        }
        let Some(swp) = &self.swp_hw else {
            return (None, Vec::new());
        };
        let result = swp.next();
        match result {
            Ok(frame) => {
                self.swp_fail_streak = 0;
                self.status_warning = frame.warning;
                self.note_auto_acquisition(frame.warning);
                if self
                    .swp_error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("IF overflow")) && frame.warning == 0
                {
                    self.swp_error = None;
                }
                if frame.warning == -12 {
                    self.swp_error = Some(
                        "IF overflow (-12): raise the reference level until this warning clears".into(),
                    );
                }
                self.refclk_ppm = frame.refclk_ppm;
                if frame.warning == 0 {
                    self.observe_peak(&frame.power, Some(&frame.freq));
                }
                let points = frame.freq.len();
                let encode = |tag: &[u8; 4], values: &[u8]| -> Vec<u8> {
                    let mut out = tag.to_vec();
                    out.extend_from_slice(&(self.freq_version as u32).to_le_bytes());
                    out.extend_from_slice(&(points as u32).to_le_bytes());
                    out.extend_from_slice(&(self.sweep_ms as f32).to_le_bytes());
                    out.extend_from_slice(values);
                    out
                };
                let freq_bytes: Vec<u8> = frame
                    .freq
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                let power_bytes: Vec<u8> = frame
                    .power
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                let freq = encode(b"FREQ", &freq_bytes);
                let powr = encode(b"POWR", &power_bytes);
                if self.last_freq.as_deref() != Some(freq.as_slice()) {
                    self.last_freq = Some(freq.clone());
                    (Some(freq), powr)
                } else {
                    (None, powr)
                }
            }
            Err(failure) => {
                self.note_auto_acquisition(failure.status);
                self.swp_failed(failure);
                (None, Vec::new())
            }
        }
    }

    /// A retune or mode change makes every stored peak/floor stale. Auto scale must not
    /// place a reference from another band or another mode's acquisition.
    fn invalidate_observation(&mut self) {
        self.auto.last_peak = None;
        self.auto.last_peak_hz = None;
        self.auto.last_floor = None;
        self.auto.observation_config = u64::MAX;
        self.auto.iqs_packets = self.pipeline.as_ref().map(|p| p.packet_observation().0)
            .or_else(|| self.ddc.as_ref().and_then(|d| d.0.hardware.as_ref())
                .map(|c| c.diagnostics().packet_observation().0)).unwrap_or(0);
        self.auto.target = None;
        if self.auto.session.take().is_some() {
            self.auto.result = "cancelled".into();
            self.auto.seq += 1;
        }
    }

    /// One explicitly requested, bounded recovery/placement session. Acquisition, never
    /// a retained STATUS warning, supplies the evidence for each subsequent step.
    fn auto_scale(&mut self, current: Option<f64>, display_only: bool) -> bool {
        self.auto.target = None;
        self.auto.result = "adjusting".into();
        self.auto.seq += 1;
        self.auto.session = Some(AutoSession {
            mode: self.swp.mode.clone(), config: self.config_version,
            seen: self.auto.observation, deadline: std::time::Instant::now() + Duration::from_secs(20),
            remaining: 24, display_only, display_ref: current.filter(|v| v.is_finite()),
            pending: None, previous_ref: None, recovering: false, fitted: false,
            packet_baseline: self.auto.iqs_packets,
        });
        true
    }

    /// A superseding manual command cancels the running session before its own
    /// admission waits, so a queued Auto step can never race the fresh revision.
    fn cancel_auto_session(&mut self) -> bool {
        if self.auto.session.take().is_none() { return false; }
        self.auto.target = None;
        self.auto.result = "cancelled".into();
        self.auto.seq += 1;
        true
    }

    fn note_auto_acquisition(&mut self, status: i32) {
        self.auto.observation += 1;
        self.auto.observation_config = self.config_version;
        self.auto.observation_status = status;
        self.auto.last_peak = None;
        self.auto.last_floor = None;
        self.auto.last_peak_hz = None;
    }

    fn finish_auto(&mut self, result: &str, target: Option<f64>) -> bool {
        self.auto.session = None;
        self.auto.result = result.into();
        self.auto.target = target;
        self.auto.seq += 1;
        true
    }

    fn active_auto_ref(&self) -> Option<f64> {
        let value = match self.swp.mode.as_str() {
            "rta" => self.rta_actual["ref"].as_f64(),
            "sdr" => self.pipeline.as_ref().and_then(|p| p.front_end()).map(|f| f.ref_dbm)
                .or_else(|| self.ddc.as_ref().and_then(|d| d.0.hardware.as_ref())
                    .and_then(|c| c.diagnostics().front_end()).map(|f| f.ref_dbm)),
            _ => self.actual["ref"].as_f64(),
        };
        value.filter(|v| v.is_finite()).or_else(|| {
            self.hardware_shim.is_none().then_some(match self.swp.mode.as_str() {
                "rta" => self.rta_ref, "sdr" => self.sdr_ref, _ => self.swp.ref_level,
            })
        })
    }

    fn advance_auto(&mut self) -> bool {
        let Some(mut session) = self.auto.session.take() else { return false; };
        if session.mode != self.swp.mode || session.config != self.config_version {
            return self.finish_auto("cancelled", None);
        }
        if std::time::Instant::now() >= session.deadline {
            return self.finish_auto(if session.pending.is_some() { "timeout" } else { "no_data" }, None);
        }
        if self.auto.observation_config != session.config || self.auto.observation <= session.seen {
            self.auto.session = Some(session);
            return false;
        }
        session.seen = self.auto.observation;
        let status = self.auto.observation_status;
        let clean = status == 0 || status == -301;
        if status != -12 && !clean {
            if matches!(status, -10 | -11 | -304 | -302 | -303 | -19..=-14 | -39..=-36) {
                self.auto.session = Some(session);
                return false;
            }
            return self.finish_auto("failed", None);
        }
        let sdr = session.mode == "sdr";
        let actual = self.active_auto_ref();
        let current = if sdr && !session.recovering {
            session.display_ref.or(actual)
        } else { actual };
        let Some(current) = current else { return self.finish_auto("failed", None); };
        if let (Some(previous), Some(requested), Some(actual)) =
            (session.previous_ref, session.pending, actual) {
            if (requested - previous).abs() >= 0.5 && (actual - previous).abs() <= 0.01 {
                return self.finish_auto("stalled", None);
            }
        }
        let target = if status == -12 {
            if session.display_only { return self.finish_auto("overflow", None); }
            let Some(actual) = actual else { return self.finish_auto("failed", None); };
            let maximum = self.caps()["ref_max"].as_f64().unwrap_or(30.0);
            if actual >= maximum || session.pending.is_some_and(|p| p >= maximum) {
                return self.finish_auto("limit", None);
            }
            session.recovering = true;
            session.fitted = false;
            (actual + 10.0).min(maximum)
        } else {
            if self.auto.spectrum_observation != self.auto.observation {
                self.auto.session = Some(session);
                return false;
            }
            let Some(peak) = self.auto.last_peak else {
                self.auto.session = Some(session);
                return false;
            };
            if session.fitted {
                return self.finish_auto("applied", Some(if sdr { session.pending.unwrap_or(current) } else { current }));
            }
            let floor = self.auto.last_floor;
            let bottom = current - self.swp.ref_range_db;
            let placed = floor.is_some_and(|f| (5.0..=60.0).contains(&(f - bottom)) && current - peak >= 8.0);
            if placed { return self.finish_auto(if session.recovering { "applied" } else { "ok" }, Some(current)); }
            let raw = floor.map(|f| (f + self.swp.ref_range_db - 15.0).max(peak + 10.0)).unwrap_or(peak + 10.0);
            let lo = if sdr { -160.0 } else { self.caps()["ref_min"].as_f64().unwrap_or(-50.0) };
            let hi = if sdr { 40.0 } else { self.caps()["ref_max"].as_f64().unwrap_or(30.0) };
            let mut target = ((raw / 5.0).ceil() * 5.0).clamp(lo, hi);
            if !sdr && current > hi { target = current; }
            // Once overload has been observed, preserve the first proven clean gain
            // configuration. Display placement may still move independently in SDR.
            if session.recovering && !sdr { target = target.max(current); }
            if (target-current).abs() < 0.5 {
                return self.finish_auto(if session.recovering { "applied" } else { "ok" }, Some(current));
            }
            session.fitted = true;
            target
        };
        if session.remaining == 0 { return self.finish_auto("limit", None); }
        session.remaining -= 1;
        if sdr && status != -12 {
            // A clean SDR Auto retains the display-only placement contract. A new
            // capture observation must confirm that the displayed target is clean;
            // the display value never counts as a hardware request, so a later
            // overflow can still run a permitted recovery from the actual Ref.
            session.display_ref = Some(target);
            session.pending = None;
        } else {
            let previous = actual.unwrap_or(current);
            let configured = match session.mode.as_str() {
                "rta" => {
                    let previous = self.rta_ref;
                    self.rta_ref = target;
                    if self.configure_rta().is_ok() { true }
                    else { self.rta_ref = previous; false }
                },
                "sdr" => {
                    let old = (self.sdr_ref, self.sdr_ref_set);
                    self.sdr_ref = target;
                    self.sdr_ref_set = true;
                    if self.configure_sdr_capture().is_ok() { true }
                    else { (self.sdr_ref, self.sdr_ref_set) = old; false }
                }
                _ => {
                    let previous = self.swp.ref_level;
                    self.swp.ref_level = target;
                    if self.configure_swp().is_ok() { true }
                    else { self.swp.ref_level = previous; false }
                },
            };
            if !configured { return self.finish_auto("failed", None); }
            session.previous_ref = Some(previous);
            session.config = self.config_version;
            session.pending = Some(target);
        }
        session.seen = self.auto.observation;
        self.auto.session = Some(session);
        true
    }

    #[cfg(test)]
    fn status(&self) -> Value { self.status_snapshot().build() }
}

/// Exclusively owned DDC state. The fake path lives behind DevState's mutex;
/// the hardware path creates and uses it on its dedicated PFB owner thread.
/// Fine handles move to their worker only while the owner cannot access them.
struct SendDdc(Ddc);
unsafe impl Send for SendDdc {}

/// PFB geometry: 62.5 MSps input, 128 bins (488.28125 kHz apart), 976.5625 kSps per bin.
const IQ_FS: f64 = 62_500_000.0;
const PFB_BINS: i64 = 128;
const BIN_SPACING: f64 = IQ_FS / PFB_BINS as f64;
const COARSE_RATE: f64 = IQ_FS / 64.0;
/// Coarse bins on each side of the primary bin that the plan provisions, so that a step-tune
/// of up to 2 bins (976 kHz) retunes the fine channel without a new plan.
const PLAN_NEIGHBOURS: i64 = 2;
/// Fine-channel offset limit inside one coarse bin (`FineBank::prepare`: coarse_rate / 4).
const MAX_FINE_OFFSET: f64 = COARSE_RATE / 4.0;
/// Flat passband of one coarse bin: -0.13 dB at 430 kHz, -5.95 dB at 488 kHz (2048-tap
/// Kaiser 8.6 prototype, cutoff 0.5/64).
const COARSE_FLAT_HZ: f64 = 430_000.0;

fn display_half_width(decimate: i32) -> f64 {
    (IQ_FS * 0.8) / decimate.max(1) as f64 / 2.0
}

/// Narrowest device decimate whose capture still covers everything in use: the
/// displayed window, the primary channel and every aux channel. Search from the
/// narrowest rate upwards so a wide display really gets the wide capture back
/// (D=1). The PFB grid is fs-invariant for D<=64, so only the covered window shrinks.
fn capture_decimate(center: f64, listen: f64, aux: &[f64], display_decimate: i32, taps: usize) -> i32 {
    let mut extent = display_half_width(display_decimate);
    let flat = coarse_passband_hz(COARSE_RATE, taps).expect("validated PFB taps");
    extent = extent.max((listen - center).abs() + flat);
    for &freq in aux {
        if freq.is_finite() {
            extent = extent.max((freq - center).abs() + flat);
        }
    }
    // Capture must keep the whole plan reachable: primary ± PLAN_NEIGHBOURS bins.
    extent = extent.max((PLAN_NEIGHBOURS as f64 + 1.0) * BIN_SPACING);
    for d in (0..=6).rev() {
        let half = IQ_FS * 0.8 / (1 << d) as f64 / 2.0;
        if extent <= half {
            return 1 << d;
        }
    }
    1
}

/// Baseband rate that one demodulator needs (Python sdr.py _chain_params: 1.6 x IF bandwidth,
/// at least the 48 kHz audio rate).
fn channel_need_hz(if_bw: f64) -> f64 {
    (1.6 * if_bw).max(48_000.0)
}

/// Fine decimation for one demodulator: the largest factor whose output rate covers
/// `channel_need_hz`. The channel sits up to MAX_FINE_OFFSET from the bin centre, so its band
/// edge must stay inside COARSE_FLAT_HZ (limit: IF bandwidth about 232 kHz). Wider modes (for
/// example DTMB, 7.56 MHz) need a wideband channel type on the 62.5 MSps input.
fn fine_decimate(if_bw: f64) -> Result<i32, String> {
    let need = channel_need_hz(if_bw);
    let wideband =
        || format!("IF bandwidth {if_bw:.0} Hz needs a wideband channel (not available)");
    if MAX_FINE_OFFSET + need / 2.0 > COARSE_FLAT_HZ {
        return Err(wideband());
    }
    (2..=24)
        .rev()
        .find(|&d| COARSE_RATE / d as f64 >= need)
        .ok_or_else(wideband)
}

fn validate_channel_passband(center: f64, frequency: f64, fine: i32, taps: usize) -> Result<(), String> {
    let flat = coarse_passband_hz(COARSE_RATE, taps).ok_or("unsupported PFB taps")?;
    let offset = signed_bin(frequency, center) as f64 * BIN_SPACING - (frequency - center);
    if !frequency.is_finite() || offset.abs() + COARSE_RATE / fine as f64 / 2.0 > flat {
        Err("channel bandwidth exceeds PFB passband at this frequency".to_string())
    } else {
        Ok(())
    }
}

fn signed_bin(freq: f64, center: f64) -> i64 {
    ((freq - center) / BIN_SPACING).round() as i64
}

/// Real Rust channelizer inside the service: raw 62.5 MSps Complex16 -> PFB -> fine DDC -> IQBF.
struct Ddc {
    stream: Box<PfbStream>,
    fine: Option<sdr_pipeline::FineState>,
    fine_worker: Option<sdr_pipeline::FineWorker>,
    carry: Vec<Vec<u8>>,
    pan: Vec<(Vec<f64>, Vec<f32>, i32, u64, u64)>,
    capture_center: f64,
    usable_bandwidth: f64,
    decimate: i32,
    listen: f64,
    next_seq: u64,
    input_pos: u64,
    stamp_gaps: u64,
    last_stamp: Option<u64>,
    /// IQS configuration generation of the packets that this plan accepts.
    generation: u64,
    /// Device-side IQS decimate (1 = full rate). The stamp counts 2 sample clocks
    /// per complex sample multiplied by this factor.
    device_decimate: u64,
    hardware: Option<HardwareCapture>,
    capture_failed: bool,
    pipeline_timing: sdr_pipeline::PipelineTiming,
    registry: Arc<[sdr_pipeline::ChannelRegistryEntry]>,
    #[cfg(test)]
    cost_profile: websa_channelizer::FineCostBudget,
    service_configs: Option<Vec<receiver_workers::ChannelConfig>>,
}

impl Ddc {
    #[cfg(test)]
    /// Synthetic channel at the default 6 kHz IF bandwidth.
    fn new(center: f64, listen: f64) -> Option<Self> {
        let decimate = fine_decimate(6000.0).ok()?;
        Self::configured(center, listen, decimate, 50_000_000.0, &mut None, &[], 16).ok()
    }


    fn configured(
        center: f64,
        listen: f64,
        decimate: i32,
        bandwidth: f64,
        hardware: &mut Option<HardwareCapture>,
        aux_frequencies: &[f64],
        taps_per_phase: usize,
    ) -> Result<Self, String> {
        let fs = if hardware.is_some() {
            ((bandwidth / 0.8 / COARSE_RATE).round() * COARSE_RATE).max(COARSE_RATE)
        } else {
            IQ_FS
        };
        Self::configured_at_rate(center, listen, decimate, bandwidth, hardware, aux_frequencies, taps_per_phase, fs)
    }

    fn configured_at_rate(
        center: f64,
        listen: f64,
        decimate: i32,
        bandwidth: f64,
        hardware: &mut Option<HardwareCapture>,
        aux_frequencies: &[f64],
        taps_per_phase: usize,
        fs: f64,
    ) -> Result<Self, String> {
        let channels = 1 + aux_frequencies.len();
        Self::configured_with_budget(center, listen, decimate, bandwidth, hardware, aux_frequencies,
            taps_per_phase, fs, channels, COARSE_RATE / decimate as f64 * channels as f64)
    }

    fn configured_with_budget(
        center: f64, listen: f64, decimate: i32, bandwidth: f64,
        hardware: &mut Option<HardwareCapture>, aux_frequencies: &[f64],
        taps_per_phase: usize, fs: f64, channels: usize, output_rate_budget: f64,
    ) -> Result<Self, String> {
        let profile = sdr_pipeline::fine_cost_profile()?;
        Self::configured_with_cost_budget(center, listen, decimate, bandwidth, hardware,
            aux_frequencies, taps_per_phase, fs, channels, output_rate_budget, profile)
    }

    fn configured_with_cost_budget(
        center: f64, listen: f64, decimate: i32, bandwidth: f64,
        hardware: &mut Option<HardwareCapture>, aux_frequencies: &[f64],
        taps_per_phase: usize, fs: f64, channels: usize, output_rate_budget: f64,
        cost_profile: websa_channelizer::FineCostBudget,
    ) -> Result<Self, String> {
        sdr_pipeline::check_plan_admission(&cost_profile, decimate, 1 + aux_frequencies.len(),
            channels, output_rate_budget)?;
        let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
        let primary = signed_bin(listen, center);
        if !(-m / 2..m / 2).contains(&primary) {
            return Err("SDR channel outside coarse plan".into());
        }
        let bin = primary.rem_euclid(m) as u32;
        let offset = primary as f64 * BIN_SPACING - (listen - center);
        let mut bins = vec![bin];
        for neighbour in (primary - PLAN_NEIGHBOURS)..=(primary + PLAN_NEIGHBOURS) {
            let selected = neighbour.rem_euclid(m) as u32;
            let edge = (neighbour as f64 * BIN_SPACING).abs() + COARSE_RATE / 2.0;
            if (-m / 2..m / 2).contains(&neighbour)
                && edge <= bandwidth / 2.0
                && !bins.contains(&selected)
            {
                bins.push(selected);
            }
        }
        for &frequency in aux_frequencies {
            if !frequency.is_finite() {
                return Err("invalid aux frequency".into());
            }
            let signed = signed_bin(frequency, center);
            if !(-m / 2..m / 2).contains(&signed) {
                return Err("aux frequency outside coarse plan".into());
            }
            let selected = signed.rem_euclid(m) as u32;
            if !bins.contains(&selected) {
                bins.push(selected);
            }
        }
        let epoch = allocate_dsp_epoch()?;
        let stream = PfbStream::with_taps(fs, 16240, bins, epoch as u64, taps_per_phase).ok_or("PFB unavailable")?;
        let rate = stream.sample_rate / decimate as f64;
        let hop = stream.input_hop();
        let mut bank = FineBank::new(
            &stream,
            bandwidth,
            16240 / hop + 2,
            channels,
            output_rate_budget,
            // Same slow-consumer budget as the IQS owner queue gate.
            CONSUMER_BUDGET,
        )
        .ok_or("fine bank unavailable")?;
        bank.set_cost_budget(cost_profile.clone()).map_err(str::to_owned)?;
        bank.subscribe(1, bin, offset, decimate)
            .map_err(str::to_owned)?;
        let framer =
            BasebandFramer::new(1, epoch as u64, listen, rate).ok_or("baseband frame metadata rejected")?;
        let fine = sdr_pipeline::FineState::new(bank, framer, fs, epoch, listen);
        let registry = fine.registry.clone();
        let generation = hardware.as_ref().map_or(0, |c| c.geometry.generation);
        Ok(Self {
            stream: Box::new(stream),
            fine: Some(fine),
            fine_worker: None,
            carry: Vec::new(),
            pan: Vec::new(),
            capture_center: center,
            usable_bandwidth: bandwidth,
            device_decimate: if hardware.is_some() {
                ((IQ_FS / fs).round() as u64).max(1)
            } else {
                1
            },
            decimate,
            listen,
            next_seq: 0,
            input_pos: 0,
            stamp_gaps: 0,
            last_stamp: None,
            generation,
            hardware: hardware.take(),
            capture_failed: false,
            pipeline_timing: sdr_pipeline::PipelineTiming::new(fs),
            registry,
            #[cfg(test)]
            cost_profile,
            service_configs: None,
        })
    }

    #[cfg(test)]
    /// New PFB/fine plan on the same IQS capture (no device reopen). The IQBF stream restarts
    /// at seq 0 with the new rate/centre; the old tail frames are returned first. A failure
    /// leaves the running plan, its aux subscriptions and the capture untouched.
    fn replan(&mut self, listen: f64, decimate: i32) -> Result<Vec<Vec<u8>>, String> {
        let aux: Vec<(u32, f64)> = self.aux.iter().map(|(&ch, (_, f, _))| (ch, *f)).collect();
        self.replan_with_aux(listen, decimate, &aux)
    }

    #[cfg(test)]
    fn replan_with_aux(
        &mut self,
        listen: f64,
        decimate: i32,
        aux: &[(u32, f64)],
    ) -> Result<Vec<Vec<u8>>, String> {
        self.replan_with_taps(listen, decimate, aux, self.stream.taps_per_phase())
    }

    #[cfg(test)]
    fn replan_with_taps(
        &mut self,
        listen: f64,
        decimate: i32,
        aux: &[(u32, f64)],
        taps_per_phase: usize,
    ) -> Result<Vec<Vec<u8>>, String> {
        let frequencies: Vec<f64> = aux.iter().map(|(_, f)| *f).collect();
        // `configured` only takes the capture after the whole plan has been built, so a
        // rejected frequency or fine bank leaves the old plan's capture in place.
        let mut next = Self::configured_with_cost_budget(
            self.capture_center,
            listen,
            decimate,
            self.usable_bandwidth,
            &mut self.hardware,
            &frequencies,
            taps_per_phase,
            self.stream.input_sample_rate(),
            aux.len() + 1,
            COARSE_RATE / decimate as f64 * (aux.len() + 1) as f64,
            self.cost_profile.clone(),
        )?;
        next.generation = self.generation;
        next.stamp_gaps = self.stamp_gaps;
        for &(channel, freq) in aux {
            if let Err(error) = next.configure_aux(channel, true, freq) {
                // Hand the live capture back to the still-current plan.
                self.hardware = next.hardware.take();
                return Err(error);
            }
        }
        let drained = (|| -> Result<(), String> {
            self.stream.flush_pending().map_err(str::to_owned)?;
            let frames = self.drain()?;
            self.carry.extend(frames);
            let mut tails = Vec::new();
            self.finish_framers(&mut tails)?;
            self.carry.extend(tails);
            Ok(())
        })();
        if let Err(error) = drained {
            self.hardware = next.hardware.take();
            return Err(error);
        }
        let frames = std::mem::take(&mut self.carry);
        *self = next;
        Ok(frames)
    }

    #[cfg(test)]
    /// Retunes the IQS capture on the open handle. Later packets of the old generation are
    /// dropped; the fine plan restarts on `listen`, which must be inside the new capture.
    fn recapture(
        &mut self,
        center: f64,
        listen: f64,
        decimate: i32,
        display_decimate: i32,
        front_end: FrontEnd,
        taps_per_phase: usize,
    ) -> Result<(), String> {
        // Validate a complete replacement before changing either the device or the
        // running framers. A rejected plan leaves their sequence and tails usable.
        validate_channel_passband(center, listen, decimate, taps_per_phase)?;
        let frequencies: Vec<_> = self.aux.values().map(|(_, frequency, _)| *frequency).collect();
        let device_decimate = capture_decimate(center, listen, &frequencies, display_decimate, taps_per_phase);
        let fs = if self.hardware.is_some() { IQ_FS / device_decimate as f64 } else { self.stream.input_sample_rate() };
        let bandwidth = if self.hardware.is_some() { fs * 0.8 } else { self.usable_bandwidth };
        let aux: Vec<_> = self.aux.iter().map(|(&channel, (_, frequency, _))| (channel, *frequency))
            .filter(|(_, frequency)| (frequency-center).abs() <= bandwidth/2.0).collect();
        let frequencies: Vec<_> = aux.iter().map(|(_, frequency)| *frequency).collect();
        let mut next = Self::configured_with_cost_budget(center, listen, decimate, bandwidth, &mut None,
            &frequencies, taps_per_phase, fs, aux.len()+1, COARSE_RATE/decimate as f64*(aux.len()+1) as f64,
            self.cost_profile.clone())?;
        for &(channel, frequency) in &aux { next.configure_aux(channel, true, frequency)?; }
        next.stamp_gaps = self.stamp_gaps;
        next.generation = self.generation;
        if let Some(capture) = self.hardware.as_ref() {
            let geometry = match capture.reconfigure(center, front_end, device_decimate) {
                Ok(geometry) => geometry,
                Err(error) => { self.capture_failed = true; return Err(error); }
            };
            // Once the SDK accepted a profile, an incompatible geometry cannot resume
            // the old DSP plan. Report it as a fatal capture transition explicitly.
            if !geometry.center.is_finite() || !geometry.bandwidth.is_finite()
                || (geometry.center-center).abs() > 0.5
                || (geometry.bandwidth-bandwidth).abs() > 0.5 {
                self.capture_failed = true;
                return Err(format!("IQS geometry mismatch: requested center={center} bandwidth={bandwidth}, actual center={} bandwidth={}", geometry.center, geometry.bandwidth));
            }
            next.capture_center = geometry.center;
            next.usable_bandwidth = geometry.bandwidth;
            next.generation = geometry.generation;
            next.device_decimate = device_decimate as u64;
        }
        let drained = (|| -> Result<Vec<Vec<u8>>, String> {
            self.stream.flush_pending().map_err(str::to_owned)?;
            let mut tails = std::mem::take(&mut self.carry);
            tails.extend(self.drain()?);
            self.finish_framers(&mut tails)?;
            Ok(tails)
        })();
        next.carry = match drained {
            Ok(tails) => tails,
            Err(error) => { self.capture_failed = true; return Err(error); }
        };
        next.hardware = self.hardware.take();
        *self = next;
        Ok(())
    }

    fn channel_rate(&self) -> f64 {
        self.registry.iter().find(|row| row.channel == 1).map_or_else(
            || self.stream.sample_rate / self.decimate as f64, |row| row.output_rate)
    }

    #[cfg(test)]
    /// Moves the primary channel. Inside the provisioned bins only the fine channel changes;
    /// a bin outside the plan, or a new decimation, builds a new plan on the same capture.
    fn tune(&mut self, listen: f64, decimate: i32) -> Result<Vec<Vec<u8>>, String> {
        validate_channel_passband(self.capture_center, listen, decimate, self.stream.taps_per_phase())?;
        sdr_pipeline::check_plan_admission(&self.cost_profile, decimate, 1 + self.aux.len(),
            1 + self.aux.len(), COARSE_RATE / decimate as f64 * (1 + self.aux.len()) as f64)?;
        let signed = signed_bin(listen, self.capture_center);
        let m = self.stream.bin_count() as i64;
        let bin = signed.rem_euclid(m) as u32;
        if decimate != self.decimate
            || !(-m / 2..m / 2).contains(&signed)
            || !self.stream.bins().contains(&bin)
        {
            return self.replan(listen, decimate);
        }
        self.bank.check_admission(Some(self.channel_id), decimate).map_err(str::to_owned)?;
        let offset = signed as f64 * BIN_SPACING - (listen - self.capture_center);
        let id = self
            .next_id
            .checked_add(1)
            .filter(|&id| id <= u32::MAX as u64)
            .ok_or("fine channel ID exhausted")?;
        let next = BasebandFramer::new(id, self.stream.epoch(), listen, self.channel_rate())
            .ok_or("baseband frame metadata rejected")?;
        self.stream.flush_pending().map_err(str::to_owned)?;
        let mut frames = std::mem::take(&mut self.carry);
        frames.extend(self.drain()?);
        let old_id = self.channel_id;
        self.bank
            .retune(old_id, id, bin, offset, decimate)
            .map_err(str::to_owned)?;
        if let Some(tail) = self.framer.finish().map_err(str::to_owned)? {
            frames.push(tail);
        }
        self.framer = next;
        self.channel_id = id;
        self.next_id = id;
        self.listen = listen;
        self.refresh_registry();
        Ok(frames)
    }

    fn configure_aux(&mut self, channel: u32, enabled: bool, freq: f64) -> Result<(), String> {
        if enabled {
            validate_channel_passband(self.capture_center, freq, self.decimate, self.stream.taps_per_phase())?;
        }
        if channel < 2 {
            return Err("aux channel unavailable".into());
        }
        if !enabled && !self.aux.contains_key(&channel) {
            return Ok(());
        }
        let mut bin = self.stream.bins()[0];
        let offset = if enabled {
            let spacing = 62_500_000.0 / 128.0;
            let signed = ((freq - self.capture_center) / spacing).round() as i64;
            let m = self.stream.bin_count() as i64;
            let selected = signed.rem_euclid(m) as u32;
            if !freq.is_finite()
                || !(-m / 2..m / 2).contains(&signed)
                || !self.stream.bins().contains(&selected)
                || (freq - self.capture_center).abs() + self.channel_rate() / 2.0
                    > self.usable_bandwidth / 2.0
                || self
                    .aux
                    .iter()
                    .any(|(&other, (_, tuned, _))| other != channel && (freq - tuned).abs() < 1.0)
            {
                return Err("aux frequency outside provisioned coarse plan or bandwidth".into());
            }
            bin = selected;
            signed as f64 * spacing - (freq - self.capture_center)
        } else {
            0.0
        };
        if enabled
            && self
                .aux
                .get(&channel)
                .is_some_and(|(_, tuned, _)| *tuned == freq)
        {
            return Ok(());
        }
        if enabled {
            self.bank.check_admission(self.aux.get(&channel).map(|(id, _, _)| *id), self.decimate)
                .map_err(str::to_owned)?;
        }
        let id = if enabled {
            self.next_id.checked_add(1).filter(|&id| id <= u32::MAX as u64)
                .ok_or("fine channel ID exhausted")?
        } else { self.next_id };
        let next = enabled
            .then(|| BasebandFramer::new_aux(id, self.stream.epoch(), freq, self.channel_rate()))
            .flatten();
        if enabled && next.is_none() {
            return Err("aux frame metadata rejected".into());
        }
        self.stream.flush_pending().map_err(str::to_owned)?;
        let old = self.drain()?;
        self.carry.extend(old);
        let decimate = self.decimate;
        if enabled {
            let old_id = self.aux.get(&channel).map(|(id, _, _)| *id);
            if let Some(old_id) = old_id {
                self.bank.retune(old_id, id, bin, offset, decimate)
            } else {
                self.bank.subscribe(id, bin, offset, decimate)
            }
            .map_err(str::to_owned)?;
        } else {
            let old_id = self.aux[&channel].0;
            if !self.bank.unsubscribe(old_id) {
                return Err("aux channel not active".into());
            }
        }
        if let Some((_, _, mut framer)) = self.aux.remove(&channel) {
            if let Some(tail) = framer.finish().map_err(str::to_owned)? {
                self.carry.push(tail);
            }
        }
        if let Some(framer) = next {
            self.aux.insert(channel, (id, freq, framer));
            self.next_id = id;
        }
        self.refresh_registry();
        Ok(())
    }

    /// Hardware publishes every 8 ms; IQ framing accumulates 30 blocks/s. Fake mode keeps
    /// its 85 ms source interval.
    fn step(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let packets = if self.hardware.is_some() { 32 } else { 329 };
        let mut frames = std::mem::take(&mut self.carry);
        for _ in 0..packets {
            frames.extend(self.packet()?);
        }
        Ok(frames)
    }

    fn packet(&mut self) -> Result<Vec<Vec<u8>>, String> {
        const N: usize = 16240;
        let (raw, stamp) = if let Some(capture) = &self.hardware {
            let packet = loop {
                if let Some((received, seq, stamp, input_index)) = self.pipeline_timing.received.take() {
                    let elapsed = received.elapsed();
                    self.pipeline_timing.consumer_gap.observe(elapsed);
                    self.pipeline_timing.observe_slow("consumer_gap", seq, stamp, input_index, elapsed);
                }
                let packet = capture.next()?;
                self.pipeline_timing.received = Some((std::time::Instant::now(), self.next_seq, packet.stamp, self.input_pos));
                // Packets queued before an IQS reconfigure belong to the old centre.
                if packet.generation >= self.generation {
                    break packet;
                }
            };
            if let Some(last) = self.last_stamp {
                // Packet stamps count raw sample clocks: two ticks per complex sample
                // times device decimation.
                let expected = 2 * N as u64 * self.device_decimate;
                let delta = packet.stamp.saturating_sub(last);
                if delta.abs_diff(expected) > 8 {
                    self.stamp_gaps += 1;
                    eprintln!(
                        "IQS stamp gap seq={} last={} current={} delta={}",
                        self.next_seq, last, packet.stamp, delta
                    );
                    return Err(format!("IQS stamp discontinuity seq={} delta={delta} expected={expected} last={last} current={}", self.next_seq, packet.stamp));
                }
            }
            self.last_stamp = Some(packet.stamp);
            if let Some((freq, power)) = packet.spectrum {
                self.pan.push((freq, power, packet.warning, packet.generation, packet.seq));
            }
            (packet.iq, packet.stamp)
        } else {
            let mut raw = vec![0i16; N * 2];
            for i in 0..N {
                let t = (self.input_pos + i as u64) as f64 / 62_500_000.0;
                let envelope = 0.25 * (1.0 + 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * t).cos());
                raw[2 * i] = (envelope * 20_000.0) as i16;
            }
            (raw, self.input_pos + N as u64)
        };
        self.input_pos += N as u64;
        let seq = self.next_seq;
        let mut frames = Vec::new();
        let mut retries = 0;
        loop {
            let started = std::time::Instant::now();
            let submitted = self.stream.submit(seq, stamp, &raw);
            let elapsed = started.elapsed();
            self.pipeline_timing.submit.observe(elapsed);
            self.pipeline_timing.observe_slow("pfb_submit", seq, stamp, self.input_pos - N as u64, elapsed);
            match submitted {
                Ok(()) => break,
                Err(-2) if retries < 8 => {
                    retries += 1;
                    frames.extend(self.drain()?);
                }
                Err(code) => return Err(format!("PFB submit status={code} seq={seq}")),
            }
        }
        self.next_seq += 1;
        frames.extend(self.drain()?);
        Ok(frames)
    }

    fn drain(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let mut frames = Vec::new();
        while let Some(packet) = self.stream.take() {
            if let Some(worker) = self.fine_worker.as_mut() {
                let (seq, stamp, input_index) = (packet.seq, packet.stamp, packet.input_index);
                let started = std::time::Instant::now();
                let submitted = worker.submit(packet, &mut frames);
                let elapsed = started.elapsed();
                self.pipeline_timing.fine_handoff.observe(elapsed);
                self.pipeline_timing.observe_slow("fine_handoff", seq, stamp, input_index, elapsed);
                submitted?;
            } else {
                frames.extend(self.fine.as_mut().unwrap().deliver(packet)?);
            }
        }
        if let Some(worker) = self.fine_worker.as_mut() {
            worker.collect(&mut frames)?;
        }
        Ok(frames)
    }
}

impl std::ops::Deref for Ddc {
    type Target = sdr_pipeline::FineState;
    fn deref(&self) -> &Self::Target { self.fine.as_ref().expect("fine stage must be drained before control") }
}
impl std::ops::DerefMut for Ddc {
    fn deref_mut(&mut self) -> &mut Self::Target { self.fine.as_mut().expect("fine stage must be drained before control") }
}

fn hardware_pan(
    version: u32,
    center: f64,
    bandwidth: f64,
    freq: &[f64],
    power: &[f32],
) -> Result<Vec<u8>, String> {
    let lo = center - bandwidth / 2.0;
    let hi = center + bandwidth / 2.0;
    if freq.len() != power.len() || freq.len() < 2 || !freq.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err("SDK FFT frequency grid invalid".into());
    }
    let first = freq.partition_point(|&hz| hz < lo);
    let last = freq.partition_point(|&hz| hz <= hi);
    if last < first + 2 {
        return Err("SDK FFT has no usable IQ bins".into());
    }
    let (freq, power) = (&freq[first..last], &power[first..last]);
    let waterfall: Vec<u16> = (0..224)
        .map(|row| {
            let from = row * power.len() / 224;
            let to = (row + 1) * power.len() / 224;
            let peak = power[from..to].iter().copied().fold(-120.0f32, f32::max);
            ((peak + 120.0) * (4095.0 / 100.0)).clamp(0.0, 4095.0) as u16
        })
        .collect();
    encode_rta(version, freq, power, &waterfall, 4095, lo, hi).map_err(str::to_owned)
}

/// Shrink a span around a fixed centre (mirrors web_sa/config.py fit_span).
fn fit_span(center: f64, span: f64, fmin: f64, fmax: f64) -> f64 {
    let symmetric = 2.0 * ((center - fmin).min(fmax - center));
    span.min(symmetric).max(100.0)
}

/// Python config.py fit_center_span: clamp the centre, shrink the span to stay symmetric.
fn fit_center_span(center: f64, span: f64, fmin: f64, fmax: f64) -> (f64, f64) {
    const MIN_SPAN: f64 = 100.0;
    let center = center.clamp(fmin + MIN_SPAN / 2.0, fmax - MIN_SPAN / 2.0);
    let symmetric = 2.0 * (center - fmin).min(fmax - center);
    (center, span.min(symmetric).max(MIN_SPAN))
}

/// Python config.py fit_start_stop: clamp both edges, return (center, span).
fn fit_start_stop(start: f64, stop: f64, fmin: f64, fmax: f64) -> (f64, f64) {
    const MIN_SPAN: f64 = 100.0;
    let mut start = start.clamp(fmin, fmax);
    let mut stop = stop.clamp(fmin, fmax);
    if stop <= start {
        stop = (start + MIN_SPAN).min(fmax);
        if stop <= start {
            start = (stop - MIN_SPAN).max(fmin);
        }
    }
    ((start + stop) / 2.0, stop - start)
}

/// Python rta.py set_params: power-of-two decimate from the 50.78125 MHz full span.
fn rta_decimate(span: f64) -> u32 {
    let ratio = (50_781_250.0 / span).max(1.0);
    2u32.pow((ratio.log2().round() as u32).min(16))
}

fn set_path(value: &mut Value, path: &str, new: Value) {
    let mut parts = path.trim_start_matches('/').split('/');
    let mut at = match value.get_mut(parts.next().unwrap()) {
        Some(v) => v,
        None => return,
    };
    for part in parts {
        at = match at.get_mut(part) {
            Some(v) => v,
            None => return,
        };
    }
    *at = new;
}

fn num(data: &Value, key: &str) -> Option<f64> {
    data.get(key).and_then(Value::as_f64)
}

/// Validate `data` against the golden schema entry for `cmd` (type/required/min/max/choices).
fn validate(schema: &Value, cmd: &str, data: &Value) -> Result<(), (&'static str, String)> {
    let entry = schema
        .get("commands")
        .and_then(|c| c.get(cmd))
        .ok_or_else(|| ("unknown_command", "unknown or missing command".to_string()))?;
    for param in entry["params"].as_array().cloned().unwrap_or_default() {
        let name = param["name"].as_str().unwrap_or_default().to_string();
        let value = data.get(&name);
        if value.is_none() || value == Some(&Value::Null) {
            if param["required"].as_bool().unwrap_or(false) {
                return Err(("missing_parameter", format!("{cmd}: {name} is required")));
            }
            continue;
        }
        let value = value.unwrap();
        let bad = || {
            (
                "bad_parameter",
                format!("{cmd}: {name} has an invalid value"),
            )
        };
        match param["type"].as_str().unwrap_or_default() {
            "number" | "integer" => {
                let number = value.as_f64().ok_or_else(bad)?;
                if kind_integer(&param) && value.as_i64().is_none() {
                    return Err(bad());
                }
                if let Some(min) = param["min"].as_f64() {
                    if number < min {
                        return Err(bad());
                    }
                }
                if let Some(max) = param["max"].as_f64() {
                    if number > max {
                        return Err(bad());
                    }
                }
                if let Some(choices) = param["choices"].as_array() {
                    if !choices.is_empty() && !choices.contains(value) {
                        return Err(bad());
                    }
                }
            }
            "choice" | "token" => {
                let token = value.as_str().ok_or_else(bad)?;
                if param["min_length"].as_u64().is_some_and(|min| token.len() < min as usize)
                    || param["max_length"].as_u64().is_some_and(|max| token.len() > max as usize)
                {
                    return Err(bad());
                }
                let choices = param["choices"].as_array().cloned().unwrap_or_default();
                if !choices.is_empty() && !choices.contains(value) {
                    return Err(bad());
                }
            }
            "boolean" => {
                if !value.is_boolean() {
                    return Err(bad());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn kind_integer(param: &Value) -> bool {
    param["type"] == json!("integer")
}

/// Commands that a harmonic/PNM measurement owns (Python commands.py SWP_OWNED).
const SWP_OWNED: [&str; 15] = [
    "SET_REF",
    "AUTO_SCALE",
    "SET_FREQ",
    "SET_RBW",
    "SET_VBW",
    "SET_SWEEP",
    "SET_POINTS",
    "SET_SPUR",
    "SET_WINDOW",
    "SET_DETECTOR",
    "SET_FRAMEDET",
    "SET_TRACEGRID",
    "SET_AMP",
    "SET_REFCK",
    "SET_REFCKOUT",
];

fn sdr_mode_required(state: &DevState, cmd: &str) -> Result<(), CommandError> {
    if state.swp.mode == "sdr" {
        Ok(())
    } else {
        Err(CommandError::new(
            "sdr_mode_required",
            &format!("{cmd} requires SDR mode"),
        ))
    }
}

impl DevState {
    /// The IF channel cannot exceed the displayed capture window; Python uses 40 %
    /// of it as the upper bound and never goes below the 200 Hz fine minimum.
    fn effective_if_bw(&self, usable: f64) -> f64 {
        let window = (2.0 * display_half_width(self.sdr_decimate as i32)).min(usable).max(1.0);
        self.sdr_if_bw.min(window * 0.4).max(200.0)
    }

}

#[derive(Debug)]
struct CommandError {
    code: &'static str,
    message: String,
    params: Value,
}

impl CommandError {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
            params: json!({}),
        }
    }

    fn param(mut self, key: &str, value: &str) -> Self {
        self.params[key] = json!(value);
        self
    }

    fn payload(&self) -> Value {
        let mut payload = json!({"cmd": "ERROR", "msg": self.message, "code": self.code});
        if self.params.as_object().is_some_and(|p| !p.is_empty()) {
            payload["params"] = self.params.clone();
        }
        payload
    }

    fn reply_payload(&self, cmd: &str) -> Value {
        command_error_reply(self.payload(), cmd)
    }
}

fn dispatch(state: &mut DevState, cmd: &str, data: &Value) -> Result<bool, CommandError> {
    let previous = matches!(cmd, "SET_FREQ" | "SET_RTA" | "SET_RBW" | "SET_VBW"
        | "SET_SWEEP" | "SET_POINTS" | "SET_SPUR" | "SET_WINDOW" | "SET_DETECTOR"
        | "SET_FRAMEDET" | "SET_TRACEGRID" | "SET_REF" | "SET_AMP" | "SET_REFCK"
        | "SET_REFCKOUT" | "SET_PRESET").then(|| RequestedConfiguration::capture(state));
    let result = dispatch_inner(state, cmd, data);
    if result.is_err() {
        if let Some(previous) = previous { previous.restore(state); }
    } else if cmd.starts_with("SET_") && !(cmd == "SET_REF" && data["mode"] == "auto") {
        state.invalidate_observation();
    }
    result
}

fn dispatch_inner(state: &mut DevState, cmd: &str, data: &Value) -> Result<bool, CommandError> {
    if !data.is_object() {
        return Err(CommandError::new(
            "json_object_required",
            "JSON message must be an object",
        ));
    }
    if state.swp.mode == "server" && !matches!(cmd, "STATUS" | "SET_MODE" | "SET_SERVER") {
        return Err(CommandError::new("server_mode_active", "leave server mode before changing hardware settings"));
    }
    if matches!(cmd,"SET_SDR_CHANNEL"|"SET_SDR_AUX"|"SET_SDR_DEMOD"|"SET_SDR_TUNE"|"SET_SDR_DDC") {
        sdr_mode_required(state,cmd)?;
        if matches!(cmd,"SET_SDR_AUX"|"SET_SDR_DDC") && !matches!(data["channel"].as_u64(),Some(2|3)) {
            return Err(CommandError::new("bad_parameter","aux channel must be 2 or 3"));
        }
        let restart=state.receiver_failure.is_some()
            && ["mode","demod"].iter().any(|field|data.get(*field).is_some());
        if restart && !state.reap_failed_receivers() {
            return Err(CommandError::new("receiver_retiring","Receiver is still stopping; retry shortly"));
        }
        state.apply_channel_config(data).map_err(|error|CommandError::new("bad_parameter",&error))?;
        if state.hardware_shim.is_none() {
            let configs=state.channel_configs();
            state.fixture_sources.get_or_insert_with(service_fixture::FixtureSources::new)
                .configure(&configs).map_err(|error|CommandError::new("device_error",&error))?;
        }
        if restart {state.restart_failed_receivers().map_err(|error|CommandError::new("device_error",&error))?;}
        return Ok(true);
    }
    let Some(spec) = state.schema["commands"].get(cmd).cloned() else {
        return Err(CommandError::new(
            "unknown_command",
            "unknown or missing command",
        ));
    };
    // Python commands.py validate(): device link, measurement ownership, mode guards.
    if spec["needs_device"] == true && state.hardware_shim.is_some() && !state.link_up
        && !(cmd == "SET_MODE" && (state.swp.mode == "server" || data["mode"] == "server")) {
        return Err(CommandError::new(
            "device_not_connected",
            "device is not connected",
        ));
    }
    let mode = state.swp.mode.clone();
    if matches!(mode.as_str(), "harmonic" | "pnm") && SWP_OWNED.contains(&cmd) {
        return Err(CommandError::new(
            "cmd_unavailable_measurement",
            &format!("{cmd} is not available while the {mode} measurement is active"),
        )
        .param("cmd", cmd)
        .param("session", &mode));
    }
    if mode == "sdr" && spec["denied_in_sdr"] == true {
        return Err(CommandError::new(
            "sdr_unsupported",
            &format!("{cmd} is not available in SDR mode"),
        )
        .param("cmd", cmd));
    }
    if mode == "rta" && spec["swp_only"] == true {
        return Err(
            CommandError::new("swp_only", &format!("{cmd} is only available in SWP mode"))
                .param("cmd", cmd),
        );
    }
    if let Err((code, message)) = validate(&state.schema, cmd, data) {
        return Err(CommandError::new(code, &message));
    }
    let device = |error: String| CommandError::new("device_error", &error);
    match cmd {
        "SET_SERVER" => {
            if mode != "server" {
                return Err(CommandError::new("server_mode_required", "SET_SERVER requires server mode"));
            }
            // Start/restart can only run after SET_MODE has joined every owner
            // and released the physical SDK session.
            server_mode::parse_bind(data, "127.0.0.1".parse().unwrap(), 55132)
                .map_err(|error| CommandError::new("bad_parameter", &error))?;
            if matches!(data["action"].as_str(), Some("start" | "restart")) {
                hardware_swp::owner_slot_free().map_err(device)?;
            }
            let changed = state.server.lock().command(data)
                .map_err(|(code, message)| CommandError::new(code, &message))?;
            if changed { state.config_version += 1; }
            Ok(changed)
        }
        "CONNECT" => {
            if state.hardware_shim.is_none() || state.link_up {
                return Ok(false);
            }
            // Retry the open now; the next acquisition tick re-enters the active mode.
            state.swp_retry_at = None;
            state.rta_retry_at = None;
            state.ddc_retry_at = None;
            state.pnm_retry_at = None;
            Ok(true)
        }
        "SET_MODE" => {
            let next = data["mode"].as_str().unwrap_or_default();
            if !matches!(next, "std" | "rta" | "sdr" | "harmonic" | "pnm" | "server") {
                return Ok(false);
            }
            if mode == next { return Ok(false); }
            if mode == "server" {
                // Keep server mode and its exclusive reservation if stopping
                // fails; acquisition must not overlap an unreaped child.
                state.server.lock().stop().map_err(device)?;
            }
            if !state.reap_failed_receivers() {return Err(CommandError::new("receiver_retiring","Receiver is still stopping; retry shortly"));}
            if mode != next {
                // One vendor session at a time: drop (join) the old owner before the next opens.
                let registry=state.stream_registry();
                // Retirement is irreversible even if a later SDK handoff fails.
                // Fence detached presentations before stopping any owner.
                state.config_version += 1;
                state.freq_version += 1;
                state.invalidate_observation();
                state.cancel_auto_session();
                state.sdr_actual = json!({});
                state.actual = json!({});
                state.rta_actual = json!({});
                if next == "server" {
                    state.gnss_value = None;
                } else if let Some(gnss) = state.gnss_value.as_mut() {
                    gnss["stale"] = json!(true);
                }
                state.channel_power = [None; 3];
                state.stop_pipeline().map_err(device)?;
                state.finish_radiofax_with_registry(registry).map_err(device)?;
                state.ddc_failure_diagnostics = None;
                // SET_MODE ends the IQ session. Never replay its retained tails
                // when SDR is opened again under a later transport epoch.
                state.pending_iq.clear();
                state.last_freq = None;
                state.ddc = None;
                state.synthetic_iq = None;
                state.fixture_sources=None;
                // A deliberate mode change retires the stream, while healthy
                // workers retain their prepared models for the next SDR entry.
                if let Some(workers) = state.receiver_workers.as_mut() {
                    workers.reset_session().map_err(device)?;
                }
                state.receiver_failure = None;
                state.receiver_failure_pending = false;
                state.decode_snapshots=service_stream::DecodeSnapshots::default();
                state.consumed_receiver_iq.clear();
                state.channel_power = [None; 3];
                state.rta_hw = None;
                state.clear_rta_trigger(state.hardware_shim.is_none());
                state.swp_hw = None;
                state.pnm_hw = None;
                state.swp_error = None;
                state.pnm_error = None;
                state.pnm_retry_at = None;
                state.harm_window = None;
                state.ddc_error = None;
                state.ddc_retry_at = None;
                state.rta_error = None;
                state.rta_retry_at = None;
                state.swp_retry_at = None;
                state.status_warning = 0;
                if next == "server" && state.hardware_shim.is_some() {state.link_up = false;}
                if next == "server" {
                    hardware_swp::owner_slot_free().map_err(device)?;
                    if state.hardware_shim.is_some() {
                        hardware_swp::release_sdk_session().map_err(device)?;
                    }
                    let mut server = state.server.lock();
                    server.remember_graph(&mode);
                    server.refresh_dependencies();
                    drop(server);
                }
                let was_measure = matches!(mode.as_str(), "harmonic" | "pnm");
                let is_measure = matches!(next, "harmonic" | "pnm");
                if !was_measure && is_measure && state.swp_restore.is_none() {
                    // Measurement modes drive the same SWP device; Python snapshots the
                    // user's window on enter and restores it on exit.
                    state.swp_restore = Some((state.swp.center, state.swp.span));
                } else if was_measure && !is_measure {
                    if let Some((center, span)) = state.swp_restore.take() {
                        state.swp.center = center;
                        state.swp.span = span;
                    }
                }
                if next == "harmonic" && mode != "harmonic" {
                    // Fresh HarmonicSession: documented defaults and a new scan.
                    state.harm_f0 = 1e9;
                    state.harm_count = 5;
                    state.harm_span = 10e6;
                    state.harm_session_id = None;
                    state.harm_active_session_id = None;
                    state.harm_scan = true;
                    state.harm_scan_cnt = 0;
                    state.harm_seq.clear();
                    state.harm_n = 1;
                    state.harm_base = None;
                    state.harm_window = None;
                }
                if next == "pnm" && mode != "pnm" {
                    // Fresh PhaseNoiseSession defaults, not the abandoned cycle's state.
                    state.pnm_center = 1e9;
                    state.pnm_threshold = -50.0;
                    state.pnm_traceavg = 4;
                    state.pnm_start = 100.0;
                    state.pnm_stop = 10e6;
                    state.pnm_session_id = None;
                    state.pnm_active_session_id = None;
                    state.pnm_i = 0;
                    state.pnm_last = Value::Null;
                }
                state.invalidate_observation();
                if next == "rta" && !matches!(state.trigger.source.as_str(), "bus" | "freerun") {
                    // Python rta.py enter(): RTA always starts in free run.
                    state.trigger.source = "bus".into();
                }
            }
            state.swp.mode = next.into();
            if next == "sdr" {
                if let Err(error) = state.rebind_synthetic_iq() { return Err(device(error.to_string())); }
            }
            // Versions advanced at the retirement boundary, including failure.
            Ok(true)
        }
        "SET_HARM" => {
            if mode != "harmonic" {
                return Ok(false);
            }
            if let Some(v) = num(data, "f0") {
                state.harm_f0 = v.clamp(9_000.0, 9e9);
            }
            if let Some(v) = num(data, "count") {
                state.harm_count = (v as i64).clamp(1, 10);
            }
            if let Some(v) = num(data, "span") {
                state.harm_span = v.clamp(1.0, 100e6);
            }
            state.harm_session_id = data.get("session_id").and_then(Value::as_str).map(str::to_owned);
            state.harm_scan = true;
            state.harm_scan_cnt = 0;
            state.harm_seq.clear();
            state.harm_window = None;
            Ok(true)
        }
        "SET_PNM" => {
            if mode != "pnm" {
                return Ok(false);
            }
            if let Some(v) = num(data, "center") {
                state.pnm_center = v;
            }
            if let Some(v) = num(data, "threshold") {
                state.pnm_threshold = v;
            }
            if let Some(v) = num(data, "traceavg") {
                state.pnm_traceavg = v as i64;
            }
            let start = num(data, "start").map_or(state.pnm_start, |v| v.max(1.0));
            let stop = num(data, "stop").map_or(state.pnm_stop, |v| v.max(10.0));
            if stop <= start {
                return Err(CommandError::new(
                    "bad_parameter",
                    "PNM stop must be greater than start",
                ));
            }
            state.pnm_start = start;
            state.pnm_stop = stop;
            state.pnm_session_id = data.get("session_id").and_then(Value::as_str).map(str::to_owned);
            if state.hardware_shim.is_none() {
                state.pnm_i = 0;
            } else {
                // The open handle carries the profile: keep the running cycle and
                // apply the new profile when the next one starts.
                state.pnm_dirty = true;
            }
            state.pnm_error = None;
            state.pnm_retry_at = None;
            Ok(true)
        }
        "SET_RTA" => {
            if mode != "rta" {
                return Err(CommandError::new(
                    "rta_mode_required",
                    "SET_RTA requires RTA mode",
                ));
            }
            let (fmin, fmax) = state.freq_limits();
            if let Some(span) = num(data, "span") {
                state.rta_decimate = rta_decimate(span);
                state.rta_span = 50_781_250.0 / state.rta_decimate as f64;
            }
            let half = state.rta_span / 2.0;
            let center = num(data, "center").unwrap_or(state.rta_center);
            state.rta_center = center.clamp(fmin + half, fmax - half);
            state.configure_rta().map_err(device)?;
            Ok(true)
        }
        "SET_TRIGGER" => {
            let mut candidate = state.trigger.clone();
            let t = &mut candidate;
            for (key, field) in [
                ("source", &mut t.source),
                ("edge", &mut t.edge),
                ("out", &mut t.out),
                ("outpolarity", &mut t.out_polarity),
            ] {
                if let Some(value) = data[key].as_str() {
                    *field = value.into();
                }
            }
            for (key, field) in [
                ("level", &mut t.level_dbm),
                ("safetime", &mut t.safe_time),
                ("delay", &mut t.delay),
                ("pretime", &mut t.pre_time),
                ("acqtime", &mut t.acq_time),
                ("retriggerperiod", &mut t.retrigger_period),
            ] {
                if let Some(value) = num(data, key) {
                    *field = value;
                }
            }
            if let Some(value) = num(data, "retrigger") {
                t.retrigger_count = value as u32;
            }
            state.configure_trigger(candidate).map_err(device)?;
            Ok(true)
        }
        "SET_SDR" => {
            sdr_mode_required(state,cmd)?;
            state.apply_service_capture(data).map_err(device)?;
            Ok(true)
        }
        "SET_FREQ" => {
            let (has_start, has_stop) = (
                data.get("start").is_some_and(Value::is_number),
                data.get("stop").is_some_and(Value::is_number),
            );
            let (has_center, has_span) = (
                data.get("center").is_some_and(Value::is_number),
                data.get("span").is_some_and(Value::is_number),
            );
            if has_start != has_stop
                || ((has_start || has_stop) && (has_center || has_span))
                || (!has_start && !has_center && !has_span)
            {
                return Err(CommandError::new(
                    "bad_parameter",
                    "SET_FREQ takes either start+stop or center+span",
                ));
            }
            let (fmin, fmax) = state.freq_limits();
            let window = if let (Some(start), Some(stop)) =
                (num(data, "start"), num(data, "stop"))
            {
                fit_start_stop(start, stop, fmin, fmax)
            } else {
                fit_center_span(
                    num(data, "center").unwrap_or(state.swp.center),
                    num(data, "span").unwrap_or(state.swp.span),
                    fmin,
                    fmax,
                )
            };
            if window.1 < 100.0 {
                return Err(CommandError::new(
                    "bad_parameter",
                    "span must be at least 100 Hz",
                ));
            }
            (state.swp.center, state.swp.span) = window;
            // In RTA the swept window is a stored preference (Python _h_set_freq).
            if mode != "rta" {
                state.configure_swp().map_err(device)?;
            }
            Ok(true)
        }
        "SET_POINTS" => {
            state.swp.points = num(data, "points").unwrap_or(state.swp.points as f64) as i64;
            state.configure_swp().map_err(device)?;
            Ok(true)
        }
        "SET_RBW" => {
            if mode == "rta" {
                state.rta_rbw_mode = if data["mode"] == "manual" {
                    "manual"
                } else {
                    "auto"
                }
                .into();
                state.rta_rbw = if state.rta_rbw_mode == "manual" {
                    num(data, "rbw").unwrap_or(0.0)
                } else {
                    0.0
                };
                state.configure_rta().map_err(device)?;
                return Ok(true);
            }
            if let Some(mode) = data["mode"].as_str() {
                state.swp.rbw_mode = mode.into();
            }
            if let Some(rbw) = num(data, "rbw") {
                state.swp.rbw = rbw;
            }
            state.configure_swp().map_err(device)?;
            Ok(true)
        }
        "SET_VBW" => {
            if mode == "rta" {
                state.rta_vbw_mode = data["mode"].as_str().unwrap_or("equal").into();
                state.rta_vbw = if state.rta_vbw_mode == "manual" {
                    num(data, "vbw").unwrap_or(0.0)
                } else {
                    0.0
                };
                state.configure_rta().map_err(device)?;
                return Ok(true);
            }
            if let Some(mode) = data["mode"].as_str() {
                state.swp.vbw_mode = mode.into();
            }
            if let Some(vbw) = num(data, "vbw") {
                state.swp.vbw = vbw;
            }
            state.configure_swp().map_err(device)?;
            Ok(true)
        }
        "SET_SWEEP" => {
            if mode == "rta" {
                if let Some(m) = num(data, "mode") {
                    state.rta_sweep_time_mode = (m as i64).clamp(0, 8);
                }
                if let Some(time) = num(data, "time") {
                    state.rta_sweep_time = time;
                }
                state.configure_rta().map_err(device)?;
                return Ok(true);
            }
            if let Some(m) = num(data, "mode") {
                state.swp.sweep_time_mode = m as i64;
            }
            if let Some(time) = num(data, "time") {
                state.swp.sweep_time = time.clamp(0.0, 1e6);
            }
            state.configure_swp().map_err(device)?;
            Ok(true)
        }
        "SET_SPUR" => {
            if let Some(m) = data["mode"].as_str() {
                state.swp.spur = m.into();
            }
            state.configure_swp().map_err(device)?;
            Ok(true)
        }
        "SET_WINDOW" => {
            if let Some(w) = num(data, "window") {
                state.swp.window = w as i64;
            }
            state.configure_swp().map_err(device)?;
            Ok(true)
        }
        "SET_DETECTOR" | "SET_FRAMEDET" | "SET_TRACEGRID" => {
            if let Some(m) = data["mode"].as_str() {
                if cmd == "SET_DETECTOR" {
                    state.swp.detector = m.into();
                } else {
                    state.swp.frame_detector = m.into();
                }
            }
            if let Some(align) = data["trace_align"].as_str() {
                state.swp.trace_align = align.into();
            }
            if let Some(strategy) = data["points_strategy"].as_str() {
                state.swp.points_strategy = strategy.into();
            }
            // RTA has no trace detector: keep the swept preference only.
            if mode != "rta" {
                state.configure_swp().map_err(device)?;
            }
            Ok(true)
        }
        "SET_REF" => {
            if data.get("mode").and_then(Value::as_str) == Some("auto") {
                if let Some(range) = num(data, "range_db") {state.swp.ref_range_db=range;}
                return Ok(state.auto_scale(num(data, "ref"), data["display_only"] == true));
            }
            // The declared display domain is -160..+40 dBm. caps.ref_min/ref_max
            // bound automatic reference *placement*, not a manual request; Python
            // stores the validated user value and reports the hardware readback.
            let value = num(data, "ref").map(|v| v.clamp(-160.0, 40.0));
            match mode.as_str() {
                "rta" => {
                    if let Some(v) = value {
                        state.rta_ref = v;
                    }
                    state.configure_rta().map_err(device)?;
                }
                "sdr" => {
                    if let Some(v) = value {
                        state.sdr_ref = v;
                        state.sdr_ref_set = true;
                    }
                    state.configure_sdr_capture().map_err(device)?;
                }
                _ => {
                    if let Some(v) = value {
                        state.swp.ref_level = v;
                    }
                    state.configure_swp().map_err(device)?;
                }
            }
            if state.auto.result != "cancelled" { state.auto.result = "idle".into(); }
            if let Some(range) = num(data, "range_db") {state.swp.ref_range_db=range;}
            Ok(true)
        }
        "AUTO_SCALE" => {
            if let Some(range) = num(data, "range_db") {
                state.swp.ref_range_db = range;
            }
            if !matches!(mode.as_str(), "std" | "rta" | "sdr") {
                return Ok(false);
            }
            Ok(state.auto_scale(num(data, "current_ref"), data["display_only"] == true))
        }
        "SET_AMP" => {
            if let Some(v) = num(data, "atten") {
                state.swp.atten = (v as i64).clamp(-1, 33);
            }
            if let Some(v) = num(data, "preamp") {
                state.swp.preamp = i64::from(v != 0.0);
            }
            if let Some(v) = num(data, "ifgain") {
                state.swp.ifgain = (v as i64).clamp(0, 3);
            }
            if let Some(v) = num(data, "gain_strategy") {
                state.swp.gain_strategy = i64::from(v != 0.0);
            }
            state.configure_active().map_err(device)?;
            Ok(true)
        }
        "SET_REFCK" => {
            let requested = data["mode"].as_str().unwrap_or("internal");
            if requested == "premium" && !state.has_premium_ref_clock() {
                return Err(CommandError::new("unsupported", "Premium reference clock is unavailable on this device"));
            }
            state.ref_clock = requested.into();
            state.configure_active().map_err(device)?;
            Ok(true)
        }
        "SET_REFCKOUT" => {
            state.refclk_out = data["on"].as_bool().unwrap_or(false);
            state.configure_active().map_err(device)?;
            Ok(true)
        }
        "SET_PRESET" => {
            state.preset().map_err(device)?;
            Ok(true)
        }
        "STATUS" => Ok(false),
        _ => Err(CommandError::new(
            "not_implemented",
            &format!("{cmd} is not available on this server"),
        )),
    }
}

struct App {
    state: StateMutex,
    clients: PLMutex<Vec<Client>>,
    next_client: AtomicU64,
    /// Bumped under the state lock when a command changes the transport session.
    epoch: Arc<AtomicU64>,
    identities: Arc<[AtomicU64;3]>,
    /// Display configuration fence; independent of the lossless IQ session epoch.
    display_version: Arc<AtomicU64>,
}

fn browser_access(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    origin.to_str().ok().and_then(|origin| {
        origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://"))
    }).is_some_and(|origin_host| origin_host == host)
}

fn error_body(msg: &str, code: &str) -> Value {
    json!({"error": msg, "code": code})
}

fn command_error_reply(mut payload: Value, cmd: &str) -> Value {
    if !cmd.is_empty() {
        payload["response_to"] = json!(cmd);
    }
    payload
}

/// Request identity is transport metadata, never part of the published device state.
/// Scalars round-trip without coercion; composite values are deliberately ignored.
fn command_request_id(data: &Value) -> Option<Value> {
    data.get("request_id").filter(|id| !id.is_array() && !id.is_object()).cloned()
}

fn with_request_id(mut payload: Value, request_id: Option<&Value>) -> Value {
    if let Some(request_id) = request_id { payload["request_id"] = request_id.clone(); }
    payload
}

fn state_worker_failed(error: tokio::task::JoinError) -> StatusCode {
    eprintln!("WebSA state worker failed: {error}");
    StatusCode::SERVICE_UNAVAILABLE
}

async fn api_state(State(app): State<Arc<App>>) -> Result<Json<Value>, StatusCode> {
    let worker_app = app.clone();
    let status = tokio::task::spawn_blocking(move || worker_app.state.status_with_revision())
        .await
        .map_err(state_worker_failed)?;
    Ok(Json(app.state.current_status(status.0, status.1).1))
}

async fn api_schema(State(app): State<Arc<App>>) -> Result<Json<Value>, StatusCode> {
    let schema =
        tokio::task::spawn_blocking(move || app.state.lock().unwrap().schema.clone())
            .await
            .map_err(state_worker_failed)?;
    Ok(Json(schema))
}

async fn api_config(State(app): State<Arc<App>>, headers: HeaderMap, body: String) -> Response {
    if !browser_access(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(data) = serde_json::from_str::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(error_body("bad json", "bad_json")),
        )
            .into_response();
    };
    let cmd = data
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reply_app = app.clone();
    let response_to = cmd.clone();
    let request_id = command_request_id(&data);
    let revision=match service_preflight::preflight(&app,&cmd,&data).await {
        Ok(revision)=>revision,
        Err(error)=>return (StatusCode::BAD_REQUEST,Json(with_request_id(command_error_reply(error_body(&error,"bad_parameter"),&cmd),request_id.as_ref()))).into_response(),
    };
    let reply = match tokio::task::spawn_blocking(move || {
        let mut state = app.state.lock().unwrap();
        if state.config_version!=revision {return Err(CommandError::new("configuration_changed","configuration changed during admission; retry command"));}
        let same_capture = state.swp.mode == "sdr" && cmd != "SET_MODE" && cmd != "CONNECT";
        match dispatch(&mut state, &cmd, &data) {
            Ok(changed) => {
                if changed && !same_capture {
                    app.epoch.fetch_add(1, Ordering::AcqRel);
                }
                service_stream::deliver_retiring(&mut state,&app);
                app.display_version.store(state.freq_version, Ordering::Release);
                state.publish_identities(&app.identities);
                let snapshot = state.status_snapshot();
                drop(state);
                let status = snapshot.build();
                app.state.publish_status(snapshot.revision, status.clone());
                Ok((snapshot.revision, status, changed))
            }
            Err(error) => {
                if state.config_version != revision {
                    app.epoch.fetch_add(1, Ordering::AcqRel);
                    service_stream::deliver_retiring(&mut state,&app);
                    app.display_version.store(state.freq_version, Ordering::Release);
                    state.publish_identities(&app.identities);
                    let snapshot = state.status_snapshot();
                    drop(state);
                    let status = snapshot.build();
                    app.state.publish_status(snapshot.revision, status);
                }
                Err(error)
            },
        }
    })
    .await
    {
        Ok(reply) => reply,
        Err(error) => return state_worker_failed(error).into_response(),
    };
    match reply {
        Ok((revision, status, changed)) => {
            let (_, mut status) = reply_app.state.current_status(revision, status);
            status["changed"] = json!(changed);
            status["response_to"] = json!(response_to);
            status = with_request_id(status, request_id.as_ref());
            Json(status).into_response()
        }
        Err(error) => {
            let mut body = with_request_id(command_error_reply(error_body(&error.message, error.code), &response_to), request_id.as_ref());
            if error.params.as_object().is_some_and(|p| !p.is_empty()) {
                body["params"] = error.params.clone();
            }
            (StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
    }
}

fn morse_code(ch: char) -> &'static str {
    match ch.to_ascii_uppercase() {
        'A' => ".-",
        'B' => "-...",
        'C' => "-.-.",
        'D' => "-..",
        'E' => ".",
        'F' => "..-.",
        'G' => "--.",
        'H' => "....",
        'I' => "..",
        'J' => ".---",
        'K' => "-.-",
        'L' => ".-..",
        'M' => "--",
        'N' => "-.",
        'O' => "---",
        'P' => ".--.",
        'Q' => "--.-",
        'R' => ".-.",
        'S' => "...",
        'T' => "-",
        'U' => "..-",
        'V' => "...-",
        'W' => ".--",
        'X' => "-..-",
        'Y' => "-.--",
        'Z' => "--..",
        '0' => "-----",
        '1' => ".----",
        '2' => "..---",
        '3' => "...--",
        '4' => "....-",
        '5' => ".....",
        '6' => "-....",
        '7' => "--...",
        '8' => "---..",
        '9' => "----.",
        '/' => "-..-.",
        '.' => ".-.-.-",
        ',' => "--..--",
        '?' => "..--..",
        _ => "",
    }
}

fn cw_keying(text: &str, wpm: f64, rate: f64) -> Vec<f32> {
    let dot = (rate * 1.2 / wpm.max(1.0)) as usize;
    let mut key: Vec<f32> = Vec::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for (wi, word) in words.iter().enumerate() {
        for (ci, ch) in word.chars().enumerate() {
            let code = morse_code(ch);
            for (si, sym) in code.chars().enumerate() {
                let n = if sym == '-' { dot * 3 } else { dot };
                key.extend(std::iter::repeat_n(1.0f32, n));
                if si < code.len() - 1 {
                    key.extend(std::iter::repeat_n(0.0f32, dot));
                }
            }
            if ci < word.chars().count() - 1 {
                key.extend(std::iter::repeat_n(0.0f32, dot * 3));
            }
        }
        if wi < words.len() - 1 {
            key.extend(std::iter::repeat_n(0.0f32, dot * 7));
        }
    }
    key.extend(std::iter::repeat_n(0.0f32, dot * 10));
    key
}

/// Publisher: one acquisition step per mode, fanned out by each client's query filter.
async fn publisher(app: Arc<App>) {
    let mut status_due = tokio::time::Instant::now();
    loop {
        let started = tokio::time::Instant::now();
        let (iq_clients, aux_clients, display_clients, audio_clients,decode_clients) = {
            let clients = app.clients.lock();
            (
                clients.iter().filter(|c| c.iq).count(),
                clients.iter().filter(|c| c.aux).count(),
                clients
                    .iter()
                    .filter(|c| !c.iq && !c.aux && !c.audio && !c.decode)
                    .count(),
                clients.iter().filter(|c| c.audio).count(),
                clients.iter().filter(|c| c.decode).count(),
            )
        };
        let compute = app.clone();
        let send_status = status_due.elapsed() >= STATUS_PERIOD;
        let result = tokio::task::spawn_blocking(move || {
            let mut state = compute.state.lock().unwrap();
            let mode = state.swp.mode.clone();
            let epoch = compute.epoch.load(Ordering::Acquire);
            let mut display: Vec<Vec<u8>> = Vec::new();
            let mut iq_frame: Vec<Vec<u8>> = Vec::new();
            let mut aux_frame: Vec<Vec<u8>> = Vec::new();
            let mut msgs: Vec<Value> = Vec::new();
            let mut hardware = None;
            let acquisition_started = std::time::Instant::now();
            let pace = match mode.as_str() {
                "server" => {
                    state.server.lock().poll();
                    Duration::from_millis(100)
                }
                "harmonic" => {
                    let (mut frames, harm_msgs) = state.step_harm();
                    display.append(&mut frames);
                    msgs.extend(harm_msgs);
                    Duration::from_millis(20)
                }
                "pnm" => {
                    msgs.extend(state.step_pnm());
                    Duration::from_millis(50)
                }
                "rta" => {
                    display.push(state.step_rta());
                    if state.rta_completed_frame.is_some() { Duration::from_millis(200) }
                    else { Duration::from_millis(10) }
                }
                "sdr" if state.hardware_shim.is_some() => {
                    let listeners = iq_clients + aux_clients + display_clients + audio_clients + decode_clients
                        + usize::from(state.auto.session.is_some());
                    match state.take_hardware_presentation(listeners) {
                        Ok(output) => hardware = output,
                        Err(error) => state.hardware_presentation_failed(error),
                    }
                    Duration::from_millis(1)
                }
                "sdr" => {
                    let (pan,_,_,_)=state.step_sdr(0,0,display_clients,0);
                    display.extend(pan);
                    if iq_clients+aux_clients+audio_clients+decode_clients>0 {
                        let configs=state.channel_configs();
                        let source=state.fixture_sources.get_or_insert_with(service_fixture::FixtureSources::new);
                        match source.configure(&configs).and_then(|()|source.step()) {
                            Ok(frames)=>for frame in frames {
                                if frame.starts_with(b"IQAX") {aux_frame.push(frame);}
                                else if frame.starts_with(b"IQBF") {iq_frame.push(frame);}
                            },
                            Err(error)=>state.ddc_error=Some(error),
                        }
                    }
                    Duration::from_micros(85_333)
                }
                _ => {
                    let (freq, powr) = state.fetch_sweep();
                    if let Some(frame) = freq {
                        display.push(frame);
                    }
                    if !powr.is_empty() {
                        display.push(powr);
                    }
                    Duration::from_millis(10)
                }
            };
            if mode == "std" && display.iter().any(|frame| frame.starts_with(b"POWR")) {
                let ms = acquisition_started.elapsed().as_secs_f64() * 1000.0;
                state.sweep_ms_meas = if state.sweep_timing_version == Some(state.config_version) {
                    state.sweep_ms_meas * 0.8 + ms * 0.2
                } else {
                    ms
                };
                state.sweep_timing_version = Some(state.config_version);
            }
            let display_version = state.freq_version;
            drop(state);
            if let Some(hardware) = hardware {
                match hardware.finish() {
                    Ok(mut output) => {
                        compute.state.lock().unwrap().apply_hardware_presentation(&mut output);
                        display.extend(output.pan);
                        for frame in output.frames {
                            if frame.starts_with(b"IQAX") { aux_frame.push(frame); }
                            else { iq_frame.push(frame); }
                        }
                    }
                    Err(error) => compute.state.lock().unwrap().hardware_presentation_failed(error),
                }
            }
            if mode=="sdr" {
                let mut state=compute.state.lock().unwrap();
                state.publish_identities(&compute.identities);
                {
                    let result=if state.hardware_shim.is_some() {
                        state.drain_receiver_frames()
                    } else {
                        state.process_receiver_frames(&iq_frame).and_then(|mut a| {
                            let b=state.process_receiver_frames(&aux_frame)?;
                            a.audio.extend(b.audio); a.decode.extend(b.decode); Ok(a)
                        })
                    };
                    match result {
                        Ok(output)=>service_stream::deliver_receiver(&state,&compute,output,false),
                        Err(error)=>{
                            if let Some(message)=service_stream::fail_receiver_session(&mut state,&compute,error) {
                                msgs.push(message);
                            }
                        }
                    }
                }
                state.reap_failed_receivers();
                if let Some(message)=service_stream::publish_receiver_failure(&mut state,&compute) {msgs.push(message);}
            }
            let auto_changed = {
                let mut state = compute.state.lock().unwrap();
                let before = state.config_version;
                let changed = state.swp.mode != "server" && state.advance_auto();
                if state.config_version != before {
                    // Same capture: retire only stale display/identity state, never the
                    // IQ/audio session epoch (a gain reconfigure keeps the stream valid).
                    service_stream::deliver_retiring(&mut state, &compute);
                    compute.display_version.store(state.freq_version, Ordering::Release);
                    state.publish_identities(&compute.identities);
                }
                changed
            };
            let mut status = None;
            if send_status || auto_changed {
                let snapshot = compute.state.lock().unwrap().status_snapshot();
                let mut built = snapshot.build();
                let clients = compute.clients.lock();
                let dropped: u64 = clients
                    .iter()
                    .map(|c| c.dropped.load(Ordering::Relaxed))
                    .sum();
                built["stream"] = json!({
                    "clients": clients.len(), "dropped_frames": dropped,
                    "dropped_control": 0, "dropped_audio": 0, "dropped_iq": 0,
                });
                if compute.state.publish_status(snapshot.revision, built.clone()) {
                    status = Some((snapshot.revision, built));
                }
            }
            (display, iq_frame, aux_frame, msgs, status, pace, epoch, display_version)
        })
        .await;
        let (display, iq_frame, aux_frame, msgs, status, pace, epoch, display_version) = match result {
            Ok(value) => value,
            Err(error) => {
                eprintln!("WebSA acquisition worker failed: {error}");
                break;
            }
        };
        if send_status {
            status_due = tokio::time::Instant::now();
        }
        {
            let clients = app.clients.lock();
            for client in clients.iter() {
                if client.iq {
                    for frame in &iq_frame {
                        send_or_count(client, Message::Binary(frame.clone().into()), epoch);
                    }
                    continue;
                }
                if client.aux {
                    for frame in &aux_frame {
                        send_or_count(client, Message::Binary(frame.clone().into()), epoch);
                    }
                    continue;
                }
                if client.audio || client.decode {continue;}
                for frame in &display {
                    let version = (frame.starts_with(b"RTAF") || frame.starts_with(b"RTA2") || frame.starts_with(b"FREQ"))
                        .then_some(display_version);
                    enqueue(client, Outbound {
                        epoch, message: Message::Binary(frame.clone().into()).into(),
                        status_revision: None, display_version: version,
                        identity:None,
                    });
                }
                for msg in &msgs {
                    send_or_count(client, Message::Text(msg.to_string().into()), epoch);
                }
                if let Some((revision, status)) = &status {
                    enqueue(client, Outbound {
                        epoch, message: Message::Text(status.to_string().into()).into(),
                        status_revision: Some(*revision), display_version: None,
                        identity:None,
                    });
                }
            }
        }
        if send_status {
            // Refresh once per STATUS cadence after acquired output is enqueued.
            // The next status publishes this cached GNSS snapshot.
            let refresh = app.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                refresh.state.lock().unwrap().refresh_gnss();
            }).await {
                eprintln!("WebSA GNSS worker failed: {error}");
                break;
            }
        }
        let elapsed = started.elapsed();
        let rest = pace.saturating_sub(elapsed);
        if rest.is_zero() {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(rest).await;
        }
    }
}

/// Display frames use latest-wins per client (Python FRAME_POLICY): POWR/RTAF replace the
/// pending frame, FREQ is retained until sent. IQ/audio/aux stay FIFO (never dropped in
/// order); a full FIFO slot signals the slow-consumer close. Every packet carries the state
/// generation that produced it.
fn send_or_count(client: &Client, message: Message, epoch: u64) {
    let identity=service_stream::message_identity(&message);
    enqueue(client, Outbound { epoch, message:message.into(),identity,status_revision: None, display_version: None });
}

fn enqueue(client: &Client, packet: Outbound) {
    if client.overflow.load(Ordering::Acquire)
        || client.receiver_failed.load(Ordering::Acquire)
        || packet.epoch != client.epoch.load(Ordering::Acquire)
        || packet.status_revision.is_some_and(|r| r < client.status_revision.load(Ordering::Acquire))
        || packet.display_version.is_some_and(|v| v != client.display_version.load(Ordering::Acquire))
        || !service_stream::identity_current(packet.identity,&client.identities,client.channel)
    {
        return;
    }
    let (freq, trace) = match &packet.message {
        OutboundPayload::Single(Message::Binary(bytes)) => (
            bytes.starts_with(b"FREQ"),
            bytes.starts_with(b"POWR") || bytes.starts_with(b"RTAF") || bytes.starts_with(b"RTA2"),
        ),
        _ => (false, false),
    };
    let mut latest = client.latest.lock();
    if trace {
        if latest.1.is_some() {
            client.dropped.fetch_add(1, Ordering::Relaxed);
        }
        latest.1 = Some(packet);
        drop(latest);
        client.wake.notify_one();
    } else if freq {
        latest.0 = Some(packet);
        drop(latest);
        client.wake.notify_one();
    } else {
        drop(latest);
        if client.tx.try_send(packet).is_err() {
            client.dropped.fetch_add(1, Ordering::Relaxed);
            if client.iq || client.aux || client.audio || client.decode {
                client.overflow.store(true, Ordering::Release);
                client.wake.notify_one();
            }
        }
    }
}

type WsSink = futures_util::stream::SplitSink<WebSocket, Message>;

const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const DECODE_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(serde::Deserialize, PartialEq)]
struct DecodeApplied { channel: u32, epoch: u32, seq: u32 }

async fn wait_decode_ack(stream: &mut futures_util::stream::SplitStream<WebSocket>, expected: &DecodeApplied)
    -> Result<bool, tokio::time::error::Elapsed> {
    tokio::time::timeout(DECODE_ACK_TIMEOUT, async {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) => {
                    #[derive(serde::Deserialize)]
                    struct Ack { #[serde(rename="type")] kind: String, #[serde(flatten)] applied: DecodeApplied }
                    if serde_json::from_str::<Ack>(&text).is_ok_and(|ack| ack.kind == "ack" && ack.applied == *expected) {
                        return true;
                    }
                }
                Message::Close(_) => return false,
                _ => {}
            }
        }
        false
    }).await
}

async fn send_decode_frame(sink:&mut WsSink, stream:&mut futures_util::stream::SplitStream<WebSocket>,
    frame:Message, decode_ack:bool) -> Result<bool,tokio::time::error::Elapsed> {
    if !decode_ack {return Ok(send_frame(sink,frame).await);}
    let Message::Binary(bytes)=frame else {return Ok(send_frame(sink,frame).await);};
    if !bytes.starts_with(b"SDE2") {return Ok(send_frame(sink,Message::Binary(bytes)).await);}
    let expected=match serde_json::from_slice::<DecodeApplied>(&bytes[8..]) {
        Ok(applied)=>applied,
        Err(error)=>{eprintln!("WebSA decode ACK identity: {error}");return Ok(false);}
    };
    let frame=if expected.seq==0 {
        let encoded=(|| -> Result<Vec<u8>,String> {
            let mut envelope:Value=serde_json::from_slice(&bytes[8..]).map_err(|e|e.to_string())?;
            if envelope["event"]["type"]=="start" {envelope["event"]["snapshotAck"]=json!(true);}
            websa_channelizer::encode_decode_event(&serde_json::to_vec(&envelope).map_err(|e|e.to_string())?).map_err(str::to_owned)
        })();
        match encoded {Ok(bytes)=>Message::Binary(bytes.into()),Err(error)=>{
            eprintln!("WebSA decode ACK capability: {error}");return Ok(false);
        }}
    } else {Message::Binary(bytes)};
    if !send_frame(sink,frame).await {return Ok(false);}
    wait_decode_ack(stream,&expected).await
}

/// Bounded single-frame send. `false` means the socket is gone; the loop must remove the
/// client instead of retrying.
async fn send_frame(sink: &mut WsSink, frame: Message) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, sink.send(frame)).await,
        Ok(Ok(()))
    )
}

/// Direct replies share the publisher revision order. Refresh an overtaken build before
/// adding this command's acknowledgement, then fence the queued STATUS on this socket.
async fn send_status(
    sink: &mut WsSink,
    state: &StateMutex,
    status: (u64, Value),
    response_to: Option<&str>,
    session_id: Option<&str>,
    request_id: Option<&Value>,
    seen_revision: &mut u64,
) -> bool {
    let (revision, mut status) = state.current_status(status.0, status.1);
    if revision < *seen_revision { return true; }
    if let Some(cmd) = response_to {
        status["response_to"] = json!(cmd);
        if let Some(session_id) = session_id {
            status["session_id"] = json!(session_id);
        }
    }
    status = with_request_id(status, request_id);
    *seen_revision = revision;
    send_frame(sink, Message::Text(status.to_string().into())).await
}

async fn close_stream(sink: &mut WsSink, receiver_failed: bool) {
    let _ = tokio::time::timeout(
        Duration::from_millis(250),
        sink.send(Message::Close(Some(CloseFrame {
            code: if receiver_failed { 1011 } else { 1013 },
            reason: if receiver_failed { "receiver failed" } else { "IQ/audio consumer too slow" }.into(),
        }))),
    )
    .await;
}

/// Send at most one FREQ and the newest trace, dropping packets from older generations.
/// Returns `false` when the socket is gone.
async fn flush_latest(
    sink: &mut WsSink,
    latest: &PLMutex<(Option<Outbound>, Option<Outbound>)>,
    epoch: &AtomicU64,
    display_version: &AtomicU64,
) -> bool {
    let (freq, trace) = {
        let mut slots = latest.lock();
        (slots.0.take(), slots.1.take())
    };
    for packet in [freq, trace].into_iter().flatten() {
        if packet.epoch != epoch.load(Ordering::Acquire)
            || packet.display_version.is_some_and(|v| v != display_version.load(Ordering::Acquire))
        {
            continue;
        }
        let OutboundPayload::Single(message) = packet.message else { unreachable!("display payload must be a single frame") };
        if !send_frame(sink, message).await {
            return false;
        }
    }
    true
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(app): State<Arc<App>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !browser_access(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let iq = params.contains_key("iq");
    let aux = params.get("iqaux").is_some_and(|value| value == "1");
    let audio = params.contains_key("audio");
    if aux && (iq || audio) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    ws.on_upgrade(move |socket| client_loop(app, socket, iq, aux, audio, false, None, false))
}

async fn client_loop(app: Arc<App>, socket: WebSocket, iq: bool, aux: bool, audio: bool, decode:bool, channel:Option<u32>, decode_ack:bool) {
    let (tx, mut rx) = mpsc::channel::<Outbound>(64);
    let id = app.next_client.fetch_add(1, Ordering::Relaxed);
    let overflow = Arc::new(AtomicBool::new(false));
    let receiver_failed = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(Notify::new());
    let epoch = app.epoch.clone();
    let display_version = app.display_version.clone();
    let status_revision = app.state.revision();
    let mut seen_status_revision = 0;
    let latest_slot: Arc<PLMutex<(Option<Outbound>, Option<Outbound>)>> =
        Arc::new(PLMutex::new((None, None)));
    let mut subscriber=subscriber_stream::SubscriberStream::default();
    let initial_decode={
        let state=app.state.lock().unwrap();
        if (audio || decode) && state.receiver_failure.is_some() {
            receiver_failed.store(true,Ordering::Release);
        }
    app.clients.lock().push(Client {
        id,
        tx,
        decode_pending_bytes: Arc::new(AtomicU64::new(0)),
        dropped: Arc::new(AtomicU64::new(0)),
        overflow: overflow.clone(),
        receiver_failed: receiver_failed.clone(),
        wake: wake.clone(),
        epoch: epoch.clone(),
        status_revision: status_revision.clone(),
        display_version: display_version.clone(),
        iq,
        aux,
        audio,
        decode,
        channel,
        identities: app.identities.clone(),
        latest: Arc::clone(&latest_slot),
    });
        if decode {state.decode_snapshots.initial(channel,&state.stream_registry(),&mut subscriber)}
        else {Ok(Vec::new())}
    };
    let (mut sink, mut stream) = socket.split();
    // Capture the generation under the same lock as the frame, then re-check before the
    // untagged direct send: a command from another socket may have changed the axis.
    let initial = if iq || aux || audio || decode {
        Ok(None)
    } else {
        let worker_app = app.clone();
        tokio::task::spawn_blocking(move || {
            let state = worker_app.state.lock().unwrap();
            if state.swp.mode == "server" { return None; }
            let frame_epoch = worker_app.epoch.load(Ordering::Acquire);
            state
                .last_freq
                .clone()
                .map(|frame| (Message::Binary(frame.into()), frame_epoch, state.freq_version))
        })
        .await
    };
    let mut initial_ok = true;
    match initial_decode {
        Ok(frames)=>for frame in frames {
            if !service_stream::identity_current(service_stream::message_identity(&frame),&app.identities,channel) {continue;}
            match send_decode_frame(&mut sink,&mut stream,frame,decode_ack).await {
                Ok(true)=>{},
                Ok(false)=>{initial_ok=false;break;},
                Err(_)=>{overflow.store(true,Ordering::Release);initial_ok=false;break;}
            }
        },
        Err(error)=>{eprintln!("WebSA initial decode snapshot: {error}");initial_ok=false;}
    }
    match initial {
        Ok(Some((frame, frame_epoch, frame_version))) => {
            if epoch.load(Ordering::Acquire) == frame_epoch
                && display_version.load(Ordering::Acquire) == frame_version
            {
                initial_ok = send_frame(&mut sink, frame).await;
            }
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("WebSA initial FREQ worker failed: {error}");
            initial_ok = false;
        }
    }
    let slow = if initial_ok {
        'client: loop {
            if receiver_failed.load(Ordering::Acquire) || overflow.load(Ordering::Acquire) {
                break true;
            }
            // Fair select: a FIFO backlog or published STATUS cannot starve the wake
            // branch. The wake branch sends at most FREQ + newest trace, then selects
            // again, so command input is never delayed behind display traffic.
            tokio::select! {
                _ = wake.notified() => {
                    if !flush_latest(&mut sink, &latest_slot, &epoch, &display_version).await {
                        break false;
                    }
                }
                incoming = stream.next() => {
                    let Some(Ok(message)) = incoming else { break false };
                    let text = match message {
                        Message::Text(text) => text,
                        Message::Close(_) => break false,
                        _ => continue,
                    };
                    let Ok(data) = serde_json::from_str::<Value>(&text) else {
                        let error = json!({
                            "cmd": "ERROR", "msg": "JSON message must be an object",
                            "code": "json_object_required"}).to_string();
                        if !send_frame(&mut sink, Message::Text(error.into())).await {
                            break false;
                        }
                        continue;
                    };
                    // Application acknowledgements are transport credit, never control commands.
                    if decode && data.get("type").and_then(Value::as_str) == Some("ack") {continue;}
                    let cmd = data.get("cmd").and_then(Value::as_str).unwrap_or_default().to_string();
                    let request_id = command_request_id(&data);
                    if cmd == "STATUS" {
                        let worker_app = app.clone();
                        let status = match tokio::task::spawn_blocking(move || {
                            worker_app.state.status_with_revision()
                        })
                        .await
                        {
                            Ok(status) => status,
                            Err(error) => {
                                eprintln!("WebSA STATUS worker failed: {error}");
                                break false;
                            }
                        };
                        if !send_status(&mut sink, &app.state, status, Some(&cmd), None, request_id.as_ref(), &mut seen_status_revision).await {
                            break false;
                        }
                        continue;
                    }
                    let revision=match service_preflight::preflight(&app,&cmd,&data).await {
                        Ok(revision)=>revision,
                        Err(error)=>{
                            let error=with_request_id(CommandError::new("bad_parameter",&error).reply_payload(&cmd),request_id.as_ref()).to_string();
                            if !send_frame(&mut sink,Message::Text(error.into())).await {break false;}
                            continue;
                        }
                    };
                    let worker_app = app.clone();
                    let response_to = cmd.clone();
                    let session_id = matches!(cmd.as_str(), "SET_PNM" | "SET_HARM")
                        .then(|| data.get("session_id").and_then(Value::as_str).map(str::to_owned))
                        .flatten();
                    let error_request_id = request_id.clone();
                    let reply = match tokio::task::spawn_blocking(move || {
                        let mut state = worker_app.state.lock().unwrap();
                        if state.config_version!=revision {return Some(Err(with_request_id(CommandError::new("configuration_changed","configuration changed during admission; retry command").reply_payload(&cmd),error_request_id.as_ref()).to_string()));}
                        let same_capture = state.swp.mode == "sdr" && cmd != "SET_MODE" && cmd != "CONNECT";
                        match dispatch(&mut state, &cmd, &data) {
                            Ok(changed) => {
                                if changed && !same_capture {
                                    worker_app.epoch.fetch_add(1, Ordering::AcqRel);
                                }
                                service_stream::deliver_retiring(&mut state,&worker_app);
                                worker_app.display_version.store(state.freq_version, Ordering::Release);
                                state.publish_identities(&worker_app.identities);
                                let snapshot = state.status_snapshot();
                                drop(state);
                                let status = snapshot.build();
                                worker_app.state.publish_status(snapshot.revision, status.clone());
                                Some(Ok((snapshot.revision, status)))
                            }
                            Err(error) => {
                                if state.config_version != revision {
                                    worker_app.epoch.fetch_add(1, Ordering::AcqRel);
                                    service_stream::deliver_retiring(&mut state,&worker_app);
                                    worker_app.display_version.store(state.freq_version, Ordering::Release);
                                    state.publish_identities(&worker_app.identities);
                                    let snapshot = state.status_snapshot();
                                    drop(state);
                                    let status = snapshot.build();
                                    worker_app.state.publish_status(snapshot.revision, status);
                                }
                                Some(Err(with_request_id(error.reply_payload(&cmd),error_request_id.as_ref()).to_string()))
                            },
                        }
                    })
                    .await
                    {
                        Ok(reply) => reply,
                        Err(error) => {
                            eprintln!("WebSA command worker failed: {error}");
                            break false;
                        }
                    };
                    if let Some(reply) = reply {
                        let sent = match reply {
                            Ok(status) => send_status(&mut sink, &app.state, status, Some(&response_to), session_id.as_deref(), request_id.as_ref(), &mut seen_status_revision).await,
                            Err(text) => send_frame(&mut sink, Message::Text(text.into())).await,
                        };
                        if !sent { break false; }
                    }
                }
                packet = rx.recv() => {
                    let Some(packet) = packet else { break false };
                    if packet.epoch != epoch.load(Ordering::Acquire)
                        || packet.status_revision.is_some_and(|r| r < seen_status_revision || r < status_revision.load(Ordering::Acquire))
                        || packet.display_version.is_some_and(|v| v != display_version.load(Ordering::Acquire))
                    {
                        continue;
                    }
                    if let Some(revision) = packet.status_revision {
                        seen_status_revision = revision;
                    }
                    let mut index=0;
                    while let Some((message,identity))=packet.message.frame(index) {
                        if receiver_failed.load(Ordering::Acquire) || overflow.load(Ordering::Acquire) {break 'client true;}
                        let included=match &packet.message {
                            OutboundPayload::DecodeBatch {frames,retiring:Some(ids),..}=>
                                frames[index].1.is_some_and(|id|ids.contains(&id)),
                            _=>true,
                        };
                        index+=1;
                        if !included || packet.epoch!=epoch.load(Ordering::Acquire)
                            || !service_stream::identity_current(identity.or(packet.identity),&app.identities,channel) {continue;}
                        let frames=match subscriber.frames(message) {
                            Ok(frames)=>frames,
                            Err(error)=>{eprintln!("SDR subscriber stream: {error}");break 'client true;},
                        };
                        for frame in frames {
                            match send_decode_frame(&mut sink,&mut stream,frame,decode_ack).await {
                                Ok(true)=>{},
                                Ok(false)=>break 'client false,
                                Err(_)=>{overflow.store(true,Ordering::Release);break 'client true;}
                            }
                        }
                    }
                }
            }
        }
    } else {
        overflow.load(Ordering::Acquire) || receiver_failed.load(Ordering::Acquire)
    };
    if slow {
        close_stream(&mut sink,receiver_failed.load(Ordering::Acquire)).await;
    }
    app.clients.lock().retain(|client| client.id != id);
}

/// Frontend root that holds `dist/`: WEBSA_STATIC (Python config.py), else the repository
/// `frontend/` directory of this build.
fn static_root() -> String {
    std::env::var("WEBSA_STATIC")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../frontend").into())
}

async fn index() -> Response {
    match tokio::fs::read(format!("{}/dist/index.html", static_root())).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], bytes).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "frontend is not built").into_response(),
    }
}

async fn static_files(Path(path): Path<String>) -> Response {
    let root = match tokio::fs::canonicalize(format!("{}/dist", static_root())).await {
        Ok(root) => root,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let full = match tokio::fs::canonicalize(root.join(path)).await {
        Ok(full) if full.starts_with(&root) => full,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let kind = match full.extension().and_then(|ext| ext.to_str()) {
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("wasm") => "application/wasm",
                Some("json") => "application/json",
                Some("woff2") => "font/woff2",
                Some("png") => "image/png",
                Some("svg") => "image/svg+xml",
                _ => "application/octet-stream",
            };
            (
                [
                    (header::CONTENT_TYPE, kind),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// STATUS/schema contract captured from the Python service; compiled in, so the binary
/// does not depend on its working directory.
fn load_golden() -> (Value, Value) {
    const STATE: &str = include_str!("../../../tests/fixtures/contract/state_fake_initial.json");
    const SCHEMA: &str = include_str!("../../../tests/fixtures/contract/schema.json");
    let mut schema: Value = serde_json::from_str(SCHEMA).expect("embedded golden schema JSON");
    schema["commands"]["SET_MODE"]["params"][0]["choices"].as_array_mut().unwrap().push(json!("server"));
    schema["commands"]["SET_SERVER"] = json!({
        "needs_device": false, "denied_in_sdr": false, "swp_only": false,
        "session_exclusive": false,
        "params": [
            {"name":"action", "type":"choice", "required":true, "choices":["start","stop","restart","configure"]},
            {"name":"bind_address", "type":"token", "required":false, "min_length":2, "max_length":45},
            {"name":"port", "type":"integer", "required":false, "min":1, "max":65535}
        ]
    });
    schema["commands"]["SET_SDR"]["params"].as_array_mut().unwrap().push(json!({
        "name": "pfb_taps_per_phase", "type": "integer", "choices": [4, 8, 16],
        "default": 16, "min": 4, "max": 16, "required": false, "unit": ""
    }));
    let mut channel = schema["commands"]["SET_SDR_DEMOD"].clone();
    channel["params"].as_array_mut().unwrap().push(json!({
        "name": "channel", "type": "integer", "choices": [1, 2, 3], "default": 1,
        "min": 1, "max": 3, "required": false, "unit": ""
    }));
    schema["commands"]["SET_SDR_CHANNEL"] = channel;
    (serde_json::from_str(STATE).expect("embedded golden state JSON"), schema)
}

fn check_hardware_exclusive() -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        if pid == std::process::id() {
            continue;
        }
        let Ok(executable) = std::fs::read_link(entry.path().join("exe")) else { continue };
        let name = executable.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let native_probe = matches!(name, "san90_iqs" | "san90_iqs (deleted)")
            && std::fs::read(entry.path().join("cmdline")).is_ok_and(|args| args.split(|byte| *byte == 0).any(|arg| arg == b"--hardware"));
        let hardware_service = matches!(name, "websad" | "websad (deleted)")
            && std::fs::read(entry.path().join("environ"))?
                .split(|byte| *byte == 0).any(|item| item.starts_with(b"WEBSA_RUST_IQS_SHIM=") && item.len() > b"WEBSA_RUST_IQS_SHIM=".len());
        if native_probe || hardware_service {
            return Err("stop the native hardware owner before exclusive Rust IQS".into());
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure passive Intel OpenMP waiting before loading the SDK so idle
    // SDK workers do not starve capture.
    for (key, value) in [("OMP_WAIT_POLICY", "PASSIVE"), ("KMP_BLOCKTIME", "0")] {
        if std::env::var_os(key).is_none() {
            // SAFETY: the process has one thread here; the tokio runtime starts below.
            unsafe { std::env::set_var(key, value) };
        }
    }
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--sdr-pipeline-bench") {
        let benchmark = pipeline_bench::run(&args[2..]);
        let finished = hardware_swp::finish_sdk_session();
        return benchmark.and(finished).map_err(Into::into);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let (golden, schema) = load_golden();
    let port: u16 = std::env::var("WEBSA_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8099);
    let host = std::env::var("WEBSA_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let mut state = DevState::new(golden, schema);
    if let Ok(shim) = std::env::var("WEBSA_RUST_IQS_SHIM") {
        if shim.is_empty() {
            return Err("WEBSA_RUST_IQS_SHIM is empty".into());
        }
        check_hardware_exclusive()?;
        state.hardware_shim = Some(shim);
    }
    state.server.lock().refresh_dependencies();
    // Prepare thread-owned models once before accepting mode changes or arming
    // hardware. Each later session creates fresh channel DSP on these workers.
    state.receiver_workers = Some(receiver_workers::ReceiverWorkers::new()?);
    let app = Arc::new(App {
        state: StateMutex::new(state),
        clients: PLMutex::new(Vec::new()),
        next_client: AtomicU64::new(0),
        epoch: Arc::new(AtomicU64::new(0)),
        display_version: Arc::new(AtomicU64::new(0)),
        identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
    });
    let router = Router::new()
        .route("/api/state", get(api_state))
        .route("/api/schema", get(api_schema))
        .route("/api/config", post(api_config))
        .route("/ws", get(ws_handler))
        .route("/ws/audio", get(service_stream::audio_ws_handler))
        .route("/ws/decode", get(service_stream::decode_ws_handler))
        .route("/api/sdr/plugins", get(channel_controls::plugins))
        .route("/", get(index))
        .route("/static/dist/{*path}", get(static_files))
        .with_state(app.clone());
    let publisher = tokio::spawn(publisher(app.clone()));
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    println!("websad listening on http://{host}:{port}");
    let shutdown_app = app.clone();
    axum::serve(listener, router).with_graceful_shutdown(async move {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => (),
            _ = terminate.recv() => (),
        }
        let stop_app = shutdown_app.clone();
        let _ = tokio::task::spawn_blocking(move || stop_app.state.lock().unwrap().server.lock().stop()).await;
        // Drop every outbound sender and wake parked sockets before waiting for
        // axum's upgraded connections; otherwise an idle WS can hold shutdown.
        let mut clients = shutdown_app.clients.lock();
        for client in clients.iter() {
            client.wake.notify_one();
        }
        clients.clear();
    }).await?;
    publisher.abort();
    let _ = publisher.await;
    tokio::task::spawn_blocking(move || {
        let mut state = app.state.lock().unwrap();
        let registry=state.stream_registry();
        state.server.lock().stop()?;
        let stopped = state.stop_pipeline();
        let receivers_finished = state.finish_radiofax_with_registry(registry);
        state.ddc = None;
        state.swp_hw = None;
        state.rta_hw = None;
        state.pnm_hw = None;
        state.receiver_workers = None;
        drop(state);
        // Mode owners have joined; the permanent SDK owner can now close the
        // idle physical device and join its thread exactly once.
        let finished = hardware_swp::finish_sdk_session();
        stopped.and(receivers_finished).and(finished)
    }).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("websad/auto_recovery_tests.rs");
    include!("websad/configuration_transaction_tests.rs");

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
                    display_version:app.display_version.clone(),iq:false,aux:false,audio,decode:!audio,channel:Some(1),
                    identities:app.identities.clone(),latest:Arc::new(PLMutex::new((None,None))),
                });
                receivers.push((audio,rx));
            }
            {
                let state=app.state.lock().unwrap();
                service_stream::deliver_receiver(&state,&app,initial,false);
            }
            // Consume the established session's start/data before the command.
            // The tail will be enqueued under the *new* transport epoch.
            for (_,rx) in &mut receivers {
                assert!(rx.try_recv().is_ok());
                while rx.try_recv().is_ok() {}
            }
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
                let expected=if audio {&tail.audio} else {&tail.decode};
                let mut packet=rx.try_recv().expect("retiring tail cannot be lost on failed handoff");
                for (index,frame) in expected.iter().enumerate() {
                    if audio && index>0 {packet=rx.try_recv().expect("retiring audio tail");}
                    assert_eq!(packet.epoch,old_epoch+1);
                    let (message,identity)=packet.message.frame(if audio {0} else {index}).unwrap();
                    assert!(identity.or(packet.identity).is_none(),"old tail must survive the identity fence");
                    let Message::Binary(bytes)=message else {panic!("expected receiver bytes")};
                    assert_eq!(bytes.as_ref(),frame.as_slice());
                }
                let packet=rx.try_recv().expect("fresh capture start follows every retiring tail");
                let (message,identity)=packet.message.frame(0).unwrap();
                assert!(identity.or(packet.identity).is_some());
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
            identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
            display_version:Arc::new(AtomicU64::new(0))});
        let task = tokio::spawn(publisher(app.clone()));
        tokio::time::sleep(Duration::from_millis(250)).await;
        task.abort();let _=task.await;
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
        assert_eq!(state.status(), before);
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
        expected.push(reference.framer.finish().unwrap().unwrap());
        expected.push(reference.aux.get_mut(&2).unwrap().2.finish().unwrap().unwrap());
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
            serde_json::from_str(include_str!("../../../tests/fixtures/contract/schema.json"))
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
    fn am_sync_partial_configuration_is_strict_persistent_and_resettable() {
        let (golden, schema) = load_golden();
        let mut state = DevState::new(golden, schema);
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
        let frame = hardware_pan(7, (lo + hi) / 2.0, hi - lo, &freq, &dbm).unwrap();
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
            channel:None,
            identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
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
            identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
        });
        let router = Router::new()
            .route("/ws", get(ws_handler))
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
        struct PublisherTask(tokio::task::JoinHandle<()>);
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
        let _publisher = PublisherTask(tokio::spawn(publisher(app.clone())));
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
    async fn initial_decode_snapshot_waits_for_opted_in_application_ack() {
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
        expect_silence(&mut paced, Duration::from_millis(30)).await;
        // An ACK for another channel or sequence grants no credit.
        paced.send(WsMessage::Text(json!({"type":"ack","channel":2,"epoch":stream_epoch,"seq":0}).to_string().into())).await.unwrap();
        expect_silence(&mut paced, Duration::from_millis(30)).await;
        let mut total = 0;
        for seq in 0..=38 {
            paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"epoch":stream_epoch,"seq":seq}).to_string().into())).await.unwrap();
            if seq == 38 {break;}
            let bytes = next_binary(&mut paced).await;
            assert!(bytes.len()-8 <= 2*1024*1024);
            total += bytes.len();
            let envelope: Value = serde_json::from_slice(&bytes[8..]).unwrap();
            assert_eq!(envelope["seq"], seq+1);
            assert_eq!(envelope["event"]["row"], seq*64);
        }
        assert!(total > 4*1024*1024);
        let live = frame(39,json!({"type":"radiofax-state"}));
        fanout(&app, Message::Binary(live.into()), app.epoch.load(Ordering::Acquire));
        let bytes = next_binary(&mut paced).await;
        let envelope: Value = serde_json::from_slice(&bytes[8..]).unwrap();
        assert_eq!(envelope["seq"], 39);
        paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"epoch":stream_epoch,"seq":39}).to_string().into())).await.unwrap();
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

    #[tokio::test(flavor = "current_thread")]
    async fn live_decode_revision_batches_wait_for_exact_ack_without_losing_fifo() {
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
        for channel in 1..=3 {
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
        assert!(wire_bytes>3*4*1024*1024);
        assert!((wire_bytes as u64)<DECODE_PENDING_BYTE_LIMIT);
        {
            let state=app.state.lock().unwrap();
            service_stream::deliver_receiver(&state,&app,output,false);
            let mut following=receiver_workers::ReceiverFrames::default();
            for channel in 1..=3 {following.decode.push(frame(channel,185,json!({"type":"radiofax-state"})));}
            service_stream::deliver_receiver(&state,&app,following,false);
        }
        let mut sequences=[0u32;3];
        let mut total=0;
        for index in 0..count+3 {
            let bytes=next_binary(&mut paced).await;
            let event:Value=serde_json::from_slice(&bytes[8..]).unwrap();
            let channel=event["channel"].as_u64().unwrap() as usize;
            let seq=event["seq"].as_u64().unwrap() as u32;
            assert_eq!(seq,sequences[channel-1]);
            sequences[channel-1]+=1;
            if index<count {assert!(seq<=184);} else {assert_eq!(seq,185);}
            if seq==0 {assert_eq!(event["event"]["snapshotAck"],true);}
            if seq>0 && seq<=184 {assert_eq!(event["event"]["revision"],2);}
            if index==0 {
                expect_silence(&mut paced,Duration::from_millis(20)).await;
                for (wrong_channel,wrong_epoch,wrong_seq) in [(2,7,0),(1,8,0),(1,7,1)] {
                    paced.send(WsMessage::Text(json!({"type":"ack","channel":wrong_channel,"epoch":wrong_epoch,"seq":wrong_seq}).to_string().into())).await.unwrap();
                    expect_silence(&mut paced,Duration::from_millis(20)).await;
                }
            }
            let raw_bytes=next_binary(&mut raw).await;
            let raw_event:Value=serde_json::from_slice(&raw_bytes[8..]).unwrap();
            assert_eq!(raw_event["channel"],event["channel"]);
            assert_eq!(raw_event["seq"],event["seq"]);
            assert!(raw_event["event"].get("snapshotAck").is_none());
            if seq>0 {assert_eq!(raw_bytes,bytes);}
            total+=bytes.len();
            paced.send(WsMessage::Text(json!({"type":"ack","channel":channel,"epoch":7,"seq":seq}).to_string().into())).await.unwrap();
        }
        assert_eq!(sequences,[186;3]);
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
                audio:Vec::new(),decode:vec![frame(1,0,json!({"type":"start"}))],
            },false);
        }
        let start=next_binary(&mut paced).await;
        let start:Value=serde_json::from_slice(&start[8..]).unwrap();
        assert_eq!(start["event"]["snapshotAck"],true);
        let pending=app.clients.lock()[0].decode_pending_bytes.load(Ordering::Acquire);
        assert!(pending>0,"active ACK wait must retain its reservation");
        {
            let state=app.state.lock().unwrap();
            // No awaits: the client cannot drain a mistakenly enqueued batch.
            // Test both live filtering and retiring filtering with no matching
            // retiring identity for this channel.
            for retiring in [false,true] {
                for seq in 0..130 {
                    service_stream::deliver_receiver(&state,&app,receiver_workers::ReceiverFrames {
                        audio:Vec::new(),decode:vec![frame(2,seq,json!({"type":"radiofax-state"})),
                            frame(3,seq,json!({"type":"radiofax-state"}))],
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
                audio:Vec::new(),decode:vec![frame(1,1,json!({"type":"radiofax-state"}))],
            },false);
        }
        expect_silence(&mut paced,Duration::from_millis(20)).await;
        paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"epoch":7,"seq":0}).to_string().into())).await.unwrap();
        let following=next_binary(&mut paced).await;
        let following:Value=serde_json::from_slice(&following[8..]).unwrap();
        assert_eq!(following["channel"],1);
        assert_eq!(following["seq"],1);
        paced.send(WsMessage::Text(json!({"type":"ack","channel":1,"epoch":7,"seq":1}).to_string().into())).await.unwrap();
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
            serde_json::from_str(include_str!("../../../tests/fixtures/contract/schema.json"))
                .unwrap();
        let golden: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/contract/state_fake_initial.json"
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
        dispatch(&mut state, "SET_PRESET", &json!({})).unwrap();
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
        let manifest: Value = serde_json::from_str(include_str!("../../../tests/fixtures/frames/manifest.json")).unwrap();
        let legacy = include_bytes!("../../../tests/fixtures/frames/rta.bin");
        let golden = include_bytes!("../../../tests/fixtures/frames/rta2.bin");
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
}
