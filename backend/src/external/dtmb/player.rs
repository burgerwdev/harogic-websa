use super::{Session, SESSIONS};
use crate::external::{executable, process, Dependency};
use axum::{body::{Body, Bytes}, http::{header, StatusCode}, response::{IntoResponse, Response}};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

static VIEWERS: AtomicUsize = AtomicUsize::new(0);

fn dependency() -> &'static Dependency {
    static VALUE: LazyLock<Dependency> = LazyLock::new(|| {
        let result = executable("WEBSA_DTMB_FFMPEG", "websa-dtmb-ffmpeg").and_then(|path| {
            if !process::probe(&path, "-version", 0)?.contains("websa-dtmb-avsplus1") {
                return Err("DTMB player lacks verified AVS+ support; run tools/setup-dtmb-player.sh".into());
            }
            Ok(path)
        });
        match result {
            Ok(path) => Dependency { path:Some(path), error:None, modes:Vec::new() },
            Err(error) => Dependency { path:None, error:Some(error), modes:Vec::new() },
        }
    });
    &VALUE
}
pub(super) fn unavailable_reason() -> Option<String> { dependency().unavailable_reason() }

fn video_encoding(command: &mut tokio::process::Command, hardware: bool) {
    if hardware {
        command.args(["-vf", "fps=25,format=nv12,hwupload,deinterlace_vaapi=auto=1,scale_vaapi=w='min(960,iw)':h=-2:format=nv12",
            "-c:v", "h264_vaapi", "-profile:v", "constrained_baseline", "-bf", "0", "-b:v", "4M"]);
    } else {
        command.args(["-vf", "scale=w='min(960,iw)':h=-2:flags=fast_bilinear:interl=-1,yadif=deint=interlaced,fps=25",
            "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-threads:v", "2", "-pix_fmt", "yuv420p",
            "-profile:v", "baseline", "-keyint_min", "25", "-sc_threshold", "0", "-crf", "25"]);
    }
    command.args(["-level:v", "3.1", "-g", "25", "-maxrate", "4M", "-bufsize", "4M"]);
}

fn hardware_device(command: &mut tokio::process::Command, device: &Path) {
    command.args(["-init_hw_device", &format!("vaapi=dtmb:{}", device.display()), "-filter_hw_device", "dtmb"]);
}

fn player_affinity() -> Option<libc::cpu_set_t> {
    let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    if unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&allowed), &mut allowed) } != 0 { return None; }
    let mut physical = std::collections::HashSet::new();
    let cores: Vec<_> = (0..libc::CPU_SETSIZE as usize).filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &allowed) })
        .filter(|cpu| {
            let topology = PathBuf::from(format!("/sys/devices/system/cpu/cpu{cpu}/topology"));
            let Ok(package) = std::fs::read_to_string(topology.join("physical_package_id")) else { return false; };
            let Ok(core) = std::fs::read_to_string(topology.join("core_id")) else { return false; };
            physical.insert((package, core))
        }).collect();
    if cores.len() < 4 { return None; }
    // Leave at least half the physical cores available for live IQ and the browser.
    let mut budget: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &cpu in cores.iter().take((cores.len()/2).min(4)) { unsafe { libc::CPU_SET(cpu, &mut budget); } }
    Some(budget)
}

async fn probe_hardware(device: &Path) -> bool {
    let Some(path) = &dependency().path else { return false; };
    let mut command = tokio::process::Command::new(path);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_threads", "1"]);
    hardware_device(&mut command, device);
    command.args(["-f", "rawvideo", "-pixel_format", "nv12", "-video_size", "1280x720", "-framerate", "25", "-i", "pipe:0"]);
    video_encoding(&mut command, true);
    command.args(["-frames:v", "1", "-f", "mpegts", "pipe:1"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let Ok(mut child) = command.spawn() else { return false; };
    let mut input = child.stdin.take().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        // The frame-rate/deinterlace filters need lookahead before emitting their first frame.
        input.write_all(&vec![0u8; 3*1280*720*3/2]).await?;
        input.shutdown().await?;
        drop(input);
        child.wait().await
    }).await;
    if matches!(result, Ok(Ok(status)) if status.success()) { return true; }
    let _ = child.start_kill();
    let _ = child.wait().await;
    false
}

async fn hardware_encoder() -> Option<&'static Path> {
    if std::env::var("WEBSA_DTMB_VAAPI").as_deref() != Ok("1") { return None; }
    static DEVICE: tokio::sync::OnceCell<Option<PathBuf>> = tokio::sync::OnceCell::const_new();
    DEVICE.get_or_init(|| async {
        let mut devices: Vec<_> = std::fs::read_dir("/dev/dri").ok()?.flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
            .map(|entry| entry.path()).collect();
        devices.sort();
        for device in devices.into_iter().take(4) {
            if probe_hardware(&device).await { return Some(device); }
        }
        None
    }).await.as_deref()
}

struct Viewer(Arc<Session>);
impl Viewer {
    fn acquire(session: Arc<Session>) -> Option<Self> {
        VIEWERS.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 4).then_some(n+1)).ok()?;
        if session.viewers.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 2).then_some(n+1)).is_err() {
            VIEWERS.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(session))
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::AcqRel);
        VIEWERS.fetch_sub(1, Ordering::AcqRel);
    }
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({"error":message}))).into_response()
}

pub async fn play(id: String, program: u16) -> Response {
    let session = SESSIONS.lock().unwrap().get(&id).and_then(|value| value.upgrade());
    let Some(session) = session.filter(|s| s.live.load(Ordering::Acquire)) else {
        return error(StatusCode::GONE, "DTMB reception has stopped; tune the channel again");
    };
    let selected = {
        let data = session.data.lock().unwrap();
        let Some(selected) = data.transport.programs.get(&program) else { return error(StatusCode::NOT_FOUND, "Program is no longer available"); };
        if data.last_packet.is_none_or(|at| at.elapsed() > Duration::from_secs(2)) {
            return error(StatusCode::SERVICE_UNAVAILABLE, "Waiting for valid DTMB transport packets");
        }
        selected.clone()
    };
    if selected.scrambled { return error(StatusCode::UNPROCESSABLE_ENTITY, "This program is encrypted"); }
    if selected.video.is_empty() && selected.audio.is_empty() { return error(StatusCode::UNPROCESSABLE_ENTITY, "Program media is not ready"); }
    if matches!(selected.video.as_str(), "AVS2" | "AVS3") { return error(StatusCode::UNPROCESSABLE_ENTITY, "This video codec is not supported by the installed player"); }
    if let Some(reason) = unavailable_reason() { return error(StatusCode::SERVICE_UNAVAILABLE, &reason); }
    let Some(viewer) = Viewer::acquire(session.clone()) else { return error(StatusCode::TOO_MANY_REQUESTS, "DTMB playback limit reached; close another player"); };
    let device = if selected.video.is_empty() { None } else { hardware_encoder().await };
    let (history, mut input) = {
        let data = session.data.lock().unwrap();
        if !session.live.load(Ordering::Acquire) { return error(StatusCode::GONE, "DTMB reception has stopped"); }
        (data.history.iter().cloned().collect::<Vec<_>>(), session.tx.subscribe())
    };
    let mut command = tokio::process::Command::new(dependency().path.as_ref().unwrap());
    if let Some(device) = device { hardware_device(&mut command, device); }
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "2", "-filter_threads", "1",
        "-fflags", "+discardcorrupt", "-analyzeduration", "2000000", "-probesize", "4000000", "-readrate", "1", "-f", "mpegts", "-i", "pipe:0"]);
    if !selected.video.is_empty() {
        command.args(["-map", &format!("0:p:{program}:v:0")]);
        video_encoding(&mut command, device.is_some());
    }
    if !selected.audio.is_empty() {
        command.args(["-map", &format!("0:p:{program}:a:0"), "-c:a", "aac", "-b:a", "128k", "-ar", "48000", "-ac", "2",
            "-af", "aresample=async=1000"]);
    }
    command.args(["-sn", "-dn", "-max_muxing_queue_size", "1024", "-movflags", "+frag_keyframe+empty_moov+default_base_moof",
        "-frag_duration", "250000", "-flush_packets", "1", "-f", "mp4", "pipe:1"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let affinity = player_affinity();
    unsafe { command.pre_exec(move || {
        // Preserve live IQ deadlines while the player catches up with retained transport packets.
        if libc::setpriority(libc::PRIO_PROCESS, 0, 8) != 0 { return Err(std::io::Error::last_os_error()); }
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 { return Err(std::io::Error::last_os_error()); }
        if let Some(cpus) = &affinity {
            if libc::sched_setaffinity(0, std::mem::size_of_val(cpus), cpus) != 0 { return Err(std::io::Error::last_os_error()); }
        }
        Ok(())
    }); }
    let mut child = match command.spawn() { Ok(child) => child, Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, &format!("DTMB player: {e}")) };
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let _viewer = viewer;
        let writer = tokio::spawn(async move {
            for bytes in history {
                if !session.live.load(Ordering::Acquire) { return; }
                if !matches!(tokio::time::timeout(Duration::from_secs(5), stdin.write_all(&bytes)).await, Ok(Ok(()))) { return; }
            }
            while session.live.load(Ordering::Acquire) {
                let Ok(Ok(bytes)) = tokio::time::timeout(Duration::from_secs(5), input.recv()).await else { break; };
                if bytes.is_empty() { break; }
                if !matches!(tokio::time::timeout(Duration::from_secs(5), stdin.write_all(&bytes)).await, Ok(Ok(()))) { break; }
            }
        });
        let diagnostics = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut scratch = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut scratch).await {
                if n == 0 { break; }
                tail.extend_from_slice(&scratch[..n]);
                if tail.len() > 8192 { tail.drain(..tail.len()-8192); }
            }
            tail
        });
        let mut scratch = vec![0u8; 64*1024];
        loop {
            let read = tokio::select! {
                _ = tx.closed() => break,
                read = tokio::time::timeout(Duration::from_secs(15), stdout.read(&mut scratch)) => read,
            };
            match read {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    if !matches!(tokio::time::timeout(Duration::from_secs(5), tx.send(Ok(Bytes::copy_from_slice(&scratch[..n])))).await, Ok(Ok(()))) { break; }
                }
                _ => {
                    let _ = tx.try_send(Err(std::io::Error::other("DTMB playback stalled")));
                    break;
                }
            }
        }
        writer.abort();
        let _ = writer.await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        if let Ok(tail) = diagnostics.await {
            if !tail.is_empty() { eprintln!("DTMB player: {}", String::from_utf8_lossy(&tail)); }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async { rx.recv().await.map(|bytes| (bytes, rx)) });
    ([(header::CONTENT_TYPE, "video/mp4"), (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff")], Body::from_stream(stream)).into_response()
}
