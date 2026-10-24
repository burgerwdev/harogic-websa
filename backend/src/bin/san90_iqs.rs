//! Opt-in SAN-90 SDK-owner prototype. No Python runtime; not yet a WebSA service.
//! C++ only translates the official 0.55.100 SDK structs into a checked C ABI.
#[path = "../control_http.rs"]
mod control_http;
use control_http::{FrameHub, HttpControl};
use rustfft::{num_complex::Complex, FftPlanner};
use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::net::SocketAddr;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};
use websa_channelizer::{
    control_channel, pfb_pool_close, pfb_pool_finish, pfb_pool_open_strategy, pfb_pool_stats,
    pfb_pool_submit, pfb_pool_take, BasebandFramer, ChannelAction, ChannelCommand, ControlRequest,
    FineBank, FineChannel, FineSpec, PfbBenchPool, PfbStream, PoolResult, PoolStats, MAX_IN_FLIGHT,
};

const N: usize = 16240;
const FS: f64 = 62_500_000.0;
const CAPACITY: usize = 2;
const FIXTURE_CENTER_HZ: f64 = 100_000_000.0;

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

type Open = unsafe extern "C" fn(*mut *mut c_void, f64, *const FrontEnd, c_int, *mut f64, *mut f64) -> i32;
type Arm = unsafe extern "C" fn(*mut c_void) -> i32;

#[repr(C)]
struct FrontEnd {
    ref_dbm: f64,
    atten: c_int,
    preamp: c_int,
    ifgain: c_int,
    gain_strategy: c_int,
    ref_clock: c_int,
    refclk_out: c_int,
}
type Next = unsafe extern "C" fn(*mut c_void, *mut i16, u32, *mut PacketInfo) -> i32;
type Close = unsafe extern "C" fn(*mut c_void) -> i32;

#[repr(C)]
#[derive(Default)]
struct PacketInfo {
    stamp: u64,
    samples: u32,
    scale_to_v: f32,
}

struct Sdk {
    library: *mut c_void,
    open: Open,
    arm: Arm,
    next: Next,
    close: Close,
}

impl Sdk {
    fn load(path: &str) -> Result<Self, String> {
        let path = CString::new(path).map_err(|e| e.to_string())?;
        let library = unsafe { dlopen(path.as_ptr(), 2 | 0x1000) }; // RTLD_NOW | RTLD_NODELETE
        if library.is_null() {
            let message = unsafe { CStr::from_ptr(dlerror()) }.to_string_lossy();
            return Err(format!("SDK shim: {message}"));
        }
        let result = (|| {
            let symbol = |name: &'static [u8]| -> Result<*mut c_void, String> {
                let ptr = unsafe { dlsym(library, name.as_ptr().cast()) };
                if ptr.is_null() {
                    Err(format!(
                        "SDK shim missing {}",
                        String::from_utf8_lossy(name)
                    ))
                } else {
                    Ok(ptr)
                }
            };
            Ok(Self {
                library,
                open: unsafe {
                    std::mem::transmute::<*mut c_void, Open>(symbol(b"san90_open_v8\0")?)
                },
                arm: unsafe {
                    std::mem::transmute::<*mut c_void, Arm>(symbol(b"san90_iqs_arm_v1\0")?)
                },
                next: unsafe { std::mem::transmute::<*mut c_void, Next>(symbol(b"san90_next\0")?) },
                close: unsafe {
                    std::mem::transmute::<*mut c_void, Close>(symbol(b"san90_close\0")?)
                },
            })
        })();
        if result.is_err() {
            unsafe {
                dlclose(library);
            }
        }
        result
    }
}

impl Drop for Sdk {
    fn drop(&mut self) {
        let finish = unsafe { dlsym(self.library, b"san90_session_finish_v1\0".as_ptr().cast()) };
        if !finish.is_null() {
            let finish: unsafe extern "C" fn() -> c_int = unsafe { std::mem::transmute(finish) };
            let status = unsafe { finish() };
            if status != 0 { eprintln!("SDK session finish status={status}"); }
        }
        unsafe { dlclose(self.library); }
    }
}

struct Device {
    handle: *mut c_void,
    close: Close,
    next: Next,
    usable_bandwidth: f64,
    center_hz: f64,
}

impl Device {
    fn open(open: Open, next: Next, close: Close) -> Result<Self, String> {
        let mut handle = std::ptr::null_mut();
        let mut usable_bandwidth = 0.0;
        let mut center_hz = 0.0;
        let front_end = FrontEnd {
            ref_dbm: 0.0,
            atten: -1,
            preamp: 0,
            ifgain: 2,
            gain_strategy: 0,
            ref_clock: 0,
            refclk_out: 0,
        };
        let status = unsafe {
            open(
                &mut handle,
                FIXTURE_CENTER_HZ,
                &front_end,
                1,
                &mut usable_bandwidth,
                &mut center_hz,
            )
        };
        if status != 0 || handle.is_null() {
            if !handle.is_null() {
                unsafe {
                    close(handle);
                }
            }
            return Err(format!("san90_open status={status}"));
        }
        if !usable_bandwidth.is_finite() || !(50_000_000.0..=FS).contains(&usable_bandwidth) {
            unsafe {
                close(handle);
            }
            return Err("unexpected SDK IQ bandwidth".into());
        }
        if !center_hz.is_finite() || !(8_000.0..=9_020_000_000.0).contains(&center_hz) {
            unsafe {
                close(handle);
            }
            return Err("unexpected SDK IQ center".into());
        }
        Ok(Self {
            handle,
            next,
            close,
            usable_bandwidth,
            center_hz,
        })
    }

    fn fetch(&mut self) -> (Result<(PacketInfo, Vec<i16>), String>, Duration) {
        let mut iq = vec![0i16; N * 2];
        let mut info = PacketInfo::default();
        let started = Instant::now();
        let status = unsafe { (self.next)(self.handle, iq.as_mut_ptr(), N as u32, &mut info) };
        let sdk_time = started.elapsed();
        let result = if status != 0 {
            Err(format!("san90_next status={status}"))
        } else if info.samples as usize != N {
            Err("SDK packet length changed".into())
        } else {
            Ok((info, iq))
        };
        (result, sdk_time)
    }
    fn shutdown(&mut self) -> Result<(), String> {
        if self.handle.is_null() {
            return Ok(());
        }
        let handle = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        let status = unsafe { (self.close)(handle) };
        if status != 0 {
            Err(format!("san90_close status={status}"))
        } else {
            Ok(())
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            eprintln!("{e}");
        }
    }
}

struct Packet {
    stamp: u64,
    received: Instant,
    iq: Vec<i16>,
}

#[derive(Default)]
struct Latency {
    bins: Vec<u64>, // 20 us buckets, capped at 20.48 ms; max tracked exactly.
    count: u64,
    max_ns: u64,
}

impl Latency {
    fn record(&mut self, ns: u64) {
        if self.bins.is_empty() {
            self.bins.resize(1024, 0);
        }
        self.bins[(ns / 20_000).min(1023) as usize] += 1;
        self.count += 1;
        self.max_ns = self.max_ns.max(ns);
    }

    fn p99_upper_ms(&self) -> f64 {
        let rank = (self.count * 99).div_ceil(100);
        let mut seen = 0;
        for (index, count) in self.bins.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return (index + 1) as f64 * 0.02;
            }
        }
        0.0
    }
}

#[derive(Default, Clone, Copy)]
struct CaptureQuarter {
    fetched: u64,
    fetch: Duration,
    sdk_call: Duration,
    idle: Duration,
    wait: Duration,
}

#[derive(Default)]
struct CaptureStats {
    full: u64,
    blocked: u64,
    wait: Duration,
    wait_max: Duration,
    fetch: Duration,
    fetch_max: Duration,
    fetch_latency: Latency,
    sdk_latency: Latency,
    sdk_max: Duration,
    idle_latency: Latency,
    idle_max: Duration,
    quarters: [CaptureQuarter; 4],
    fetched: u64,
    usable_bandwidth_hz: f64,
    center_hz: f64,
    first: Option<Instant>,
    last: Option<Instant>,
}

#[derive(Clone, Copy)]
struct CaptureConfig {
    bandwidth_hz: f64,
    center_hz: f64,
}

struct CaptureOutcome {
    stats: CaptureStats,
    error: Option<String>,
}

fn capture_quarters(stats: &CaptureStats) -> [(u64, f64, f64, f64, f64); 4] {
    stats.quarters.map(|q| {
        (
            q.fetched,
            q.fetch.as_secs_f64() * 1e3,
            q.sdk_call.as_secs_f64() * 1e3,
            q.idle.as_secs_f64() * 1e3,
            q.wait.as_secs_f64() * 1e3,
        )
    })
}

fn fixture_blocks() -> Vec<Vec<i16>> {
    let mut seed = 20261019u32;
    (0..120)
        .map(|_| {
            (0..2 * N)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 8) as i16
                })
                .collect()
        })
        .collect()
}

fn capture(
    fixture: bool,
    target: usize,
    sdk: Option<(Open, Arm, Next, Close)>,
    sender: SyncSender<Packet>,
    metadata: Option<SyncSender<CaptureConfig>>,
    start: Option<Receiver<()>>,
) -> CaptureOutcome {
    let mut stats = CaptureStats::default();
    let result = (|| -> Result<(), String> {
        // Device constructed and destroyed on this thread. No other SDK calls in flight.
        let mut device = if fixture {
            None
        } else {
            let (open, _, next, close) = sdk.ok_or("missing SDK entry points")?;
            Some(Device::open(open, next, close)?)
        };
        let config = device.as_ref().map_or(
            CaptureConfig {
                bandwidth_hz: FS * 0.8,
                center_hz: FIXTURE_CENTER_HZ,
            },
            |d| CaptureConfig {
                bandwidth_hz: d.usable_bandwidth,
                center_hz: d.center_hz,
            },
        );
        stats.usable_bandwidth_hz = config.bandwidth_hz;
        stats.center_hz = config.center_hz;
        if let Some(metadata) = metadata {
            metadata
                .send(config)
                .map_err(|_| "capture metadata receiver closed")?;
        }
        if let Some(start) = start {
            start.recv().map_err(|_| "capture start sender closed")?;
        }
        if let Some(device) = device.as_ref() {
            let (_, arm, _, _) = sdk.ok_or("missing SDK entry points")?;
            let status = unsafe { arm(device.handle) };
            if status != 0 {
                return Err(format!("san90_iqs_arm_v1 status={status}"));
            }
        }
        let blocks = if fixture {
            fixture_blocks()
        } else {
            Vec::new()
        };
        let mut last_fetch_end = None;
        for seq in 0..target {
            let quarter = (seq * 4 / target).min(3);
            let begin = Instant::now();
            if let Some(previous) = last_fetch_end {
                let idle = begin.duration_since(previous);
                stats.idle_latency.record(idle.as_nanos() as u64);
                stats.idle_max = stats.idle_max.max(idle);
                stats.quarters[quarter].idle += idle;
            }
            let (fetched, sdk_time) = if let Some(device) = device.as_mut() {
                let (result, sdk_time) = device.fetch();
                (result.map(|(info, iq)| (info.stamp, iq)), sdk_time)
            } else {
                (
                    Ok((
                        1000 + seq as u64 * N as u64 * 2,
                        blocks[seq % blocks.len()].clone(),
                    )),
                    Duration::ZERO,
                )
            };
            if !fixture {
                stats.sdk_latency.record(sdk_time.as_nanos() as u64);
                stats.sdk_max = stats.sdk_max.max(sdk_time);
                stats.quarters[quarter].sdk_call += sdk_time;
            }
            let now = Instant::now();
            last_fetch_end = Some(now);
            let fetch = now.duration_since(begin);
            stats.fetch += fetch;
            stats.quarters[quarter].fetch += fetch;
            stats.fetch_max = stats.fetch_max.max(fetch);
            stats.fetch_latency.record(fetch.as_nanos() as u64);
            let (stamp, iq) = fetched?;
            stats.fetched += 1;
            stats.quarters[quarter].fetched += 1;
            stats.first.get_or_insert(now);
            stats.last = Some(now);
            let packet = Packet {
                stamp,
                received: now,
                iq,
            };
            match sender.try_send(packet) {
                Ok(()) => (),
                Err(TrySendError::Disconnected(_)) => return Err("capture consumer closed".into()),
                Err(TrySendError::Full(packet)) => {
                    stats.full += 1;
                    let wait = Instant::now();
                    let sent = sender.send(packet);
                    let elapsed = wait.elapsed();
                    stats.wait += elapsed;
                    stats.quarters[quarter].wait += elapsed;
                    stats.wait_max = stats.wait_max.max(elapsed);
                    if elapsed >= Duration::from_millis(10) {
                        stats.blocked += 1;
                    }
                    sent.map_err(|_| "capture consumer closed")?;
                }
            }
        }
        if let Some(device) = device.as_mut() {
            device.shutdown()?;
        }
        Ok(())
    })();
    CaptureOutcome {
        stats,
        error: result.err(),
    }
}

struct Pool(*mut PfbBenchPool);
impl Pool {
    fn new() -> Result<Self, String> {
        let bins: Vec<u32> = (0..32).collect();
        let ptr =
            unsafe { pfb_pool_open_strategy(FS, N as u32, bins.as_ptr(), bins.len() as u32, 1) };
        if ptr.is_null() {
            Err("PFB pool unavailable".into())
        } else {
            Ok(Self(ptr))
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        unsafe {
            pfb_pool_close(self.0);
        }
    }
}

struct OutputRow {
    seq: u64,
    stamp: u64,
    groups: u32,
    first_re: f32,
    bytes: u64,
    fine_bytes: u64,
    worker_ns: u64,
    fine_ns: u64,
}

enum Engine {
    Metadata(Pool),
    Output {
        stream: Box<PfbStream>,
        channels: Vec<FineChannel>,
        hot: Option<HotState>,
        hot_enabled: bool,
        fail_seq: Option<u64>,
    },
}

struct HotState {
    bank: FineBank,
    control: Receiver<ControlRequest>,
    next_bin: usize,
    retuned: bool,
    scripted: bool,
    fail_seq: Option<u64>,
    frame_hub: Option<FrameHub>,
    framers: BTreeMap<u64, (u64, BasebandFramer)>,
    capture_center_hz: f64,
    wire_frames: u64,
}

impl HotState {
    fn deliver(
        &mut self,
        item: &websa_channelizer::PfbOutput,
        bins: &[u32],
    ) -> Result<u64, String> {
        if self.scripted {
            let stage = (item.seq as usize).min(3) + 1;
            let goal = (bins.len() * stage).div_ceil(4);
            while self.next_bin < goal {
                let id = self.next_bin as u64 + 1;
                self.bank
                    .apply(ChannelCommand {
                        epoch: item.epoch,
                        action: ChannelAction::Subscribe {
                            id,
                            bin: bins[self.next_bin],
                            offset_hz: 0.0,
                            decimate: 12,
                        },
                    })
                    .map_err(str::to_owned)?;
                self.next_bin += 1;
            }
            if stage == 4 && !self.retuned {
                self.bank
                    .apply(ChannelCommand {
                        epoch: item.epoch,
                        action: ChannelAction::Retune {
                            old_id: 1,
                            id: self.next_bin as u64 + 1,
                            bin: bins[0],
                            offset_hz: -10_000.0,
                            decimate: 12,
                        },
                    })
                    .map_err(str::to_owned)?;
                self.retuned = true;
            }
        }
        loop {
            match self.control.try_recv() {
                Ok(request) => {
                    let removed = match &request.command.action {
                        ChannelAction::Retune { old_id, .. } => Some(*old_id),
                        ChannelAction::Unsubscribe { id } => Some(*id),
                        ChannelAction::Subscribe { .. } => None,
                    };
                    let result = self.bank.apply(request.command);
                    if result.is_ok() {
                        if let Some(id) = removed {
                            if let Err(error) = self.finish_frame(id) {
                                let _ = request.reply.try_send(Err("IQ tail delivery failed"));
                                return Err(error);
                            }
                            self.framers.remove(&id);
                        }
                    }
                    let _ = request.reply.try_send(result.map(|_| item.seq));
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let capture_center_hz = self.capture_center_hz;
        let frame_hub = self.frame_hub.as_ref();
        let framers = &mut self.framers;
        let wire_frames = &mut self.wire_frames;
        let fail_seq = self.fail_seq;
        let mut bytes = 0;
        let mut delayed = false;
        self.bank
            .deliver(item, |out| {
                bytes += (out.iq.len() * 4) as u64;
                if let Some(hub) = frame_hub {
                    let id = out.channel_id;
                    if let Some(generation) = hub.selected(id) {
                        if framers
                            .get(&id)
                            .is_none_or(|(active, _)| *active != generation)
                        {
                            let offset =
                                out.center_offset_hz.ok_or("fine IQ center unavailable")?;
                            let center = capture_center_hz + offset;
                            framers.insert(
                                id,
                                (
                                    generation,
                                    BasebandFramer::new(id, out.epoch, center, out.output_rate)
                                        .ok_or("invalid fine IQ frame metadata")?,
                                ),
                            );
                        }
                        for frame in framers.get_mut(&id).unwrap().1.push(out)? {
                            hub.send(id, generation, frame)?;
                            *wire_frames += 1;
                        }
                    } else {
                        framers.remove(&id);
                    }
                }
                if fail_seq == Some(item.seq) && !delayed {
                    delayed = true;
                    thread::sleep(Duration::from_millis(2));
                }
                Ok(())
            })
            .map_err(|error| format!("hot seq={} bins={} {error}", item.seq, self.next_bin))?;
        Ok(bytes)
    }

    fn finish_frame(&mut self, id: u64) -> Result<(), String> {
        if let (Some(hub), Some((generation, framer))) =
            (&self.frame_hub, self.framers.get_mut(&id))
        {
            if hub.selected(id) == Some(*generation) {
                if let Some(frame) = framer.finish().map_err(str::to_owned)? {
                    hub.send(id, *generation, frame).map_err(str::to_owned)?;
                    self.wire_frames += 1;
                }
            }
        }
        Ok(())
    }

    fn finish_frames(&mut self) -> Result<(), String> {
        let ids: Vec<u64> = self.framers.keys().copied().collect();
        for id in ids {
            self.finish_frame(id)?;
        }
        Ok(())
    }
}

impl Engine {
    fn new(
        outputs: Option<Vec<u32>>,
        fine: bool,
        hot: bool,
        fail_seq: Option<u64>,
    ) -> Result<Self, String> {
        if let Some(bins) = outputs {
            let stream =
                PfbStream::new(FS, N, bins.clone(), 7).ok_or("PFB output stream unavailable")?;
            let channels = if fine && !hot {
                bins.iter()
                    .map(|&bin| {
                        FineChannel::new(
                            FineSpec {
                                id: bin as u64 + 1,
                                epoch: 7,
                                first_seq: 0,
                                bin,
                                offset_hz: 0.0,
                                decimate: 12,
                                coarse_delay_samples: stream.delay_samples,
                                fused_fir: true,
                            },
                            &bins,
                            stream.sample_rate,
                            stream.bin_count() / 2,
                            N / 64 + 2,
                        )
                        .ok_or("fine DDC unavailable")
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            Ok(Self::Output {
                stream: Box::new(stream),
                channels,
                hot: None,
                hot_enabled: hot,
                fail_seq,
            })
        } else if fine {
            Err("--fine requires --outputs".into())
        } else {
            Pool::new().map(Self::Metadata)
        }
    }

    fn configure_bandwidth(
        &mut self,
        config: CaptureConfig,
        control: Receiver<ControlRequest>,
        scripted: bool,
        frame_hub: Option<FrameHub>,
    ) -> Result<(), String> {
        if !config.bandwidth_hz.is_finite() || !(50_000_000.0..=FS).contains(&config.bandwidth_hz) {
            return Err("unexpected SDK IQ bandwidth".into());
        }
        if !config.center_hz.is_finite() || !(8_000.0..=9_020_000_000.0).contains(&config.center_hz)
        {
            return Err("unexpected SDK IQ center".into());
        }
        if let Self::Output {
            stream,
            hot,
            hot_enabled: true,
            fail_seq,
            ..
        } = self
        {
            if hot.is_some() {
                return Err("fine bank already configured".into());
            }
            *hot = Some(HotState {
                bank: FineBank::new(
                    stream,
                    config.bandwidth_hz,
                    N / 64 + 2,
                    stream.bins().len(),
                    stream.sample_rate * stream.bins().len() as f64 / 12.0,
                    Duration::from_millis(1),
                )
                .ok_or("fine bank unavailable")?,
                control,
                next_bin: 0,
                retuned: false,
                scripted,
                fail_seq: *fail_seq,
                frame_hub,
                framers: BTreeMap::new(),
                capture_center_hz: config.center_hz,
                wire_frames: 0,
            });
        }
        Ok(())
    }

    fn submit(&mut self, seq: u64, stamp: u64, iq: &[i16]) -> i32 {
        match self {
            Self::Metadata(pool) => unsafe {
                pfb_pool_submit(pool.0, seq, stamp, iq.as_ptr(), N as u32)
            },
            Self::Output { stream, .. } => stream.submit(seq, stamp, iq).err().unwrap_or(0),
        }
    }

    fn take(&mut self) -> Result<Option<OutputRow>, String> {
        match self {
            Self::Metadata(pool) => {
                let mut row = PoolResult::default();
                let status = unsafe { pfb_pool_take(pool.0, &mut row) };
                if status < 0 {
                    return Err("pfb_pool_take failed".into());
                }
                Ok((status == 1).then_some(OutputRow {
                    seq: row.seq,
                    stamp: row.stamp,
                    groups: row.groups,
                    first_re: row.first_re,
                    bytes: 0,
                    fine_bytes: 0,
                    worker_ns: row.process_ns,
                    fine_ns: 0,
                }))
            }
            Self::Output {
                stream,
                channels,
                hot,
                hot_enabled,
                ..
            } => {
                if *hot_enabled && hot.is_none() {
                    return Err("fine bank not configured".into());
                }
                let Some(item) = stream.take() else {
                    return Ok(None);
                };
                if (item.epoch, item.input_index) != (7, item.seq * N as u64)
                    || item.iq.len() != item.groups as usize * stream.bins().len() * 2
                {
                    return Err("PFB output epoch/shape mismatch".into());
                }
                let started_fine = Instant::now();
                let mut fine_bytes = 0;
                if let Some(hot) = hot {
                    fine_bytes = hot.deliver(&item, stream.bins())?;
                } else {
                    for channel in channels {
                        let result = channel.process(&item).map_err(str::to_owned)?;
                        fine_bytes += (result.iq.len() * 4) as u64;
                    }
                }
                Ok(Some(OutputRow {
                    seq: item.seq,
                    stamp: item.stamp,
                    groups: item.groups,
                    first_re: item.iq.first().copied().unwrap_or_default(),
                    bytes: (item.iq.len() * 4) as u64,
                    fine_bytes,
                    worker_ns: item.process_ns,
                    fine_ns: if fine_bytes > 0 {
                        started_fine.elapsed().as_nanos() as u64
                    } else {
                        0
                    },
                }))
            }
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        match self {
            Self::Metadata(pool) if unsafe { pfb_pool_finish(pool.0) } != 0 => {
                Err("pfb_pool_finish failed".into())
            }
            Self::Output { stream, .. } => stream.finish().map_err(str::to_owned),
            _ => Ok(()),
        }
    }

    fn finish_frames(&mut self) -> Result<(), String> {
        match self {
            Self::Output { hot: Some(hot), .. } => hot.finish_frames(),
            _ => Ok(()),
        }
    }

    fn stats(&self) -> Result<PoolStats, String> {
        match self {
            Self::Metadata(pool) => {
                let mut stats = PoolStats::default();
                if unsafe { pfb_pool_stats(pool.0, &mut stats) } != 0 {
                    Err("pfb_pool_stats failed".into())
                } else {
                    Ok(stats)
                }
            }
            Self::Output { stream, .. } => Ok(stream.stats()),
        }
    }
}

#[derive(Default)]
struct DeliveryStats {
    delivered: u64,
    groups: u64,
    checksum: f64,
    output_bytes: u64,
    fine_bytes: u64,
    worker_latency: Latency,
    fine_latency: Latency,
    spectrum_frames: u64,
    spectrum_missed: u64,
    clock_gaps: u64,
    clock_backwards: u64,
}

fn take(
    pool: &mut Engine,
    ready: &mut VecDeque<u64>,
    stats: &mut DeliveryStats,
) -> Result<bool, String> {
    let Some(row) = pool.take()? else {
        return Ok(false);
    };
    let stamp = ready.pop_front().ok_or("PFB result without input")?;
    if row.seq != stats.delivered || row.stamp != stamp {
        return Err("PFB order/clock mismatch".into());
    }
    stats.delivered += 1;
    stats.groups += row.groups as u64;
    stats.checksum += row.first_re as f64;
    stats.output_bytes += row.bytes;
    stats.fine_bytes += row.fine_bytes;
    stats.worker_latency.record(row.worker_ns);
    if row.fine_ns > 0 {
        stats.fine_latency.record(row.fine_ns);
    }
    Ok(true)
}

fn check_service_stopped() -> Result<(), String> {
    for entry in fs::read_dir("/proc").map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        if pid == std::process::id() { continue; }
        let Ok(executable) = fs::read_link(entry.path().join("exe")) else { continue };
        let name = executable.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let probe = matches!(name, "san90_iqs" | "san90_iqs (deleted)")
            && fs::read(entry.path().join("cmdline")).is_ok_and(|args| args.split(|byte| *byte == 0).any(|arg| arg == b"--hardware"));
        let service = matches!(name, "websad" | "websad (deleted)")
            && fs::read(entry.path().join("environ")).map_err(|error| format!("cannot inspect native service environment: {error}"))?
                .split(|byte| *byte == 0).any(|item| item.starts_with(b"WEBSA_RUST_IQS_SHIM=") && item.len() > b"WEBSA_RUST_IQS_SHIM=".len());
        if probe || service {
            return Err("stop the native hardware owner before exclusive hardware probe".into());
        }
    }
    Ok(())
}

fn parse_bins(spec: &str) -> Result<Vec<u32>, String> {
    let bins = if spec.contains(',') {
        spec.split(',')
            .map(|value| {
                value
                    .parse::<u32>()
                    .map_err(|_| "invalid output bin list".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let count = spec
            .parse::<u32>()
            .map_err(|_| "invalid output bin count".to_owned())?;
        (0..count).collect()
    };
    if bins.is_empty() || bins.len() > 128 || bins.iter().any(|&bin| bin >= 128) {
        return Err("output bins must be 1..128 distinct indices in 0..128".into());
    }
    let mut unique = bins.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != bins.len() {
        return Err("duplicate output bin".into());
    }
    Ok(bins)
}

fn advance_spectrum_deadline(next: &mut Instant, now: Instant) -> u64 {
    let period = Duration::from_millis(50);
    *next += period;
    let mut missed = 0;
    while *next <= now {
        *next += period;
        missed += 1;
    }
    missed
}

fn clock_lag_ms(first_host: Instant, now: Instant, first_stamp: u64, stamp: u64) -> f64 {
    (now.duration_since(first_host).as_secs_f64()
        - stamp.saturating_sub(first_stamp) as f64 / (2.0 * FS))
        * 1e3
}

fn run(
    fixture: bool,
    seconds: f64,
    outputs: Option<Vec<u32>>,
    fine: bool,
    hot: bool,
    fail_seq: Option<u64>,
    http_bind: Option<SocketAddr>,
) -> Result<(), String> {
    if http_bind.is_some() && !hot {
        return Err("control HTTP requires --hot".into());
    }
    if hot && (!fine || outputs.is_none()) {
        return Err("--hot requires --outputs --fine".into());
    }
    // Hardware hot mode only takes external commands: no scripted retunes or fault injection.
    if hot && !fixture && http_bind.is_none() {
        return Err("hardware --hot requires WEBSA_RUST_CONTROL_HTTP".into());
    }
    if fail_seq.is_some() && !fixture {
        return Err("--fail-consumer is fixture only".into());
    }
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("seconds must be positive".into());
    }
    let target = (FS * seconds / N as f64).round() as usize;
    if target == 0 {
        return Err("requested duration shorter than one packet".into());
    }
    if fail_seq.is_some_and(|seq| !hot || seq >= target as u64) {
        return Err("--fail-consumer sequence outside hot fixture".into());
    }
    if !fixture {
        check_service_stopped()?;
    }
    let bin_count = outputs.as_ref().map_or(0, Vec::len);
    let mut pool = Engine::new(outputs, fine, hot, fail_seq)?;
    let sdk = if fixture {
        None
    } else {
        let path = env::var("WEBSA_SAN90_SHIM").or_else(|_| {
            env::var("HOME").map(|home| format!("{home}/.local/share/harogic-websa/shims/libwebsa_san90_sdk.so"))
        }).map_err(|_| "set WEBSA_SAN90_SHIM or HOME to locate the SAN-90 probe shim".to_owned())?;
        Some(Sdk::load(&path)?)
    };
    let entry = sdk.as_ref().map(|s| (s.open, s.arm, s.next, s.close));
    let (sender, receiver) = sync_channel::<Packet>(CAPACITY);
    let (meta_tx, meta_rx) = sync_channel(1);
    let (start_tx, start_rx) = sync_channel(1);
    let (control_tx, control_rx) = control_channel();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(2048);
    let mut spectrum = vec![Complex::<f32>::default(); 2048];
    let launched = Instant::now();
    let reader = thread::spawn(move || capture(fixture, target, entry, sender, Some(meta_tx), Some(start_rx)));
    let config = match meta_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(value) => value,
        Err(error) => {
            drop(start_tx);
            drop(receiver);
            let captured = reader.join().map_err(|_| "capture thread panicked")?;
            return Err(format!(
                "capture setup: {}",
                captured.error.unwrap_or_else(|| error.to_string())
            ));
        }
    };
    let frame_hub = http_bind.map(|_| FrameHub::new(bin_count));
    if let Err(error) =
        pool.configure_bandwidth(config, control_rx, http_bind.is_none(), frame_hub.clone())
    {
        drop(start_tx);
        drop(receiver);
        let captured = reader.join().map_err(|_| "capture thread panicked")?;
        return Err(format!(
            "channel configuration: {error}; capture: {:?}",
            captured.error
        ));
    }
    let http = if let Some(addr) = http_bind {
        let bins = match &pool {
            Engine::Output { stream, .. } => stream.bins().to_vec(),
            Engine::Metadata(_) => Vec::new(),
        };
        match HttpControl::start(
            addr,
            control_tx.clone(),
            7,
            bins.clone(),
            config.bandwidth_hz,
            config.center_hz,
            FS,
            FS / 64.0 * bins.len() as f64 / 12.0,
            frame_hub.clone().unwrap(),
        ) {
            Ok(http) => Some(http),
            Err(error) => {
                drop(start_tx);
                drop(receiver);
                let captured = reader.join().map_err(|_| "capture thread panicked")?;
                return Err(format!(
                    "control HTTP: {error}; capture: {:?}",
                    captured.error
                ));
            }
        }
    } else {
        None
    };
    if start_tx.send(()).is_err() {
        drop(receiver);
        let captured = reader.join().map_err(|_| "capture thread panicked")?;
        return Err(format!("capture start: {:?}", captured.error));
    }
    if let Some(ref http) = http {
        println!(
            "prototype_control_http=http://{}/prototype/channels",
            http.addr()
        );
    }
    let _control_tx = control_tx;
    let mut delivery = DeliveryStats::default();
    let mut checkpoints = Vec::with_capacity(4);
    let result = (|| -> Result<_, String> {
        let mut next_fft = Instant::now();
        let mut frames = 0;
        let mut missed_spectrum_frames = 0u64;
        let mut last_stamp = 0u64;
        let mut first_stamp = 0u64;
        let mut gaps = 0;
        let mut backwards = 0;
        let mut ready = VecDeque::with_capacity(3);
        let mut start = None;
        let mut startup_s = 0.0;
        let mut first_host = None;
        let mut last_host = None;
        for seq in 0..target {
            let packet = receiver
                .recv_timeout(Duration::from_secs(2))
                .map_err(|e| format!("capture stalled: {e}"))?;
            first_host.get_or_insert(packet.received);
            last_host = Some(packet.received);
            if seq == 0 {
                first_stamp = packet.stamp;
                let now = Instant::now();
                startup_s = now.duration_since(launched).as_secs_f64();
                start = Some(now);
                next_fft = now;
            }
            if last_stamp != 0 {
                if packet.stamp <= last_stamp {
                    backwards += 1;
                    delivery.clock_backwards += 1;
                }
                if packet.stamp > last_stamp + (N * 3) as u64 {
                    gaps += 1;
                    delivery.clock_gaps += 1;
                }
            }
            last_stamp = packet.stamp;
            if checkpoints.len() < 4 && seq + 1 >= target * (checkpoints.len() + 1) / 4 {
                checkpoints.push((
                    seq + 1,
                    clock_lag_ms(
                        first_host.unwrap(),
                        packet.received,
                        first_stamp,
                        packet.stamp,
                    ),
                    Instant::now().duration_since(packet.received).as_secs_f64() * 1e3,
                    pool.stats()?.result_wait_ns as f64 / 1e6,
                ));
            }
            let now = Instant::now();
            if now >= next_fft {
                missed_spectrum_frames += advance_spectrum_deadline(&mut next_fft, now);
                delivery.spectrum_missed = missed_spectrum_frames;
                for (i, x) in spectrum.iter_mut().enumerate() {
                    let window = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / 2047.0).cos();
                    x.re = packet.iq[2 * (N - 2048 + i)] as f32 * window;
                    x.im = packet.iq[2 * (N - 2048 + i) + 1] as f32 * window;
                }
                fft.process(&mut spectrum);
                frames += 1;
                delivery.spectrum_frames = frames;
            }
            ready.push_back(packet.stamp);
            if ready.len() > 5 {
                return Err("PFB metadata exceeded five entries".into());
            }
            loop {
                let status = pool.submit(seq as u64, packet.stamp, &packet.iq);
                if status == 0 {
                    break;
                }
                if status != -2 || !take(&mut pool, &mut ready, &mut delivery)? {
                    return Err(format!("pfb_pool_submit status={status}"));
                }
            }
            while take(&mut pool, &mut ready, &mut delivery)? {}
        }
        pool.finish()?;
        while take(&mut pool, &mut ready, &mut delivery)? {}
        pool.finish_frames()?;
        let elapsed = start.unwrap().elapsed().as_secs_f64();
        if delivery.delivered != target as u64 || !ready.is_empty() {
            return Err("PFB did not deliver every packet".into());
        }
        let stats = pool.stats()?;
        if stats.submitted != target as u64
            || stats.completed != target as u64
            || stats.groups != delivery.groups
            || stats.task_capacity != MAX_IN_FLIGHT as u32
            || stats.result_capacity != MAX_IN_FLIGHT as u32
            || stats.max_ready > MAX_IN_FLIGHT as u32
            || stats.max_pending > MAX_IN_FLIGHT as u32
        {
            return Err("PFB boundedness/count mismatch".into());
        }
        Ok((
            elapsed,
            startup_s,
            first_host.unwrap(),
            last_host.unwrap(),
            first_stamp,
            last_stamp,
            frames,
            missed_spectrum_frames,
            gaps,
            backwards,
            stats,
        ))
    })();
    drop(receiver); // On failure unblock capture's bounded send before joining/closing SDK.
    let captured = reader.join().map_err(|_| "capture thread panicked")?;
    if result.is_err() || captured.error.is_some() {
        eprintln!("partial: error={:?} usable_bw_hz={:.0} capture_center_hz={:.0} fetched={} fetch_attempts={} fetch_total_p99_upper_ms={:.3} fetch_total_max_ms={:.3} sdk_call_p99_upper_ms={:.3} sdk_call_max_ms={:.3} idle_p99_upper_ms={:.3} idle_max_ms={:.3} capture_quarters_fetched_fetch_sdk_idle_wait_ms={:?} queue_full={} queue_wait_s={:.3} queue_wait_max_ms={:.3} delivered={} worker_p99_upper_ms={:.3} fine_p99_upper_ms={:.3} spectrum_frames={} spectrum_missed={} clock_gaps={} clock_backwards={} clock_lag_ms_by_quarter={checkpoints:?} capture_error={:?}",
            result.as_ref().err(), captured.stats.usable_bandwidth_hz, captured.stats.center_hz, captured.stats.fetched, captured.stats.fetch_latency.count,
            captured.stats.fetch_latency.p99_upper_ms(), captured.stats.fetch_max.as_secs_f64()*1e3,
            captured.stats.sdk_latency.p99_upper_ms(), captured.stats.sdk_max.as_secs_f64()*1e3,
            captured.stats.idle_latency.p99_upper_ms(), captured.stats.idle_max.as_secs_f64()*1e3,
            capture_quarters(&captured.stats),
            captured.stats.full, captured.stats.wait.as_secs_f64(),
            captured.stats.wait_max.as_secs_f64()*1e3, delivery.delivered,
            delivery.worker_latency.p99_upper_ms(), delivery.fine_latency.p99_upper_ms(),
            delivery.spectrum_frames, delivery.spectrum_missed,
            delivery.clock_gaps, delivery.clock_backwards,
            captured.error);
        if let Engine::Output {
            hot: Some(state), ..
        } = &pool
        {
            eprintln!(
                "partial hot: planned_bins={} retuned={} fail_seq={:?} iq_frames_queued={}",
                state.next_bin, state.retuned, state.fail_seq, state.wire_frames
            );
        }
        if let Ok(pool_stats) = pool.stats() {
            eprintln!(
                "partial pool: submitted={} completed={} pending_max={} ready_max={}",
                pool_stats.submitted,
                pool_stats.completed,
                pool_stats.max_pending,
                pool_stats.max_ready
            );
        }
    }
    let (
        elapsed,
        startup_s,
        first_host,
        last_host,
        first_stamp,
        last_stamp,
        frames,
        missed_spectrum_frames,
        gaps,
        backwards,
        stats,
    ) = result.map_err(|error: String| match captured.error.as_deref() {
        Some(cause) => format!("{error}; capture: {cause}"),
        None => error,
    })?;
    if let Some(error) = captured.error {
        return Err(format!("capture: {error}"));
    }
    let captured = captured.stats;
    let DeliveryStats {
        groups,
        checksum,
        output_bytes,
        fine_bytes,
        worker_latency,
        fine_latency,
        ..
    } = delivery;
    let host_window = last_host.duration_since(first_host).as_secs_f64() + N as f64 / FS;
    let device_window = (last_stamp - first_stamp) as f64 / (2.0 * FS) + N as f64 / FS;
    let rate = target as f64 * N as f64 / elapsed / 1e6;
    println!(
        "mode={} bins={} usable_bw_hz={:.0} capture_center_hz={:.0} fine={fine} hot={hot} iq_frames_queued={} packets={target} processed_msps={rate:.4} startup_s={startup_s:.4} capture_host_msps={:.4} \
        device_msps={:.4} spectrum_fps={:.4} spectrum_missed={missed_spectrum_frames} groups={groups} checksum={checksum:.3} output_mb={:.1} fine_mb={:.1} \
        clock_gaps={gaps} clock_backwards={backwards} checkpoints_seq_host_device_ms_consumer_ms_result_wait_ms={checkpoints:?} queue_capacity={CAPACITY} \
        queue_full={} queue_blocked={} queue_wait_s={:.4} queue_wait_max_ms={:.4} capture_quarters_fetched_fetch_sdk_idle_wait_ms={:?} idle_p99_upper_ms={:.4} idle_max_ms={:.4} fetch_total_mean_ms={:.4} \
        fetch_total_p99_upper_ms={:.4} fetch_total_max_ms={:.4} sdk_call_p99_upper_ms={:.4} sdk_call_max_ms={:.4} worker_p99_upper_ms={:.4} fine_p99_upper_ms={:.4} history_mean_ms={:.4} clone_mean_ms={:.4} task_wait_mean_ms={:.4} result_wait_mean_ms={:.4} pool_task_capacity={} pool_result_capacity={}",
        if fixture { "fixture" } else { "hardware" },
        bin_count,
        captured.usable_bandwidth_hz,
        captured.center_hz,
        match &pool {
            Engine::Output { hot: Some(state), .. } => state.wire_frames,
            _ => 0,
        },
        target as f64 * N as f64 / host_window / 1e6,
        target as f64 * N as f64 / device_window / 1e6,
        frames as f64 / elapsed,
        output_bytes as f64 / 1e6,
        fine_bytes as f64 / 1e6,
        captured.full,
        captured.blocked,
        captured.wait.as_secs_f64(),
        captured.wait_max.as_secs_f64() * 1e3,
        capture_quarters(&captured),
        captured.idle_latency.p99_upper_ms(),
        captured.idle_max.as_secs_f64() * 1e3,
        captured.fetch.as_secs_f64() * 1e3 / target as f64,
        captured.fetch_latency.p99_upper_ms(),
        captured.fetch_max.as_secs_f64() * 1e3,
        captured.sdk_latency.p99_upper_ms(),
        captured.sdk_max.as_secs_f64() * 1e3,
        worker_latency.p99_upper_ms(),
        fine_latency.p99_upper_ms(),
        stats.advance_ns as f64 / target as f64 / 1e6,
        stats.clone_ns as f64 / target as f64 / 1e6,
        stats.task_wait_ns as f64 / target as f64 / 1e6,
        stats.result_wait_ns as f64 / target as f64 / 1e6,
        stats.task_capacity,
        stats.result_capacity
    );
    if gaps != 0
        || backwards != 0
        || missed_spectrum_frames != 0
        || captured.blocked != 0
        || (seconds >= 1.0 && (rate < FS / 1e6 * 0.995 || (frames as f64 + 1.0) < elapsed * 20.0))
    {
        return Err("capture/PFB did not sustain realtime".into());
    }
    Ok(())
}

fn main() {
    let args: Vec<_> = env::args().collect();
    if args.len() < 2
        || args.len() == 8
        || args.len() > 9
        || !["--fixture", "--hardware"].contains(&args[1].as_str())
        || (args.len() >= 4 && args[3] != "--outputs")
        || (args.len() >= 6 && args[5] != "--fine")
        || (args.len() >= 7 && args[6] != "--hot")
        || (args.len() == 9 && (args[1] != "--fixture" || args[7] != "--fail-consumer"))
    {
        eprintln!(
            "usage: san90_iqs --fixture|--hardware [seconds] [--outputs [count|bin,bin,...] [--fine [--hot (hardware: WEBSA_RUST_CONTROL_HTTP) [--fail-consumer seq (fixture only)]]]]"
        );
        std::process::exit(2);
    }
    let seconds = args.get(2).map_or(Ok(3.0), |value| value.parse::<f64>());
    let outputs = if args.len() >= 4 {
        Some(parse_bins(args.get(4).map_or("32", String::as_str)))
    } else {
        None
    };
    let fail_seq = (args.len() == 9).then(|| {
        args[8]
            .parse::<u64>()
            .map_err(|_| "invalid --fail-consumer sequence".to_owned())
    });
    let http_bind = env::var("WEBSA_RUST_CONTROL_HTTP").ok().map(|value| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| "invalid WEBSA_RUST_CONTROL_HTTP address".to_owned())
    });
    let result = seconds.map_err(|e| e.to_string()).and_then(|s| {
        outputs.transpose().and_then(|bins| {
            fail_seq.transpose().and_then(|fault| {
                http_bind.transpose().and_then(|bind| {
                    run(
                        args[1] == "--fixture",
                        s,
                        bins,
                        args.len() >= 6,
                        args.len() >= 7,
                        fault,
                        bind,
                    )
                })
            })
        })
    });
    if let Err(e) = result {
        eprintln!("probe failed: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config(bandwidth_hz: f64) -> CaptureConfig {
        CaptureConfig {
            bandwidth_hz,
            center_hz: FIXTURE_CENTER_HZ,
        }
    }
    #[test]
    fn fixture_has_contiguous_clock_and_bounded_packets() {
        run(true, 0.02, None, false, false, None, None).unwrap();
        run(
            true,
            0.02,
            Some(vec![0, 5, 31, 127]),
            false,
            false,
            None,
            None,
        )
        .unwrap();
        run(
            true,
            0.02,
            Some(vec![0, 5, 31, 127]),
            true,
            false,
            None,
            None,
        )
        .unwrap();
        run(
            true,
            0.02,
            Some(vec![0, 5, 31, 127]),
            true,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(run(false, 0.02, Some(vec![0]), true, true, Some(8), None).is_err());
        assert_eq!(
            run(false, 0.02, Some(vec![0]), true, true, None, None).unwrap_err(),
            "hardware --hot requires WEBSA_RUST_CONTROL_HTTP"
        );
        assert!(run(true, 0.02, Some(vec![0]), true, true, Some(1000), None).is_err());
        assert!(run(
            false,
            0.02,
            Some(vec![0]),
            true,
            false,
            None,
            Some("127.0.0.1:0".parse().unwrap())
        )
        .is_err());
    }

    #[test]
    fn fixture_hot_schedule_fills_budget_and_retunes_without_id_reuse() {
        let bins = vec![
            0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 80, 85, 90, 95, 100, 127,
        ];
        let mut engine = Engine::new(Some(bins.clone()), true, true, None).unwrap();
        let (_, invalid_rx) = control_channel();
        assert!(engine
            .configure_bandwidth(fixture_config(49_999_999.0), invalid_rx, true, None)
            .is_err());
        let (_control_tx, control_rx) = control_channel();
        engine
            .configure_bandwidth(fixture_config(50_000_000.0), control_rx, true, None)
            .unwrap();
        let Engine::Output { hot: Some(hot), .. } = &mut engine else {
            panic!("missing hot bank");
        };
        for (seq, expected) in [4, 8, 12, 16].into_iter().enumerate() {
            let packet = websa_channelizer::PfbOutput {
                seq: seq as u64,
                epoch: 7,
                stamp: seq as u64 * 32480,
                input_index: seq as u64 * N as u64,
                groups: 0,
                process_ns: 0,
                iq: vec![],
            };
            assert_eq!(hot.deliver(&packet, &bins), Ok(0));
            assert_eq!(hot.next_bin, expected);
        }
        assert!(hot.retuned);
        assert!(!hot.bank.unsubscribe(1));
        assert_eq!(
            hot.bank.subscribe(18, bins[0], 0.0, 12),
            Err("fine channel budget exceeded")
        );
    }

    #[test]
    fn bounded_control_rejects_bad_epoch_and_acks_ordered_start() {
        let bins = vec![0, 5, 10, 15];
        let mut engine = Engine::new(Some(bins.clone()), true, true, None).unwrap();
        let (control_tx, control_rx) = control_channel();
        engine
            .configure_bandwidth(fixture_config(50_000_000.0), control_rx, true, None)
            .unwrap();
        let Engine::Output { hot: Some(hot), .. } = &mut engine else {
            panic!("missing hot bank");
        };
        let packet = |seq| websa_channelizer::PfbOutput {
            seq,
            epoch: 7,
            stamp: seq * 32480,
            input_index: seq * N as u64,
            groups: 0,
            process_ns: 0,
            iq: vec![],
        };
        for seq in 0..4 {
            hot.deliver(&packet(seq), &bins).unwrap();
        }
        let (bad_tx, bad_rx) = sync_channel(1);
        let (ok_tx, ok_rx) = sync_channel(1);
        control_tx
            .try_send(ControlRequest {
                command: ChannelCommand {
                    epoch: 6,
                    action: ChannelAction::Unsubscribe { id: 2 },
                },
                reply: bad_tx,
            })
            .unwrap();
        control_tx
            .try_send(ControlRequest {
                command: ChannelCommand {
                    epoch: 7,
                    action: ChannelAction::Unsubscribe { id: 2 },
                },
                reply: ok_tx,
            })
            .unwrap();
        let (overflow_tx, _overflow_rx) = sync_channel(1);
        assert!(matches!(
            control_tx.try_send(ControlRequest {
                command: ChannelCommand {
                    epoch: 7,
                    action: ChannelAction::Unsubscribe { id: 3 }
                },
                reply: overflow_tx,
            }),
            Err(TrySendError::Full(_))
        ));
        hot.deliver(&packet(4), &bins).unwrap();
        assert_eq!(
            bad_rx.recv().unwrap(),
            Err("fine bank command epoch mismatch")
        );
        assert_eq!(ok_rx.recv().unwrap(), Ok(4));
        let (add_tx, add_rx) = sync_channel(1);
        control_tx
            .try_send(ControlRequest {
                command: ChannelCommand {
                    epoch: 7,
                    action: ChannelAction::Subscribe {
                        id: 6,
                        bin: 5,
                        offset_hz: 0.0,
                        decimate: 12,
                    },
                },
                reply: add_tx,
            })
            .unwrap();
        hot.deliver(&packet(5), &bins).unwrap();
        assert_eq!(add_rx.recv().unwrap(), Ok(5));
        assert!(hot.bank.unsubscribe(6));
    }

    #[test]
    fn external_control_mode_does_not_run_scripted_subscriptions() {
        let bins = vec![5];
        let mut engine = Engine::new(Some(bins.clone()), true, true, None).unwrap();
        let (tx, rx) = control_channel();
        engine
            .configure_bandwidth(fixture_config(50_000_000.0), rx, false, None)
            .unwrap();
        let Engine::Output { hot: Some(hot), .. } = &mut engine else {
            panic!("missing bank")
        };
        let packet = |seq| websa_channelizer::PfbOutput {
            seq,
            epoch: 7,
            stamp: seq * 32480,
            input_index: seq * N as u64,
            groups: 0,
            process_ns: 0,
            iq: vec![],
        };
        assert_eq!(hot.deliver(&packet(0), &bins), Ok(0));
        assert_eq!(hot.next_bin, 0);
        let (reply, answer) = sync_channel(1);
        tx.try_send(ControlRequest {
            command: ChannelCommand {
                epoch: 7,
                action: ChannelAction::Subscribe {
                    id: 1,
                    bin: 5,
                    offset_hz: 0.0,
                    decimate: 12,
                },
            },
            reply,
        })
        .unwrap();
        assert_eq!(hot.deliver(&packet(1), &bins), Ok(0));
        assert_eq!(answer.recv().unwrap(), Ok(1));
        assert!(hot.bank.unsubscribe(1));
    }

    #[test]
    fn fixture_http_control_replies_and_rejects_unsafe_requests() {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let (control_tx, control_rx) = control_channel();
        assert!(HttpControl::start(
            "0.0.0.0:0".parse().unwrap(),
            control_tx.clone(),
            7,
            vec![5],
            50_000_000.0,
            100_200_000.0,
            FS,
            FS / 64.0 / 12.0,
            FrameHub::default(),
        )
        .is_err());
        let http = HttpControl::start(
            "127.0.0.1:0".parse().unwrap(),
            control_tx.clone(),
            7,
            vec![5],
            50_000_000.0,
            100_200_000.0,
            FS,
            FS / 64.0 / 12.0,
            FrameHub::default(),
        )
        .unwrap();
        let addr = http.addr();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(
            stream,
            "GET /prototype/channels HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut plan = String::new();
        stream.read_to_string(&mut plan).unwrap();
        assert!(plan.starts_with("HTTP/1.1 200"), "{plan}");
        assert!(plan.contains("\"epoch\":7") && plan.contains("\"bins\":[5]"));
        assert!(plan.contains("\"usable_bandwidth_hz\":50000000.0"));
        assert!(plan.contains("\"capture_center_hz\":100200000.0"));
        assert!(plan.contains("\"iq_sample_rate_hz\":62500000.0"));
        let request = move |body: &'static str, origin: bool| {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let origin_header = if origin {
                "Origin: http://attacker.invalid\r\n"
            } else {
                ""
            };
            write!(stream, "POST /prototype/channels HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{origin_header}\r\n{body}", body.len()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let body = r#"{"op":"subscribe","epoch":7,"id":18,"bin":5,"offset_hz":0.0,"decimate":12}"#;
        let client = std::thread::spawn(move || request(body, false));
        let accepted = control_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            accepted.command.action,
            ChannelAction::Subscribe { id: 18, .. }
        ));
        accepted.reply.send(Ok(42)).unwrap();
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"seq\":42"), "{response}");
        assert!(
            response.contains("\"center_hz\":102641406.25"),
            "{response}"
        );
        assert!(
            response.contains("\"sample_rate_hz\":81380.20833333333"),
            "{response}"
        );

        let client = std::thread::spawn(move || request(body, false));
        control_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .reply
            .send(Err("fine bank command epoch mismatch"))
            .unwrap();
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(response.contains("fine bank command epoch mismatch"));
        assert!(!response.contains("center_hz"));
        let response = request(body, true);
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(control_rx.try_recv().is_err());

        for id in 0..2 {
            let (reply, _response) = sync_channel(1);
            control_tx
                .try_send(ControlRequest {
                    command: ChannelCommand {
                        epoch: 7,
                        action: ChannelAction::Unsubscribe { id },
                    },
                    reply,
                })
                .unwrap();
        }
        let response = request(body, false);
        assert!(response.starts_with("HTTP/1.1 429"), "{response}");
        assert!(response.contains("control queue full"));
        drop(http);
    }

    #[test]
    fn hot_fixture_consumer_fault_stops_without_recovering_dropped_packet() {
        let err = run(
            true,
            0.02,
            Some(vec![0, 5, 31, 127]),
            true,
            true,
            Some(8),
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("hot seq=8 bins=4 fine consumer over budget"),
            "{err}"
        );
    }

    #[test]
    fn spectrum_deadlines_stay_on_20hz_clock_and_count_misses() {
        let start = Instant::now();
        let mut due = start;
        assert_eq!(advance_spectrum_deadline(&mut due, start), 0);
        assert_eq!(due, start + Duration::from_millis(50));
        assert_eq!(
            advance_spectrum_deadline(&mut due, start + Duration::from_millis(51)),
            0
        );
        assert_eq!(due, start + Duration::from_millis(100));
        assert_eq!(
            advance_spectrum_deadline(&mut due, start + Duration::from_millis(216)),
            2
        );
        assert_eq!(due, start + Duration::from_millis(250));
    }

    #[test]
    fn latency_histogram_reports_bounded_p99_and_exact_max() {
        let mut latency = Latency::default();
        assert_eq!(latency.p99_upper_ms(), 0.0);
        for _ in 0..99 {
            latency.record(25_000);
        }
        latency.record(5_000_000);
        assert!((latency.p99_upper_ms() - 0.04).abs() < 1e-12);
        assert_eq!(latency.max_ns, 5_000_000);
    }

    #[test]
    fn clock_lag_uses_same_host_and_device_packet_window() {
        let first = Instant::now();
        assert!(
            (clock_lag_ms(
                first,
                first + Duration::from_millis(5001),
                1000,
                1000 + 125_000_000 * 5
            ) - 1.0)
                .abs()
                < 1e-8
        );
    }

    #[test]
    fn sdk_open_bandwidth_is_checked_and_closes_invalid_handles() {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
        static BANDWIDTH: AtomicU64 = AtomicU64::new(0);
        static CENTER: AtomicU64 = AtomicU64::new(0);
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn open(
            handle: *mut *mut c_void,
            _: f64,
            _: *const FrontEnd,
            _: c_int,
            bw: *mut f64,
            center: *mut f64,
        ) -> i32 {
            *handle = 1usize as *mut c_void;
            *bw = f64::from_bits(BANDWIDTH.load(Ordering::SeqCst));
            *center = f64::from_bits(CENTER.load(Ordering::SeqCst));
            0
        }
        unsafe extern "C" fn arm(_: *mut c_void) -> i32 {
            0
        }
        unsafe extern "C" fn next(_: *mut c_void, _: *mut i16, _: u32, _: *mut PacketInfo) -> i32 {
            -10
        }
        unsafe extern "C" fn close(_: *mut c_void) -> i32 {
            CLOSES.fetch_add(1, Ordering::SeqCst);
            0
        }
        CLOSES.store(0, Ordering::SeqCst);
        BANDWIDTH.store(50_000_000.0f64.to_bits(), Ordering::SeqCst);
        CENTER.store(100_010_000.0f64.to_bits(), Ordering::SeqCst);
        let device = Device::open(open, next, close).unwrap();
        assert_eq!(device.usable_bandwidth, 50_000_000.0);
        assert_eq!(device.center_hz, 100_010_000.0);
        drop(device);
        assert_eq!(CLOSES.load(Ordering::SeqCst), 1);
        for (i, invalid) in [49_999_999.0, FS + 1.0, f64::NAN].into_iter().enumerate() {
            BANDWIDTH.store(invalid.to_bits(), Ordering::SeqCst);
            assert_eq!(
                Device::open(open, next, close).err().as_deref(),
                Some("unexpected SDK IQ bandwidth")
            );
            assert_eq!(CLOSES.load(Ordering::SeqCst), i + 2);
        }
        BANDWIDTH.store(50_000_000.0f64.to_bits(), Ordering::SeqCst);
        for (i, center) in [0.0, 9_020_000_001.0, f64::NAN].into_iter().enumerate() {
            CENTER.store(center.to_bits(), Ordering::SeqCst);
            assert_eq!(
                Device::open(open, next, close).err().as_deref(),
                Some("unexpected SDK IQ center")
            );
            assert_eq!(CLOSES.load(Ordering::SeqCst), i + 5);
        }
        CENTER.store(f64::NAN.to_bits(), Ordering::SeqCst);
        let (packet_tx, _packet_rx) = sync_channel(2);
        let (meta_tx, meta_rx) = sync_channel(1);
        let failed = capture(
            false,
            4,
            Some((open, arm, next, close)),
            packet_tx,
            Some(meta_tx),
            None,
        );
        assert_eq!(failed.error.as_deref(), Some("unexpected SDK IQ center"));
        assert!(meta_rx.recv().is_err());
        assert_eq!(CLOSES.load(Ordering::SeqCst), 8);
    }

    #[test]
    fn sdk_and_consumer_faults_preserve_partial_capture_metrics() {
        unsafe extern "C" fn open(
            handle: *mut *mut c_void,
            _: f64,
            _: *const FrontEnd,
            _: c_int,
            bw: *mut f64,
            center: *mut f64,
        ) -> i32 {
            *handle = 1usize as *mut c_void;
            *bw = 50_000_000.0;
            *center = 100_200_000.0;
            0
        }
        unsafe extern "C" fn arm(_: *mut c_void) -> i32 {
            0
        }
        unsafe extern "C" fn next(_: *mut c_void, _: *mut i16, _: u32, _: *mut PacketInfo) -> i32 {
            -10
        }
        unsafe extern "C" fn close(_: *mut c_void) -> i32 {
            0
        }
        let (sender, _receiver) = sync_channel(2);
        let (meta_tx, meta_rx) = sync_channel(1);
        let failure = capture(false, 4, Some((open, arm, next, close)), sender, Some(meta_tx), None);
        let metadata = meta_rx.recv().unwrap();
        assert_eq!(
            (metadata.bandwidth_hz, metadata.center_hz),
            (50_000_000.0, 100_200_000.0)
        );
        assert_eq!(failure.error.as_deref(), Some("san90_next status=-10"));
        assert_eq!(failure.stats.fetched, 0);
        assert_eq!(failure.stats.fetch_latency.count, 1);
        assert_eq!(failure.stats.sdk_latency.count, 1);
        assert!(failure.stats.quarters[0].fetch > Duration::ZERO);
        assert!(failure.stats.quarters[0].sdk_call > Duration::ZERO);
        assert!(failure.stats.fetch_max > Duration::ZERO);

        let (sender, receiver) = sync_channel(2);
        let (meta_tx, meta_rx) = sync_channel(1);
        drop(receiver);
        let failure = capture(true, 4, None, sender, Some(meta_tx), None);
        let metadata = meta_rx.recv().unwrap();
        assert_eq!(
            (metadata.bandwidth_hz, metadata.center_hz),
            (FS * 0.8, FIXTURE_CENTER_HZ)
        );
        assert_eq!(failure.error.as_deref(), Some("capture consumer closed"));
        assert_eq!(failure.stats.fetched, 1);
        assert_eq!(failure.stats.fetch_latency.count, 1);
        assert_eq!(failure.stats.sdk_latency.count, 0);
        assert_eq!(capture_quarters(&failure.stats)[0].0, 1);
    }

    #[test]
    fn rejects_invalid_bin_lists_before_opening_device() {
        assert_eq!(parse_bins("4").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_bins("127,5,0").unwrap(), vec![127, 5, 0]);
        for invalid in ["", "0", "129", "0,0", "0,128", "1,", "-1", "1,x"] {
            assert!(parse_bins(invalid).is_err(), "{invalid}");
        }
    }
}
