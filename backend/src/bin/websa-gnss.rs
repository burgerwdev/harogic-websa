//! Native CF32LE transport and protobuf monitor bridge to the separate GNSS-SDR
//! executable. GNSS-SDR owns acquisition, tracking, navigation decoding and PVT.
//! No receiver implementation is linked or copied into this binary.
#[path = "websa_gnss/schema.rs"]
mod schema;

use prost::Message;
use schema::{GnssSynchro, MonitorPvt, NavMessage, Observables};
use serde_json::{json, Value};
use std::env;
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::UdpSocket;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const RATE: u64 = 4_000_000;
const CENTER: u64 = 1_575_420_000;
const STALE: Duration = Duration::from_secs(2);
const DRAIN: Duration = Duration::from_secs(3);
const STALL: Duration = Duration::from_secs(30);
const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;
const POLLHUP: i16 = 16;
const O_NONBLOCK: i32 = 0x800;
static STOP: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct PollFd { fd: i32, events: i16, revents: i16 }
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
    fn mkfifo(path: *const i8, mode: u32) -> i32;
    fn mkdtemp(template: *mut i8) -> *mut i8;
    fn prctl(option: i32, ...) -> i32;
    fn getppid() -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn signal(number: i32, handler: usize) -> usize;
}
extern "C" fn stop_signal(_: i32) { STOP.store(true, Ordering::Relaxed); }

struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new() -> io::Result<Self> {
        let mut template = b"/tmp/websa-gnss-XXXXXX\0".to_vec();
        if unsafe { mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        Ok(Self(PathBuf::from(OsStr::from_bytes(&template))))
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}
struct ReceiverChild(Child);
impl Drop for ReceiverChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// GNSS logging is continuously drained into a fixed tail, never retained in full.
struct Diagnostics {
    stream: UnixStream,
    tail: [u8; 8192],
    len: usize,
}
impl Diagnostics {
    fn drain(&mut self) -> io::Result<()> {
        let mut buf = [0u8; 4096];
        for _ in 0..32 {
            match self.stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let keep = self.len.min(self.tail.len() - n);
                    self.tail.copy_within(self.len - keep..self.len, 0);
                    self.tail[keep..keep + n].copy_from_slice(&buf[..n]);
                    self.len = keep + n;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn text(&self) -> String { String::from_utf8_lossy(&self.tail[..self.len]).into_owned() }
}

fn spawn_receiver(executable: &Path, args: &[&OsStr], cwd: Option<&Path>) -> io::Result<(ReceiverChild, Diagnostics)> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    let stderr: OwnedFd = writer.try_clone()?.into();
    let stdout: OwnedFd = writer.into();
    let mut command = Command::new(executable);
    command.args(args).env("GLOG_logtostderr", "1")
        .stdin(Stdio::null()).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr));
    if let Some(path) = cwd { command.current_dir(path); }
    let expected_parent = std::process::id() as i32;
    // Only async-signal-safe libc operations execute between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if prctl(1, 9, 0, 0, 0) != 0 { return Err(io::Error::last_os_error()); }
            if getppid() != expected_parent { return Err(io::Error::from_raw_os_error(10)); }
            Ok(())
        });
    }
    Ok((ReceiverChild(command.spawn()?), Diagnostics { stream: reader, tail: [0; 8192], len: 0 }))
}

fn executable() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("WEBSA_GNSS_SDR_PATH") {
        if path.is_empty() { return Err(io::Error::other("WEBSA_GNSS_SDR_PATH is empty")); }
        return fs::canonicalize(path);
    }
    if let Ok(own) = env::current_exe() {
        if let Some(parent) = own.parent() {
            let sibling = parent.join("gnss-sdr");
            if sibling.is_file() { return fs::canonicalize(sibling); }
        }
    }
    for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join("gnss-sdr");
        if path.is_file() { return fs::canonicalize(path); }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "GNSS-SDR not found: set WEBSA_GNSS_SDR_PATH or install sibling/PATH gnss-sdr"))
}

fn poll_fds(fds: &mut [PollFd], timeout: i32) -> io::Result<()> {
    if unsafe { poll(fds.as_mut_ptr(), fds.len(), timeout) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
    }
    Ok(())
}

fn probe(executable: &Path) -> io::Result<String> {
    let (mut child, mut diagnostics) = spawn_receiver(executable, &[OsStr::new("--version")], None)?;
    let start = Instant::now();
    loop {
        diagnostics.drain()?;
        if let Some(status) = child.0.try_wait()? {
            diagnostics.drain()?;
            let text = diagnostics.text();
            let has_version = text.split_whitespace().any(|s| {
                let token = s.trim_start_matches('v');
                token.as_bytes().first().is_some_and(u8::is_ascii_digit) && token.contains('.')
            });
            if !status.success() || !text.to_ascii_lowercase().contains("gnss-sdr") || !has_version {
                return Err(io::Error::other(format!("GNSS-SDR --version failed ({status}): {}", text.trim())));
            }
            return Ok(text.trim().to_owned());
        }
        if start.elapsed() >= Duration::from_secs(5) || STOP.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "GNSS-SDR --version did not finish within 5 seconds"));
        }
        poll_fds(&mut [PollFd { fd: diagnostics.stream.as_raw_fd(), events: POLLIN, revents: 0 }], 20)?;
    }
}

fn configuration(fifo: &Path, ports: &[u16; 5], historical: bool) -> String {
    format!(r#"[GNSS-SDR]
GNSS-SDR.internal_fs_sps=4000000
GNSS-SDR.pre_2009_file={historical}
SignalSource.implementation=Fifo_Signal_Source
SignalSource.filename={fifo}
SignalSource.sample_type=gr_complex
SignalSource.dump=false
SignalConditioner.implementation=Pass_Through
Channels_1C.count=12
Channels.in_acquisition=1
Channel.signal=1C
Acquisition_1C.implementation=GPS_L1_CA_PCPS_Acquisition
Acquisition_1C.item_type=gr_complex
Acquisition_1C.pfa=0.01
Acquisition_1C.doppler_max=10000
Acquisition_1C.doppler_step=250
Acquisition_1C.dump=false
Tracking_1C.implementation=GPS_L1_CA_DLL_PLL_Tracking
Tracking_1C.item_type=gr_complex
Tracking_1C.pll_bw_hz=40.0
Tracking_1C.dll_bw_hz=4.0
Tracking_1C.dump=false
TelemetryDecoder_1C.implementation=GPS_L1_CA_Telemetry_Decoder
TelemetryDecoder_1C.dump=false
Observables.implementation=Hybrid_Observables
Observables.dump=false
PVT.implementation=RTKLIB_PVT
PVT.positioning_mode=Single
PVT.output_rate_ms=100
PVT.display_rate_ms=60000
PVT.iono_model=Broadcast
PVT.trop_model=Saastamoinen
PVT.output_enabled=false
PVT.dump=false
PVT.enable_monitor=true
PVT.enable_protobuf=true
PVT.monitor_client_addresses=127.0.0.1
PVT.monitor_udp_port={pvt}
Monitor.enable_monitor=true
Monitor.enable_protobuf=true
Monitor.client_addresses=127.0.0.1
Monitor.udp_port={observables}
Monitor.decimation_factor=1
TrackingMonitor.enable_monitor=true
TrackingMonitor.enable_protobuf=true
TrackingMonitor.client_addresses=127.0.0.1
TrackingMonitor.udp_port={tracking}
TrackingMonitor.decimation_factor=1
AcquisitionMonitor.enable_monitor=true
AcquisitionMonitor.enable_protobuf=true
AcquisitionMonitor.client_addresses=127.0.0.1
AcquisitionMonitor.udp_port={acquisition}
AcquisitionMonitor.decimation_factor=1
NavDataMonitor.enable_monitor=true
NavDataMonitor.client_addresses=127.0.0.1
NavDataMonitor.port={navigation}
"#, fifo = fifo.display(), observables = ports[0], tracking = ports[1], acquisition = ports[2], pvt = ports[3], navigation = ports[4])
}

struct Seen<T> { value: T, at: Instant }
#[derive(Default)]
struct Channel {
    prn: u32,
    acquisition_sample_stamp: u64,
    acquisition: Option<Seen<GnssSynchro>>,
    tracking: Option<Seen<GnssSynchro>>,
    observable: Option<Seen<GnssSynchro>>,
}
struct Output<W> {
    writer: W,
    version: String,
    historical: bool,
    center_hz: u64,
    input_bytes: u64,
    received_bytes: u64,
    bad_packets: u64,
    channels: [Channel; 12],
    pvt: Option<Seen<MonitorPvt>>,
}
fn fresh<T>(seen: &Option<Seen<T>>, now: Instant) -> bool {
    seen.as_ref().is_some_and(|s| now.duration_since(s.at) < STALE)
}
fn finite(value: f64) -> Option<f64> { value.is_finite().then_some(value) }
impl<W: Write> Output<W> {
    fn emit(&mut self, mut value: Value) -> io::Result<()> {
        value["input_samples"] = json!(self.input_bytes / 8);
        value["input_received_bytes"] = json!(self.received_bytes);
        value["input_rate_hz"] = json!(RATE);
        value["center_hz"] = json!(self.center_hz);
        value["sourceFrequencyHz"] = json!(self.center_hz);
        value["nominalCarrierHz"] = json!(CENTER);
        value["bench"] = json!(self.center_hz != CENTER);
        value["gnss_version"] = json!(self.version);
        value["pre_2009_file"] = json!(self.historical);
        serde_json::to_writer(&mut self.writer, &value)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
    fn status(&mut self, state: &str, message: &str) -> io::Result<()> {
        self.emit(json!({"kind":"status", "state":state, "message":message,
            "malformed_monitor_packets":self.bad_packets, "upstreamCompletionKnown":false}))
    }
    fn tables(&mut self, force_stale: bool) -> io::Result<()> {
        let now = Instant::now();
        let rows: Vec<Value> = self.channels.iter().enumerate().filter(|(_, c)| c.prn != 0).map(|(id, c)| {
            let tracking = c.tracking.as_ref();
            let obs = c.observable.as_ref();
            let acq = c.acquisition.as_ref();
            let latest = [tracking.map(|x| x.at), obs.map(|x| x.at), acq.map(|x| x.at)].into_iter().flatten().max();
            let track_fresh = !force_stale && fresh(&c.tracking, now);
            let obs_fresh = !force_stale && fresh(&c.observable, now);
            let tracking_valid = track_fresh && tracking.is_some_and(|t| t.value.flag_valid_symbol_output);
            let pseudorange_valid = obs_fresh && obs.is_some_and(|o| o.value.flag_valid_pseudorange && o.value.pseudorange_m.is_finite() && o.value.pseudorange_m > 0.0);
            let nav_valid = obs_fresh && obs.is_some_and(|o| o.value.flag_valid_word);
            json!({"channel":id, "prn":c.prn, "system":"G", "signal":"1C",
                "acquisition":tracking_valid || (!force_stale && fresh(&c.acquisition, now)),
                "acquisition_evidence":"success_monitor_or_valid_tracking_symbol",
                "acquisition_flag":acq.map(|a| a.value.flag_valid_acquisition),
                "tracking":tracking_valid,
                "cn0_db_hz":tracking.and_then(|t| finite(t.value.cn0_db_hz)),
                "doppler_hz":tracking.and_then(|t| finite(t.value.carrier_doppler_hz)),
                "acquisition_doppler_hz":acq.and_then(|a| finite(a.value.acq_doppler_hz)),
                "acquisition_code_delay_samples":acq.and_then(|a| finite(a.value.acq_delay_samples)),
                "tracking_sample_counter":tracking.map(|t| t.value.tracking_sample_counter),
                "nav_word_valid":nav_valid,
                "tow_ms":obs.filter(|o| o.value.flag_valid_word).map(|o| o.value.tow_at_current_symbol_ms),
                "pseudorange_valid":pseudorange_valid,
                "pseudorange_m":obs.filter(|o| o.value.flag_valid_pseudorange).and_then(|o| finite(o.value.pseudorange_m)),
                "stale":force_stale || latest.is_none_or(|at| now.duration_since(at) >= STALE),
                "tracking_stale":!track_fresh, "observables_stale":!obs_fresh,
                "age_ms":latest.map(|at| now.duration_since(at).as_millis() as u64)})
        }).collect();
        self.emit(json!({"kind":"table", "table":"satellites", "rows":rows}))?;
        let row = if let Some(seen) = &self.pvt {
            let p = &seen.value;
            let stale = force_stale || now.duration_since(seen.at) >= STALE;
            let valid = !stale && p.solution_status != 0 && p.solution_type == 0 && p.valid_sats >= 4
                && p.latitude.is_finite() && p.longitude.is_finite() && p.height.is_finite()
                && p.latitude.abs() <= 90.0 && p.longitude.abs() <= 180.0;
            json!({"valid":valid, "stale":stale, "age_ms":now.duration_since(seen.at).as_millis() as u64,
                "lat_deg":finite(p.latitude), "lon_deg":finite(p.longitude), "height_m":finite(p.height),
                "solution_status":p.solution_status, "solution_type":p.solution_type,
                "valid_sats":p.valid_sats, "week":p.week, "tow_ms":p.tow_at_current_symbol_ms,
                "utc_time":p.utc_time, "gdop":finite(p.gdop), "pdop":finite(p.pdop),
                "hdop":finite(p.hdop), "vdop":finite(p.vdop),
                "velocity_e_mps":finite(p.vel_e), "velocity_n_mps":finite(p.vel_n), "velocity_u_mps":finite(p.vel_u)})
        } else {
            json!({"valid":false, "stale":true, "age_ms":null, "lat_deg":null,
                "lon_deg":null, "height_m":null, "solution_status":0, "valid_sats":0})
        };
        self.emit(json!({"kind":"table", "table":"pvt", "rows":[row]}))
    }
    fn packet(&mut self, source: usize, bytes: &[u8]) -> io::Result<()> {
        let now = Instant::now();
        match source {
            0..=2 => match Observables::decode(bytes) {
                Ok(message) => for observation in message.observable {
                    if !(0..12).contains(&observation.channel_id) { self.bad_packets += 1; continue; }
                    let c = &mut self.channels[observation.channel_id as usize];
                    // Empty observables carry no acquisition identity. They cannot
                    // invalidate a newer generation from another UDP monitor;
                    // absent observations expire through the independent timer.
                    if observation.prn == 0 { continue; }
                    if observation.prn > 32 || observation.system != "G" || observation.signal != "1C" { continue; }
                    let stamp = observation.acq_samplestamp_samples;
                    if c.prn != 0 && (stamp < c.acquisition_sample_stamp
                        || (stamp == c.acquisition_sample_stamp && observation.prn != c.prn)) {
                        continue;
                    }
                    if c.prn == 0 || stamp > c.acquisition_sample_stamp {
                        // Reacquisition of the same PRN also starts a new generation.
                        *c = Channel { prn: observation.prn, acquisition_sample_stamp: stamp,
                            ..Channel::default() };
                    }
                    let previous = match source { 0 => &c.observable, 1 => &c.tracking, _ => &c.acquisition };
                    if previous.as_ref().is_some_and(|p|
                        observation.tracking_sample_counter < p.value.tracking_sample_counter) {
                        continue;
                    }
                    let seen = Some(Seen { value: observation, at: now });
                    match source { 0 => c.observable = seen, 1 => c.tracking = seen, _ => c.acquisition = seen }
                },
                Err(_) => self.bad_packets += 1,
            },
            3 => match MonitorPvt::decode(bytes) {
                Ok(value) => self.pvt = Some(Seen { value, at: now }),
                Err(_) => self.bad_packets += 1,
            },
            4 => match NavMessage::decode(bytes) {
                Ok(n) if n.system == "G" && n.signal == "1C" && (1..=32).contains(&n.prn)
                    && n.nav_message.len() == 300 && n.nav_message.bytes().all(|b| b == b'0' || b == b'1') => {
                    self.emit(json!({"kind":"table", "table":"navigation", "rows":[{
                        "system":n.system, "signal":n.signal, "prn":n.prn,
                        "tow_ms":n.tow_at_current_symbol_ms, "nav_message":n.nav_message,
                        "encoding":"gps_lnav_300_bits_with_parity", "parity_valid":true,
                        "stale":false, "age_ms":0}]}))?;
                }
                _ => self.bad_packets += 1,
            },
            _ => unreachable!(),
        }
        Ok(())
    }
}

fn run(executable: &Path, version: String, historical: bool, center_hz: u64) -> io::Result<()> {
    if !cfg!(target_endian = "little") {
        return Err(io::Error::other("GNSS-SDR gr_complex FIFO requires a little-endian host for CF32LE input"));
    }
    let private = PrivateDir::new()?;
    let fifo_path = private.0.join("input.cf32.fifo");
    let c_fifo = CString::new(fifo_path.as_os_str().as_bytes())?;
    if unsafe { mkfifo(c_fifo.as_ptr(), 0o600) } != 0 { return Err(io::Error::last_os_error()); }
    let mut sockets = Vec::with_capacity(5);
    let mut ports = [0; 5];
    for port in &mut ports {
        let socket = UdpSocket::bind(("127.0.0.1", 0))?;
        socket.set_nonblocking(true)?;
        *port = socket.local_addr()?.port();
        sockets.push(socket);
    }
    let config_path = private.0.join("receiver.conf");
    fs::write(&config_path, configuration(&fifo_path, &ports, historical))?;
    let config_argument = format!("--config_file={}", config_path.display());
    let (mut child, mut diagnostics) = spawn_receiver(executable,
        &[OsStr::new(&config_argument)], Some(&private.0))?;
    let stdout = io::stdout();
    let mut out = Output { writer: stdout.lock(), version, historical, center_hz, input_bytes: 0,
        received_bytes: 0, bad_packets: 0, channels: std::array::from_fn(|_| Channel::default()), pvt: None };
    out.status("starting", "GNSS-SDR GPS L1 C/A receiver: 12 channels, CF32LE at 4000000 Hz")?;
    out.tables(false)?;
    let stdin = io::stdin();
    // Read the descriptor directly: StdinLock's hidden buffered bytes would not
    // be reflected in poll readiness at an arbitrary producer chunk boundary.
    let mut input = File::from(stdin.as_fd().try_clone_to_owned()?);
    let mut fifo: Option<File> = None;
    let mut opened = false;
    let mut buffer = [0u8; 256 * 1024];
    let mut filled = 0usize;
    let mut written = 0usize;
    let mut eof = false;
    let mut draining: Option<Instant> = None;
    let mut interrupted = false;
    let mut last_progress = Instant::now();
    let mut last_table = Instant::now();
    let mut udp_buffer = [0u8; 65536];
    loop {
        diagnostics.drain()?;
        if let Some(status) = child.0.try_wait()? {
            diagnostics.drain()?;
            out.tables(true)?;
            out.emit(json!({"kind":"status", "state":"error", "message":format!("GNSS-SDR exited: {status}"),
                "diagnostics":diagnostics.text(), "upstreamCompletionKnown":false}))?;
            return Err(io::Error::other("GNSS-SDR exited before bounded shutdown"));
        }
        if STOP.load(Ordering::Relaxed) && !interrupted {
            interrupted = true;
            eof = true;
            // Finish the already accepted complete samples before closing the writer.
        }
        if !opened {
            match OpenOptions::new().write(true).custom_flags(O_NONBLOCK).open(&fifo_path) {
                Ok(file) => {
                    fifo = Some(file);
                    opened = true;
                    last_progress = Instant::now();
                    // Both endpoints now own the FIFO inode and configuration is
                    // loaded. Unlink names so a later SIGKILL leaves no input files.
                    fs::remove_file(&fifo_path)?;
                    fs::remove_file(&config_path)?;
                    let _ = fs::remove_dir(&private.0);
                    out.status("ready", "FIFO connected; waiting for actual acquisition and navigation observations")?;
                }
                Err(e) if e.raw_os_error() == Some(6) => {}
                Err(e) => return Err(e),
            }
        }
        if opened && eof && written == filled && draining.is_none() {
            fifo.take();
            draining = Some(Instant::now());
            out.status("draining", "All accepted complete IQ samples forwarded; bounded 3-second monitor settling, no upstream EOS acknowledgment")?;
        }
        if draining.is_some_and(|at| at.elapsed() >= DRAIN) { break; }
        let stall_limit = if opened { STALL } else { Duration::from_secs(15) };
        // A prepared receiver may wait indefinitely for its first IQ block.
        // Startup and stalls after input begins still have finite deadlines.
        if (!opened || out.received_bytes != 0) && draining.is_none() && last_progress.elapsed() >= stall_limit {
            out.emit(json!({"kind":"status", "state":"error", "message":format!("GNSS-SDR input/FIFO made no progress for {} seconds", stall_limit.as_secs()),
                "diagnostics":diagnostics.text(), "upstreamCompletionKnown":false}))?;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "GNSS-SDR input stalled"));
        }
        let can_read = opened && !eof && filled < buffer.len() && written == 0;
        let can_write = fifo.is_some() && filled - (filled % 8) > written;
        let mut fds: [PollFd; 8] = std::array::from_fn(|_| PollFd { fd: -1, events: POLLIN, revents: 0 });
        fds[0].fd = if can_read { input.as_raw_fd() } else { -1 };
        fds[1] = PollFd { fd: if can_write { fifo.as_ref().unwrap().as_raw_fd() } else { -1 }, events: POLLOUT, revents: 0 };
        fds[2].fd = diagnostics.stream.as_raw_fd();
        for (index, socket) in sockets.iter().enumerate() { fds[index + 3].fd = socket.as_raw_fd(); }
        poll_fds(&mut fds, 20)?;
        if can_read && fds[0].revents & (POLLIN | POLLHUP) != 0 {
            match input.read(&mut buffer[filled..]) {
                Ok(0) => eof = true,
                Ok(n) => { filled += n; out.received_bytes += n as u64; last_progress = Instant::now(); }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        if can_write && fds[1].revents != 0 {
            let end = filled - (filled % 8);
            match fifo.as_mut().unwrap().write(&buffer[written..end]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "GNSS FIFO returned zero write")),
                Ok(n) => {
                    written += n;
                    out.input_bytes += n as u64;
                    last_progress = Instant::now();
                    if written == end {
                        buffer.copy_within(end..filled, 0);
                        filled -= end;
                        written = 0;
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(e) => {
                    out.emit(json!({"kind":"status", "state":"error", "message":format!("GNSS FIFO write failed: {e}"), "diagnostics":diagnostics.text()}))?;
                    return Err(e);
                }
            }
        }
        if eof && filled < 8 && filled != 0 && written == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("CF32LE input ends with {filled} bytes of an incomplete 8-byte IQ sample")));
        }
        for (index, socket) in sockets.iter().enumerate() {
            // Bound work per socket so a hot monitor cannot starve IQ or shutdown.
            for _ in 0..128 {
                match socket.recv_from(&mut udp_buffer) {
                    Ok((n, sender)) if sender.ip().is_loopback() => out.packet(index, &udp_buffer[..n])?,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
        }
        if out.received_bytes != 0 && last_table.elapsed() >= Duration::from_millis(250) {
            out.tables(false)?;
            last_table = Instant::now();
        }
    }
    // FIFO v0.0.20 never emits GNU Radio EOS. Stop is cancellation, not proof that
    // the receiver processed its delayed final observables; report this explicitly.
    unsafe { kill(child.0.id() as i32, 2); }
    let stop_started = Instant::now();
    while child.0.try_wait()?.is_none() && stop_started.elapsed() < Duration::from_millis(200) {
        diagnostics.drain()?;
        poll_fds(&mut [PollFd { fd: diagnostics.stream.as_raw_fd(), events: POLLIN, revents: 0 }], 10)?;
    }
    if child.0.try_wait()?.is_none() { child.0.kill()?; child.0.wait()?; }
    out.tables(true)?;
    out.emit(json!({"kind":"status", "state":if interrupted { "stopped" } else { "eof" },
        "message":"FIFO closed after forwarding complete accepted samples; GNSS-SDR stopped after bounded monitor settling",
        "upstreamCompletionKnown":false, "drain_ms":DRAIN.as_millis() as u64,
        "input_bytes_forwarded":out.input_bytes, "malformed_monitor_packets":out.bad_packets}))
}

// These arguments describe the received RF source. GNSS-SDR still decodes the
// same zero-centered L1 C/A baseband at 4 MS/s after frequency translation.
fn parse_center(args: &[&OsStr], bench: Option<&OsStr>) -> io::Result<u64> {
    fn integer(value: &OsStr, name: &str) -> io::Result<u64> {
        let text = value.to_str().ok_or_else(|| io::Error::other(format!("{name} must be an ASCII integer")))?;
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(io::Error::other(format!("{name} must be an unsigned integer in Hz")));
        }
        text.parse().map_err(|_| io::Error::other(format!("{name} is outside the integer range")))
    }
    let center_hz = match args {
        [] => CENTER,
        [flag, value] if *flag == OsStr::new("--center-hz") => integer(value, "--center-hz")?,
        _ => return Err(io::Error::other("usage: websa-gnss [--center-hz HZ] | --version; stdin: 4000000 Hz CF32LE IQ; stdout: JSON lines")),
    };
    let bench = bench.map(|value| value.to_str().ok_or_else(||
        io::Error::other("WEBSA_GPS_BENCH_HZ must be an ASCII integer"))).transpose()?;
    websa_channelizer::external::gnss::validate_center_hz(center_hz, bench)
        .map_err(io::Error::other)?;
    Ok(center_hz)
}

fn main() {
    unsafe {
        signal(2, stop_signal as *const () as usize);
        signal(15, stop_signal as *const () as usize);
        signal(1, stop_signal as *const () as usize);
    }
    let result = (|| -> io::Result<()> {
        let args: Vec<_> = env::args_os().skip(1).collect();
        let version_only = args.len() == 1 && args[0] == "--version";
        let center_hz = if version_only { CENTER } else {
            parse_center(&args.iter().map(|arg| arg.as_os_str()).collect::<Vec<_>>(),
                env::var_os("WEBSA_GPS_BENCH_HZ").as_deref())?
        };
        let executable = executable()?;
        let version = probe(&executable)?;
        if version_only {
            println!("websa-gnss protocol=1 iq=cf32le rate=4000000");
            return Ok(());
        }
        let historical = match env::var("WEBSA_GNSS_PRE_2009_FILE") {
            Ok(s) if s == "true" => true,
            Ok(s) if s == "false" => false,
            Err(env::VarError::NotPresent) => false,
            _ => return Err(io::Error::other("WEBSA_GNSS_PRE_2009_FILE must be true or false")),
        };
        run(&executable, version, historical, center_hz)
    })();
    if let Err(error) = result {
        eprintln!("websa-gnss: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_center, CENTER};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    fn parse(args: &[&str], bench: Option<&str>) -> std::io::Result<u64> {
        let args: Vec<_> = args.iter().map(OsStr::new).collect();
        parse_center(&args, bench.map(OsStr::new))
    }

    #[test]
    fn nominal_receive_is_default_even_with_bench_enabled() {
        assert_eq!(parse(&[], None).unwrap(), CENTER);
        assert_eq!(parse(&[], Some("435000000")).unwrap(), CENTER);
        assert_eq!(parse(&["--center-hz", "1575420000"], Some("435000000")).unwrap(), CENTER);
    }

    #[test]
    fn translated_receive_requires_exact_configured_frequency() {
        for hz in ["400000000", "435000000", "450000000"] {
            assert_eq!(parse(&["--center-hz", hz], Some(hz)).unwrap(), hz.parse::<u64>().unwrap());
        }
        assert!(parse(&["--center-hz", "435000000"], None).is_err());
        assert!(parse(&["--center-hz", "435000001"], Some("435000000")).is_err());
        assert!(parse(&["--center-hz", "0"], Some("435000000")).is_err());
    }

    #[test]
    fn malformed_or_out_of_range_bench_configuration_is_rejected() {
        for value in ["", " 435000000", "435000000 ", "+435000000", "-435000000",
            "435000000.0", "435e6", "399999999", "450000001", "18446744073709551616"] {
            assert!(parse(&[], Some(value)).is_err(), "accepted {value:?}");
        }
        assert!(parse_center(&[], Some(OsStr::from_bytes(b"\xff"))).is_err());
    }

    #[test]
    fn malformed_center_and_extra_arguments_are_rejected() {
        for value in ["", " 1575420000", "+1575420000", "1575420000.0", "18446744073709551616"] {
            assert!(parse(&["--center-hz", value], None).is_err());
        }
        assert!(parse(&["--center-hz"], None).is_err());
        assert!(parse(&["--other", "1575420000"], None).is_err());
        assert!(parse(&["--center-hz", "1575420000", "--center-hz", "1575420000"], None).is_err());
        assert!(parse(&["--version", "--center-hz", "1575420000"], None).is_err());
    }

    fn monitor_output() -> Output<Vec<u8>> {
        Output { writer: Vec::new(), version: "test".into(), historical: false,
            center_hz: CENTER, input_bytes: 0, received_bytes: 0, bad_packets: 0,
            channels: std::array::from_fn(|_| Channel::default()), pvt: None }
    }

    fn observation(prn: u32, stamp: u64, counter: u64) -> GnssSynchro {
        GnssSynchro { system: "G".into(), signal: "1C".into(), prn, channel_id: 0,
            acq_samplestamp_samples: stamp, tracking_sample_counter: counter,
            flag_valid_symbol_output: true, flag_valid_word: true,
            flag_valid_pseudorange: true, pseudorange_m: 21_000_000.0,
            cn0_db_hz: 42.0, ..Default::default() }
    }

    fn receive_observation(output: &mut Output<Vec<u8>>, source: usize, value: GnssSynchro) {
        output.packet(source, &Observables { observable: vec![value] }.encode_to_vec()).unwrap();
    }

    fn snapshot(output: &mut Output<Vec<u8>>, table: &str) -> Value {
        output.writer.clear();
        output.tables(false).unwrap();
        std::str::from_utf8(&output.writer).unwrap().lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|value| value["table"] == table).unwrap()["rows"][0].clone()
    }

    #[test]
    fn delayed_monitor_packets_cannot_restore_a_retired_channel_generation() {
        let mut output = monitor_output();
        receive_observation(&mut output, 1, observation(3, 100, 150));
        receive_observation(&mut output, 0, observation(3, 100, 150));
        receive_observation(&mut output, 2, observation(7, 200, 200));
        receive_observation(&mut output, 1, observation(7, 200, 250));
        // Independent UDP ports can deliver the previous channel after reassignment.
        for source in 0..=2 {
            receive_observation(&mut output, source, observation(3, 100, 999));
            receive_observation(&mut output, source, observation(3, 200, 999));
        }
        receive_observation(&mut output, 0, GnssSynchro::default());
        let mut older_tracking = observation(7, 200, 249);
        older_tracking.flag_valid_symbol_output = false;
        receive_observation(&mut output, 1, older_tracking);
        let row = snapshot(&mut output, "satellites");
        assert_eq!(row["prn"], 7);
        assert_eq!(row["tracking"], true);
        assert_eq!(row["tracking_sample_counter"], 250);
        assert_eq!(row["nav_word_valid"], false);
        assert_eq!(row["pseudorange_valid"], false);
    }

    #[test]
    fn reacquiring_same_prn_invalidates_previous_tracking_and_navigation() {
        let mut output = monitor_output();
        receive_observation(&mut output, 1, observation(7, 200, 250));
        receive_observation(&mut output, 0, observation(7, 200, 250));
        assert_eq!(snapshot(&mut output, "satellites")["nav_word_valid"], true);
        receive_observation(&mut output, 2, observation(7, 300, 300));
        receive_observation(&mut output, 0, observation(7, 200, 999));
        let row = snapshot(&mut output, "satellites");
        assert_eq!(row["prn"], 7);
        assert_eq!(row["acquisition"], true);
        assert_eq!(row["tracking"], false);
        assert_eq!(row["nav_word_valid"], false);
        assert_eq!(row["pseudorange_valid"], false);
        assert!(row["cn0_db_hz"].is_null());
    }

    #[test]
    fn pvt_requires_a_fresh_valid_solution_and_expires_without_fabricating_coordinates() {
        let mut output = monitor_output();
        let initial = snapshot(&mut output, "pvt");
        assert_eq!(initial["valid"], false);
        assert!(initial["lat_deg"].is_null());
        let solution = MonitorPvt { solution_status: 5, solution_type: 0, valid_sats: 5,
            latitude: 41.2749, longitude: 1.98754, height: 100.795, ..Default::default() };
        output.packet(3, &solution.encode_to_vec()).unwrap();
        assert_eq!(snapshot(&mut output, "pvt")["valid"], true);
        output.pvt.as_mut().unwrap().at = Instant::now() - STALE - Duration::from_secs(1);
        let expired = snapshot(&mut output, "pvt");
        assert_eq!(expired["valid"], false);
        assert_eq!(expired["stale"], true);
        assert_eq!(expired["lat_deg"], 41.2749);
        assert!(expired["age_ms"].as_u64().unwrap() >= 3000);
        output.packet(3, &solution.encode_to_vec()).unwrap();
        assert_eq!(snapshot(&mut output, "pvt")["valid"], true);
        let invalid = MonitorPvt { solution_status: 0, ..solution };
        output.packet(3, &invalid.encode_to_vec()).unwrap();
        assert_eq!(snapshot(&mut output, "pvt")["valid"], false);
    }
}
