mod transport;
mod player;
#[cfg(test)]
mod tests;
pub use player::play;

use super::{executable, process::{self, Process}, Dependency, ExternalDecodeEvent};
use axum::body::Bytes;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use transport::Transport;

const HISTORY_BYTES: usize = 4 * 1024 * 1024;
const TRANSPORT_BATCH: usize = 188 * 64;
static SESSIONS: LazyLock<Mutex<HashMap<String, Weak<Session>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn dependency() -> &'static Dependency {
    static VALUE: LazyLock<Dependency> = LazyLock::new(|| {
        let result = executable("WEBSA_DTMB_PATH", "websa-dtmb").and_then(|path| {
            if !process::probe(&path, "--version", 0)?.contains("websa-dtmb protocol=1 iq=cf32le transport=tcp") {
                return Err("DTMB companion protocol mismatch; run tools/setup-dtmb-decoder.sh".into());
            }
            Ok(path)
        });
        match result {
            Ok(path) => Dependency { path: Some(path), error: None, modes: Vec::new() },
            Err(error) => Dependency { path: None, error: Some(error), modes: Vec::new() },
        }
    });
    &VALUE
}
pub fn unavailable_reason() -> Option<String> { dependency().unavailable_reason() }

#[derive(Default)]
struct Data {
    transport: Transport,
    history: VecDeque<Bytes>,
    staged: Vec<u8>,
    bytes: usize,
    last_packet: Option<Instant>,
    error: Option<String>,
}
struct Session {
    data: Mutex<Data>,
    tx: broadcast::Sender<Bytes>,
    live: AtomicBool,
    viewers: AtomicUsize,
}
impl Session {
    fn new() -> Self {
        Self { data: Mutex::new(Data::default()), tx: broadcast::channel(512).0,
            live: AtomicBool::new(true), viewers: AtomicUsize::new(0) }
    }
    fn push(&self, input: &[u8]) {
        let mut data = self.data.lock().unwrap();
        let bytes = data.transport.push(input);
        if bytes.is_empty() { return; }
        data.last_packet = Some(Instant::now());
        data.staged.extend_from_slice(&bytes);
        while data.staged.len() >= TRANSPORT_BATCH {
            let remainder = data.staged.split_off(TRANSPORT_BATCH);
            let bytes = Bytes::from(std::mem::replace(&mut data.staged, remainder));
            self.publish(&mut data, bytes);
        }
    }
    fn publish(&self, data: &mut Data, bytes: Bytes) {
        data.bytes += bytes.len();
        data.history.push_back(bytes.clone());
        while data.bytes > HISTORY_BYTES || data.history.len() > 4096 {
            if let Some(old) = data.history.pop_front() { data.bytes -= old.len(); }
        }
        let _ = self.tx.send(bytes);
    }
    fn close(&self) {
        self.live.store(false, Ordering::Release);
        let mut data = self.data.lock().unwrap();
        if !data.staged.is_empty() {
            let bytes = Bytes::from(std::mem::take(&mut data.staged));
            self.publish(&mut data, bytes);
        }
        let _ = self.tx.send(Bytes::new());
    }
    fn snapshot(&self, id: &str, mut status: Value) -> Value {
        let data = self.data.lock().unwrap();
        let fresh = data.last_packet.is_some_and(|at| at.elapsed() < Duration::from_secs(2));
        status["type"] = "dtmb".into();
        status["streamId"] = id.into();
        status["programs"] = json!(data.transport.programs.values().collect::<Vec<_>>());
        status["transportPackets"] = data.transport.packets.into();
        status["transportErrors"] = data.transport.errors.into();
        status["continuityErrors"] = data.transport.continuity_errors.into();
        status["syncLosses"] = data.transport.sync_losses.into();
        status["streamReady"] = (fresh && self.live.load(Ordering::Acquire)).into();
        status["playbackReason"] = json!(player::unavailable_reason());
        if !fresh && status["state"] == "receiving" { status["state"] = "correcting".into(); }
        status
    }
}

pub struct DtmbBridge {
    process: Option<Process>,
    session: Arc<Session>,
    reader: Option<JoinHandle<()>>,
    id: String,
}
impl DtmbBridge {
    pub fn spawn(rate: u32) -> Result<Self, String> {
        if rate != 15_625_000 { return Err("DTMB requires 15625000 complex samples/s at the adapter".into()); }
        if let Some(error) = unavailable_reason() { return Err(error); }
        let mut random = [0u8; 16];
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)).map_err(|e| e.to_string())?;
        let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let session = Arc::new(Session::new());
        let copy = session.clone();
        let token = id.clone();
        let reader = thread::Builder::new().name("dtmb-transport".into()).spawn(move || {
            let result = receive(listener, &copy, &token);
            if let Err(error) = result {
                if copy.live.load(Ordering::Acquire) { copy.data.lock().unwrap().error = Some(error); }
            }
            copy.close();
        }).map_err(|e| e.to_string())?;
        let mut bridge = Self { process: None, session, reader: Some(reader), id };
        let args = vec!["--rate".into(), rate.to_string(), "--port".into(), port.to_string(), "--token".into(), bridge.id.clone()];
        // Two seconds of wideband IQ absorb acquisition and scheduler bursts.
        bridge.process = Some(Process::spawn_buffered(dependency().path.as_ref().ok_or("DTMB unavailable")?, &args, 256*1024*1024)?);
        let mut sessions = SESSIONS.lock().unwrap();
        sessions.retain(|_, value| value.strong_count() > 0);
        sessions.insert(bridge.id.clone(), Arc::downgrade(&bridge.session));
        Ok(bridge)
    }
    pub fn buffer_status(&self) -> Value {
        json!({"input":self.process.as_ref().map(Process::buffer_status), "transportBytes":self.session.data.lock().unwrap().bytes})
    }
    pub fn push_iq(&mut self, iq: &[f32]) -> Result<Vec<ExternalDecodeEvent>, String> {
        if let Some(error) = self.session.data.lock().unwrap().error.clone() { return Err(error); }
        let process = self.process.as_mut().ok_or("DTMB stopped")?;
        let mut bytes = process.buffer();
        crate::wire::append_f32_le(&mut bytes, iq);
        let lines = process.push(bytes)?;
        self.events(lines)
    }
    fn events(&self, lines: Vec<String>) -> Result<Vec<ExternalDecodeEvent>, String> {
        lines.into_iter().map(|line| {
            let data: Value = serde_json::from_str(&line).map_err(|e| format!("DTMB status: {e}"))?;
            if !data.is_object() || !matches!(data["state"].as_str(), Some("searching" | "synchronizing" | "correcting" | "receiving")) {
                return Err("Invalid DTMB status".into());
            }
            Ok(ExternalDecodeEvent { provider: "dtmb".into(), mode: "dtmb".into(), kind: "status".into(), data: self.session.snapshot(&self.id, data) })
        }).collect()
    }
    pub fn finish(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let lines = self.process.as_mut().ok_or("DTMB stopped")?.finish()?;
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
        self.events(lines)
    }
    pub fn retire(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        self.session.close();
        self.process.take();
        Ok(vec![ExternalDecodeEvent { provider:"dtmb".into(), mode:"dtmb".into(), kind:"status".into(),
            data:json!({"type":"dtmb","state":"stopped","streamReady":false,"programs":[]}) }])
    }
}
impl Drop for DtmbBridge {
    fn drop(&mut self) {
        self.session.close();
        self.process.take();
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
        SESSIONS.lock().unwrap().remove(&self.id);
    }
}

fn receive(listener: TcpListener, session: &Session, token: &str) -> Result<(), String> {
    let start = Instant::now();
    let mut socket: TcpStream = loop {
        if !session.live.load(Ordering::Acquire) { return Ok(()); }
        if start.elapsed() > Duration::from_secs(10) { return Err("DTMB transport connection timed out".into()); }
        match listener.accept() {
            Ok((mut socket, _)) => {
                socket.set_read_timeout(Some(Duration::from_millis(200))).map_err(|e| e.to_string())?;
                let mut handshake = [0u8; 32];
                if socket.read_exact(&mut handshake).is_ok() && handshake == token.as_bytes() { break socket; }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e.to_string()),
        }
    };
    drop(listener);
    let mut bytes = [0u8; 188*256];
    while session.live.load(Ordering::Acquire) {
        match socket.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => session.push(&bytes[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => (),
            Err(e) => return Err(format!("DTMB transport: {e}")),
        }
    }
    Ok(())
}
