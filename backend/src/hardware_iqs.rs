//! Exclusive SDK 0.55.100 IQS owner for the opt-in WebSA SDR path.
//! Only this thread calls open/next/close. Sustained stalls end the stream explicitly.
use crate::hardware_swp::{
    owner_slot_free, park_stuck_owner, read_defaults, read_front_end_actual, FrontEnd,
    FrontEndActual, GnssInfo, SwpDefaults, OPEN_TIMEOUT,
};
use websa_channelizer::timing::{Timing, TimingSnapshot};
use parking_lot::Mutex;

use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const N: usize = 16240;
const FS: f64 = 62_500_000.0;
/// Bounded SDK-owner handoff. The depth absorbs host scheduling tails without
/// dropping packets or creating an unbounded buffer; it is a latency/memory
/// tradeoff, not an admission threshold.
const PACKET_QUEUE: usize = 64;

/// Schedule in full-rate sample ticks, so decimation does not reduce 20 fps.
/// Advance only after successful packets; reconfigure starts a fresh timeline.
#[derive(Default)]
struct SpectrumSchedule {
    ticks: u64,
    next: u64,
}

impl SpectrumSchedule {
    fn due(&self) -> bool { self.ticks >= self.next }

    fn received(&mut self, device_decimate: c_int) {
        if self.due() { self.next += (FS / 20.0) as u64; }
        self.ticks += N as u64 * device_decimate as u64;
    }
}

/// Queue-wait accounting slice, shared with FineBank's delivery budget.
/// Retain the packet across slices until delivery or the backlog deadline.
pub const CONSUMER_BUDGET: Duration = Duration::from_millis(250);

/// Sustained vendor stalls end this stream; the service recaptures with a new DSP epoch.
const STALL_RECOVERY: Duration = Duration::from_millis(750);
const PACKET_WAIT: Duration = Duration::from_secs(4);
/// Reopening cannot fix consumer backlog. Fail explicitly after this continuous wait.
const BACKLOG_TIMEOUT: Duration = Duration::from_secs(2);
/// Give healthy teardown a short grace period, then park the exclusive SDK owner.
const OWNER_STOP_TIMEOUT: Duration = Duration::from_millis(250);

/// Bus failures without a valid packet. IF overflow is valid data, not a retry.
const TRANSIENT_IQS: [i32; 3] = [-8, -9, -10];

/// One failed SDK IQS call. `status` is the vendor/shim status when the call
/// itself failed; `None` marks local rejections of malformed data (nonfinite
/// spectrum, bad point counts), which are never transient.
struct NextFailure {
    status: Option<i32>,
    message: String,
}

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

type OpenV8 = unsafe extern "C" fn(
    *mut *mut c_void,
    f64,
    *const FrontEnd,
    c_int,
    *mut f64,
    *mut f64,
) -> i32;
type ReconfigV2 =
    unsafe extern "C" fn(*mut c_void, f64, *const FrontEnd, c_int, *mut f64, *mut f64) -> i32;
type Next = unsafe extern "C" fn(
    *mut c_void,
    *mut i16,
    u32,
    *mut PacketInfo,
    *mut f64,
    *mut f32,
    u32,
    *mut u32,
    *mut SdkTiming,
    *mut c_int,
) -> i32;
type Close = unsafe extern "C" fn(*mut c_void) -> i32;
type Arm = unsafe extern "C" fn(*mut c_void) -> i32;
type IqsClock = unsafe extern "C" fn(*mut c_void, *mut f64, *mut c_int) -> i32;

/// Shim timing layout; measured bits distinguish absent stages from 0 ns.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SdkTiming {
    entry_interval_ns: u64,
    fetch_ns: u64,
    memcpy_ns: u64,
    fft_ns: u64,
    measured: u32,
    fetch_status: i32,
    fft_status: i32,
}
#[allow(clippy::too_many_arguments)]
type Gnss = unsafe extern "C" fn(
    *mut c_void,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut f32,
    *mut f32,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
) -> c_int;

#[repr(C)]
#[derive(Default)]
struct PacketInfo {
    stamp: u64,
    samples: u32,
    scale_to_v: f32,
}

#[derive(Clone, Copy)]
pub struct CaptureGeometry {
    pub center: f64,
    pub bandwidth: f64,
    pub refclk_hz: Option<f64>,
    pub refclk_src: Option<i32>,
    pub front_end: Option<FrontEndActual>,
    /// Generation that packets of this geometry carry (0 at start).
    pub generation: u64,
}

#[derive(Clone, Copy, Default, Debug)]
/// Timing distributions are cumulative for this capture lifetime.
/// Quantiles cannot be subtracted to form window quantiles.
pub struct CaptureStats {
    pub packets: u64,
    /// Vendor warning on the latest valid packet; 0 clears the prior warning.
    pub warning: i32,
    pub device_seconds: f64,
    pub host_seconds: f64,
    pub fetch_max_ms: f64,
    pub queue_full: u64,
    pub queue_wait_ms: f64,
    pub queue_wait_max_ms: f64,
    pub spectrum_frames: u64,
    /// Failed vendor calls and the longest one (successful-call timing alone hid the
    /// -10 calls that trigger recovery).
    pub fail_calls: u64,
    pub fail_max_ms: f64,
    /// Longest gap since the last packet when a sustained stall ended this stream.
    pub recovery_stall_ms: f64,
    /// Vendor transient statuses skipped without failing the stream.
    pub skips: u64,
    /// Vendor status of the last skipped call (0 = none yet).
    pub last_skip_status: i32,
    /// Reserved cumulative recovery counter; stream recovery requires a new capture.
    pub recoveries: u64,
    /// False for old shims or a failed optional timing query. Empty SDK timings
    /// must not be interpreted as zero-cost SDK work when this is false.
    pub sdk_timing_available: bool,
    pub sdk_entry_interval: TimingSnapshot,
    pub sdk_fetch: TimingSnapshot,
    pub sdk_memcpy: TimingSnapshot,
    pub sdk_fft: TimingSnapshot,
    pub sdk_failed_fetch: TimingSnapshot,
    pub sdk_failed_fft: TimingSnapshot,
    /// Complete Rust next call, including allocation and validation, all results.
    pub fetch: TimingSnapshot,
    /// Every successfully enqueued packet; spans all budget slices and controls.
    pub queue_wait: TimingSnapshot,
    /// Successful enqueue attempt to consumer receive, excluding producer wait.
    pub queue_residence: TimingSnapshot,
    /// Consumer next service duration, including its receive wait and errors.
    pub queue_serve: TimingSnapshot,
    pub control_serve: TimingSnapshot,
}
/// One failed-enqueue episode, retained in fixed storage for diagnostic snapshots.
/// Only the first `Full` for each held packet records an entry; retries do not.
#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
pub struct QueueFullAttribution {
    /// Zero-based successful SDK packet ordinal over this capture lifetime.
    /// Like CaptureEvent.packet_seq, this survives reconfiguration.
    pub seq: u64,
    /// Device sample counter of that packet; a fresh capture may reset it.
    pub stamp: u64,
    /// Complex samples successfully enqueued over this capture lifetime before
    /// this packet. This is not the DDC plan's input_index, which can reset.
    pub input_index: u64,
    /// `try_send` reported the channel full, so occupancy equals capacity.
    pub occupancy: u32,
    pub capacity: u32,
    /// Time since the latest successful HardwareCapture::next completion.
    pub since_last_service_us: Option<u64>,
    /// DDC plan packet ordinal at the latest successful output batch send.
    /// Plan changes can reset it independently of capture-lifetime seq above.
    pub last_output_seq: Option<u64>,
    /// DDC plan input position at that send, not a capture-lifetime index.
    pub last_output_input_index: Option<u64>,
    pub last_output_frames: Option<u64>,
    pub since_last_output_us: Option<u64>,
}

/// Both marks are protected by CaptureState's existing timing mutex, so a
/// failure snapshot cannot combine fields from different output batches.
#[derive(Default)]
struct ServiceMarks {
    last_service: Option<Instant>,
    last_output: Option<OutputMark>,
}

struct OutputMark {
    completed_at: Instant,
    seq: u64,
    input_index: u64,
    frames: u64,
}

impl ServiceMarks {
    fn attribution(&self, event: CaptureEvent, input_index: u64, now: Instant) -> QueueFullAttribution {
        let age_us = |mark| duration_ns(now.saturating_duration_since(mark)) / 1000;
        QueueFullAttribution {
            seq: event.packet_seq,
            stamp: event.stamp.expect("a queued packet has a device stamp"),
            input_index,
            occupancy: PACKET_QUEUE as u32,
            capacity: PACKET_QUEUE as u32,
            since_last_service_us: self.last_service.map(age_us),
            last_output_seq: self.last_output.as_ref().map(|mark| mark.seq),
            last_output_input_index: self.last_output.as_ref().map(|mark| mark.input_index),
            last_output_frames: self.last_output.as_ref().map(|mark| mark.frames),
            since_last_output_us: self.last_output.as_ref().map(|mark| age_us(mark.completed_at)),
        }
    }
}

/// Diagnostic retention only; independent of the packet channel's capacity.
const QUEUE_FULL_CAPACITY: usize = 64;

/// Diagnostic-only retention; neither this capacity nor the four-packet trigger
/// participates in admission, backlog deadlines, or stream control.
const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
pub struct CaptureEvent {
    pub event_seq: u64,
    pub host_ns: u64,
    pub kind: &'static str,
    pub attempt: u64,
    /// Successful packet ordinal, or next ordinal when fetch failed.
    pub packet_seq: u64,
    pub generation: u64,
    /// None when the failed fetch could not supply a valid packet.
    pub stamp: Option<u64>,
    pub with_fft: bool,
    /// Vendor status when known; local validation/control failures use None.
    pub status: Option<i32>,
    pub failed: bool,
    pub period_ns: u64,
    pub fetch_ns: u64,
    pub sdk_available: bool,
    pub sdk_measured: u32,
    pub sdk_entry_interval_ns: u64,
    pub sdk_fetch_ns: u64,
    pub sdk_memcpy_ns: u64,
    pub sdk_fft_ns: u64,
    pub sdk_fetch_status: i32,
    pub sdk_fft_status: i32,
    pub queue_wait_ns: u64,
    pub queue_residence_ns: u64,
    pub queue_serve_ns: u64,
    pub control_serve_ns: u64,
    pub recovery_ns: u64,
}

impl CaptureEvent {
    fn slow(&self, ns: u64) -> bool { ns >= self.period_ns.saturating_mul(4) }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptureEvents {
    pub total: u64,
    pub overwritten: u64,
    pub trigger_packet_periods: u32,
    pub events: Vec<CaptureEvent>,
    /// Preserve the latest recovery boundary even after routine slow events wrap.
    pub last_recovery: Option<CaptureEvent>,
    /// First failed enqueue for each held packet, oldest retained entry first.
    pub queue_full_attribution: Vec<QueueFullAttribution>,
    pub queue_full_total: u64,
    pub queue_full_overwritten: u64,
}

#[derive(Clone)]
struct EventHistory {
    entries: [CaptureEvent; EVENT_CAPACITY],
    total: u64,
    started: Instant,
    latest: CaptureEvent,
    last_recovery: Option<CaptureEvent>,
    queue_full: [QueueFullAttribution; QUEUE_FULL_CAPACITY],
    queue_full_total: u64,
}

impl Default for EventHistory {
    fn default() -> Self {
        Self {
            entries: [CaptureEvent::default(); EVENT_CAPACITY],
            total: 0,
            started: Instant::now(),
            latest: CaptureEvent::default(),
            last_recovery: None,
            queue_full: [QueueFullAttribution::default(); QUEUE_FULL_CAPACITY],
            queue_full_total: 0,
        }
    }
}

impl EventHistory {
    fn record(&mut self, mut event: CaptureEvent, kind: &'static str) {
        event.kind = kind;
        event.event_seq = self.total;
        event.host_ns = duration_ns(self.started.elapsed());
        self.entries[self.total as usize % EVENT_CAPACITY] = event;
        self.total += 1;
        if kind.starts_with("recovery_") { self.last_recovery = Some(event); }
    }

    fn record_queue_full(&mut self, attribution: QueueFullAttribution) {
        self.queue_full[self.queue_full_total as usize % QUEUE_FULL_CAPACITY] = attribution;
        self.queue_full_total += 1;
    }

    fn snapshot(&self) -> CaptureEvents {
        let retained = self.total.min(EVENT_CAPACITY as u64);
        let queue_full_retained = self.queue_full_total.min(QUEUE_FULL_CAPACITY as u64);
        CaptureEvents {
            total: self.total,
            overwritten: self.total - retained,
            trigger_packet_periods: 4,
            events: (self.total - retained..self.total)
                .map(|seq| self.entries[seq as usize % EVENT_CAPACITY]).collect(),
            last_recovery: self.last_recovery,
            queue_full_attribution: (self.queue_full_total - queue_full_retained..self.queue_full_total)
                .map(|seq| self.queue_full[seq as usize % QUEUE_FULL_CAPACITY]).collect(),
            queue_full_total: self.queue_full_total,
            queue_full_overwritten: self.queue_full_total - queue_full_retained,
        }
    }
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

fn packet_period_ns(device_decimate: c_int) -> u64 {
    N as u64 * device_decimate as u64 * 1_000_000_000 / FS as u64
}

#[derive(Clone, Default)]
struct CaptureTimings {
    sdk_entry_interval: Timing,
    sdk_fetch: Timing,
    sdk_memcpy: Timing,
    sdk_fft: Timing,
    sdk_failed_fetch: Timing,
    sdk_failed_fft: Timing,
    fetch: Timing,
    queue_wait: Timing,
    queue_residence: Timing,
    queue_serve: Timing,
    control_serve: Timing,
}

#[derive(Default)]
struct CaptureState {
    stats: CaptureStats,
    timings: CaptureTimings,
    events: EventHistory,
    marks: ServiceMarks,
    refclk_hz: Option<f64>,
    refclk_src: Option<i32>,
    front_end: Option<FrontEndActual>,
}

/// STATUS reader requesting only the vendor's packet-refreshed GNSS snapshot.
#[derive(Clone)]
pub struct CaptureGnss {
    requests: SyncSender<Request>,
    error: Arc<Mutex<Option<String>>>,
}

/// Admission and reply share one deadline, including a full request queue.
fn send_until<T>(sender: &SyncSender<T>, mut value: T, deadline: Instant, what: &str) -> Result<(), String> {
    loop {
        match sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err("SDK IQS owner closed".into()),
            Err(TrySendError::Full(pending)) => value = pending,
        }
        if Instant::now() >= deadline { return Err(format!("{what} stalled during request admission")); }
        thread::sleep(Duration::from_millis(1));
    }
}

impl CaptureGnss {
    pub fn gnss(&self) -> Result<GnssInfo, String> {
        if let Some(error) = self.error.lock().clone() { return Err(error); }
        let (reply, received) = mpsc::sync_channel(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        send_until(&self.requests, Request::Gnss(reply), deadline, "SDK GNSS query")?;
        received.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "SDK GNSS query stalled")?
    }
}

/// Independent read handle for the polling/status thread. Cloning this handle
/// does not copy observations; SDK and PFB owners need not materialize events.
#[derive(Clone)]
pub struct CaptureDiagnostics {
    state: Arc<Mutex<CaptureState>>,
}

impl CaptureDiagnostics {
    pub fn stats(&self) -> CaptureStats {
        capture_stats_snapshot(&self.state)
    }

    pub fn clock(&self) -> (Option<f64>, Option<i32>) {
        let state = self.state.lock();
        (state.refclk_hz, state.refclk_src)
    }

    pub fn front_end(&self) -> Option<FrontEndActual> {
        self.state.lock().front_end
    }

    pub fn warning(&self) -> i32 {
        self.state.lock().stats.warning
    }

    pub fn packet_observation(&self) -> (u64, i32) {
        let state = self.state.lock();
        (state.stats.packets, state.stats.warning)
    }

    /// Allocates only at the diagnostic reader's request, never during capture.
    pub fn events(&self) -> CaptureEvents {
        let events = self.state.lock().events.clone();
        events.snapshot()
    }
}

fn capture_stats_snapshot(state: &Mutex<CaptureState>) -> CaptureStats {
    // Copy fixed storage under the lock; quantile scans run on the reader.
    let (stats, timings) = {
        let state = state.lock();
        (state.stats, state.timings.clone())
    };
    timings.snapshot(stats)
}

impl CaptureState {
    fn record_sdk(&mut self, timing: Option<SdkTiming>) {
        self.stats.sdk_timing_available = timing.is_some();
        let Some(timing) = timing else { return };
        if timing.measured & 1 != 0 {
            self.timings.sdk_entry_interval.observe(Duration::from_nanos(timing.entry_interval_ns));
        }
        if timing.measured & 2 != 0 {
            let elapsed = Duration::from_nanos(timing.fetch_ns);
            self.timings.sdk_fetch.observe(elapsed);
            if timing.fetch_status != 0 && timing.fetch_status != -12 {
                self.timings.sdk_failed_fetch.observe(elapsed);
            }
        }
        if timing.measured & 4 != 0 {
            self.timings.sdk_memcpy.observe(Duration::from_nanos(timing.memcpy_ns));
        }
        if timing.measured & 8 != 0 {
            let elapsed = Duration::from_nanos(timing.fft_ns);
            self.timings.sdk_fft.observe(elapsed);
            if timing.fft_status != 0 && timing.fft_status != -12 {
                self.timings.sdk_failed_fft.observe(elapsed);
            }
        }
    }

}

impl CaptureTimings {
    fn snapshot(&self, stats: CaptureStats) -> CaptureStats {
        CaptureStats {
            sdk_entry_interval: self.sdk_entry_interval.snapshot(),
            sdk_fetch: self.sdk_fetch.snapshot(),
            sdk_memcpy: self.sdk_memcpy.snapshot(),
            sdk_fft: self.sdk_fft.snapshot(),
            sdk_failed_fetch: self.sdk_failed_fetch.snapshot(),
            sdk_failed_fft: self.sdk_failed_fft.snapshot(),
            fetch: self.fetch.snapshot(),
            queue_wait: self.queue_wait.snapshot(),
            queue_residence: self.queue_residence.snapshot(),
            queue_serve: self.queue_serve.snapshot(),
            control_serve: self.control_serve.snapshot(),
            ..stats
        }
    }
}

struct QueuedPacket {
    packet: Packet,
    enqueued_at: Instant,
    event: CaptureEvent,
}

pub struct Packet {
    pub stamp: u64,
    /// Monotonic capture sequence; increases once per successfully fetched packet.
    pub seq: u64,
    pub warning: i32,
    pub iq: Vec<i16>,
    pub spectrum: Option<(Vec<f64>, Vec<f32>)>,
    /// Reconfigure count at fetch time. Queued packets of an old generation stay in the channel; the consumer drops them.
    pub generation: u64,
}

struct Device {
    library: *mut c_void,
    handle: *mut c_void,
    arm: Arm,
    next: Next,
    last_timing: SdkTiming,
    reconfig: ReconfigV2,
    close: Close,
    gnss: Option<Gnss>,
    clock: Option<IqsClock>,
    /// Stale after a bus error (-8/-9): never hand the handle back to the vendor library.
    abandoned: bool,
}

/// Checks the geometry that the shim reports for one IQS configure. With a device
/// decimate the usable bandwidth shrinks proportionally; the minimum is the rate a
/// fully-decimated stream still provides (one coarse-bin flat span).
fn geometry_ok(bandwidth: f64, configured: f64, center: f64) -> bool {
    bandwidth.is_finite()
        && (1_000_000.0..=FS).contains(&bandwidth)
        && configured.is_finite()
        && (configured - center).abs() <= 1000.0
}

fn read_clock(query: Option<IqsClock>, handle: *mut c_void) -> (Option<f64>, Option<i32>) {
    let Some(query) = query else { return (None, None); };
    let (mut hz, mut source) = (0.0, 0);
    if unsafe { query(handle, &mut hz, &mut source) } == 0 {
        (Some(hz), Some(source))
    } else {
        (None, None)
    }
}

impl Device {
    fn open(
        path: &str,
        center: f64,
        front_end: FrontEnd,
        device_decimate: c_int,
    ) -> Result<(Self, CaptureGeometry), String> {
        owner_slot_free()?;
        let path = CString::new(path).map_err(|_| "invalid SDK shim path")?;
        let library = unsafe { dlopen(path.as_ptr(), crate::hardware_swp::SDK_DLOPEN_FLAGS) };
        if library.is_null() {
            return Err(format!(
                "SDK shim: {}",
                unsafe { CStr::from_ptr(dlerror()) }.to_string_lossy()
            ));
        }
        let result = (|| {
            unsafe { crate::hardware_swp::register_sdk_session(library)?; }
            let symbol = |name: &'static [u8]| -> Result<*mut c_void, String> {
                let ptr = unsafe { dlsym(library, name.as_ptr().cast()) };
                (!ptr.is_null())
                    .then_some(ptr)
                    .ok_or_else(|| format!("SDK shim missing {}", String::from_utf8_lossy(name)))
            };
            let next: Next = unsafe { std::mem::transmute(symbol(b"san90_next_v3\0")?) };
            let close: Close = unsafe { std::mem::transmute(symbol(b"san90_close\0")?) };
            let (mut handle, mut bandwidth, mut configured) = (std::ptr::null_mut(), 0.0, 0.0);
            // v8 guarantees that metadata and consumer DSP setup precede acquisition.
            let open: OpenV8 = unsafe { std::mem::transmute(symbol(b"san90_open_v8\0")?) };
            let arm: Arm = unsafe { std::mem::transmute(symbol(b"san90_iqs_arm_v1\0")?) };
            let reconfig: ReconfigV2 = unsafe {
                std::mem::transmute(symbol(b"san90_iqs_reconfig_v2\0")?)
            };
            let status = unsafe {
                open(&mut handle, center, &front_end, device_decimate,
                     &mut bandwidth, &mut configured)
            };
            if status != 0 || handle.is_null() || !geometry_ok(bandwidth, configured, center) {
                if !handle.is_null() {
                    unsafe {
                        (close)(handle);
                    }
                }
                return Err(format!("SDK IQS open/geometry rejected status={status}"));
            }
            let gnss_ptr = unsafe { dlsym(library, b"san90_gnss_cached_v1\0".as_ptr().cast()) };
            let gnss: Option<Gnss> =
                (!gnss_ptr.is_null()).then(|| unsafe { std::mem::transmute(gnss_ptr) });
            let clock = unsafe {
                let p = dlsym(library, b"san90_iqs_clock_v1\0".as_ptr().cast());
                (!p.is_null()).then(|| std::mem::transmute::<*mut c_void, IqsClock>(p))
            };
            let (refclk_hz, refclk_src) = read_clock(clock, handle);
            Ok((
                Self {
                    library,
                    handle,
                    arm,
                    next,
                    last_timing: SdkTiming::default(),
                    reconfig,
                    close,
                    gnss,
                    clock,
                    abandoned: false,
                },
                CaptureGeometry {
                    center: configured,
                    bandwidth,
                    refclk_hz,
                    refclk_src,
                    front_end: read_front_end_actual(library, handle, b"san90_iqs_front_end_actual_v1\0"),
                    generation: 0,
                },
            ))
        })();
        if result.is_err() {
            unsafe {
                dlclose(library);
            }
        }
        result
    }

    fn arm(&mut self) -> Result<(), String> {
        let status = unsafe { (self.arm)(self.handle) };
        if status == -8 || status == -9 {
            self.abandoned = true;
        }
        if status != 0 {
            return Err(format!("SDK IQS arm rejected status={status}"));
        }
        Ok(())
    }

    /// Retunes or re-rates the open handle in place (no Device_Close/Device_Open).
    fn reconfigure(
        &mut self,
        center: f64,
        front_end: FrontEnd,
        device_decimate: c_int,
    ) -> Result<CaptureGeometry, String> {
        let (mut bandwidth, mut configured) = (0.0, 0.0);
        let status = unsafe {
            (self.reconfig)(self.handle, center, &front_end, device_decimate,
                            &mut bandwidth, &mut configured)
        };
        if status == -8 || status == -9 {
            self.abandoned = true;
        }
        if status != 0 || !geometry_ok(bandwidth, configured, center) {
            return Err(format!("SDK IQS reconfigure rejected status={status}"));
        }
        let (refclk_hz, refclk_src) = read_clock(self.clock, self.handle);
        Ok(CaptureGeometry {
            center: configured,
            bandwidth,
            refclk_hz,
            refclk_src,
            front_end: read_front_end_actual(self.library, self.handle, b"san90_iqs_front_end_actual_v1\0"),
            generation: 0,
        })
    }

    fn timing(&self) -> Option<SdkTiming> {
        (self.last_timing.measured != 0).then_some(self.last_timing)
    }

    fn next(&mut self, with_spectrum: bool) -> Result<Packet, NextFailure> {
        let mut iq = vec![0i16; N * 2];
        let mut info = PacketInfo::default();
        let (mut freq, mut power) = if with_spectrum {
            (vec![0.0f64; N], vec![0.0f32; N])
        } else {
            (Vec::new(), Vec::new())
        };
        let mut points = 0;
        let mut warning = 0;
        self.last_timing = SdkTiming::default();
        let status = unsafe {
            (self.next)(
                self.handle,
                iq.as_mut_ptr(),
                N as u32,
                &mut info,
                if with_spectrum {
                    freq.as_mut_ptr()
                } else {
                    std::ptr::null_mut()
                },
                if with_spectrum {
                    power.as_mut_ptr()
                } else {
                    std::ptr::null_mut()
                },
                freq.len() as u32,
                &mut points,
                &mut self.last_timing,
                &mut warning,
            )
        };
        if status != 0 || info.samples as usize != N {
            if status == -8 || status == -9 {
                self.abandoned = true;
            }
            return Err(NextFailure {
                status: (status != 0).then_some(status),
                message: format!("SDK IQS next/FFT status={status} samples={}", info.samples),
            });
        }
        if !info.scale_to_v.is_finite() {
            return Err(NextFailure {
                status: None,
                message: "SDK IQS nonfinite scale".into(),
            });
        }
        if points as usize > N || (with_spectrum && points < 2) {
            return Err(NextFailure {
                status: None,
                message: "SDK IQS FFT point count rejected".into(),
            });
        }
        freq.truncate(points as usize);
        power.truncate(points as usize);
        if freq.iter().any(|hz| !hz.is_finite()) || power.iter().any(|dbm| !dbm.is_finite()) {
            return Err(NextFailure {
                status: None,
                message: "SDK IQS FFT nonfinite data".into(),
            });
        }
        Ok(Packet {
            seq: 0, stamp: info.stamp,
            warning,
            iq,
            spectrum: with_spectrum.then_some((freq, power)),
            generation: 0,
        })
    }
}

impl Device {
    fn gnss(&mut self) -> Result<GnssInfo, String> {
        let query = self.gnss.ok_or("SDK shim has no GNSS entry")?;
        let mut info = GnssInfo::default();
        let status = unsafe {
            query(
                self.handle,
                &mut info.lock,
                &mut info.sats,
                &mut info.docxo,
                &mut info.docxo_mode,
                &mut info.antenna,
                &mut info.latitude,
                &mut info.longitude,
                &mut info.altitude,
                &mut info.year,
                &mut info.month,
                &mut info.day,
                &mut info.hour,
                &mut info.minute,
                &mut info.second,
            )
        };
        if status != 0 {
            if status == -8 || status == -9 {
                self.abandoned = true;
            }
            return Err(format!("SDK GNSS status={status}"));
        }
        Ok(info)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Err(error) = unsafe {
            crate::hardware_swp::release_sdk_mode(
                self.library, self.handle, self.close, self.abandoned,
            )
        } {
            eprintln!("SDK IQS {error}");
        }
    }
}


/// Current stream settings on the exclusive SDK owner thread.
struct Settings {
    center: f64,
    front_end: FrontEnd,
    device_decimate: c_int,
    generation: u64,
}

/// Consumer request that the owner thread executes on its handle.
enum Request {
    Gnss(SyncSender<Result<GnssInfo, String>>),
    Defaults(SyncSender<Result<SwpDefaults, String>>),
    Reconfigure {
        center: f64,
        front_end: FrontEnd,
        device_decimate: c_int,
        reply: SyncSender<Result<CaptureGeometry, String>>,
    },
}

/// Executes one request. Returns the error that must stop the owner.
fn serve(request: Request, device: &mut Device, settings: &mut Settings) -> Result<(), String> {
    match request {
        Request::Gnss(reply) => {
            let _ = reply.send(device.gnss());
        }
        Request::Defaults(reply) => {
            let _ = reply.send(read_defaults(device.library, device.handle));
        }
        Request::Reconfigure {
            center,
            front_end,
            device_decimate,
            reply,
        } => {
            let mut result = device.reconfigure(center, front_end, device_decimate);
            let fatal = result.as_ref().err().cloned();
            if let Ok(geometry) = result.as_mut() {
                settings.center = center;
                settings.front_end = front_end;
                settings.device_decimate = device_decimate;
                settings.generation += 1;
                geometry.generation = settings.generation;
            }
            let _ = reply.send(result);
            if let Some(message) = fatal {
                return Err(message);
            }
        }
    }
    Ok(())
}

pub struct HardwareCapture {
    pub geometry: CaptureGeometry,
    pub device_info: Option<crate::hardware_swp::DeviceInfo>,
    requests: Option<SyncSender<Request>>,
    packets: Option<Receiver<QueuedPacket>>,
    error: Arc<Mutex<Option<String>>>,
    stats: Arc<Mutex<CaptureState>>,
    stop: Arc<AtomicBool>,
    arm: Option<SyncSender<SyncSender<Result<(), String>>>>,
    owner: Option<JoinHandle<()>>,
}

impl HardwareCapture {
    pub fn start(
        path: String,
        center: f64,
        front_end: FrontEnd,
        device_decimate: c_int,
    ) -> Result<Self, String> {
        let (packet_tx, packets) = mpsc::sync_channel(PACKET_QUEUE);
        let (request_tx, request_rx) = mpsc::sync_channel::<Request>(1);
        let (meta_tx, meta_rx) = mpsc::sync_channel(1);
        let (arm_tx, arm_rx) = mpsc::sync_channel::<SyncSender<Result<(), String>>>(0);
        let error = Arc::new(Mutex::new(None));
        let stats = Arc::new(Mutex::new(CaptureState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let owner_error = error.clone();
        let owner_stats = stats.clone();
        let owner_stop = stop.clone();
        let owner = thread::spawn(move || match Device::open(&path, center, front_end, device_decimate)
        {
            Ok((mut device, geometry)) => {
                let info = crate::hardware_swp::read_device_info(device.library, device.handle).ok().flatten();
                if meta_tx.send(Ok((geometry, info))).is_err() {
                    return;
                }
                let mut settings = Settings {
                    center,
                    front_end,
                    device_decimate,
                    generation: 0,
                };
                // Configuration queries and retunes remain valid while consumer
                // setup holds acquisition unarmed. Service them on this owner
                // instead of waiting exclusively for the acquisition handshake.
                let armed = loop {
                    if owner_stop.load(Ordering::Acquire) { return; }
                    match arm_rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(reply) => break reply,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                    }
                    if let Ok(request) = request_rx.try_recv() {
                        if let Err(message) = serve(request, &mut device, &mut settings) {
                            *owner_error.lock() = Some(message);
                            return;
                        }
                    }
                };
                let mut first = None;
                let mut spectrum_schedule = SpectrumSchedule::default();
                // Consecutive transient statuses of any kind.
                let mut transient_streak = 0u32;
                // Last successful vendor packet: a real stall is measured in wall time,
                // not in the number of fast -10 returns.
                let mut last_packet_at = Instant::now();
                let mut attempt = 0u64;
                let mut packet_seq = 0u64;
                // Complex samples in successfully enqueued packets; the failing
                // packet's host input index is this value.
                let mut enqueued_samples = 0u64;
                // Configuration and consumer setup are complete. Start the vendor
                // stream only now, immediately before the first fetch.
                let started = device.arm();
                if let Err(message) = &started {
                    *owner_error.lock() = Some(message.clone());
                }
                let failed = started.is_err();
                let _ = armed.send(started);
                if failed { return; }
                loop {
                    if owner_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(request) = request_rx.try_recv() {
                        let generation = settings.generation;
                        let served_at = Instant::now();
                        let result = serve(request, &mut device, &mut settings);
                        let served = served_at.elapsed();
                        {
                            let mut state = owner_stats.lock();
                            state.timings.control_serve.observe(served);
                            let event = CaptureEvent {
                                attempt, packet_seq, generation,
                                period_ns: packet_period_ns(settings.device_decimate),
                                control_serve_ns: duration_ns(served),
                                failed: result.is_err(),
                                ..CaptureEvent::default()
                            };
                            if event.failed || event.slow(event.control_serve_ns) {
                                state.events.record(event, "control");
                            }
                        }
                        if let Err(message) = result {
                            *owner_error.lock() = Some(message);
                            break;
                        }
                        if settings.generation != generation {
                            // Old-stream failures do not count against the new stream.
                            transient_streak = 0;
                            last_packet_at = Instant::now();
                            spectrum_schedule = SpectrumSchedule::default();
                        }
                        continue;
                    }
                    let with_spectrum = spectrum_schedule.due();
                    let fetched_at = Instant::now();
                    attempt += 1;
                    let result = device.next(with_spectrum);
                    let fetched = fetched_at.elapsed();
                    let sdk_timing = device.timing();
                    let sdk = sdk_timing.unwrap_or_default();
                    let event = CaptureEvent {
                        attempt, packet_seq, generation: settings.generation,
                        stamp: result.as_ref().ok().map(|packet| packet.stamp),
                        with_fft: with_spectrum,
                        status: result.as_ref().err().and_then(|failure| failure.status)
                            .or_else(|| result.as_ref().ok().map(|packet| packet.warning)),
                        failed: result.is_err(),
                        period_ns: packet_period_ns(settings.device_decimate),
                        fetch_ns: duration_ns(fetched),
                        sdk_available: sdk_timing.is_some(),
                        sdk_measured: sdk.measured,
                        sdk_entry_interval_ns: sdk.entry_interval_ns,
                        sdk_fetch_ns: sdk.fetch_ns,
                        sdk_memcpy_ns: sdk.memcpy_ns,
                        sdk_fft_ns: sdk.fft_ns,
                        sdk_fetch_status: sdk.fetch_status,
                        sdk_fft_status: sdk.fft_status,
                        ..CaptureEvent::default()
                    };
                    {
                        let mut state = owner_stats.lock();
                        state.timings.fetch.observe(fetched);
                        state.record_sdk(sdk_timing);
                        state.events.latest = event;
                        if event.failed || event.slow(event.fetch_ns.max(event.sdk_entry_interval_ns)) {
                            state.events.record(event, "fetch");
                        }
                    }
                    let mut packet = match result {
                        Ok(packet) => {
                            transient_streak = 0;
                            last_packet_at = Instant::now();
                            packet
                        }
                        Err(failure) => {
                            // Resume only if the next packet passes the consumer's timestamp
                            // continuity check. A sustained stall ends the stream with its vendor cause.
                            if let Some(status) =
                                failure.status.filter(|s| TRANSIENT_IQS.contains(s))
                            {
                                transient_streak += 1;
                                {
                                    let mut state = owner_stats.lock();
                                    let stats = &mut state.stats;
                                    stats.skips += 1;
                                    stats.last_skip_status = status;
                                    stats.fail_calls += 1;
                                    stats.fail_max_ms = stats
                                        .fail_max_ms
                                        .max(fetched_at.elapsed().as_secs_f64() * 1e3);
                                }
                                let streaked = transient_streak >= 60
                                    || last_packet_at.elapsed() >= STALL_RECOVERY;
                                if streaked {
                                    {
                                        let mut state = owner_stats.lock();
                                        let stats = &mut state.stats;
                                        stats.recovery_stall_ms = stats
                                            .recovery_stall_ms
                                            .max(last_packet_at.elapsed().as_secs_f64() * 1e3);
                                    }
                                    owner_stats.lock().events.record(event, "recovery_required");
                                    *owner_error.lock() = Some(format!(
                                        "{}; sustained capture stall requires a new stream epoch",
                                        failure.message));
                                    // Retire the failed physical session on its owner only.
                                    // Publish the original failure first: teardown can itself stall.
                                    if let Err(error) = unsafe {
                                        crate::hardware_swp::invalidate_sdk_session(
                                            device.library, device.handle, device.abandoned)
                                    } {
                                        eprintln!("SDK IQS failed-session invalidation: {error}");
                                    }
                                    break;
                                }
                                continue;
                            }
                            *owner_error.lock() = Some(failure.message);
                            break;
                        }
                    };
                    let received_at = Instant::now();
                    packet_seq += 1;
                    spectrum_schedule.received(settings.device_decimate);
                    let (first_stamp, first_host) =
                        first.get_or_insert((packet.stamp, received_at));
                    {
                        let mut state = owner_stats.lock();
                        let stats = &mut state.stats;
                        stats.packets += 1;
                        stats.warning = packet.warning;
                        stats.spectrum_frames += u64::from(with_spectrum);
                        stats.device_seconds =
                            packet.stamp.saturating_sub(*first_stamp) as f64 / 125_000_000.0;
                        stats.host_seconds = received_at.duration_since(*first_host).as_secs_f64();
                        stats.fetch_max_ms = stats
                            .fetch_max_ms
                            .max(received_at.duration_since(fetched_at).as_secs_f64() * 1e3);
                    }
                    packet.generation = settings.generation;
                    packet.seq = packet_seq;
                    let mut wait = Instant::now();
                    let queue_started = wait;
                    let mut packet = QueuedPacket { packet, enqueued_at: wait, event };
                    let mut full = false;
                    let mut pending_recorded = false;
                    loop {
                        packet.enqueued_at = Instant::now();
                        packet.event.queue_wait_ns = duration_ns(packet.enqueued_at.duration_since(queue_started));
                        match packet_tx.try_send(packet) {
                            Ok(()) => {
                                enqueued_samples = enqueued_samples.saturating_add(N as u64);
                                let queued = queue_started.elapsed();
                                let mut state = owner_stats.lock();
                                state.timings.queue_wait.observe(queued);
                                let event = CaptureEvent { queue_wait_ns: duration_ns(queued), ..event };
                                if event.slow(event.queue_wait_ns) { state.events.record(event, "queue_complete"); }
                                if full {
                                    let stats = &mut state.stats;
                                    stats.queue_full += 1;
                                    stats.queue_wait_ms += wait.elapsed().as_secs_f64() * 1e3;
                                    stats.queue_wait_max_ms = stats
                                        .queue_wait_max_ms
                                        .max(wait.elapsed().as_secs_f64() * 1e3);
                                }
                                break;
                            }
                            Err(TrySendError::Full(queued)) => {
                                let first = !full;
                                full = true;
                                packet = queued;
                                if first {
                                    let mut state = owner_stats.lock();
                                    let attribution = state.marks.attribution(
                                        event, enqueued_samples, Instant::now());
                                    state.events.record_queue_full(attribution);
                                }
                                if !pending_recorded && event.slow(duration_ns(queue_started.elapsed())) {
                                    owner_stats.lock().events.record(CaptureEvent {
                                        queue_wait_ns: duration_ns(queue_started.elapsed()), ..event
                                    }, "queue_pending");
                                    pending_recorded = true;
                                }
                                // Service control requests while blocked on queue capacity:
                                // the consumer may be waiting for the reply before draining.
                                // The held packet retains its original generation tag.
                                if let Ok(request) = request_rx.try_recv() {
                                    let generation = settings.generation;
                                    let served_at = Instant::now();
                                    let result = serve(request, &mut device, &mut settings);
                                    let served = served_at.elapsed();
                                    {
                                        let mut state = owner_stats.lock();
                                        state.timings.control_serve.observe(served);
                                        let event = CaptureEvent {
                                            control_serve_ns: duration_ns(served),
                                            queue_wait_ns: duration_ns(queue_started.elapsed()),
                                            failed: result.is_err(), ..event
                                        };
                                        if event.failed || event.slow(event.control_serve_ns) {
                                            state.events.record(event, "control_while_queued");
                                        }
                                    }
                                    if let Err(message) = result {
                                        *owner_error.lock() = Some(message);
                                        return;
                                    }
                                    if settings.generation != generation {
                                        transient_streak = 0;
                                        last_packet_at = Instant::now();
                                        spectrum_schedule = SpectrumSchedule::default();
                                    }
                                }
                                // Retain the packet; reopening cannot fix consumer backlog.
                                if wait.elapsed() >= CONSUMER_BUDGET {
                                    {
                                        let mut state = owner_stats.lock();
                                        state.events.record(CaptureEvent {
                                            queue_wait_ns: duration_ns(queue_started.elapsed()), ..event
                                        }, "queue_pending");
                                        let stats = &mut state.stats;
                                        stats.queue_full += 1;
                                        stats.queue_wait_ms += wait.elapsed().as_secs_f64() * 1e3;
                                        stats.queue_wait_max_ms = stats
                                            .queue_wait_max_ms
                                            .max(wait.elapsed().as_secs_f64() * 1e3);
                                    }
                                    if queue_started.elapsed() >= BACKLOG_TIMEOUT {
                                        *owner_error.lock() = Some(
                                            "SDK IQS consumer backlog exceeded 2 seconds; capture ended".into());
                                        return;
                                    }
                                    wait = Instant::now();
                                    continue;
                                }
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                owner_stats.lock().events.record(CaptureEvent {
                                    queue_wait_ns: duration_ns(queue_started.elapsed()),
                                    failed: true, status: None, ..event
                                }, "queue_disconnected");
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let _ = meta_tx.send(Err(e));
            }
        });
        // An SDK open that hangs must not leave a half-ready service advertising IQS. The
        // stuck owner is parked so that no new session overlaps it.
        let (geometry, device_info) = match meta_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(result) => result?,
            Err(_) => {
                stop.store(true, Ordering::Release);
                park_stuck_owner(owner);
                return Err("SDK IQS startup timed out; owner parked".to_owned());
            }
        };
        {
            let mut state = stats.lock();
            state.refclk_hz = geometry.refclk_hz;
            state.refclk_src = geometry.refclk_src;
            state.front_end = geometry.front_end;
        }
        Ok(Self {
            geometry,
            device_info,
            requests: Some(request_tx),
            packets: Some(packets),
            error,
            stats,
            stop,
            arm: Some(arm_tx),
            owner: Some(owner),
        })
    }

    /// Sends one request to the owner thread and waits for its reply.
    fn call<T>(
        &self,
        request: impl FnOnce(SyncSender<Result<T, String>>) -> Request,
        timeout: Duration,
        what: &str,
    ) -> Result<T, String> {
        if let Some(e) = self.error.lock().clone() {
            return Err(e);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + timeout;
        let result = send_until(self.requests.as_ref().ok_or("IQS capture closed")?,
            request(reply_tx), deadline, what)
            .and_then(|()| reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| format!("{what} stalled")));
        if let Err(message) = &result {
            self.stop.store(true, Ordering::Release);
            *self.error.lock() = Some(message.clone());
        }
        result?
    }

    /// GNSS snapshot; runs on the owner thread with every other vendor call.
    pub fn gnss(&self) -> Result<GnssInfo, String> {
        self.call(Request::Gnss, Duration::from_secs(5), "SDK GNSS query")
    }

    pub fn gnss_reader(&self) -> CaptureGnss {
        CaptureGnss { requests:self.requests.as_ref().expect("open IQS capture").clone(), error:self.error.clone() }
    }

    /// Device SWP power-on defaults, read through the open IQS handle on the owner thread.
    pub fn defaults(&self) -> Result<SwpDefaults, String> {
        self.call(
            Request::Defaults,
            Duration::from_secs(5),
            "SDK defaults query",
        )
    }

    /// Retunes the open handle between packets. Later packets carry the next generation.
    /// A failure stops the owner; `next` then returns the error.
    pub fn reconfigure(
        &self,
        center: f64,
        front_end: FrontEnd,
        device_decimate: c_int,
    ) -> Result<CaptureGeometry, String> {
        let geometry = self.call(
            |reply| Request::Reconfigure {
                center,
                front_end,
                device_decimate,
                reply,
            },
            OPEN_TIMEOUT,
            "SDK IQS reconfigure",
        )?;
        let mut state = self.stats.lock();
        state.refclk_hz = geometry.refclk_hz;
        state.refclk_src = geometry.refclk_src;
        state.front_end = geometry.front_end;
        Ok(geometry)
    }

    #[cfg(test)]
    pub fn stats(&self) -> CaptureStats {
        capture_stats_snapshot(&self.stats)
    }

    pub fn diagnostics(&self) -> CaptureDiagnostics {
        CaptureDiagnostics { state: self.stats.clone() }
    }

    /// Latest successfully sent pipeline batch for enqueue-failure attribution.
    /// seq/input_index describe the current DDC plan at send completion; they
    /// need not identify the source packet of asynchronously framed IQ output.
    pub fn note_output(&self, seq: u64, input_index: u64, frames: usize, completed_at: Instant) {
        self.stats.lock().marks.last_output = Some(OutputMark {
            completed_at, seq, input_index, frames: frames as u64,
        });
    }

    /// Starts vendor acquisition after consumer setup and waits for its result.
    /// A second successful call is a no-op.
    pub fn arm(&mut self) -> Result<(), String> {
        self.arm_with_timeout(OPEN_TIMEOUT)
    }

    fn arm_with_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        if let Some(error) = self.error.lock().clone() { return Err(error); }
        let Some(arm) = self.arm.take() else { return Ok(()); };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + timeout;
        let result = send_until(&arm, reply_tx, deadline, "SDK IQS acquisition arm")
            .and_then(|()| reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "SDK IQS acquisition arm stalled".to_owned()))
            .and_then(|reply| reply);
        if let Err(message) = &result {
            self.stop.store(true, Ordering::Release);
            *self.error.lock() = Some(message.clone());
        }
        result
    }

    pub fn next(&self) -> Result<Packet, String> {
        self.next_with_timeout(PACKET_WAIT)
    }

    fn next_with_timeout(&self, timeout: Duration) -> Result<Packet, String> {
        let served_at = Instant::now();
        let mut event = None;
        let result = (|| {
            if let Some(e) = self.error.lock().clone() {
                return Err(e);
            }
            let packet = self
                .packets
                .as_ref()
                .ok_or("IQS capture closed")?
                .recv_timeout(timeout);
            if let Ok(packet) = &packet {
                let residence = packet.enqueued_at.elapsed();
                event = Some(CaptureEvent { queue_residence_ns: duration_ns(residence), ..packet.event });
                self.stats.lock().timings.queue_residence.observe(residence);
            }
            if let Some(e) = self.error.lock().clone() {
                return Err(e);
            }
            packet.map(|queued| queued.packet).map_err(|_| {
                self.stop.store(true, Ordering::Release);
                let message = "SDK IQS capture stalled/closed".to_owned();
                *self.error.lock() = Some(message.clone());
                message
            })
        })();
        let completed_at = Instant::now();
        let served = completed_at.duration_since(served_at);
        let mut state = self.stats.lock();
        if result.is_ok() { state.marks.last_service = Some(completed_at); }
        state.timings.queue_serve.observe(served);
        let event = CaptureEvent {
            queue_serve_ns: duration_ns(served),
            failed: result.is_err(),
            ..event.unwrap_or(state.events.latest)
        };
        if event.failed || event.slow(event.queue_serve_ns.max(event.queue_residence_ns)) {
            state.events.record(event, "queue_serve");
        }
        result
    }
}

/// Never close SDK state on another thread while an owner call is still running.
/// Parking keeps the existing session exclusion gate closed until owner teardown ends.
fn finish_owner(owner: JoinHandle<()>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !owner.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    if owner.is_finished() { let _ = owner.join(); }
    else { park_stuck_owner(owner); }
}

impl Drop for HardwareCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.arm.take();
        self.packets.take(); // unblock a full producer before joining the SDK owner
        self.requests.take();
        if let Some(owner) = self.owner.take() {
            finish_owner(owner, OWNER_STOP_TIMEOUT);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn request_admission_obeys_deadline_when_owner_queue_is_full() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(1).unwrap();
        let error = send_until(&tx, 2, Instant::now(), "control").unwrap_err();
        assert!(error.contains("request admission"));
        assert_eq!(rx.recv().unwrap(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn stuck_arm_and_fetch_park_exclusive_owner_until_call_returns() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        for stuck_arm in [true, false] {
            let gate = r#"
static std::atomic<bool> blocked{true}, entered{false};
static void test_wait() {
    entered = true;
    while (blocked.load()) usleep(1000);
}
extern "C" int san90_test_gate(int release) {
    if (release) blocked = false;
    return entered.load() ? 1 : 0;
}
"#;
            let source = FAKE_IQS_SHIM.replace("struct PacketInfo {", &format!("{gate}\nstruct PacketInfo {{"));
            let source = if stuck_arm {
                source.replace("    armed = true;", "    test_wait();\n    armed = true;")
            } else {
                source.replace("    (void)handle;", "    test_wait();\n    (void)handle;")
            };
            let shim = compile_fake_iqs_source(if stuck_arm { "stuck_arm" } else { "stuck_fetch" }, &source);
            let cpath = CString::new(shim.clone()).unwrap();
            let library = unsafe { dlopen(cpath.as_ptr(), crate::hardware_swp::SDK_DLOPEN_FLAGS) };
            assert!(!library.is_null());
            let symbol = unsafe { dlsym(library, b"san90_test_gate\0".as_ptr().cast()) };
            assert!(!symbol.is_null());
            let control: unsafe extern "C" fn(c_int) -> c_int = unsafe { std::mem::transmute(symbol) };
            let mut capture = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let caller = thread::spawn(move || {
                let error = if stuck_arm {
                    capture.arm_with_timeout(Duration::from_secs(1)).unwrap_err()
                } else {
                    capture.arm().unwrap();
                    capture.next_with_timeout(Duration::from_secs(1)).err().unwrap()
                };
                drop(capture);
                done_tx.send(error).unwrap();
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            while unsafe { control(0) } == 0 {
                assert!(Instant::now() < deadline, "fake owner never entered SDK call");
                thread::sleep(Duration::from_millis(1));
            }
            // Teardown must return while the vendor call is blocked; the timeout detects deadlock.
            let result = done_rx.recv_timeout(Duration::from_secs(10));
            let excluded = owner_slot_free().is_err();
            unsafe { control(1); }
            caller.join().unwrap();
            let error = result.expect("capture teardown waited for the blocked vendor call");
            assert!(error.contains("stalled"), "{error}");
            assert!(excluded, "new SDK session admitted while the previous owner was blocked");
            let deadline = Instant::now() + Duration::from_secs(10);
            while owner_slot_free().is_err() {
                assert!(Instant::now() < deadline, "released owner did not finish teardown");
                thread::sleep(Duration::from_millis(1));
            }
            unsafe { dlclose(library); }
        }
    }

    #[test]
    fn diagnostic_ring_keeps_order_and_recovery_after_wrap() {
        let mut history = EventHistory::default();
        history.record(CaptureEvent { attempt: 7, failed: true, status: Some(-10), ..CaptureEvent::default() }, "recovery_begin");
        for attempt in 8..8 + EVENT_CAPACITY as u64 + 3 {
            history.record(CaptureEvent { attempt, ..CaptureEvent::default() }, "fetch");
        }
        for seq in 0..QUEUE_FULL_CAPACITY as u64 + 3 {
            history.record_queue_full(QueueFullAttribution {
                seq, stamp: seq * 2 * N as u64, input_index: seq * N as u64,
                ..QueueFullAttribution::default()
            });
        }
        let snapshot = history.snapshot();
        assert_eq!(snapshot.overwritten, 4);
        assert_eq!(snapshot.events.first().unwrap().event_seq, 4);
        assert_eq!(snapshot.events.last().unwrap().event_seq, snapshot.total - 1);
        for pair in snapshot.events.windows(2) {
            assert_eq!(pair[1].event_seq, pair[0].event_seq + 1);
            assert_eq!(pair[1].attempt, pair[0].attempt + 1);
        }
        let recovery = snapshot.last_recovery.unwrap();
        assert_eq!(recovery.attempt, 7);
        assert_eq!(recovery.status, Some(-10));
        assert!(recovery.failed);
        assert_eq!(snapshot.queue_full_total, QUEUE_FULL_CAPACITY as u64 + 3);
        assert_eq!(snapshot.queue_full_overwritten, 3);
        assert_eq!(snapshot.queue_full_attribution.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            (3..QUEUE_FULL_CAPACITY as u64 + 3).collect::<Vec<_>>());
        for entry in &snapshot.queue_full_attribution {
            assert_eq!(entry.stamp, entry.seq * 2 * N as u64);
            assert_eq!(entry.input_index, entry.seq * N as u64);
        }
    }

    #[test]
    fn spectrum_cadence_tracks_device_time_at_all_decimations() {
        for decimate in [1, 2, 64] {
            let mut schedule = SpectrumSchedule::default();
            let packets = ((FS * 10.0) as u64 / (N as u64 * decimate as u64)) as usize;
            let mut frames = 0;
            for _ in 0..packets {
                frames += usize::from(schedule.due());
                schedule.received(decimate);
            }
            assert_eq!(frames, 200, "decimate={decimate}");
            // A retune/rerate must expose a fresh spectrum on its first packet.
            schedule = SpectrumSchedule::default();
            assert!(schedule.due());
            schedule.received(decimate);
            assert!(!schedule.due());
        }
    }

    // Serialise every fake-IQS test: the failure-injection file is a fixed path, so
    // an injected streak must never leak into a sibling test.
    pub(crate) static TEST_IQS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) const FRONT_END: FrontEnd = FrontEnd {
        ref_dbm: 0.0,
        atten: -1,
        preamp: 0,
        ifgain: 0,
        gain_strategy: 0,
        ref_clock: 0,
        refclk_out: 0,
    };

    const FAKE_IQS_FAIL: &str = "/tmp/websa_fake_iqs_fail.txt";

    const FAKE_IQS_SHIM: &str = r#"// Fake SAN-90 IQS shim: valid streaming geometry with a
// file-driven transient failure streak for owner-semantics contract tests.
// /tmp/websa_fake_iqs_fail.txt holds "<vendor status> <consecutive failures>";
// /tmp/websa_fake_iqs_open_fail.txt containing 1 makes san90_open_v8 fail (-110).
#include <atomic>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <unistd.h>

struct PacketInfo {
    unsigned long long stamp;
    unsigned int samples;
    float scale_to_v;
};
struct Timing {
    uint64_t entry_interval_ns, fetch_ns, memcpy_ns, fft_ns;
    uint32_t measured;
    int32_t fetch_status, fft_status;
};

struct FrontEndActual { double ref_dbm; int atten, preamp, ifgain; };
static_assert(sizeof(FrontEndActual) == 24, "front-end actual ABI");
static FrontEndActual front_end_actual{-7.5, 12, 1, 2};
static double auto_threshold = -INFINITY;
static double auto_ref_ceiling = INFINITY;
static bool auto_ref_enabled = false;
static double configured_ref = 0.0;
static void auto_ref_actual(const void* fe, bool opening) {
    // FrontEnd starts with the requested reference double in the C ABI.
    configured_ref = *static_cast<const double*>(fe);
    std::FILE* control = std::fopen("/tmp/websa_fake_iqs_auto.txt", "r");
    auto_ref_enabled = control != nullptr;
    if (control) {
        auto_threshold = -INFINITY;
        auto_ref_ceiling = INFINITY;
        std::fscanf(control, "%lf %lf", &auto_threshold, &auto_ref_ceiling);
        std::fclose(control);
    }
    if (auto_ref_enabled) front_end_actual = {std::fmin(configured_ref - 2.5, auto_ref_ceiling), 18, 1, 3};
    else front_end_actual = opening ? FrontEndActual{-7.5, 12, 1, 2} : FrontEndActual{-27.5, 18, 1, 3};
}

static const unsigned long long kStampStep = 2ull * 16240ull;
static int capture_decimate = 1;
static unsigned long long stamp = 0;
static int served = 0;
static char last_sig[128] = {0};
static int sig_seen = 0;
static std::atomic<bool> armed{false};
static int clock_source = 1;
static int warning = 0;

static int load_injection(char* sig, int injection[2]) {
    std::FILE* f = std::fopen("/tmp/websa_fake_iqs_fail.txt", "r");
    if (!f) return 0;
    const int ok = std::fgets(sig, 128, f) != nullptr;
    std::fclose(f);
    if (!ok) return 0;
    if (std::sscanf(sig, "%d %d", &injection[0], &injection[1]) != 2) return 0;
    return injection[1] > 0;
}

extern "C" int san90_open_v8(void** out, double center, const void* fe, int decimate,
                             double* bandwidth, double* configured) {
    if (!out || !fe || !bandwidth || !configured || decimate < 1 || decimate > 64 || (decimate & (decimate - 1))) return -100;
    std::FILE* of = std::fopen("/tmp/websa_fake_iqs_open_fail.txt", "r");
    if (of) {
        const int c = std::fgetc(of);
        std::fclose(of);
        if (c == '1') return -110;
    }
    *out = reinterpret_cast<void*>(0x1);
    capture_decimate = decimate;
    *bandwidth = 50000000.0 / decimate;
    *configured = center;
    auto_ref_actual(fe, true);
    stamp = 0;
    served = 0;
    sig_seen = 0;
    last_sig[0] = 0;
    armed = false;
    clock_source = 1;
    warning = 0;
    return 0;
}

extern "C" int san90_iqs_arm_v1(void* handle) {
    if (!handle) return -100;
    armed = true;
    return 0;
}

extern "C" int san90_test_iqs_armed() { return armed.load() ? 1 : 0; }

static double reconfig_center = 0.0;

extern "C" int san90_iqs_reconfig_v2(void* handle, double center, const void* fe,
                                     int decimate, double* bandwidth, double* configured) {
    if (!handle || !fe || !bandwidth || !configured || decimate < 1 || decimate > 64 || (decimate & (decimate - 1))) return -100;
    std::FILE* failure = std::fopen("/tmp/websa_fake_iqs_reconfig_fail.txt", "r");
    if (failure) {
        int status = 0;
        const int parsed = std::fscanf(failure, "%d", &status);
        std::fclose(failure);
        if (parsed == 1 && status != 0) return status;
    }
    capture_decimate = decimate;
    reconfig_center = center;
    clock_source = 0;
    *bandwidth = 50000000.0 / decimate;
    *configured = center;
    auto_ref_actual(fe, false);
    return 0;
}

extern "C" int san90_iqs_clock_v1(void* handle, double* hz, int* source) {
    if (!handle || !hz || !source) return -100;
    *hz = clock_source == 1 ? 10000000.0 : 100000000.0;
    *source = clock_source;
    return 0;
}

extern "C" int san90_iqs_front_end_actual_v1(void* handle, FrontEndActual* out) {
    if (!handle || !out) return -100;
    *out = front_end_actual;
    return 0;
}

extern "C" int san90_gnss_cached_v1(void* handle, int* lock, int* sats, int* docxo,
    int* mode, int* antenna, float* latitude, float* longitude, int* altitude,
    int* year, int* month, int* day, int* hour, int* minute, int* second) {
    if (!handle || !armed) return -100;
    *lock = 1; *sats = 22; *docxo = 0; *mode = 1; *antenna = 1;
    *latitude = 30.0f; *longitude = 120.0f; *altitude = 50;
    *year = 2026; *month = 10; *day = 23; *hour = 12; *minute = 0;
    *second = static_cast<int>(stamp / kStampStep);
    return 0;
}


extern "C" int san90_next_v3(void* handle, short* dest, unsigned int capacity,
                             PacketInfo* info, double* freq, float* power,
                             unsigned int spectrum_capacity, unsigned int* points,
                             Timing* timing, int* packet_warning) {
    (void)handle;
    if (!timing || !packet_warning) return -100;
    *timing = {};
    *packet_warning = 0;
    if (!armed || !dest || !info || capacity < 16240u) return -100;
    char sig[128] = {0};
    int injection[2] = {0, 0};
    if (load_injection(sig, injection)
        && (sig_seen == 0 || std::strncmp(last_sig, sig, sizeof(last_sig)) != 0)) {
        std::memcpy(last_sig, sig, sizeof(last_sig));
        sig_seen = 1;
        served = 0;
    }
    warning = 0;
    if (injection[1] > 0 && served < injection[1]) {
        served += 1;
        usleep(5000);
        if (injection[0] != -12) return injection[0];
        warning = -12;
    }
    if (auto_ref_enabled && front_end_actual.ref_dbm < auto_threshold) warning = -12;
    info->stamp = stamp;
    stamp += kStampStep * capture_decimate;
    info->samples = 16240;
    info->scale_to_v = 1.0f;
    if (warning == -12) {
        for (int i = 0; i < 16240 * 2; i += 2) {
            dest[i] = 32767;
            dest[i + 1] = -32768;
        }
    } else {
        for (int i = 0; i < 16240 * 2; i += 251) dest[i] = static_cast<short>(i);
    }
    if (freq && power && points) {
        if (spectrum_capacity < 8u) return -107;
        for (unsigned int i = 0; i < 8u; ++i) {
            freq[i] = 100000000.0 + 1000.0 * static_cast<double>(i);
            power[i] = -60.0f;
        }
        *points = 8;
    }
    *packet_warning = warning;
    return 0;
}

extern "C" int san90_close(void* handle) {
    (void)handle;
    return 0;
}
"#;

    pub(crate) fn compile_fake_iqs_shim() -> String {
        compile_fake_iqs_source("shim", FAKE_IQS_SHIM)
    }

    fn compile_fake_iqs_source(name: &str, contents: &str) -> String {
        let dir = std::env::temp_dir().join(format!("websa_fake_iqs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join(format!("{name}.cpp"));
        std::fs::write(&source, contents).unwrap();
        let built = dir.join(format!("libwebsa_fake_iqs_{name}.so"));
        let status = std::process::Command::new("g++")
            .args(["-O2", "-std=c++17", "-fPIC", "-shared"])
            .arg(&source)
            .arg("-o")
            .arg(&built)
            .status()
            .expect("g++ required for the fake IQS shim contract tests");
        assert!(status.success(), "fake IQS shim compile failed");
        built.to_str().unwrap().to_owned()
    }

    fn fake_iqs_armed(path: &str) -> bool {
        let path = CString::new(path).unwrap();
        unsafe {
            let library = dlopen(path.as_ptr(), crate::hardware_swp::SDK_DLOPEN_FLAGS);
            assert!(!library.is_null());
            let symbol = dlsym(library, b"san90_test_iqs_armed\0".as_ptr().cast());
            assert!(!symbol.is_null());
            let query: unsafe extern "C" fn() -> c_int = std::mem::transmute(symbol);
            let armed = query() != 0;
            dlclose(library);
            armed
        }
    }

    fn write_injection(status: i32, count: usize) {
        std::fs::write(FAKE_IQS_FAIL, format!("{status} {count}\n")).unwrap();
    }

    fn write_open_fail(enabled: bool) {
        let path = "/tmp/websa_fake_iqs_open_fail.txt";
        if enabled {
            std::fs::write(path, "1\n").unwrap();
        } else {
            let _ = std::fs::remove_file(path);
        }
    }


    fn wait_for_packets(capture: &HardwareCapture, packets: u64) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let stats = capture.stats();
            if stats.packets >= packets {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("capture did not flow: {stats:?}"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn startup_waits_for_acquisition_arm_and_preserves_first_samples() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        let shim = compile_fake_iqs_shim();
        // Exercise the reused loaded shim as well as the initial session.
        for _ in 0..2 {
            let mut capture = HardwareCapture::start(shim.clone(), 100e6, FRONT_END, 1).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            assert!(!fake_iqs_armed(&shim), "consumer setup started vendor acquisition");
            assert_eq!(capture.stats().packets, 0);
            capture.arm().unwrap();
            assert!(fake_iqs_armed(&shim));
            let first = capture.next().unwrap();
            let second = capture.next().unwrap();
            assert_eq!(first.stamp, 0, "initial samples were discarded");
            assert_eq!(second.stamp - first.stamp, 2 * N as u64);
        }
    }

    #[test]
    fn cached_gnss_keeps_iqs_timeline_and_reports_configured_clock() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        let mut capture = HardwareCapture::start(compile_fake_iqs_shim(), 100e6, FRONT_END, 1).unwrap();
        // The SDK resolves an external clock despite an internal request.
        assert_eq!(capture.geometry.refclk_src, Some(1));
        assert_eq!(capture.geometry.refclk_hz, Some(10e6));
        let actual = capture.geometry.front_end.unwrap();
        assert_eq!((actual.ref_dbm, actual.atten, actual.preamp, actual.ifgain), (-7.5, 12, 1, 2));
        let reader = capture.gnss_reader();
        capture.arm().unwrap();
        let first = capture.next().unwrap();
        let gnss = reader.gnss().unwrap();
        assert_eq!((gnss.lock, gnss.sats, gnss.docxo), (1, 22, 0));
        let second = capture.next().unwrap();
        assert_eq!(second.stamp - first.stamp, 2 * N as u64);
        let geometry = capture.reconfigure(101e6, FRONT_END, 1).unwrap();
        assert_eq!(geometry.refclk_src, Some(0));
        assert_eq!(geometry.refclk_hz, Some(100e6));
        assert_eq!(capture.diagnostics().clock(), (Some(100e6), Some(0)));
        let actual = geometry.front_end.unwrap();
        assert_eq!((actual.ref_dbm, actual.atten, actual.preamp, actual.ifgain), (-27.5, 18, 1, 3));
        let published = capture.diagnostics().front_end().unwrap();
        assert_eq!((published.ref_dbm, published.atten, published.preamp, published.ifgain), (-27.5, 18, 1, 3));
        assert_eq!(capture.stats().recoveries, 0);
    }

    #[test]
    fn overflow_packets_reach_consumer_in_order_beyond_recovery_streak_and_clear_warning() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        write_injection(-12, 1000);
        let mut capture = HardwareCapture::start(compile_fake_iqs_shim(), 100e6, FRONT_END, 1).unwrap();
        let diagnostics = capture.diagnostics();
        capture.arm().unwrap();
        let mut expected_stamp = 0;
        let mut spectrum_seen = false;
        for _ in 0..80 {
            let packet = capture.next().unwrap();
            assert_eq!(packet.stamp, expected_stamp);
            expected_stamp += 2 * N as u64;
            assert_eq!(packet.warning, -12);
            for pair in packet.iq.chunks_exact(2) {
                assert_eq!(pair, &[i16::MAX, i16::MIN]);
            }
            if let Some((freq, power)) = packet.spectrum {
                assert_eq!(freq[0], 100e6);
                assert_eq!(power[0], -60.0);
                spectrum_seen = true;
            }
        }
        assert!(spectrum_seen);
        assert_eq!(diagnostics.warning(), -12);
        let stats = diagnostics.stats();
        assert_eq!((stats.skips, stats.fail_calls, stats.recoveries), (0, 0, 0));
        std::fs::remove_file(FAKE_IQS_FAIL).unwrap();
        // At most the fixed queue and the held packet can precede the clean packet.
        let mut cleared = false;
        for _ in 0..66 {
            let packet = capture.next().unwrap();
            assert_eq!(packet.stamp, expected_stamp);
            expected_stamp += 2 * N as u64;
            if packet.warning == 0 {
                cleared = true;
                break;
            }
            assert_eq!(packet.warning, -12);
        }
        assert!(cleared, "clean packet did not reach the consumer");
        assert_eq!(diagnostics.warning(), 0);
        let stats = diagnostics.stats();
        assert_eq!((stats.skips, stats.fail_calls, stats.recoveries), (0, 0, 0));
    }

    #[test]
    #[ignore = "requires /opt/htraapi SDK headers and libhtraapi.so.0.55.100; no hardware access"]
    fn production_shim_preserves_valid_overflow_and_rejects_malformed_outputs() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sdk = std::path::Path::new("/opt/htraapi");
        let source = format!("#include {:?}\n{}", concat!(env!("CARGO_MANIFEST_DIR"), "/sdk/san90_sdk.cpp"), r#"
#include <cassert>
#include <limits>
extern "C" int IQS_ProfileDeInit(void**, IQS_Profile_TypeDef* out) { *out = {}; return 0; }
extern "C" int SWP_ProfileDeInit(void**, SWP_Profile_TypeDef* out) { *out = {}; return 0; }
extern "C" int SWP_Configuration(void**, const SWP_Profile_TypeDef* in, SWP_Profile_TypeDef* out, SWP_TraceInfo_TypeDef* trace) {
    *out = *in; *trace = {}; return 0;
}
extern "C" int IQS_Configuration(void**, const IQS_Profile_TypeDef* in, IQS_Profile_TypeDef* out, IQS_StreamInfo_TypeDef* stream) {
    *out = *in;
    out->RefLevel_dBm = -17.5; out->Atten = 13; out->Preamplifier = ForcedOff; out->IFGainGrade = 2;
    *stream = {}; stream->PacketSamples = 16240; stream->IQSampleRate = 3906250.0; stream->Bandwidth = 3125000.0;
    return 0;
}
static int packet_mode = -12;
static int fft_mode = -12;
static bool overflow_flag = false;
static uint64_t next_stamp = 0;
static int16_t samples[16240 * 2];
extern "C" int IQS_GetIQStream_PM1(void**, IQStream_TypeDef* stream) {
    *stream = {};
    stream->AlternIQStream = packet_mode == 2 ? nullptr : samples;
    stream->IQS_StreamInfo.PacketSamples = packet_mode == 1 ? 1 : 16240;
    stream->IQS_TriggerInfo.SysTimerCountOfFirstDataPoint = next_stamp;
    next_stamp += 519680;
    stream->IQS_ScaleToV = packet_mode == 3 ? std::numeric_limits<float>::quiet_NaN() : 1.0f;
    stream->DeviceState.IFOverflow = overflow_flag;
    return packet_mode < 0 ? packet_mode : 0;
}
extern "C" int DSP_FFT_IQSToSpectrum(void**, const IQStream_TypeDef* stream, double* freq, float* power) {
    assert(stream->IQS_StreamInfo.Bandwidth == 3125000.0);
    assert(stream->IQS_StreamInfo.IQSampleRate == 3906250.0);
    for (int i = 0; i < 8; ++i) {
        freq[i] = 100000000.0 + 1000.0 * i;
        power[i] = fft_mode == 1 ? std::numeric_limits<float>::quiet_NaN() : -5.0f;
    }
    return fft_mode < 0 ? fft_mode : 0;
}
int main() {
    San90Device state{};
    state.triggered = true;
    state.center_hz = 100000000.0;
    state.bandwidth_hz = 3125000.0;
    state.sample_rate_hz = 3906250.0;
    state.fft_points = 8;
    san90_session.call(nullptr, [&] {
        san90_session.active = &state;
        san90_session.handle = reinterpret_cast<void*>(1);
        san90_session.healthy = true;
        return 0;
    });
    san90_session.call(&state, [&] {
        state.triggered = false;
        San90FrontEnd request{};
        request.ref_dbm = -5.0; request.atten = -1; request.preamp = 0; request.ifgain = 0;
        double bandwidth = 0.0, center = 0.0;
        assert(san90_iqs_configure(&state, 100000000.0, request, &bandwidth, &center, 16) == 0);
        San90FrontEndActual actual{};
        assert(sizeof(actual) == 24);
        assert(san90_iqs_front_end_actual_v1(&state, &actual) == 0);
        assert(actual.ref_dbm == -17.5 && actual.atten == 13 && actual.preamp == 1 && actual.ifgain == 2);
        state.triggered = true;
        return 0;
    });
    for (int i = 0; i < 16240 * 2; ++i) samples[i] = i % 2 ? -32768 : 32767;
    int16_t copied[16240 * 2];
    double freq[8];
    float power[8];
    San90PacketInfo info{};
    San90IqsTiming timing{};
    uint32_t points = 0;
    int warning = 0;
    auto next = [&] {
        return san90_next_v3(&state, copied, 16240, &info, freq, power, 8, &points,
                             &timing, &warning);
    };
    for (uint64_t i = 0; i < 80; ++i) {
        int status = i % 3 == 0 ? san90_next(&state, copied, 16240, &info)
            : i % 3 == 1 ? san90_next_v2(&state, copied, 16240, &info, freq, power, 8, &points)
            : next();
        assert(status == 0);
        assert(info.stamp == i * 519680 && info.samples == 16240);
        assert(std::memcmp(copied, samples, sizeof(samples)) == 0);
        assert(state.warning == -12);
        if (i % 3 == 2) {
            assert(warning == -12 && timing.fetch_status == -12 && timing.fft_status == -12);
            assert((timing.measured & 14) == 14);
        }
        if (i % 3 != 0) assert(points == 8 && power[0] == -5.0f);
    }
    packet_mode = 0;
    fft_mode = -12;
    assert(next() == 0 && warning == -12);
    assert(timing.fetch_status == 0 && timing.fft_status == -12);
    fft_mode = 1;
    assert(next() == -103 && warning == 0);
    for (packet_mode = 1; packet_mode <= 3; ++packet_mode) {
        assert(san90_next(&state, copied, 16240, &info) == -103);
        assert(san90_next_v2(&state, copied, 16240, &info, nullptr, nullptr, 0, &points) == -103);
        assert(next() == -103 && warning == 0);
    }
    packet_mode = fft_mode = 0;
    assert(next() == 0 && warning == 0);
    overflow_flag = true;
    assert(next() == 0 && warning == -12);
    assert(san90_next(&state, copied, 16240, &info) == 0 && state.warning == -12);
    overflow_flag = false;
    packet_mode = -12;
    assert(san90_next(&state, copied, 16240, &info) == 0);
    packet_mode = 0;
    assert(san90_next(&state, copied, 16240, &info) == 0 && state.warning == 0);
    assert(next() == 0 && warning == 0);
    packet_mode = -10;
    assert(next() == -10 && warning == 0 && timing.fetch_status == -10);
    san90_session.call(&state, [&] {
        san90_session.active = nullptr;
        san90_session.handle = nullptr;
        san90_session.healthy = false;
        return 0;
    });
}
"#);
        let dir = std::env::temp_dir().join(format!("websa_fake_iqs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("adapter.cpp");
        let executable = dir.join("adapter");
        std::fs::write(&input, source).unwrap();
        let status = std::process::Command::new("g++")
            .args(["-O2", "-std=c++17", "-pthread", "-I"])
            .arg(sdk.join("inc"))
            .arg(&input)
            .arg("-L").arg(sdk.join("lib/x86_64"))
            .arg("-Wl,-rpath,/opt/htraapi/lib/x86_64")
            .arg("-l:libhtraapi.so.0.55.100")
            .arg("-o").arg(&executable)
            .status().expect("g++ and SDK development files required for production shim contract");
        assert!(status.success(), "production adapter fixture failed to compile");
        assert!(std::process::Command::new(executable).status().unwrap().success());
    }

    #[test]
    fn transient_iqs_status_skips_and_keeps_streaming() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let shim = compile_fake_iqs_shim();
        // Two consecutive -10 BusTimeOut: below the streak threshold, so the owner
        // must skip both and keep the stream alive. This exact pattern failed the
        // whole session (and reset the IQBF sequence) before the fix.
        write_injection(-10, 2);
        let mut capture = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        wait_for_packets(&capture, 3).unwrap();
        let stats = capture.stats();
        assert_eq!(stats.skips, 2);
        assert_eq!(stats.last_skip_status, -10);
        // No error latched: the next call delivers a packet.
        capture.next().unwrap();
    }

    #[test]
    fn sustained_iqs_stall_requires_new_stream_before_timestamp_reset() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        let shim = compile_fake_iqs_shim();
        let mut capture = HardwareCapture::start(shim.clone(), 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        assert_eq!(capture.next().unwrap().stamp, 0);
        assert_eq!(capture.next().unwrap().stamp, 2 * N as u64);
        write_injection(-10, 1000);
        let error = loop {
            if let Err(error) = capture.next() { break error; }
        };
        assert!(error.contains("status=-10"), "{error}");
        assert!(error.contains("new stream epoch"), "{error}");
        assert_eq!(capture.stats().recoveries, 0);
        assert!(capture.next().is_err(), "failed capture must not deliver old queued IQ");
        drop(capture);
        std::fs::remove_file(FAKE_IQS_FAIL).unwrap();
        let mut fresh = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
        fresh.arm().unwrap();
        assert_eq!(fresh.next().unwrap().stamp, 0);
        assert_eq!(fresh.next().unwrap().stamp, 2 * N as u64);
    }

    #[test]
    fn consumer_backpressure_retains_packets_without_reopening() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        let shim = compile_fake_iqs_shim();
        // The held packet must retain its timestamp while the consumer catches up.
        let mut capture = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while capture.stats().queue_full == 0 {
            assert!(Instant::now() < deadline, "consumer queue never filled");
            std::thread::sleep(Duration::from_millis(10));
        }
        let diagnostics = capture.diagnostics();
        let initial = diagnostics.events();
        // Retries of this held packet must not duplicate the first failed enqueue.
        assert_eq!(initial.queue_full_total, 1);
        assert_eq!(initial.queue_full_overwritten, 0);
        let attribution = initial.queue_full_attribution[0];
        assert_eq!((attribution.seq, attribution.stamp, attribution.input_index),
            (PACKET_QUEUE as u64, PACKET_QUEUE as u64 * 2 * N as u64, PACKET_QUEUE as u64 * N as u64));
        assert_eq!((attribution.occupancy, attribution.capacity),
            (PACKET_QUEUE as u32, PACKET_QUEUE as u32));
        assert!(attribution.since_last_service_us.is_none());
        assert!(attribution.last_output_seq.is_none());
        assert!(attribution.last_output_input_index.is_none());
        assert!(attribution.last_output_frames.is_none());
        assert!(attribution.since_last_output_us.is_none());
        assert!(capture.stats().queue_full >= 1);

        capture.note_output(7, 1234, 3, Instant::now() - Duration::from_millis(10));
        for expected_total in 2..=3 {
            // Drain exactly one slot, then let the next packet fail to enqueue.
            // By the third episode, a prior next() has completed successfully.
            capture.next().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let events = loop {
                let events = diagnostics.events();
                if events.queue_full_total >= expected_total { break events; }
                assert!(Instant::now() < deadline, "queue never refilled");
                std::thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(events.queue_full_total, expected_total);
            let latest = events.queue_full_attribution.last().unwrap();
            assert_eq!(latest.seq, PACKET_QUEUE as u64 + expected_total - 1);
            assert_eq!(latest.input_index, latest.seq * N as u64);
            assert_eq!(latest.last_output_seq, Some(7));
            assert_eq!(latest.last_output_input_index, Some(1234));
            assert_eq!(latest.last_output_frames, Some(3));
            assert!(latest.since_last_output_us.unwrap() >= 10_000);
            if expected_total == 3 { assert!(latest.since_last_service_us.is_some()); }
            // Earlier failure evidence survives later successful service/output.
            assert!(events.queue_full_attribution[0].since_last_service_us.is_none());
            assert!(events.queue_full_attribution[0].last_output_seq.is_none());
        }
        for ordinal in 2..PACKET_QUEUE + 3 {
            assert_eq!(capture.next().unwrap().stamp, ordinal as u64 * 2 * N as u64);
        }
        assert_eq!(capture.stats().recoveries, 0);
    }

    #[test]
    fn sustained_consumer_backlog_fails_with_distinct_cause() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        let shim = compile_fake_iqs_shim();
        let mut capture = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while capture.error.lock().is_none() {
            assert!(Instant::now() < deadline, "backlog never failed");
            thread::sleep(Duration::from_millis(10));
        }
        let error = capture.next().err().expect("old queued IQ escaped after failure");
        assert!(error.contains("consumer backlog"), "{error}");
        assert_eq!(capture.stats().recoveries, 0);
        assert_eq!(capture.stats().fail_calls, 0);
    }

    #[test]
    fn transient_streak_boundary_59_survives_and_60_ends_stream() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let shim = compile_fake_iqs_shim();
        // 59 consecutive non-timeout transients (-9 BusDataError): the 60-streak
        // threshold must not trip and the timeout streak must stay reset.
        write_injection(-9, 59);
        let mut capture = HardwareCapture::start(shim.clone(), 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        wait_for_packets(&capture, 2).unwrap();
        let stats = capture.stats();
        assert_eq!(stats.skips, 59);
        assert_eq!(stats.last_skip_status, -9);
        assert_eq!(stats.recoveries, 0);
        capture.next().unwrap();
        drop(capture);
        write_injection(-9, 60);
        let mut capture = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        let error = capture.next().err().expect("sustained transient must fail");
        assert!(error.contains("status=-9"), "{error}");
        assert!(error.contains("new stream epoch"), "{error}");
        std::fs::remove_file(FAKE_IQS_FAIL).unwrap();
    }

    #[test]
    fn reconfigure_while_streaming_tags_new_generation_without_recovery() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        let shim = compile_fake_iqs_shim();
        let mut capture = HardwareCapture::start(shim, 100e6, FRONT_END, 1).unwrap();
        capture.arm().unwrap();
        assert_eq!(capture.next().unwrap().generation, 0);
        let front_end = FrontEnd {
            ref_dbm: -20.0,
            ..FRONT_END
        };
        let geometry = capture.reconfigure(200e6, front_end, 1).unwrap();
        assert_eq!(geometry.center, 200e6);
        assert_eq!(geometry.generation, 1);
        capture.arm().unwrap();
        // Up to two old-generation packets can still be queued; later fetches are generation 1.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let packet = capture.next().unwrap();
            if packet.generation == 1 {
                break;
            }
            assert_eq!(packet.generation, 0);
            assert!(Instant::now() < deadline, "no generation 1 packet");
        }
        assert_eq!(capture.next().unwrap().generation, 1);
        assert_eq!(capture.stats().recoveries, 0);
    }

    #[test]
    fn optional_front_end_query_missing_or_failed_keeps_gain_unknown() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_open_fail(false);
        let _ = std::fs::remove_file(FAKE_IQS_FAIL);
        for (name, source) in [
            ("no_front_end", FAKE_IQS_SHIM.replace("san90_iqs_front_end_actual_v1", "san90_test_no_front_end")),
            ("failed_front_end", FAKE_IQS_SHIM.replace("*out = front_end_actual;", "return -100;")),
        ] {
            let shim = compile_fake_iqs_source(name, &source);
            let mut capture = HardwareCapture::start(shim.clone(), 100e6, FRONT_END, 1).unwrap();
            assert!(capture.geometry.front_end.is_none());
            assert!(capture.diagnostics().front_end().is_none());
            let geometry = capture.reconfigure(101e6, FRONT_END, 1).unwrap();
            assert!(geometry.front_end.is_none());
            assert!(capture.diagnostics().front_end().is_none());
            assert_eq!(geometry.generation, 1);
            assert!(!fake_iqs_armed(&shim), "reconfigure must not start acquisition");
            assert_eq!(capture.stats().packets, 0);
            capture.arm().unwrap();
            let first = capture.next().unwrap();
            assert_eq!((first.generation, first.stamp), (1, 0));
        }
    }

    #[test]
    fn missing_or_old_shim_fails_without_opening_hardware() {
        let _guard = TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(HardwareCapture::start(
            "/does/not/exist.so".into(), 100e6, FRONT_END, 1
        ).is_err());
        // A streaming-open ABI cannot satisfy a configure-before-arm consumer.
        let old = FAKE_IQS_SHIM.replace("san90_open_v8", "san90_open_v7");
        let error = HardwareCapture::start(
            compile_fake_iqs_source("old", &old), 100e6, FRONT_END, 1
        ).err().expect("old acquisition ABI accepted");
        assert!(error.contains("san90_open_v8"), "{error}");
        let no_arm = FAKE_IQS_SHIM.replace("san90_iqs_arm_v1", "san90_test_no_arm");
        let error = HardwareCapture::start(
            compile_fake_iqs_source("no_arm", &no_arm), 100e6, FRONT_END, 1
        ).err().expect("shim without acquisition arm accepted");
        assert!(error.contains("san90_iqs_arm_v1"), "{error}");
        let old_next = FAKE_IQS_SHIM.replace("san90_next_v3", "san90_test_old_next");
        let error = HardwareCapture::start(
            compile_fake_iqs_source("old_next", &old_next), 100e6, FRONT_END, 1
        ).err().expect("shim without combined packet metadata accepted");
        assert!(error.contains("san90_next_v3"), "{error}");
    }
}
