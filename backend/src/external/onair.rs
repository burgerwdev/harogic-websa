//! Independent OnAir 3.3 Web decoder client. No hardware is opened by this provider.
//!
//! The private rtl_tcp source carries fixed-scale CU8 IQ from the existing receiver.
//! Web decoder frequencies are quantized to kHz upstream: we label this virtual
//! source with the same rounded frequency and translate reported RF frequencies
//! back to the user's frequency. Sample timing is deliberately not claimed.
//! AIS normally requires exactly 162 MHz. For bench IQ only, setting
//! WEBSA_ONAIR_AIS_BENCH_HZ to an exact integer center in 400–450 MHz maps that
//! center to the fixed AIS decoder pair while preserving actual RF metadata
//! and emitting an explicit bench status before decoded results.

use super::ExternalDecodeEvent;
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{self, Message, WebSocket};

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(12);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_MESSAGE: usize = 70 * 1024;
const MAX_EVENTS: usize = 32;
const MAX_IQ_FLOATS: usize = 131_072;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn getpid() -> i32;
    fn getppid() -> i32;
    fn prctl(option: i32, ...) -> i32;
}

#[derive(Debug, Clone)]
pub struct OnAirCapability {
    pub mode: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub input_rate_hz: u32,
    pub minimum_rate_hz: u32,
    pub bandwidth_hz: u32,
}

// APRS's Web slice is 192 kHz, but the network source has a 250 kHz floor.
// Tuples are (mode, exact adapter rate, minimum receiver rate, occupied bandwidth).
// OnAir v3.3.0 server/web/web_decoders.cpp (kSpecs/AdsbDec) fixes ADS-B at 2 MS/s.
// Its core/src/adsb/adsb_rx.cpp documents the narrower 2 MS/s frontend's
// sensitivity tradeoff. Our 0.9 MHz pass edge leaves transition room below
// the exact adapter's 1 MHz Nyquist; the wider receiver cadence is intentional.
const MODES: [(&str, u32, u32, u32); 6] = [
    ("ais", 250_000, 250_000, 125_000),
    ("aprs", 250_000, 250_000, 20_000),
    ("ft4", 250_000, 250_000, 6_000),
    ("wspr", 250_000, 250_000, 6_000),
    ("meteor", 288_000, 288_000, 150_000),
    ("adsb", 2_000_000, 2_500_000, 1_800_000),
];

const AIS_CENTER_HZ: u64 = 162_000_000;
const AIS_BENCH_ENV: &str = "WEBSA_ONAIR_AIS_BENCH_HZ";

fn parse_ais_bench_frequency(value: Option<&str>) -> Result<Option<u64>, String> {
    let Some(value) = value else { return Ok(None); };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{AIS_BENCH_ENV} must be an integer center frequency in Hz between 400000000 and 450000000"));
    }
    let center = value.parse::<u64>().ok().filter(|hz| (400_000_000..=450_000_000).contains(hz))
        .ok_or_else(|| format!("{AIS_BENCH_ENV} must be an integer center frequency in Hz between 400000000 and 450000000"))?;
    Ok(Some(center))
}

fn source_virtual_frequency(mode: &str, center_hz: u64) -> Result<u64, String> {
    let bench = if mode == "ais" {
        let value = match std::env::var(AIS_BENCH_ENV) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => return Err(format!("{AIS_BENCH_ENV} must contain an integer frequency in Hz")),
        };
        parse_ais_bench_frequency(value.as_deref())?
    } else { None };
    validated_virtual_frequency(mode, center_hz, bench)
}

fn virtual_frequency(center_hz: u64) -> u64 {
    ((center_hz + 500) / 1000) * 1000
}

pub fn validate_frequency(mode: &str, center_hz: u64) -> Result<(), String> {
    source_virtual_frequency(mode, center_hz).map(|_| ())
}

pub fn frequency_supported(mode: &str, center_hz: u64) -> bool {
    validate_frequency(mode, center_hz).is_ok()
}

/// Canonical center used when a decoder is selected while the receiver is tuned
/// outside the decoder's band.
pub fn default_frequency(mode: &str) -> Option<u64> {
    if mode == "ais" {
        // A configured bench center is the intended receive frequency for AIS.
        if let Ok(value) = std::env::var(AIS_BENCH_ENV) {
            if let Ok(Some(hz)) = parse_ais_bench_frequency(Some(&value)) { return Some(hz); }
        }
        return Some(AIS_CENTER_HZ);
    }
    Some(match mode {
        "adsb" => 1_090_000_000,
        "meteor" => 137_100_000,
        "aprs" => 144_800_000,
        "ft4" => 14_080_000,
        "wspr" => 14_095_600,
        _ => return None,
    })
}

/// True for the OnAir modes with a fixed RF band contract. Other external
/// providers (GNSS, RTTY, ISM) accept any tuned frequency.
pub fn band_limited(mode: &str) -> bool {
    default_frequency(mode).is_some()
}

fn validated_virtual_frequency(mode: &str, center_hz: u64, bench: Option<u64>) -> Result<u64, String> {
    // Only the explicitly configured, exact bench center can relabel AIS IQ.
    if mode == "ais" && bench == Some(center_hz) && (400_000_000..=450_000_000).contains(&center_hz) {
        return Ok(AIS_CENTER_HZ);
    }
    let (low, high) = match mode {
        "ais" => (161_900_000, 162_100_000),
        "aprs" => (29_000_000, 470_000_000),
        "ft4" | "wspr" => (100_000, 450_000_000),
        "meteor" => (136_000_000, 138_500_000),
        "adsb" => (1_087_000_000, 1_093_000_000),
        _ => return Err(format!("unsupported OnAir decoder: {mode}")),
    };
    if center_hz < low || center_hz > high {
        return Err(format!("OnAir {mode} requires a frequency between {low} and {high} Hz"));
    }
    let virtual_hz = virtual_frequency(center_hz);
    if virtual_hz < low || virtual_hz > high {
        return Err(format!("OnAir {mode} rounded virtual frequency is outside its supported band"));
    }
    // Normal reception must keep the fixed maritime channel pair unchanged.
    if mode == "ais" && center_hz != AIS_CENTER_HZ {
        return Err("OnAir AIS requires 162000000 Hz, the center of both AIS channels".into());
    }
    Ok(virtual_hz)
}

pub fn capabilities() -> Vec<OnAirCapability> {
    static CAPS: LazyLock<Vec<OnAirCapability>> = LazyLock::new(|| {
        let deadline = Instant::now() + DISCOVERY_TIMEOUT;
        let discovery = Server::launch(250_000, 162_000_000, deadline).and_then(|server| {
            let (_, welcome) = server.websocket(deadline)?;
            Ok(welcome)
        });
        MODES.iter().map(|&(mode, rate, minimum, bandwidth)| {
            let reason = match &discovery {
                Err(error) => Some(error.clone()),
                Ok(welcome) if !advertises(welcome, mode) => {
                    Some(format!("installed OnAir server does not advertise the {mode} Web decoder"))
                }
                Ok(_) => None,
            };
            OnAirCapability {
                mode: mode.into(), available: reason.is_none(), unavailable_reason: reason,
                input_rate_hz: rate, minimum_rate_hz: minimum, bandwidth_hz: bandwidth,
            }
        }).collect()
    });
    CAPS.clone()
}

fn advertises(welcome: &Value, mode: &str) -> bool {
    welcome["proto"] == 1 && welcome["decoders"].as_array().is_some_and(|decoders| {
        decoders.iter().any(|decoder| decoder["kind"] == mode && decoder["needs_freq"] == true)
    })
}

struct Server {
    child: Child,
    lifetime: Option<mpsc::Sender<()>>,
    supervisor: Option<JoinHandle<()>>,
    directory: PathBuf,
    control_port: u16,
    web_port: u16,
    input: TcpListener,
    source_name: String,
    admin_token: String,
}

impl Server {
    fn launch(rate: u32, center: u64, deadline: Instant) -> Result<Self, String> {
        let executable = executable()?;
        let input = TcpListener::bind(("127.0.0.1", 0)).map_err(io_error)?;
        let input_port = input.local_addr().map_err(io_error)?.port();
        let control = TcpListener::bind(("127.0.0.1", 0)).map_err(io_error)?;
        let control_port = control.local_addr().map_err(io_error)?.port();
        let web = TcpListener::bind(("127.0.0.1", 0)).map_err(io_error)?;
        let web_port = web.local_addr().map_err(io_error)?.port();
        let mut secret = [0_u8; 16];
        File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut secret)).map_err(io_error)?;
        let admin_token: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        let directory = std::env::temp_dir().join(format!("websa-onair-{}-{}-{}", std::process::id(), NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed), admin_token));
        DirBuilder::new().mode(0o700).create(&directory).map_err(io_error)?;
        let source_name = format!("rtl_tcp 127.0.0.1:{input_port} (network)");
        let config_path = directory.join("server.conf");
        let config = format!(
            "port={control_port}\nlan=0\ndevice={source_name}\nnet_radio=rtltcp,127.0.0.1,{input_port}\nmode=fm\nfreq_mhz={}\nrate_msps={}\ngain_db=0\nautostart=0\nshare_rtltcp=0\nweb=1\nweb_port={web_port}\nweb_bind=127.0.0.1\nweb_lan=0\nweb_admin_token={admin_token}\nweb_directory=0\nweb_threads=1\nweb_idle_min=0\nweb_max_users=1\nweb_max_per_ip=1\nweb_max_decoders=1\nweb_allow_tx=0\n",
            center as f64 / 1e6, rate as f64 / 1e6,
        );
        if let Err(error) = fs::write(&config_path, config) {
            let _ = fs::remove_dir_all(&directory);
            return Err(io_error(error));
        }
        drop(control);
        drop(web);
        let mut command = Command::new(executable);
        command.arg("--config").arg(&config_path)
            .current_dir(&directory).env("XDG_CONFIG_HOME", &directory)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (lifetime_tx, lifetime_rx) = mpsc::channel();
        let supervisor = thread::Builder::new().name("onair-process".into()).spawn(move || {
            // PDEATHSIG follows the spawning thread, which must outlive the
            // child even when receiver construction ran on a pooled worker.
            let parent = unsafe { getpid() };
            unsafe {
                command.pre_exec(move || {
                    if prctl(1, 9_usize, 0_usize, 0_usize, 0_usize) != 0 { return Err(std::io::Error::last_os_error()); }
                    if getppid() != parent { return Err(std::io::Error::from_raw_os_error(3)); }
                    Ok(())
                });
            }
            let spawned = command.spawn().map_err(|e| format!("cannot start OnAir server: {e}"));
            if let Err(mpsc::SendError(Ok(mut child))) = ready_tx.send(spawned) {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            let _ = lifetime_rx.recv();
        });
        let supervisor = match supervisor {
            Ok(supervisor) => supervisor,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory);
                return Err(format!("cannot start OnAir supervisor: {error}"));
            }
        };
        let child = match ready_rx.recv().unwrap_or_else(|_| Err("OnAir supervisor exited before spawn".into())) {
            Ok(child) => child,
            Err(error) => {
                drop(lifetime_tx);
                let _ = supervisor.join();
                let _ = fs::remove_dir_all(&directory);
                return Err(error);
            }
        };
        let mut server = Self { child, lifetime: Some(lifetime_tx), supervisor: Some(supervisor),
            directory, control_port, web_port, input, source_name, admin_token };
        loop {
            if let Some(status) = server.child.try_wait().map_err(io_error)? {
                return Err(format!("OnAir server exited during startup: {status}; check WEBSA_ONAIR_SERVER and its portable libraries"));
            }
            match http_json_until(control_port, "GET", "/api/v1/status", None, None, deadline) {
                Ok(status) => {
                    if status["running"] != false || status["wanted"] != false || status["device"] != server.source_name || status["deviceKind"] != "native" {
                        return Err("OnAir did not select the private rtl_tcp source while stopped; refusing to start any device".into());
                    }
                    let devices = http_json_until(control_port, "GET", "/api/v1/devices", None, None, deadline)?;
                    let expected = format!("127.0.0.1:{input_port}");
                    if !devices.as_array().is_some_and(|list| list.iter().any(|d| d["name"] == server.source_name && d["board"] == "rtltcp" && d["kind"] == "native" && d["serial"] == expected && d["selected"] == true && d["index"] == status["deviceIndex"])) {
                        return Err("OnAir private network device identity could not be verified; receiver remains stopped".into());
                    }
                    return Ok(server);
                }
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                Err(error) => return Err(format!("OnAir control startup timed out: {error}")),
            }
        }
    }

    fn websocket(&self, deadline: Instant) -> Result<(WebSocket<TcpStream>, Value), String> {
        let address = format!("ws://127.0.0.1:{}/web/v1/ws", self.web_port);
        // The control listener is brought up before the Web listener.
        let socket = loop {
            match TcpStream::connect(("127.0.0.1", self.web_port)) {
                Ok(socket) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused && Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Err(error) => return Err(io_error(error)),
            }
        };
        socket.set_read_timeout(Some(remaining(deadline)?)).map_err(io_error)?;
        socket.set_write_timeout(Some(remaining(deadline)?)).map_err(io_error)?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
        let (mut ws, _) = tungstenite::client::client_with_config(address, socket, Some(config)).map_err(|e| format!("OnAir Web handshake failed: {e}"))?;
        ws.get_mut().set_write_timeout(Some(remaining(deadline)?)).map_err(io_error)?;
        ws.send(Message::Text(json!({"type":"hello","proto":1,"name":"WebSA"}).to_string().into())).map_err(ws_error)?;
        while Instant::now() < deadline {
            ws.get_mut().set_read_timeout(Some(remaining(deadline)?)).map_err(io_error)?;
            if let Some(value) = read_json(&mut ws)? {
                if value["type"] == "welcome" { return Ok((ws, value)); }
                reject_error(&value)?;
            }
        }
        Err("OnAir Web welcome timed out".into())
    }

    fn check_health(&mut self, rate: u32, center: u64) -> Result<(), String> {
        if let Some(status) = self.child.try_wait().map_err(io_error)? {
            return Err(format!("OnAir server exited: {status}"));
        }
        let status = http_json(self.control_port, "GET", "/api/v1/status", None, None)?;
        if status["running"] != true || status["device"] != self.source_name || status["sampleRateHz"].as_u64() != Some(rate as u64) || status["frequencyHz"].as_u64() != Some(center) {
            return Err(format!("OnAir source stopped or changed geometry: {status}"));
        }
        if status["droppedSamples"].as_u64() != Some(0) {
            return Err("OnAir input dropped samples; decoder stream retired".into());
        }
        let stats = http_json(self.web_port, "GET", "/web/v1/admin/stats", None, Some(&self.admin_token))?;
        if stats["iq_dropped"].as_u64() != Some(0) {
            return Err("OnAir Web decoder queue dropped samples; decoder stream retired".into());
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.lifetime.take();
        if let Some(supervisor) = self.supervisor.take() { let _ = supervisor.join(); }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub struct OnAirBridge {
    server: Option<Server>,
    iq: TcpStream,
    ws: WebSocket<TcpStream>,
    mode: String,
    subscription: String,
    rate: u32,
    center: u64,
    virtual_center: u64,
    bench_status_pending: bool,
    encoded: Vec<u8>,
    command: [u8; 5],
    command_len: usize,
    next_health: Instant,
}

impl OnAirBridge {
    pub fn spawn(mode: &str, input_rate: u32, center_hz: u64) -> Result<Self, String> {
        let virtual_center = source_virtual_frequency(mode, center_hz)?;
        let expected = MODES.iter().find(|m| m.0 == mode).unwrap().1;
        if input_rate != expected {
            return Err(format!("OnAir {mode} requires exactly {expected} IQ samples/s, got {input_rate}"));
        }
        let bench = mode == "ais" && center_hz != virtual_center;
        if bench {
            eprintln!("OnAir AIS bench mode: receiving actual RF at {center_hz} Hz; decoder uses virtual {virtual_center} Hz. Results are bench test data.");
        }
        let deadline = Instant::now() + START_TIMEOUT;
        let mut server = Server::launch(input_rate, virtual_center, deadline)?;
        let (mut ws, welcome) = server.websocket(deadline)?;
        if !advertises(&welcome, mode) {
            return Err(format!("installed OnAir server does not advertise {mode}"));
        }
        let listener = server.input.try_clone().map_err(io_error)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();
        let source: JoinHandle<Result<TcpStream, String>> = thread::spawn(move || accept_source(listener, input_rate, virtual_center, stop_worker));
        // Omitting device makes the remote API pass its internal -1 sentinel;
        // an explicit negative index is rejected by the HTTP validator.
        // refreshDevices() preserves selection by name, avoiding stale indices.
        let start = http_json(server.control_port, "POST", "/api/v1/start", Some(json!({})), None);
        if start.is_err() { stop.store(true, Ordering::Relaxed); }
        let accepted = source.join().map_err(|_| "OnAir IQ source thread panicked".to_string())?;
        start?;
        let iq = accepted?;
        server.check_health(input_rate, virtual_center)?;
        // The hub learns capture geometry only from actual samples. The source
        // sends one explicit zero preamble before user IQ; none is decoded.
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            let info = http_json(server.web_port, "GET", "/web/v1/info", None, None)?;
            if info["capture"]["rate_hz"].as_u64() == Some(input_rate as u64) && info["capture"]["centre_hz"].as_u64() == Some(virtual_center) { break; }
            if Instant::now() >= deadline { return Err("OnAir did not publish the verified IQ capture geometry".into()); }
            thread::sleep(Duration::from_millis(10));
        }
        let subscription = format!("{mode}@{}k", virtual_center / 1000);
        ws.send(Message::Text(json!({"type":"subscribe","decoder":mode,"freq_hz":virtual_center}).to_string().into())).map_err(ws_error)?;
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            if Instant::now() >= deadline { return Err("OnAir decoder subscription timed out".into()); }
            if let Some(value) = read_json(&mut ws)? {
                reject_error(&value)?;
                if value["type"] == "state" && value["subscriptions"].as_array().is_some_and(|list| list.iter().any(|v| v == &subscription)) { break; }
            }
        }
        ws.get_mut().set_nonblocking(true).map_err(io_error)?;
        iq.set_read_timeout(Some(Duration::from_millis(1))).map_err(io_error)?;
        iq.set_write_timeout(Some(Duration::from_millis(250))).map_err(io_error)?;
        Ok(Self { server: Some(server), iq, ws, mode: mode.into(), subscription, rate: input_rate,
            center: center_hz, virtual_center, bench_status_pending: bench, encoded: Vec::with_capacity(MAX_IQ_FLOATS),
            command: [0; 5], command_len: 0, next_health: Instant::now() + Duration::from_secs(1) })
    }

    pub fn push_iq(&mut self, samples: &[f32]) -> Result<Vec<ExternalDecodeEvent>, String> {
        if self.server.is_none() { return Err("OnAir receiver has stopped".into()); }
        if samples.len() % 2 != 0 || samples.len() > MAX_IQ_FLOATS || samples.iter().any(|v| !v.is_finite()) {
            return Err("OnAir requires bounded finite interleaved IQ pairs".into());
        }
        self.check_commands()?;
        self.encoded.clear();
        // Inverse of upstream source_rtltcp.cpp's (byte - 127.4) / 128.
        // Fixed gain, with saturation: listener AGC/NR never affects this path.
        self.encoded.extend(samples.iter().map(|v| (v * 128.0 + 127.4).round().clamp(0.0, 255.0) as u8));
        self.iq.write_all(&self.encoded).map_err(|e| format!("OnAir IQ input failed or stalled (samples were not silently dropped): {e}"))?;
        let events = self.drain()?;
        if Instant::now() >= self.next_health {
            self.server.as_mut().unwrap().check_health(self.rate, self.virtual_center)?;
            self.next_health = Instant::now() + Duration::from_secs(1);
        }
        Ok(events)
    }

    fn check_commands(&mut self) -> Result<(), String> {
        loop {
            match self.iq.read(&mut self.command[self.command_len..]) {
                Ok(0) => return Err("OnAir disconnected from its IQ source".into()),
                Ok(n) => {
                    self.command_len += n;
                    if self.command_len == 5 {
                        validate_command(&self.command, self.rate, self.virtual_center)?;
                        self.command_len = 0;
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(()),
                Err(e) => return Err(io_error(e)),
            }
        }
    }

    fn drain(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let mut events = Vec::new();
        if self.bench_status_pending {
            events.push(ais_bench_status(self.center, self.virtual_center));
            self.bench_status_pending = false;
        }
        for _ in 0..128 {
            let Some(value) = read_json(&mut self.ws)? else { return Ok(events); };
            reject_error(&value)?;
            if value["type"] == "capture" && (value["rate_hz"].as_u64() != Some(self.rate as u64) || value["centre_hz"].as_u64() != Some(self.virtual_center)) {
                return Err("OnAir capture geometry changed during reception".into());
            }
            if let Some(event) = decode_event(value, &self.mode, &self.subscription, self.center, self.virtual_center)? {
                if events.len() == MAX_EVENTS { return Err("OnAir decoder result backlog exceeded 32 events".into()); }
                events.push(event);
            }
        }
        Err("OnAir control message backlog exceeded its bound".into())
    }

    /// A live reconfiguration cannot wait for the upstream poller while capture
    /// continues. Preserve every currently available result and explicitly mark
    /// the unfinished upstream work as unknown before stopping the old process.
    pub fn retire(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        if self.server.is_none() { return Ok(Vec::new()); }
        let result = self.drain().map(|mut events| {
            events.push(ExternalDecodeEvent { provider: "onair".into(), mode: self.mode.clone(), kind: "status".into(),
                data: json!({"type":"status","state":"stopped","reason":"reconfigured","upstreamCompletionKnown":false,
                    "message":"OnAir live decoder reconfigured after draining available results; upstream has no flush completion acknowledgement"}) });
            events
        });
        self.stop();
        result
    }

    /// OnAir has no EOF/flush acknowledgement. Give its live poller a bounded
    /// drain interval, then report that completeness is unknown and reap it.
    pub fn finish(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        if self.server.is_none() { return Ok(Vec::new()); }
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        let result = (|| {
            while Instant::now() < deadline {
                let next = self.drain()?;
                if events.len() + next.len() > MAX_EVENTS { return Err("OnAir final result backlog exceeded 32 events".into()); }
                events.extend(next);
                thread::sleep(Duration::from_millis(20));
            }
            self.server.as_mut().unwrap().check_health(self.rate, self.virtual_center)?;
            events.push(ExternalDecodeEvent { provider: "onair".into(), mode: self.mode.clone(), kind: "status".into(),
                data: json!({"type":"status","state":"stopped","upstreamCompletionKnown":false,"message":"OnAir live decoder stopped after a bounded drain; upstream has no EOF completion acknowledgement"}) });
            Ok(events)
        })();
        self.stop();
        result
    }

    pub fn stop(&mut self) {
        let _ = self.iq.shutdown(Shutdown::Both);
        let _ = self.ws.get_mut().shutdown(Shutdown::Both);
        self.server.take();
    }
}

impl Drop for OnAirBridge {
    fn drop(&mut self) { self.stop(); }
}

fn accept_source(listener: TcpListener, rate: u32, center: u64, stop: Arc<AtomicBool>) -> Result<TcpStream, String> {
    listener.set_nonblocking(true).map_err(io_error)?;
    let deadline = Instant::now() + START_TIMEOUT;
    let mut stream = loop {
        if stop.load(Ordering::Relaxed) || Instant::now() >= deadline { return Err("OnAir did not connect to the private IQ source".into()); }
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => break stream,
            Ok(_) => return Err("non-loopback connection to private OnAir source".into()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(io_error(e)),
        }
    };
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(io_error)?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).map_err(io_error)?;
    stream.set_nodelay(true).map_err(io_error)?;
    // RTL0 tuner=0 identifies a generic server (250 kHz minimum), not hardware.
    stream.write_all(b"RTL0\0\0\0\0\0\0\0\0").map_err(io_error)?;
    let mut have_rate = false;
    let mut have_frequency = false;
    while !have_rate || !have_frequency {
        let mut command = [0; 5];
        stream.read_exact(&mut command).map_err(io_error)?;
        validate_command(&command, rate, center)?;
        have_rate |= command[0] == 2;
        have_frequency |= command[0] == 1;
    }
    stream.write_all(&[127_u8; 8192]).map_err(io_error)?;
    Ok(stream)
}

fn validate_command(command: &[u8; 5], rate: u32, center: u64) -> Result<(), String> {
    let value = u32::from_be_bytes(command[1..5].try_into().unwrap());
    if (command[0] == 1 && value as u64 != center) || (command[0] == 2 && value != rate) {
        return Err(format!("OnAir requested unexpected source geometry: command {} value {value}, expected {center} Hz / {rate} samples/s", command[0]));
    }
    Ok(())
}

fn ais_bench_status(center: u64, virtual_center: u64) -> ExternalDecodeEvent {
    ExternalDecodeEvent { provider: "onair".into(), mode: "ais".into(), kind: "status".into(),
        data: json!({"type":"status","state":"bench","bench":true,
            "sourceFrequencyHz":center,"onairFrequencyHz":virtual_center,
            "frequencyOffsetHz":center as i64 - virtual_center as i64,
            "message":format!("AIS bench test receiving at {center} Hz; decoded vessels are test data, using a virtual {virtual_center} Hz decoder center.")}) }
}

fn decode_event(mut value: Value, mode: &str, subscription: &str, center: u64, virtual_center: u64) -> Result<Option<ExternalDecodeEvent>, String> {
    if value["type"] != "event" { return Ok(None); }
    if value["decoder"] != subscription { return Err("OnAir returned an event for an unexpected subscription".into()); }
    let mut data = value.get_mut("data").ok_or("OnAir event is missing data")?.take();
    if data["v"] != 1 || data["kind"] != mode { return Err("OnAir decoder event schema or identity mismatch".into()); }
    let kind = match (mode, data["type"].as_str()) {
        ("adsb", Some("aircraft")) | ("ais", Some("vessels")) | ("aprs", Some("packets")) | ("ft4" | "wspr", Some("decodes")) => "table",
        ("meteor", Some("image")) => "image",
        (_, Some("status")) | ("meteor", Some("image_end")) => "status",
        // Protocol v1 permits new server event types; never promote generic
        // telemetry or unknown values into claimed decoded content.
        _ => return Ok(None),
    };
    if mode == "adsb" && kind == "table" && !data["aircraft"].is_array() {
        return Err("OnAir ADS-B aircraft snapshot is missing its aircraft array".into());
    }
    let delta = center as i64 - virtual_center as i64;
    let object = data.as_object_mut().ok_or("OnAir decoder payload is not an object")?;
    object.insert("sourceFrequencyHz".into(), json!(center));
    object.insert("onairFrequencyHz".into(), json!(virtual_center));
    object.insert("frequencyOffsetHz".into(), json!(delta));
    if mode == "ais" {
        object.insert("bench".into(), json!(center != virtual_center));
    }
    if let Some(hz) = object.get("dial_hz").and_then(Value::as_f64) {
        object.insert("onairDialHz".into(), json!(hz));
        object.insert("dial_hz".into(), json!(hz + delta as f64));
    }
    if let Some(decodes) = object.get_mut("decodes").and_then(Value::as_array_mut) {
        for decode in decodes {
            if let Some(hz) = decode.get("rf_hz").and_then(Value::as_f64) {
                if let Some(fields) = decode.as_object_mut() {
                    fields.insert("onairRfHz".into(), json!(hz));
                    fields.insert("rf_hz".into(), json!(hz + delta as f64));
                }
            }
        }
    }
    Ok(Some(ExternalDecodeEvent { provider: "onair".into(), mode: mode.into(), kind: kind.into(), data }))
}

fn executable() -> Result<PathBuf, String> {
    let candidates = if let Some(path) = std::env::var_os("WEBSA_ONAIR_SERVER") {
        vec![PathBuf::from(path)]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|p| p.join("onair-server")).collect()
    };
    for candidate in candidates {
        if fs::metadata(&candidate).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
            return fs::canonicalize(candidate).map_err(io_error);
        }
    }
    Err("OnAir server is not installed; set WEBSA_ONAIR_SERVER to the OnAir 3.3 portable bin/onair-server executable".into())
}

fn http_json(port: u16, method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Result<Value, String> {
    http_json_until(port, method, path, body, token, Instant::now() + IO_TIMEOUT)
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())
        .ok_or_else(|| "OnAir operation exceeded its deadline".into())
}

fn http_json_until(port: u16, method: &str, path: &str, body: Option<Value>, token: Option<&str>, deadline: Instant) -> Result<Value, String> {
    let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), remaining(deadline)?).map_err(io_error)?;
    stream.set_write_timeout(Some(remaining(deadline)?)).map_err(io_error)?;
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    let auth = token.map(|s| format!("Authorization: Bearer {s}\r\n")).unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).map_err(io_error)?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?)).map_err(io_error)?;
        let count = stream.read(&mut chunk).map_err(io_error)?;
        if count == 0 { break; }
        if bytes.len() + count > MAX_MESSAGE { return Err("OnAir HTTP response exceeded its bound".into()); }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.len() > MAX_MESSAGE { return Err("OnAir HTTP response exceeded its bound".into()); }
    let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").ok_or("invalid OnAir HTTP response")?;
    let headers = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
    if !headers.lines().next().is_some_and(|line| line.starts_with("HTTP/1.1 200 ") || line.starts_with("HTTP/1.0 200 ")) {
        return Err(format!("OnAir HTTP request failed: {}", String::from_utf8_lossy(&bytes)));
    }
    serde_json::from_slice(&bytes[end + 4..]).map_err(|e| format!("invalid OnAir HTTP JSON: {e}"))
}

fn read_json(ws: &mut WebSocket<TcpStream>) -> Result<Option<Value>, String> {
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).map(Some).map_err(|e| format!("invalid OnAir Web JSON: {e}")),
            Ok(Message::Ping(_)) => { ws.flush().map_err(ws_error)?; }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => return Err("OnAir Web connection closed".into()),
            Ok(_) => return Err("unexpected OnAir Web binary message (audio and spectrum are disabled)".into()),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(None),
            Err(e) => return Err(ws_error(e)),
        }
    }
}

fn reject_error(value: &Value) -> Result<(), String> {
    if value["type"] == "error" || value["type"] == "kicked" {
        Err(format!("OnAir Web rejected the decoder session: {value}"))
    } else { Ok(()) }
}
fn io_error(error: std::io::Error) -> String { format!("OnAir I/O: {error}") }
fn ws_error(error: tungstenite::Error) -> String { format!("OnAir Web: {error}") }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frequencies_are_supported_centers() {
        assert_eq!(default_frequency("adsb"), Some(1_090_000_000));
        assert_eq!(default_frequency("ais"), Some(162_000_000));
        assert_eq!(default_frequency("am"), None);
        for mode in ["ais", "aprs", "ft4", "wspr", "meteor", "adsb"] {
            let hz = default_frequency(mode).unwrap();
            assert!(frequency_supported(mode, hz), "{mode}");
        }
        assert!(!frequency_supported("adsb", 435_000_000));
        assert!(!frequency_supported("meteor", 435_000_000));
    }

    #[test]
    fn adsb_rate_and_frequency_contract_is_independent_of_discovery() {
        assert_eq!(MODES.iter().find(|mode| mode.0 == "adsb"), Some(&("adsb", 2_000_000, 2_500_000, 1_800_000)));
        assert_eq!(crate::catalog::external_rates("adsb"), Some((2_000_000, 2_500_000)));
        assert!(crate::catalog::is_external("adsb"));
        for frequency in [1_087_000_000, 1_090_000_000, 1_093_000_000] {
            assert_eq!(validated_virtual_frequency("adsb", frequency, None).unwrap(), frequency);
        }
        for frequency in [435_000_000, 1_086_999_999, 1_093_000_001] {
            assert!(validated_virtual_frequency("adsb", frequency, Some(frequency)).is_err());
        }
        let command = [2, 0, 30, 132, 128]; // rtl_tcp rate command: 2,000,000
        assert!(validate_command(&command, 2_000_000, 1_090_000_000).is_ok());
        assert!(OnAirBridge::spawn("adsb", 250_000, 1_090_000_000).is_err());
    }

    #[test]
    fn adsb_preserves_aircraft_snapshots_and_actual_source_frequency() {
        let aircraft = json!([{"icao":"40621D","callsign":"EZY123","alt_ft":38000,
            "lat":52.2572,"lon":3.91937}]);
        let payload = json!({"type":"event","decoder":"adsb@1090000k","data":{
            "v":1,"kind":"adsb","type":"aircraft","aircraft":aircraft}});
        let event = decode_event(payload, "adsb", "adsb@1090000k", 1_090_000_083, 1_090_000_000).unwrap().unwrap();
        assert_eq!(event.kind, "table");
        assert_eq!(event.data["aircraft"], aircraft);
        assert_eq!(event.data["sourceFrequencyHz"], 1_090_000_083_u64);
        assert_eq!(event.data["onairFrequencyHz"], 1_090_000_000_u64);
        assert_eq!(event.data["frequencyOffsetHz"], 83);
        for event_type in ["telemetry", "packets", "decodes"] {
            let payload = json!({"type":"event","decoder":"adsb@1090000k","data":{
                "v":1,"kind":"adsb","type":event_type,"aircraft":[]}});
            assert!(decode_event(payload, "adsb", "adsb@1090000k", 1_090_000_000, 1_090_000_000).unwrap().is_none());
        }
        let malformed = json!({"type":"event","decoder":"adsb@1090000k","data":{
            "v":1,"kind":"adsb","type":"aircraft"}});
        assert!(decode_event(malformed, "adsb", "adsb@1090000k", 1_090_000_000, 1_090_000_000).is_err());
    }

    #[test]
    fn ais_bench_configuration_requires_an_exact_bounded_integer() {
        assert_eq!(parse_ais_bench_frequency(None).unwrap(), None);
        for center in ["400000000", "435000000", "450000000"] {
            assert_eq!(parse_ais_bench_frequency(Some(center)).unwrap(), Some(center.parse::<u64>().unwrap()));
        }
        for invalid in ["", "true", "false", "435MHz", "435000000.0", "4.35e8", "NaN", "inf",
            " 435000000", "435000000 ", "+435000000", "-435000000", "399999999", "450000001",
            "162000000", "18446744073709551616"] {
            assert!(parse_ais_bench_frequency(Some(invalid)).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn ais_bench_mapping_does_not_relax_normal_frequency_contract() {
        for bench in [None, Some(435_000_000)] {
            assert_eq!(validated_virtual_frequency("ais", 162_000_000, bench).unwrap(), 162_000_000);
            for center in [161_900_000, 161_975_000, 161_999_999, 162_000_001, 162_025_000, 162_100_000,
                434_999_999, 435_000_001, 440_000_000] {
                assert!(validated_virtual_frequency("ais", center, bench).is_err(), "accepted {center}");
            }
        }
        assert!(validated_virtual_frequency("ais", 435_000_000, None).is_err());
        assert_eq!(validated_virtual_frequency("ais", 435_000_000, Some(435_000_000)).unwrap(), 162_000_000);
        // The option cannot move unrelated modes or override their supported bands.
        assert_eq!(validated_virtual_frequency("aprs", 435_000_000, Some(435_000_000)).unwrap(), 435_000_000);
        assert!(validated_virtual_frequency("meteor", 435_000_000, Some(435_000_000)).is_err());
        assert!(validated_virtual_frequency("ais", 470_000_000, Some(470_000_000)).is_err());
    }

    #[test]
    fn ais_bench_results_and_status_keep_actual_rf_identity() {
        let center = 435_000_000;
        let status = ais_bench_status(center, AIS_CENTER_HZ);
        assert_eq!(status.kind, "status");
        assert_eq!(status.data["bench"], true);
        assert_eq!(status.data["sourceFrequencyHz"], center);
        assert_eq!(status.data["onairFrequencyHz"], AIS_CENTER_HZ);
        assert_eq!(status.data["frequencyOffsetHz"], 273_000_000);
        assert!(status.data["message"].as_str().unwrap().contains("435000000 Hz"));
        let payload = json!({"type":"event","decoder":"ais@162000k","data":{
            "v":1,"kind":"ais","type":"vessels","bench":false,"sourceFrequencyHz":162000000,
            "vessels":[{"mmsi":123456789,"name":"BENCH TEST"}]}});
        let event = decode_event(payload.clone(), "ais", "ais@162000k", center, AIS_CENTER_HZ).unwrap().unwrap();
        assert_eq!(event.data["bench"], true);
        assert_eq!(event.data["sourceFrequencyHz"], center);
        assert_eq!(event.data["onairFrequencyHz"], AIS_CENTER_HZ);
        assert_eq!(event.data["frequencyOffsetHz"], 273_000_000);
        assert_eq!(event.data["vessels"][0]["name"], "BENCH TEST");
        let normal = decode_event(payload, "ais", "ais@162000k", AIS_CENTER_HZ, AIS_CENTER_HZ).unwrap().unwrap();
        assert_eq!(normal.data["bench"], false);
        assert_eq!(normal.data["frequencyOffsetHz"], 0);
        let mut tune = [1, 0, 0, 0, 0];
        tune[1..].copy_from_slice(&(AIS_CENTER_HZ as u32).to_be_bytes());
        assert!(validate_command(&tune, 250_000, AIS_CENTER_HZ).is_ok());
        tune[1..].copy_from_slice(&(center as u32).to_be_bytes());
        assert!(validate_command(&tune, 250_000, AIS_CENTER_HZ).is_err());
    }

    #[test]
    fn live_retirement_drains_ready_results_without_poll_window() {
        fn pair() -> (TcpStream, TcpStream) {
            let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();
            let client=TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            (client,listener.accept().unwrap().0)
        }
        let (iq,mut input)=pair();
        let (web,peer)=pair();
        let mut upstream=WebSocket::from_raw_socket(peer,tungstenite::protocol::Role::Server,None);
        let directory=std::env::temp_dir().join(format!("websa-retire-test-{}-{}",std::process::id(),NEXT_DIRECTORY.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir(&directory).unwrap();
        let server=Server {
            child:std::process::Command::new("/usr/bin/sleep").arg("30").spawn().unwrap(),
            lifetime:None,supervisor:None,directory,control_port:0,web_port:0,
            input:TcpListener::bind(("127.0.0.1",0)).unwrap(),source_name:String::new(),admin_token:String::new(),
        };
        web.set_nonblocking(true).unwrap();
        iq.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        let mut bridge=OnAirBridge {server:Some(server),iq,
            ws:WebSocket::from_raw_socket(web,tungstenite::protocol::Role::Client,None),
            mode:"ais".into(),subscription:"ais@162000k".into(),rate:250_000,
            center:162_000_000,virtual_center:162_000_000,bench_status_pending:false,encoded:Vec::new(),command:[0;5],command_len:0,
            next_health:Instant::now()+Duration::from_secs(60)};
        assert!(bridge.push_iq(&[0.0,0.25,-0.25,0.5]).unwrap().is_empty());
        let mut admitted=[0;4];
        input.read_exact(&mut admitted).unwrap();
        assert_eq!(admitted,[127,159,95,191]);
        // Even when a result is already waiting, the bench diagnostic must
        // appear first and must only be emitted once.
        bridge.center = 435_000_000;
        bridge.bench_status_pending = true;
        upstream.send(Message::Text(json!({"type":"event","decoder":"ais@162000k",
            "data":{"v":1,"kind":"ais","type":"vessels","vessels":[{"mmsi":123456789}]}}).to_string().into())).unwrap();
        // Synchronize with readiness rather than sleeping for a polling cadence.
        bridge.ws.get_mut().set_nonblocking(false).unwrap();
        bridge.ws.get_mut().set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        assert!(bridge.ws.get_mut().peek(&mut [0;1]).unwrap()>0);
        bridge.ws.get_mut().set_nonblocking(true).unwrap();
        let started=Instant::now();
        let events=bridge.retire().unwrap();
        assert!(started.elapsed()<Duration::from_secs(1),"live retirement waited for the EOF poll window");
        assert_eq!(events.len(),3);
        assert_eq!(events[0].kind,"status");
        assert_eq!(events[0].data["bench"],true);
        assert_eq!(events[0].data["sourceFrequencyHz"],435_000_000);
        assert_eq!(events[1].data["vessels"][0]["mmsi"],123456789);
        assert_eq!(events[1].data["bench"],true);
        assert_eq!(events[2].data["reason"],"reconfigured");
        assert_eq!(events[2].data["upstreamCompletionKnown"],false);
        assert!(!bridge.bench_status_pending);
        assert!(bridge.server.is_none());
        assert!(bridge.retire().unwrap().is_empty());
    }

    #[test]
    fn preserves_decoded_fields_and_maps_virtual_frequency() {
        let event = decode_event(json!({"type":"event","decoder":"wspr@14097k","data":{
            "v":1,"kind":"wspr","type":"decodes","dial_hz":14097000,"clock_err_s":0.3,
            "decodes":[{"rf_hz":14098450.5,"slot_utc":1234567890,"msg":"K1ABC FN42 30","dbm":30}]
        }}), "wspr", "wspr@14097k", 14097083, 14097000).unwrap().unwrap();
        assert_eq!(event.data["dial_hz"], 14097083.0);
        assert_eq!(event.data["decodes"][0]["rf_hz"], 14098533.5);
        assert_eq!(event.data["decodes"][0]["onairRfHz"], 14098450.5);
        assert_eq!(event.data["decodes"][0]["slot_utc"], 1234567890);
        assert_eq!(event.data["decodes"][0]["msg"], "K1ABC FN42 30");
        assert_eq!(event.data["clock_err_s"], 0.3);
    }

    #[test]
    fn preserves_meteor_strips_and_completion_fields() {
        for (event_type, expected_kind) in [("image", "image"), ("image_end", "status"), ("status", "status")] {
            let data = json!({"v":1,"kind":"meteor","type":event_type,"id":42,"w":1568,"h":20000,"ch":1,
                "y":15,"rows":3,"pw":1568,"ph":3,"png":"original-base64","preview":false,"scale":1,"final":true,"state":"locked"});
            let result = decode_event(json!({"type":"event","decoder":"meteor@137100k","data":data}),
                "meteor", "meteor@137100k", 137100000, 137100000).unwrap().unwrap();
            assert_eq!(result.kind, expected_kind);
            for (key, value) in data.as_object().unwrap() { assert_eq!(&result.data[key], value); }
        }
    }

    #[test]
    fn rejects_wrong_stream_and_does_not_promote_telemetry() {
        assert!(decode_event(json!({"type":"event","decoder":"host","data":{"telemetry":{}}}), "ais", "ais@162000k", 162000000, 162000000).is_err());
        assert!(decode_event(json!({"type":"event","decoder":"ais@162000k","data":{"v":1,"kind":"ais","type":"telemetry"}}), "ais", "ais@162000k", 162000000, 162000000).unwrap().is_none());
        assert!(validate_command(&[2, 0, 3, 208, 144], 250000, 162000000).is_ok());
        assert!(validate_command(&[2, 0, 3, 208, 145], 250000, 162000000).is_err());
        assert!(validate_frequency("ais", 161975000).is_err());
        assert!(validate_frequency("wspr", 14097083).is_ok());
    }
}
