// SPDX-License-Identifier: LicenseRef-Harogic-WebSA
// Project-owned code: AGPL-3.0-only with ../../../LICENSE-EXCEPTION; scope in ../../../LICENSING.md.
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
#[path = "websad/fine_workers.rs"]
mod fine_workers;
#[path = "websad/reception_recovery.rs"]
mod reception_recovery;
#[path = "websad/display_fft.rs"]
mod display_fft;
#[path = "websad/scan.rs"]
mod scan;
#[path = "websad/sdr_hardware.rs"]
mod sdr_hardware;
#[path = "websad/state_lock.rs"]
mod state_lock;
#[path = "websad/pipeline_bench.rs"]
mod pipeline_bench;
#[path = "websad/receiver_workers.rs"]
mod receiver_workers;
#[path = "websad/receiver_controls.rs"]
mod receiver_controls;
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
#[cfg(test)]
#[path = "websad/coverage_tests.rs"]
mod coverage_tests;
#[path = "websad/service_cost.rs"]
mod service_cost;
#[path = "websad/service_power.rs"]
mod service_power;
#[path = "websad/subscriber_stream.rs"]
mod subscriber_stream;
#[path = "websad/recording.rs"]
mod recording;
#[path = "websad/recording_read.rs"]
mod recording_read;
#[path = "websad/recording_export.rs"]
mod recording_export;
#[path = "websad/recording_collection.rs"]
mod recording_collection;
#[path = "websad/recording_demod.rs"]
mod recording_demod;
#[path = "websad/multichannel.rs"]
mod multichannel;
#[path = "websad/receiver_presets.rs"]
mod receiver_presets;
#[path = "websad/processing_admission.rs"]
mod processing_admission;
#[path = "websad/service_preflight.rs"]
mod service_preflight;
#[path = "websad/service_commands.rs"]
mod service_commands;
#[path = "websad/service_routes.rs"]
mod service_routes;
#[path = "websad/service_lifecycle.rs"]
mod service_lifecycle;
#[path = "websad/rta_control.rs"]
mod rta_control;
use service_routes::{browser_access, error_body, state_worker_failed};
#[cfg(test)]
use service_routes::api_config;
#[cfg(test)]
#[path = "websad/sixteen_channel_tests.rs"]
mod sixteen_channel_tests;
#[path = "websad/server_mode.rs"]
mod server_mode;
#[path = "websad/receiver_limits.rs"]
mod receiver_limits;
use receiver_limits::*;
fn channel_identities() -> Arc<[AtomicU64]> {
    (0..MAX_CHANNELS).map(|_| AtomicU64::new(0)).collect()
}
use state_lock::StateMutex;
use websa_channelizer::{IqFrame,IqInput};
type IqFrames=Vec<IqFrame>;
use sdr_pipeline::{Pipeline, PipelineConfig};
use websa_channelizer::timing::Timing;
use websa_channelizer::capture_scheduling;
use axum::{
    body::Bytes,
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
#[cfg(test)]
use axum::{routing::get, Router};
use futures_util::{FutureExt, SinkExt, StreamExt};
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
    future::Future,
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
    DSP_EPOCH.try_update(Ordering::Relaxed, Ordering::Relaxed, |epoch| {
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
    Iq {message:Message, queued_at:std::time::Instant, _reservation:DecodeReservation},
    DecodeBatch {
        frames: Arc<[(Message, Option<u64>)]>,
        // Admission is fixed under state; a later retune cannot remove a predecessor.
        admitted: Vec<u64>,
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
            Self::Iq {message,..} => (index == 0).then(|| (message.clone(), None)),
            Self::DecodeBatch { frames, .. } => frames.get(index).cloned(),
        }
    }
}

impl Outbound {
    fn epoch_current(&self, epoch:u64) -> bool {
        self.epoch==epoch || match &self.message {
            OutboundPayload::DecodeBatch {..} | OutboundPayload::Iq {..} => true,
            OutboundPayload::Single(Message::Binary(bytes)) => bytes.starts_with(b"AUD2") || bytes.starts_with(b"SDE2"),
            _ => false,
        }
    }
    fn frames(&self) -> impl Iterator<Item = Message> + '_ {
        let count = match &self.message {
            OutboundPayload::Single(_) => 1,
            OutboundPayload::Iq {..} => 1,
            OutboundPayload::DecodeBatch { frames, .. } => frames.len(),
        };
        (0..count).filter_map(|index| {
            let (message, identity) = self.message.frame(index)?;
            match &self.message {
                OutboundPayload::DecodeBatch { admitted, .. }
                    if !identity.is_some_and(|id| admitted.contains(&id)) => None,
                _ => Some(message),
            }
        })
    }
}

#[derive(Debug)]
struct DecodeReservation { pending: Arc<AtomicU64>, bytes: u64 }
impl DecodeReservation {
    fn iq(pending:&Arc<AtomicU64>,bytes:u64)->Option<Self> {
        pending.try_update(Ordering::AcqRel,Ordering::Acquire,|used|
            used.checked_add(bytes).filter(|&total|total<=IQ_PENDING_BYTES)).ok()?;
        Some(Self {pending:pending.clone(),bytes})
    }
    fn acquire(pending: &Arc<AtomicU64>, bytes: u64) -> Option<Self> {
        pending.try_update(Ordering::AcqRel, Ordering::Acquire, |used|
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
    receiver_uid: Option<String>,
    identities: Arc<[AtomicU64]>,
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
    receivers: multichannel::Directory,
    recordings: Option<recording::Manager>,
    recording_error: Option<String>,
    receiver_presets: Option<Arc<std::sync::Mutex<receiver_presets::Store>>>,
    receiver_workers: Option<receiver_workers::ReceiverWorkers>,
    receiver_failure: Option<String>,
    receiver_admission:Option<Value>,
    receiver_failure_pending: bool,
    fixture_sources: Option<service_fixture::FixtureSources>,
    channel_power: Vec<Option<(std::time::Instant,f64)>>,
    sdr_actual: Value,
    scan: Option<Arc<scan::Engine>>,
    scan_probe: Option<scan::Probe>,
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
    last_capture: Option<sdr_hardware::CaptureParameters>,
    spectrum_timing: Timing,
    state_lock_wait: Timing,
    state_lock_hold: Timing,
    pending_iq: IqFrames,
    pending_receiver: receiver_workers::ReceiverFrames,
    retiring_registry: Arc<[sdr_pipeline::ChannelRegistryEntry]>,
    decode_snapshots: service_stream::DecodeSnapshots,
    processing_branches: multichannel::ProcessingBranches,
    consumed_receiver_iq: std::collections::BTreeSet<(u32,u32,u32)>,
    reception: reception_recovery::Supervisor,
    ddc_error: Option<String>,
    ddc_error_count: u64,
    ddc_error_last: Option<String>,
    ddc_failure_diagnostics: Option<Value>,
    ddc_first_failure_diagnostics: Option<Value>,
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
            sdr_squelch: websa_dsp::audio::SQUELCH_OPEN_DBM,
            sdr_volume: 0.8,
            sdr_agc: true,
            sdr_pitch: 700.0,
            sdr_deemph: -1.0,
            sdr_ref: 0.0,
            sdr_ref_set: false,
            sdr_aux: json!({}),
            sdr_channels: (1..=MAX_CHANNELS as u32).map(receiver_workers::ChannelConfig::for_slot).collect(),
            receivers: multichannel::Directory::default(),
            recordings: None,
            recording_error: None,
            receiver_presets: None,
            receiver_workers: None,
            receiver_failure: None,
            receiver_admission:None,
            receiver_failure_pending: false,
            fixture_sources: None,
            channel_power: vec![None;MAX_CHANNELS],
            sdr_actual: json!({}),
            scan: None,
            scan_probe: None,
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
            last_capture: None,
            spectrum_timing: Timing::default(),
            state_lock_wait: Timing::default(),
            state_lock_hold: Timing::default(),
            pending_iq: Vec::new(),
            pending_receiver: receiver_workers::ReceiverFrames::default(),
            retiring_registry: Arc::from([]),
            decode_snapshots: service_stream::DecodeSnapshots::default(),
            processing_branches: multichannel::ProcessingBranches::default(),
            consumed_receiver_iq: std::collections::BTreeSet::new(),
            reception: Default::default(),
            ddc_error: None,
            ddc_error_count: 0,
            ddc_error_last: None,
            ddc_failure_diagnostics: None,
            ddc_first_failure_diagnostics: None,
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

    /// Keep failures visible during cooldown and bound SDK reopen attempts.
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
            self.sdr_squelch = websa_dsp::audio::SQUELCH_OPEN_DBM;
            self.sdr_volume = 0.8;
            self.sdr_agc = true;
            self.sdr_pitch = 700.0;
            self.sdr_deemph = -1.0;
            // A preset in another graph mode must not turn saved SDR receivers
            // into disabled 0 Hz placeholders. Preserve their settings and UIDs.
            let saved = self.channel_intents();
            let saved_slots: Vec<_> = saved.iter().filter(|c| self.receiver_uid_for_slot(c.channel).is_some())
                .map(|c| c.channel).collect();
            self.sdr_channels = Self::preset_service_channels();
            for config in self.sdr_channels.iter_mut().skip(1) {
                if saved_slots.contains(&config.channel) {
                    *config = saved[(config.channel - 1) as usize].clone();
                }
            }
            self.sdr_aux = json!({"2":self.sdr_channels[1],"3":self.sdr_channels[2]});
        }
        self.sdr_ref = 0.0;
        self.sdr_ref_set = false;
        self.refclk_ppm = 0.0;
        self.swp_error = None;
        self.rta_error = None;
        self.status_warning = 0;
        if self.receiver_failure.is_none() {
            self.ddc_failure_diagnostics = None;
            self.ddc_first_failure_diagnostics = None;
        }
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
                        match actual {
                            Ok(actual) => {
                                self.note_auto_acquisition(failure.status);
                                self.apply_rta_trigger_actual(actual, false);
                            }
                            Err(readback) => {
                                self.note_auto_acquisition(readback.status);
                                self.retire_rta(readback);
                            }
                        }
                    }
                    Err(failure) => {
                        self.note_auto_acquisition(failure.status);
                        self.retire_rta(failure);
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
                    self.pnm_retry_at = Some(std::time::Instant::now() + Duration::from_secs(1));
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
    ) -> (Vec<Vec<u8>>, IqFrames, IqFrames, Vec<Vec<u8>>) {
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
            "iq_rate": self.capture_input_rate(),
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
            "start": (center - bandwidth / 2.0).max(self.freq_limits().0), "stop": (center + bandwidth / 2.0).min(self.freq_limits().1),
            "atten": atten, "preamp": preamp, "ifgain": ifgain,
            "ref": ref_dbm,
            "refclk_src": ref_clock_code(&self.ref_clock), "refclk_out": self.refclk_out,
            "listen": self.sdr_listen,
            "demod": self.sdr_demod, "if_bw": effective_if, "ddc_offset": 0.0,
            "mix_offset": 0.0, "ddc_decimate": 64 * fine, "ddc_delay": 102, "ddc_batch": 1,
            "audio_rate": 48_000.0, "deemph_us": self.sdr_deemph,
        });
        let (lo, hi) = (
            (center - bandwidth / 2.0).max(self.freq_limits().0),
            (center + bandwidth / 2.0).min(self.freq_limits().1),
        );
        let wf = self.wf_row(224);
        let mut pan = if self.hardware_shim.is_some() {
            self.sdr_actual["pan_available"] = json!(false);
            Vec::new()
        } else {
            vec![self.encode_rta_frame(&freq, &spec, &wf, lo, hi)]
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
        let iq: IqFrames = if (iq_clients + aux_clients == 0
            && (self.hardware_shim.is_none() || display_clients == 0))
            || ddc_blocked
        {
            Vec::new()
        } else if fixture {
            match self.baseband_frame(48_000.0, self.sdr_center) {
                Ok(frames) => frames.into_iter().map(IqFrame::from).collect(),
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
            let (rf_lo, rf_hi) = self.freq_limits();
            let rf_center = self.sdr_center;
            let usable_now = self.ddc.as_ref().map_or(usable, |d| d.0.usable_bandwidth);
            let pan_lo = (rf_center - bandwidth.min(usable_now) / 2.0).max(rf_lo);
            let pan_hi = (rf_center + bandwidth / 2.0).min(rf_hi).min(rf_center + usable_now / 2.0);
            match self.ddc.as_mut() {
                Some(ddc) => {
                    real_rate = Some(ddc.0.channel_rate());
                    let result = ddc.0.step();
                    let pan_raw = std::mem::take(&mut ddc.0.pan);
                    let real_pan = if result.is_ok() {
                        pan_raw
                            .iter()
                            .filter(|p| p.5 == ddc.0.display_fft.as_ref().filter(|a| a.completed > 0).map_or(0, |a| a.size)).map(|(freq, power, _warning, _generation, _seq, _display)| {
                                hardware_pan(self.freq_version as u32, pan_lo, pan_hi - pan_lo, freq, power)
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
                            if let Some((freq, power, warning, _generation, seq, _)) = pan_raw.iter().rev().find(|p| p.5 == 0) {
                                self.note_auto_acquisition(*warning);
                                self.observe_channel_power(freq,power);
                                let inside: Vec<f32> = freq
                                    .iter()
                                    .zip(power)
                                    .filter(|(f, _)| (pan_lo..=pan_hi).contains(*f))
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
            if multichannel::auxiliary(&frame) {
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

    fn auto_receiver_preparation_required(&self) -> bool {
        self.swp.mode == "sdr" && self.receiver_failure.is_none()
            && self.auto.observation_status == -12
            && self.auto.session.as_ref().is_some_and(|session| !session.display_only
                && session.config == self.config_version && self.auto.observation_config == session.config
                && self.auto.observation > session.seen && session.remaining > 0
                && std::time::Instant::now() < session.deadline)
    }

    fn missing_receiver_preparations(&self) -> Vec<receiver_workers::ChannelConfig> {
        let configs = self.channel_configs();
        match self.receiver_workers.as_ref() {
            Some(workers) => workers.missing_preparations(&configs),
            None => configs.into_iter().filter(|config| config.enabled && config.processing_enabled
                && websa_channelizer::catalog::is_external(&config.mode)).collect(),
        }
    }

    fn advance_auto(&mut self) -> bool {
        if self.auto_receiver_preparation_required() && !self.missing_receiver_preparations().is_empty() {
            return false;
        }
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
const SDR_PACKET_SAMPLES: usize = 16_240;
const SDR_BATCH_PACKETS: usize = 4;
const SDR_BATCH_SAMPLES: usize = SDR_PACKET_SAMPLES * SDR_BATCH_PACKETS;

struct Ddc {
    stream: Box<PfbStream>,
    fine: Option<sdr_pipeline::FineState>,
    fine_worker: Option<sdr_pipeline::FineWorker>,
    direct: BTreeMap<u32, sdr_pipeline::DirectChannel>,
    carry: IqFrames,
    pan: Vec<(Vec<f64>, Vec<f32>, i32, u64, u64, usize)>,
    scan_tap: Option<scan::Tap>,
    default_fft: Option<display_fft::Analyzer>,
    display_fft: Option<display_fft::Analyzer>,
    display_fft_error: Option<String>,
    capture_center: f64,
    usable_bandwidth: f64,
    decimate: i32,
    listen: f64,
    next_seq: u64,
    input_pos: u64,
    capture_scratch: Vec<i16>,
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
    cost_profile: Option<websa_channelizer::FineCostBudget>,
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
        let fs = hardware.as_ref().map_or(IQ_FS, |capture| capture.geometry.sample_rate);
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
        Self::configured_with_optional_profile(center,listen,decimate,bandwidth,hardware,
            aux_frequencies,taps_per_phase,fs,channels,output_rate_budget,None)
    }

    #[cfg(test)]
    fn configured_with_cost_budget(center:f64,listen:f64,decimate:i32,bandwidth:f64,hardware:&mut Option<HardwareCapture>,
        aux:&[f64],taps:usize,fs:f64,channels:usize,output:f64,profile:websa_channelizer::FineCostBudget)->Result<Self,String> {
        Self::configured_with_optional_profile(center,listen,decimate,bandwidth,hardware,aux,taps,fs,channels,output,Some(profile))
    }

    fn configured_with_optional_profile(
        center: f64, listen: f64, decimate: i32, bandwidth: f64,
        hardware: &mut Option<HardwareCapture>, aux_frequencies: &[f64],
        taps_per_phase: usize, fs: f64, channels: usize, _output_rate_budget: f64,
        _cost_profile: Option<websa_channelizer::FineCostBudget>,
    ) -> Result<Self, String> {
        sdr_pipeline::check_plan_geometry(decimate,1+aux_frequencies.len(),channels)?;
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
        let packet_capacity = if fs == IQ_FS { SDR_BATCH_SAMPLES } else { SDR_PACKET_SAMPLES };
        let stream = PfbStream::with_taps(fs, packet_capacity, bins, epoch as u64, taps_per_phase).ok_or("PFB unavailable")?;
        let rate = stream.sample_rate / decimate as f64;
        let hop = stream.input_hop();
        let mut bank = FineBank::new(
            &stream,
            bandwidth,
            packet_capacity / hop + 2,
            channels,
            COARSE_RATE*channels as f64,
            // Same slow-consumer budget as the IQS owner queue gate.
            CONSUMER_BUDGET,
        )
        .ok_or("fine bank unavailable")?;
        bank.subscribe(1, bin, offset, decimate)
            .map_err(str::to_owned)?;
        let framer =
            BasebandFramer::new(1, epoch as u64, listen, rate)
                .and_then(|framer|framer.with_source_rate(fs)).ok_or("baseband frame metadata rejected")?;
        let fine = sdr_pipeline::FineState::new(bank, framer, fs, epoch, listen);
        let registry = fine.registry.clone();
        let generation = hardware.as_ref().map_or(0, |c| c.geometry.generation);
        Ok(Self {
            stream: Box::new(stream),
            fine: Some(fine),
            fine_worker: None,
            direct: BTreeMap::new(),
            carry: Vec::new(),
            pan: Vec::new(),
            scan_tap: None,
            default_fft: None, display_fft: None, display_fft_error: None,
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
            capture_scratch: Vec::new(),
            stamp_gaps: 0,
            last_stamp: None,
            generation,
            hardware: hardware.take(),
            capture_failed: false,
            pipeline_timing: sdr_pipeline::PipelineTiming::new(fs),
            registry,
            #[cfg(test)]
            cost_profile:_cost_profile,
            service_configs: None,
        })
    }

    #[cfg(test)]
    /// New PFB/fine plan on the same IQS capture (no device reopen). The IQBF stream restarts
    /// at seq 0 with the new rate/centre; the old tail frames are returned first. A failure
    /// leaves the running plan, its aux subscriptions and the capture untouched.
    fn replan(&mut self, listen: f64, decimate: i32) -> Result<IqFrames, String> {
        let aux: Vec<(u32, f64)> = self.aux.iter().map(|(&ch, (_, f, _))| (ch, *f)).collect();
        self.replan_with_aux(listen, decimate, &aux)
    }

    #[cfg(test)]
    fn replan_with_aux(
        &mut self,
        listen: f64,
        decimate: i32,
        aux: &[(u32, f64)],
    ) -> Result<IqFrames, String> {
        self.replan_with_taps(listen, decimate, aux, self.stream.taps_per_phase())
    }

    #[cfg(test)]
    fn replan_with_taps(
        &mut self,
        listen: f64,
        decimate: i32,
        aux: &[(u32, f64)],
        taps_per_phase: usize,
    ) -> Result<IqFrames, String> {
        let frequencies: Vec<f64> = aux.iter().map(|(_, f)| *f).collect();
        // `configured` only takes the capture after the whole plan has been built, so a
        // rejected frequency or fine bank leaves the old plan's capture in place.
        let mut next = Self::configured_with_optional_profile(
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
        let mut next = Self::configured_with_optional_profile(center, listen, decimate, bandwidth, &mut None,
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
        let drained = (|| -> Result<IqFrames, String> {
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
    fn tune(&mut self, listen: f64, decimate: i32) -> Result<IqFrames, String> {
        validate_channel_passband(self.capture_center, listen, decimate, self.stream.taps_per_phase())?;
        sdr_pipeline::check_plan_geometry(decimate,1+self.aux.len(),1+self.aux.len())?;
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
            .and_then(|framer|framer.with_source_rate(self.stream.input_sample_rate()))
            .ok_or("baseband frame metadata rejected")?;
        self.stream.flush_pending().map_err(str::to_owned)?;
        let mut frames = std::mem::take(&mut self.carry);
        frames.extend(self.drain()?);
        let old_id = self.channel_id;
        self.bank
            .retune(old_id, id, bin, offset, decimate)
            .map_err(str::to_owned)?;
        if let Some(tail) = self.framer.finish_native().map_err(str::to_owned)? {
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
            .then(|| BasebandFramer::new_aux(id, self.stream.epoch(), freq, self.channel_rate())
                .and_then(|framer|framer.with_source_rate(self.stream.input_sample_rate())))
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
            if let Some(tail) = framer.finish_native().map_err(str::to_owned)? {
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
    fn step(&mut self) -> Result<IqFrames, String> {
        let packets = if self.hardware.is_some() { 32 / self.capture_batch_packets() } else { 329 };
        let mut frames = std::mem::take(&mut self.carry);
        for _ in 0..packets {
            frames.extend(self.packet()?);
        }
        Ok(frames)
    }

    fn capture_batch_packets(&self) -> usize {
        if self.hardware.is_some() && self.device_decimate == 1 && self.direct.is_empty() {
            SDR_BATCH_PACKETS
        } else { 1 }
    }

    fn packet(&mut self) -> Result<IqFrames, String> {
        const N: usize = SDR_PACKET_SAMPLES;
        let batch = self.capture_batch_packets();
        let (raw, stamp) = if self.hardware.is_some() {
            let mut raw = std::mem::take(&mut self.capture_scratch);
            raw.clear();
            let mut first_stamp = 0;
            for index in 0..batch {
                let capture = self.hardware.as_ref().unwrap();
                let packet = loop {
                    if let Some((received, seq, stamp, input_index)) = self.pipeline_timing.received.take() {
                        let elapsed = received.elapsed();
                        self.pipeline_timing.consumer_gap.observe(elapsed);
                        self.pipeline_timing.observe_slow("consumer_gap", seq, stamp, input_index, elapsed);
                    }
                    let packet = capture.next()?;
                    self.pipeline_timing.received = Some((std::time::Instant::now(), self.next_seq, packet.stamp,
                        self.input_pos + (index * N) as u64));
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
                if let Some(tap) = &mut self.scan_tap {
                    tap.push(display_fft::Meta {epoch:self.stream.epoch(),generation:packet.generation,center:self.capture_center,rate:self.stream.input_sample_rate(),bandwidth:self.usable_bandwidth,scale:packet.scale_to_v},packet.seq,packet.stamp,&packet.iq,packet.warning);
                }
                if let Some(analyzer) = &mut self.default_fft {
                    let meta = display_fft::Meta {epoch:self.stream.epoch(),generation:packet.generation,
                        center:self.capture_center,rate:self.stream.input_sample_rate(),bandwidth:self.usable_bandwidth,
                        scale:packet.scale_to_v};
                    match analyzer.push(meta,packet.seq,packet.stamp,&packet.iq,packet.warning) {
                        Ok(Some((freq,power,warning,generation,seq)))=>self.pan.push((freq,power,warning,generation,seq,0)),
                        Ok(None)=>(), Err(error)=>{self.default_fft=None;self.display_fft_error=Some(error);}
                    }
                }
                if let Some(analyzer) = &mut self.display_fft {
                    let meta = display_fft::Meta {epoch:self.stream.epoch(),generation:packet.generation,
                        center:self.capture_center,rate:self.stream.input_sample_rate(),bandwidth:self.usable_bandwidth,
                        scale:packet.scale_to_v};
                    match analyzer.push(meta,packet.seq,packet.stamp,&packet.iq,packet.warning) {
                        Ok(Some((freq,power,warning,generation,seq)))=>self.pan.push((freq,power,warning,generation,seq,analyzer.size)),
                        Ok(None)=>(), Err(error)=>{self.display_fft=None;self.display_fft_error=Some(error);}
                    }
                }
                if let Some((freq, power)) = packet.spectrum {
                    self.pan.push((freq, power, packet.warning, packet.generation, packet.seq,0));
                }
                if batch == 1 { return self.process_capture_packet(&packet.iq, packet.stamp); }
                if index == 0 { first_stamp = packet.stamp; }
                raw.extend_from_slice(&packet.iq);
            }
            (raw, first_stamp)
        } else {
            let mut raw = vec![0i16; N * 2];
            for i in 0..N {
                let t = (self.input_pos + i as u64) as f64 / 62_500_000.0;
                let envelope = 0.25 * (1.0 + 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * t).cos());
                raw[2 * i] = (envelope * 20_000.0) as i16;
            }
            (raw, self.input_pos + N as u64)
        };
        let result = self.process_capture_packet(&raw, stamp);
        self.capture_scratch = raw;
        result
    }

    /// Raw capture input follows the same PFB, fine filters and framing for hardware and probes.
    fn process_capture_packet(&mut self, raw: &[i16], stamp:u64) -> Result<IqFrames,String> {
        let count = raw.len()/2;
        if !matches!(count, SDR_PACKET_SAMPLES | SDR_BATCH_SAMPLES) || raw.len()%2 != 0
            || (count != SDR_PACKET_SAMPLES && !self.direct.is_empty()) {
            return Err("invalid capture packet size".into());
        }
        self.input_pos += count as u64;
        let seq = self.next_seq;
        let mut frames = Vec::new();
        if let Some(worker)=self.fine_worker.as_mut() {worker.submit_raw(seq,stamp,self.input_pos-count as u64,raw)?;}
        else {for route in self.direct.values_mut() {frames.extend(route.process(seq,stamp,self.input_pos-count as u64,raw)?);}}
        let mut retries = 0;
        loop {
            let started = std::time::Instant::now();
            let submitted = self.stream.submit(seq, stamp, raw);
            let elapsed = started.elapsed();
            self.pipeline_timing.submit.observe(elapsed);
            self.pipeline_timing.observe_slow("pfb_submit", seq, stamp, self.input_pos - count as u64, elapsed);
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

    fn drain(&mut self) -> Result<IqFrames, String> {
        let mut frames = Vec::new();
        while let Some(packet) = self.stream.take() {
            if let Some(worker) = self.fine_worker.as_mut() {
                let (seq, stamp, input_index) = (packet.seq, packet.stamp, packet.input_index);
                let started = std::time::Instant::now();
                let submitted = worker.submit(&packet, &mut frames);
                let elapsed = started.elapsed();
                self.pipeline_timing.fine_handoff.observe(elapsed);
                self.pipeline_timing.observe_slow("fine_handoff", seq, stamp, input_index, elapsed);
                submitted?;
            } else {
                frames.extend(self.fine.as_mut().unwrap().deliver(&packet)?);
            }
            self.stream.recycle_output(packet.iq);
        }
        if let Some(worker) = self.fine_worker.as_mut() {
            worker.collect(&mut frames)?;
        }
        Ok(frames)
    }
}

impl std::ops::Deref for Ddc {
    type Target = sdr_pipeline::FineState;
    fn deref(&self) -> &Self::Target { self.fine.as_ref().or_else(||self.fine_worker.as_ref().map(|worker|&worker.state)).expect("fine stage absent") }
}
impl std::ops::DerefMut for Ddc {
    fn deref_mut(&mut self) -> &mut Self::Target { if let Some(state)=self.fine.as_mut() {state} else {&mut self.fine_worker.as_mut().expect("fine stage absent").state} }
}

fn hardware_pan(
    version: u32,
    lo: f64,
    bandwidth: f64,
    freq: &[f64],
    power: &[f32],
) -> Result<Vec<u8>, String> {
    let hi = lo + bandwidth;
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

#[cfg(test)]
fn dispatch(state: &mut DevState, cmd: &str, data: &Value) -> Result<bool, CommandError> {
    prepare_receiver_recovery(state, cmd, data)?;
    dispatch_ready(state, cmd, data)
}

fn prepare_receiver_recovery(state: &mut DevState, cmd: &str, data: &Value) -> Result<(), CommandError> {
    let recovery = matches!(cmd, "SET_SDR_CHANNEL" | "SET_SDR_AUX" | "SET_SDR_DEMOD" | "SET_SDR_TUNE" | "SET_SDR_DDC")
        && state.receiver_failure.is_some() && ["mode", "demod"].iter().any(|field| data.get(*field).is_some());
    let mode_change = cmd == "SET_MODE" && data["mode"].as_str().is_some_and(|mode| mode != state.swp.mode);
    if (recovery || mode_change) && !state.reap_failed_receivers() {
        return Err(CommandError::new("receiver_retiring", "Receiver is still stopping; retry shortly"));
    }
    Ok(())
}

fn dispatch_ready(state: &mut DevState, cmd: &str, data: &Value) -> Result<bool, CommandError> {
    state.remember_receiver_streams();
    let previous = matches!(cmd, "SET_FREQ" | "SET_RTA" | "SET_RBW" | "SET_VBW"
        | "SET_SWEEP" | "SET_POINTS" | "SET_SPUR" | "SET_WINDOW" | "SET_DETECTOR"
        | "SET_FRAMEDET" | "SET_TRACEGRID" | "SET_REF" | "SET_AMP" | "SET_REFCK"
        | "SET_REFCKOUT" | "SET_PRESET").then(|| RequestedConfiguration::capture(state));
    let result = dispatch_inner(state, cmd, data);
    if result.is_err() {
        if let Some(previous) = previous { previous.restore(state); }
    } else if cmd.starts_with("SET_") && cmd != "SET_SDR_FFT" && !(cmd == "SET_REF" && data["mode"] == "auto") {
        state.invalidate_observation();
    }
    if result.is_ok() {
        // Capture fitting and Preset can change channel settings without going
        // through channel controls. Publish their new revisions in this reply,
        // before a following command validates the acknowledged receiver.
        state.remember_receiver_streams();
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
                state.channel_power = vec![None; MAX_CHANNELS];
                state.stop_pipeline().map_err(device)?;
                state.finish_radiofax_with_registry(registry).map_err(device)?;
                state.ddc_failure_diagnostics = None;
                state.ddc_first_failure_diagnostics = None;
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
                state.channel_power = vec![None; MAX_CHANNELS];
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
            let start = num(data, "start").map_or(state.pnm_start, |v| v.max(1.0));
            let stop = num(data, "stop").map_or(state.pnm_stop, |v| v.max(10.0));
            if stop <= start {
                return Err(CommandError::new(
                    "bad_parameter",
                    "PNM stop must be greater than start",
                ));
            }
            // Commit the complete candidate only after its coupled bounds pass.
            // Rejection must preserve both the active cycle and its successor.
            if let Some(v) = num(data, "center") {
                state.pnm_center = v;
            }
            if let Some(v) = num(data, "threshold") {
                state.pnm_threshold = v;
            }
            if let Some(v) = num(data, "traceavg") {
                state.pnm_traceavg = v as i64;
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
        "SET_SDR_FFT" => {
            sdr_mode_required(state,cmd)?;
            let size=data["size"].as_u64().filter(|n|[0,32768,65536].contains(n))
                .ok_or_else(||CommandError::new("bad_parameter","FFT size must be 0, 32768 or 65536"))? as usize;
            if state.pipeline.is_none() && state.ddc.is_none() {return Err(CommandError::new("bad_parameter","Display FFT requires a running IQ capture"));}
            let path=state.hardware_shim.clone().ok_or_else(||CommandError::new("bad_parameter","Real display FFT requires a hardware IQ source; fixture spectrum is illustrative"))?;
            // Validate and prepare before changing the working analyzer. Failure keeps it intact.
            let analyzer=if size==0 {None} else {Some(display_fft::Analyzer::start(path,size).map_err(device)?)};
            state.ddc_command(move |ddc|{ddc.display_fft=analyzer;ddc.display_fft_error=None;ddc.pan.clear();Ok(Vec::new())}).map_err(device)?;
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
    identities: Arc<[AtomicU64]>,
    /// Display configuration fence; independent of the lossless IQ session epoch.
    display_version: Arc<AtomicU64>,
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
async fn publisher(
    app: Arc<App>, stopping: Arc<AtomicBool>, fatal: Option<tokio::sync::oneshot::Sender<String>>,
) -> Result<(), String> {
    #[cfg(test)]
    return publisher_loop(app, stopping, fatal, PublisherTestHooks::default()).await;
    #[cfg(not(test))]
    publisher_loop(app, stopping, fatal).await
}

type PublisherPreparation = tokio::task::JoinHandle<(u64, Vec<receiver_workers::ChannelConfig>, Result<receiver_workers::PreparedDecoders, String>)>;

#[cfg(test)]
#[derive(Default)]
struct PublisherTestHooks {
    before_step: Option<Box<dyn FnOnce() + Send>>,
    before_loop: Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
    preparing: Option<PublisherPreparation>,
}

async fn publisher_loop(
    app: Arc<App>, stopping: Arc<AtomicBool>, mut fatal: Option<tokio::sync::oneshot::Sender<String>>,
    #[cfg(test)] mut hooks: PublisherTestHooks,
) -> Result<(), String> {
    #[cfg(not(test))]
    let mut preparing: Option<PublisherPreparation> = None;
    #[cfg(test)]
    let mut preparing = hooks.preparing.take();
    // Keep preparation ownership outside the unwind boundary. A panic in the
    // async loop must not detach work before lifecycle releases the owners.
    let result = std::panic::AssertUnwindSafe(async {
        #[cfg(test)]
        if let Some(before_loop) = hooks.before_loop.take() { before_loop.await; }
        let mut status_due = tokio::time::Instant::now();
        let mut preparation_failed = None;
        let mut fixture_cadence = service_fixture::Cadence::default();
        while !stopping.load(Ordering::Acquire) {
            let started = tokio::time::Instant::now();
            let mut prepared = None;
            if preparing.as_ref().is_some_and(|task| task.is_finished()) {
                match preparing.take().unwrap().await {
                    Ok(result) => prepared = Some(result),
                    Err(error) => {
                        return Err(format!("decoder preparation worker failed: {error}"));
                    }
                }
            }
            if preparing.is_none() && prepared.is_none() {
                let candidate = {
                    let state = app.state.lock_async().await.unwrap();
                    let restarting = state.swp.mode == "sdr" && state.receiver_failure.is_none()
                        && ((state.hardware_shim.is_some() && state.pipeline.is_none() && state.ddc.is_none())
                            || state.auto_receiver_preparation_required());
                    let missing = if restarting { state.missing_receiver_preparations() }
                        else { state.missing_reception_preparations() };
                    // Failed recovery preparation stops only its own lanes. A
                    // later fault on another lane must still prepare at this revision.
                    (!missing.is_empty() && (!restarting || preparation_failed != Some((state.config_version, state.auto.seq))))
                        .then_some((state.config_version, state.channel_configs(), missing))
                };
                if let Some((revision, configs, missing)) = candidate {
                    preparing = Some(tokio::task::spawn_blocking(move ||
                        (revision, configs, receiver_workers::PreparedDecoders::new(&missing))));
                }
            }
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
            #[cfg(test)]
            let before_step = hooks.before_step.take();
            let result = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                if let Some(before_step) = before_step { before_step(); }
                let mut discarded_preparation = None;
                let mut state = compute.state.lock().unwrap();
                let mut preparation_error = None;
                let mut failed_revision = None;
                if let Some((revision, configs, result)) = prepared {
                    if state.config_version == revision && state.channel_configs() == configs && state.swp.mode == "sdr"
                        && state.receiver_failure.is_none() {
                        match result {
                            Ok(batch) => {
                                if state.receiver_workers.is_none() {
                                    match receiver_workers::ReceiverWorkers::new() {
                                        Ok(workers) => state.receiver_workers = Some(workers),
                                        Err(error) => preparation_error = Some(error),
                                    }
                                }
                                if let Some(workers) = state.receiver_workers.as_mut() {
                                    discarded_preparation = Some(workers.exchange_prepared(batch));
                                } else {
                                    discarded_preparation = Some(batch);
                                }
                            }
                            Err(error) => preparation_error = Some(error),
                        }
                        if let Some(error) = preparation_error.as_ref() {
                            state.reception_preparation_failed(error);
                            if state.auto.session.is_some() { state.finish_auto("failed", None); }
                            failed_revision = Some((revision, state.auto.seq));
                            eprintln!("WebSA receiver preparation failed: {error}");
                        }
                    }
                    else if let Ok(batch) = result { discarded_preparation = Some(batch); }
                }
                let recording_clients = usize::from(state.recordings.as_ref().is_some_and(|r|r.has_active()));
                let receiver_clients = usize::from(state.sdr_channels.iter().any(|config|config.enabled));
                state.scan_tick_due();
                state.flush_scan_control(&compute);
                let mode = state.swp.mode.clone();
                let epoch = compute.epoch.load(Ordering::Acquire);
                let mut display: Vec<Bytes> = Vec::new();
                let mut iq_frame: IqFrames = Vec::new();
                let mut aux_frame: IqFrames = Vec::new();
                let mut msgs: Vec<Value> = Vec::new();
                if let Some(error) = preparation_error {
                    msgs.push(json!({"type":"receiver_preparation_error", "error":error}));
                }
                let mut hardware = None;
                let acquisition_started = std::time::Instant::now();
                let pace = match mode.as_str() {
                    "server" => {
                        state.server.lock().poll();
                        Duration::from_millis(100)
                    }
                    "harmonic" => {
                        let (frames, harm_msgs) = state.step_harm();
                        display.extend(frames.into_iter().map(Bytes::from));
                        msgs.extend(harm_msgs);
                        Duration::from_millis(20)
                    }
                    "pnm" => {
                        msgs.extend(state.step_pnm());
                        Duration::from_millis(50)
                    }
                    "rta" => {
                        display.push(state.step_rta().into());
                        if state.rta_completed_frame.is_some() { Duration::from_millis(200) }
                        else { Duration::from_millis(10) }
                    }
                    "sdr" if state.hardware_shim.is_some() => {
                        let listeners = iq_clients + aux_clients + display_clients + audio_clients + decode_clients + recording_clients + receiver_clients
                            + usize::from(state.auto.session.is_some());
                        let waiting = state.pipeline.is_none() && state.ddc.is_none()
                            && state.receiver_failure.is_none() && !state.missing_receiver_preparations().is_empty();
                        if !waiting {
                            match state.take_hardware_presentation(listeners) {
                                Ok(output) => hardware = output,
                                Err(error) => state.hardware_presentation_failed(error),
                            }
                        }
                        Duration::from_millis(1)
                    }
                    "sdr" => {
                        let (pan,retiring_iq,retiring_aux,_)=state.step_sdr(0,0,display_clients,0);
                        iq_frame.extend(retiring_iq); aux_frame.extend(retiring_aux);
                        display.extend(pan.into_iter().map(Bytes::from));
                        if iq_clients+aux_clients+audio_clients+decode_clients+recording_clients+receiver_clients>0 {
                            let configs=state.channel_configs();
                            let source=state.fixture_sources.get_or_insert_with(service_fixture::FixtureSources::new);
                            match source.configure(&configs).and_then(|()|source.step()) {
                                Ok(frames)=>for frame in frames {
                                    if multichannel::auxiliary(&frame) {aux_frame.push(frame);}
                                    else if frame.header().starts_with(b"IQBF") {iq_frame.push(frame);}
                                },
                                Err(error)=>state.ddc_error=Some(error),
                            }
                        }
                        service_fixture::PERIOD
                    }
                    _ => {
                        let (freq, powr) = state.fetch_sweep();
                        if let Some(frame) = freq {
                            display.push(frame.into());
                        }
                        if !powr.is_empty() {
                            display.push(powr.into());
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
                drop(discarded_preparation);
                if let Some(hardware) = hardware {
                    match hardware.finish() {
                        Ok(mut output) => {
                            compute.state.lock().unwrap().apply_hardware_presentation(&mut output);
                            display.extend(output.pan.into_iter().map(Bytes::from));
                            for frame in output.frames {
                                if multichannel::auxiliary(&frame) { aux_frame.push(frame); }
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
                                a.append(b); Ok(a)
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
                    service_stream::publish_fine_failures(&mut state,&compute);
                    state.advance_reception_recovery();
                    state.reap_failed_receivers();
                    if let Some(message)=service_stream::publish_receiver_failure(&mut state,&compute) {msgs.push(message);}
                }
                let mut unused_preparation = None;
                let mut superseded_preparation = None;
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
                    let configs = state.channel_configs();
                    if let Some(workers) = state.receiver_workers.as_mut() {
                        superseded_preparation = Some(workers.release_prepared(&configs));
                    }
                    if !state.auto_receiver_preparation_required() {
                        let registry = state.stream_registry();
                        if let Some(workers) = state.receiver_workers.as_mut() {
                            unused_preparation = Some(workers.discard_staged(&registry));
                        }
                    }
                    changed
                };
                drop(unused_preparation);
                drop(superseded_preparation);
                let mut status = None;
                if send_status || auto_changed || failed_revision.is_some() {
                    let snapshot = compute.state.lock().unwrap().live_status_snapshot();
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
                (display, iq_frame, aux_frame, msgs, status, pace, epoch, display_version, failed_revision)
            })
            .await;
            let (display, iq_frame, aux_frame, msgs, status, pace, epoch, display_version, failed_revision) = match result {
                Ok(value) => value,
                Err(error) => {
                    return Err(format!("acquisition worker failed: {error}"));
                }
            };
            if let Some(revision) = failed_revision { preparation_failed = Some(revision); }
            if send_status {
                status_due = tokio::time::Instant::now();
            }
            let (iq_frame,aux_frame)={
                let mut state=app.state.lock_async().await.unwrap();
                let clients=app.clients.lock();
                let iq_all=clients.iter().any(|client|client.iq&&client.receiver_uid.is_none());
                let aux_all=clients.iter().any(|client|client.aux&&client.receiver_uid.is_none());
                let iq_uids:Vec<_>=clients.iter().filter(|client|client.iq).filter_map(|client|client.receiver_uid.clone()).collect();
                let aux_uids:Vec<_>=clients.iter().filter(|client|client.aux).filter_map(|client|client.receiver_uid.clone()).collect();drop(clients);
                let tagged=(multichannel::tag_iq_selected(&mut state,iq_frame,iq_all,&iq_uids),multichannel::tag_iq_selected(&mut state,aux_frame,aux_all,&aux_uids));
                let error=if state.swp.mode!="sdr" {Some("capture left SDR mode".to_owned())}
                    else if !state.link_up {Some("capture disconnected".to_owned())} else {state.ddc_error.clone()};
                if let Some(recordings)=state.recordings.as_mut() {recordings.tick(error.as_deref());}
                multichannel::recording_processing_sources(&state);
                tagged
            };
            {
                let clients = app.clients.lock();
                for client in clients.iter() {
                    if client.iq {
                        for (frame,uid) in &iq_frame {
                            if client.receiver_uid.as_ref().is_some_and(|wanted|Some(wanted)!=uid.as_ref())
                                || (client.receiver_uid.is_none() && frame.starts_with(b"IQEN")) {continue;}
                            send_or_count(client, Message::Binary(frame.clone()), epoch);
                        }
                        continue;
                    }
                    if client.aux {
                        for (frame,uid) in &aux_frame {
                            if client.receiver_uid.as_ref().is_some_and(|wanted|Some(wanted)!=uid.as_ref())
                                || (client.receiver_uid.is_none() && frame.starts_with(b"IQEN")) {continue;}
                            send_or_count(client, Message::Binary(frame.clone()), epoch);
                        }
                        continue;
                    }
                    if client.audio || client.decode {continue;}
                    for frame in &display {
                        let version = (frame.starts_with(b"RTAF") || frame.starts_with(b"RTA2") || frame.starts_with(b"FREQ"))
                            .then_some(display_version);
                        enqueue(client, Outbound {
                            epoch, message: Message::Binary(frame.clone()).into(),
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
                    return Err(format!("GNSS worker failed: {error}"));
                }
            }
            let deadline = if pace == service_fixture::PERIOD {
                fixture_cadence.deadline(epoch, started)
            } else {
                fixture_cadence.reset();
                started + pace
            };
            if deadline <= tokio::time::Instant::now() {
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep_until(deadline).await;
            }
        }
        Ok::<(), String>(())
    }).catch_unwind().await;
    let mut failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(panic) => {
            let detail = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic payload");
            Some(format!("publisher loop panicked: {detail}"))
        }
    };
    if let Some(error) = failure.as_ref() {
        eprintln!("WebSA {error}");
        // Stop listening before draining potentially slow model work. The
        // lifecycle still joins this publisher before tearing down SDK/storage.
        if let Some(fatal) = fatal.take() { let _ = fatal.send(error.clone()); }
    }
    if let Some(task) = preparing {
        if let Err(error) = task.await {
            let error = format!("decoder preparation worker failed: {error}");
            eprintln!("WebSA {error}");
            if failure.is_none() {
                if let Some(fatal) = fatal { let _ = fatal.send(error.clone()); }
                failure = Some(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Display frames use latest-wins per client (Python FRAME_POLICY): POWR/RTAF replace the
/// pending frame, FREQ is retained until sent. IQ/audio/aux stay FIFO (never dropped in
/// order); a full FIFO slot signals the slow-consumer close. Every packet carries the state
/// generation that produced it.
fn send_or_count(client: &Client, message: Message, epoch: u64) {
    let identity=service_stream::message_identity(&message);
    let payload=if client.receiver_uid.is_some() {
        let bytes=match &message {Message::Binary(bytes)=>bytes.len() as u64,_=>0};
        let Some(reservation)=DecodeReservation::iq(&client.decode_pending_bytes,bytes) else {
            client.overflow.store(true,Ordering::Release);client.wake.notify_one();return;
        };
        OutboundPayload::Iq {message,queued_at:std::time::Instant::now(),_reservation:reservation}
    } else {message.into()};
    enqueue(client, Outbound { epoch, message:payload,identity,status_revision: None, display_version: None });
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


#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq)]
struct DecodeApplied { channel: u32, channel_id: u32, epoch: u32, seq: u32 }

struct DecodePending { applied: DecodeApplied, deadline: tokio::time::Instant, bytes: usize }

#[derive(Default)]
struct DecodeCredit { pending: std::collections::VecDeque<DecodePending>, bytes: usize }

impl DecodeCredit {
    fn can_send(&self, bytes: usize) -> bool {
        self.pending.len() < DECODE_ACK_WINDOW && self.bytes + bytes <= DECODE_ACK_BYTES
    }

    fn sent(&mut self, applied: DecodeApplied, bytes: usize) {
        self.pending.push_back(DecodePending {
            applied, bytes, deadline: tokio::time::Instant::now() + DECODE_ACK_TIMEOUT,
        });
        self.bytes += bytes;
    }

    fn acknowledge(&mut self, applied: DecodeApplied) -> bool {
        // Only a frame actually sent can grant cumulative credit. A guessed future
        // sequence, another identity, or a duplicate ACK cannot release memory.
        if !self.pending.iter().any(|entry| entry.applied == applied) { return false; }
        self.pending.retain(|entry| {
            let prior = entry.applied.channel == applied.channel
                && entry.applied.channel_id == applied.channel_id
                && entry.applied.epoch == applied.epoch && entry.applied.seq <= applied.seq;
            if prior { self.bytes -= entry.bytes; }
            !prior
        });
        true
    }

    fn acknowledge_text(&mut self, text: &str) -> bool {
        #[derive(serde::Deserialize)]
        struct Ack { #[serde(rename="type")] kind: String, #[serde(flatten)] applied: DecodeApplied }
        serde_json::from_str::<Ack>(text).is_ok_and(|ack| ack.kind == "ack" && self.acknowledge(ack.applied))
    }

    fn deadline(&self) -> Option<tokio::time::Instant> { self.pending.front().map(|entry| entry.deadline) }
}

#[derive(Debug)]
struct DecodeAckTimeout;

async fn send_with_decode_deadline(deadline:Option<tokio::time::Instant>,
    send:impl Future<Output=bool>) -> Result<bool,DecodeAckTimeout> {
    let Some(deadline)=deadline else {return Ok(send.await);};
    // timeout_at polls its future before checking the timer. Check both sides
    // explicitly so even an immediately ready send cannot outlive old credit.
    if tokio::time::Instant::now()>=deadline {return Err(DecodeAckTimeout);}
    let sent=tokio::time::timeout_at(deadline,send).await.map_err(|_|DecodeAckTimeout)?;
    if tokio::time::Instant::now()>=deadline {return Err(DecodeAckTimeout);}
    Ok(sent)
}

async fn wait_decode_credit(stream: &mut futures_util::stream::SplitStream<WebSocket>, credit: &mut DecodeCredit)
    -> Result<bool, DecodeAckTimeout> {
    let Some(deadline) = credit.deadline() else { return Ok(false); };
    tokio::time::timeout_at(deadline, async {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) if credit.acknowledge_text(&text) => return true,
                Message::Close(_) => return false,
                _ => {}
            }
        }
        false
    }).await.map_err(|_|DecodeAckTimeout)
}

async fn send_decode_frame(sink:&mut WsSink, stream:&mut futures_util::stream::SplitStream<WebSocket>,
    frame:Message, decode_ack:bool, credit:&mut DecodeCredit) -> Result<bool,DecodeAckTimeout> {
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
    let bytes = match &frame { Message::Binary(bytes) => bytes.len(), _ => 0 };
    if bytes > DECODE_ACK_BYTES { return Ok(false); }
    while !credit.can_send(bytes) {
        if !wait_decode_credit(stream,credit).await? { return Ok(false); }
    }
    if !send_with_decode_deadline(credit.deadline(),send_frame(sink,frame)).await? {return Ok(false);}
    credit.sent(expected,bytes);
    Ok(true)
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

async fn close_stream(sink: &mut WsSink, receiver_failed: bool, subscriber_failed: bool, ack_timeout: bool) {
    let _ = tokio::time::timeout(
        Duration::from_millis(250),
        sink.send(Message::Close(Some(CloseFrame {
            code: if receiver_failed || subscriber_failed { 1011 } else { 1013 },
            reason: if receiver_failed { "receiver failed" } else if subscriber_failed {
                "subscriber source stream invalid"
            } else if ack_timeout { "decoded-output acknowledgement timeout" }
            else { "IQ/audio consumer too slow" }.into(),
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
    ws.on_upgrade(move |socket| client_loop(app, socket, iq, aux, audio, false, None, false,None))
}

async fn client_loop(app: Arc<App>, socket: WebSocket, iq: bool, aux: bool, audio: bool, decode:bool, channel:Option<u32>, decode_ack:bool, receiver_uid:Option<String>) {
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
    let mut iq_subscriber=multichannel::IqSubscriber::default();
    let mut iq_health=tokio::time::interval(Duration::from_millis(500));
    let initial_decode={
        let mut state=app.state.lock_async().await.unwrap();
        if (audio || decode) && (state.receiver_failure.is_some() || channel.is_some_and(|channel|
            state.receiver_workers.as_ref().and_then(|workers|workers.channel_failure(channel)).is_some())) {
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
        receiver_uid:receiver_uid.clone(),
        identities: app.identities.clone(),
        latest: Arc::clone(&latest_slot),
    });
        if receiver_uid.as_deref().is_some_and(|uid|state.receiver_slot(uid).is_none_or(|slot|
            !state.channel_configs()[(slot-1) as usize].enabled ||
                ((audio||decode)&&!state.channel_configs()[(slot-1) as usize].processing_enabled))) {Err("receiver stopped during subscription".into())}
        else if decode {
            state.decode_snapshots.initial(channel,&state.processing_registry(),&mut subscriber).and_then(|frames| {
                if let Some(uid)=receiver_uid.as_deref() {frames.into_iter().filter_map(|frame|
                    multichannel::processing_frame(&mut state,uid,frame).transpose()).collect()}
                else {Ok(frames)}
            })
        }
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
    let mut decode_credit = DecodeCredit::default();
    let mut subscriber_failed = false;
    let mut ack_timeout = false;
    match initial_decode {
        Ok(frames)=>for frame in frames {
            match send_decode_frame(&mut sink,&mut stream,frame,decode_ack,&mut decode_credit).await {
                Ok(true)=>{},
                Ok(false)=>{initial_ok=false;break;},
                Err(_)=>{ack_timeout=true;overflow.store(true,Ordering::Release);initial_ok=false;break;}
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
            let ack_deadline = decode_credit.deadline();
            tokio::select! {
                _ = iq_health.tick(), if receiver_uid.is_some() => {
                    let closing={let mut state=app.state.lock_async().await.unwrap();
                        let failed=!state.link_up || state.swp.mode!="sdr" || state.ddc_error.is_some()
                            || ((audio||decode)&&receiver_uid.as_deref().is_some_and(|uid|
                                state.receiver_slot(uid).is_none_or(|slot| {
                                    let c=state.channel_configs()[(slot-1) as usize].clone();!c.enabled||!c.processing_enabled
                                })));
                        failed.then(||(service_stream::receiver_end_frame(&state,channel,receiver_uid.as_deref()),state.swp.mode!="sdr"))};
                    if let Some((closing,mode_inactive))=closing {
                        if audio||decode {
                            if !rx.is_empty() {continue;}
                            let _=send_frame(&mut sink,closing).await;break false;
                        }
                        let boundary=json!({"type":"stream_boundary","receiver_uid":receiver_uid,
                            "previous":null,"next":null,"clock":null,"reason":"capture_unavailable","drained":false});
                        let _=send_frame(&mut sink,Message::Text(boundary.to_string().into())).await;
                        if mode_inactive {
                            let _=send_frame(&mut sink,closing).await;break false;
                        }
                        break true;
                    }
                }
                _ = tokio::time::sleep_until(ack_deadline.unwrap_or_else(||tokio::time::Instant::now()+DECODE_ACK_TIMEOUT)), if ack_deadline.is_some() => {
                    ack_timeout=true;
                    overflow.store(true,Ordering::Release);
                    break true;
                }
                _ = wake.notified() => {
                    if !flush_latest(&mut sink, &latest_slot, &epoch, &display_version).await {
                        break false;
                    }
                }
                incoming = stream.next() => {
                    let Some(Ok(message)) = incoming else { break false };
                    let text = match message {
                        Message::Text(text) => text,
                        Message::Close(_) => { let _=sink.flush().await; break false; },
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
                    if decode && data.get("type").and_then(Value::as_str) == Some("ack") {
                        decode_credit.acknowledge_text(&text);
                        continue;
                    }
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
                    let admission=match service_preflight::preflight(&app,&cmd,&data).await {
                        Ok(admission)=>admission,
                        Err(error)=>{
                            let error=with_request_id(CommandError::new("bad_parameter",&error).reply_payload(&cmd),request_id.as_ref()).to_string();
                            if !send_frame(&mut sink,Message::Text(error.into())).await {break false;}
                            continue;
                        }
                    };
                    let session_id = matches!(cmd.as_str(), "SET_PNM" | "SET_HARM")
                        .then(|| data.get("session_id").and_then(Value::as_str).map(str::to_owned))
                        .flatten();
                    let reply = match service_commands::execute(app.clone(), admission, cmd.clone(), data).await {
                        Ok(reply) => reply,
                        Err(error) => {
                            eprintln!("WebSA command worker failed: {error}");
                            break false;
                        }
                    };
                    let sent = match reply {
                        Ok(reply) => send_status(&mut sink, &app.state, (reply.revision, reply.status), Some(&cmd), session_id.as_deref(), request_id.as_ref(), &mut seen_status_revision).await,
                        Err(error) => send_frame(&mut sink, Message::Text(with_request_id(error.reply_payload(&cmd), request_id.as_ref()).to_string().into())).await,
                    };
                    if !sent { break false; }
                }
                packet = rx.recv() => {
                    let Some(packet) = packet else { break false };
                    if !packet.epoch_current(epoch.load(Ordering::Acquire))
                        || packet.status_revision.is_some_and(|r| r < seen_status_revision || r < status_revision.load(Ordering::Acquire))
                        || packet.display_version.is_some_and(|v| v != display_version.load(Ordering::Acquire))
                    {
                        continue;
                    }
                    if let Some(revision) = packet.status_revision {
                        seen_status_revision = revision;
                    }
                    if matches!(&packet.message,OutboundPayload::Iq {queued_at,..} if queued_at.elapsed()>Duration::from_secs(2)) {
                        overflow.store(true,Ordering::Release);break 'client true;
                    }
                    for message in packet.frames() {
                        if receiver_failed.load(Ordering::Acquire) || overflow.load(Ordering::Acquire) {break 'client true;}
                        if !packet.epoch_current(epoch.load(Ordering::Acquire)) {continue;}
                        let framed=if let Some(uid)=receiver_uid.as_deref() {
                            if iq||aux {iq_subscriber.frames(&mut app.state.lock_async().await.unwrap(),uid,message)}
                            else {
                                match multichannel::processing_frame(&mut app.state.lock_async().await.unwrap(),uid,message) {
                                    Ok(Some(frame))=>subscriber.frames(frame),Ok(None)=>Ok(Vec::new()),Err(error)=>Err(error),
                                }
                            }
                        } else {subscriber.frames(message)};
                        let frames=match framed {
                            Ok(frames)=>frames,
                            Err(error)=>{eprintln!("SDR subscriber stream: {error}");subscriber_failed=true;break 'client true;},
                        };
                        for frame in frames {
                            let closing=matches!(&frame,Message::Close(_));
                            match send_decode_frame(&mut sink,&mut stream,frame,decode_ack,&mut decode_credit).await {
                                Ok(true)=>{},
                                Ok(false)=>break 'client false,
                                Err(_)=>{ack_timeout=true;overflow.store(true,Ordering::Release);break 'client true;}
                            }
                            if closing {break 'client false;}
                        }
                        if iq_subscriber.ended {
                            let _=send_frame(&mut sink,Message::Close(None)).await;
                            break 'client false;
                        }
                    }
                }
            }
        }
    } else {
        overflow.load(Ordering::Acquire) || receiver_failed.load(Ordering::Acquire)
    };
    if slow {
        close_stream(&mut sink,receiver_failed.load(Ordering::Acquire),subscriber_failed,ack_timeout).await;
    }
    app.clients.lock().retain(|client| client.id != id);
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
    schema["commands"]["SET_SDR_FFT"]=json!({
        "needs_device":true,"denied_in_sdr":false,"swp_only":false,"session_exclusive":false,
        "params":[{"name":"size","type":"integer","required":true,"choices":[0,32768,65536],"unit":"samples"}]
    });
    schema["commands"]["SET_SDR"]["params"].as_array_mut().unwrap().push(json!({
        "name": "pfb_taps_per_phase", "type": "integer", "choices": [4, 8, 16],
        "default": 16, "min": 4, "max": 16, "required": false, "unit": ""
    }));
    schema["commands"]["SET_SDR"]["params"].as_array_mut().unwrap().push(json!({
        "name":"listen", "type":"number", "required":false, "unit":"Hz"
    }));
    for command in ["SET_SDR_DEMOD","SET_SDR_DDC"] {
        if let Some(params)=schema["commands"][command]["params"].as_array_mut() {
            params.extend([
                json!({"name":"rf_snr","type":"boolean","required":false,"default":false}),
                json!({"name":"nr","type":"boolean","required":false,"default":false}),
                json!({"name":"nr_algorithm","type":"choice","required":false,"default":"wiener",
                    "choices":["wiener","dfn","gtcrn","ul-unas","fastenhancer"]}),
                json!({"name":"nr_strength","type":"number","required":false,"default":0.6,"min":0.0,"max":1.0}),
                json!({"name":"nr_atten_db","type":"number","required":false,"default":10.0,"min":0.0,"max":100.0,"unit":"dB"}),
            ]);
        }
    }
    schema["commands"]["SET_SDR_AUX"] = schema["commands"]["SET_SDR_DDC"].clone();
    let mut channel = schema["commands"]["SET_SDR_DEMOD"].clone();
    channel["params"].as_array_mut().unwrap().push(json!({
        "name": "channel", "type": "integer", "choices": (1..=MAX_CHANNELS).collect::<Vec<_>>(), "default": 1,
        "min": 1, "max": MAX_CHANNELS, "required": false, "unit": ""
    }));
    schema["commands"]["SET_SDR_CHANNEL"] = channel;
    (serde_json::from_str(STATE).expect("embedded golden state JSON"), schema)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Bound vendor MKL/OpenMP parallelism before the SDK is loaded. The SDK's
    // nested teams compete with acquisition under load; Rust receiver workers
    // keep their own independent scheduling and admission limits.
    for (key, value) in [("OMP_WAIT_POLICY", "PASSIVE"), ("KMP_BLOCKTIME", "0"),
        ("MKL_NUM_THREADS", "1"), ("MKL_DYNAMIC", "FALSE")] {
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
        .block_on(service_lifecycle::serve())
}

#[cfg(test)]
#[path = "websad/tests.rs"]
mod tests;
