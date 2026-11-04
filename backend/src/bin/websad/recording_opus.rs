//! Offline Ogg Opus export. Checked PCM is piped to FFmpeg one source segment at
//! a time; only encoded files are spooled, under the shared archive reader lease.
use super::*;
use std::{
    io::Read,
    os::unix::fs::DirBuilderExt,
    process::{Child, Command, Stdio},
    sync::OnceLock,
};

const MAX_SPOOL: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TIMELINE: u64 = 64 * 1024 * 1024;

pub(crate) fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-h", "encoder=libopus"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("Encoder libopus")
            })
    })
}

struct Spool {
    path: PathBuf,
}
impl Spool {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "websa-opus-{}-{}-{}",
            std::process::id(),
            millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }
}
impl Drop for Spool {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Encoder {
    child: Child,
    path: PathBuf,
    id: String,
    channels: u64,
    frames: u64,
}
impl Encoder {
    fn new(root: &FsPath, id: &str, row: &Value, source_frames: u64) -> io::Result<Self> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(io_error("invalid audio segment id"));
        }
        let channels = row["audio"]["channels"]
            .as_u64()
            .ok_or_else(|| io_error("missing channel count"))?;
        let rate = row["audio"]["rate_hz"]
            .as_f64()
            .ok_or_else(|| io_error("missing audio rate"))? as u32;
        let path = root.join(format!("{id}.opus"));
        // Flush the resampling filter even for one-sample recordings, then trim
        // to the source duration in the Opus 48 kHz clock (at least one sample).
        let output_frames =
            ((source_frames * 48000 + u64::from(rate) / 2) / u64::from(rate)).max(1);
        let filter = format!("apad=pad_len=64,aresample=48000,atrim=end_sample={output_frames}");
        let child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-n",
                "-f",
                "f32le",
                "-ar",
                &rate.to_string(),
                "-ac",
                &channels.to_string(),
                "-i",
                "pipe:0",
                "-map_metadata",
                "-1",
                "-af",
                &filter,
                "-c:a",
                "libopus",
                "-application",
                "audio",
                "-b:a",
                if channels == 1 { "48k" } else { "96k" },
                "-vbr",
                "on",
                "-ar",
                "48000",
                "-threads",
                "1",
                "-f",
                "opus",
            ])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            child,
            path,
            id: id.into(),
            channels,
            frames: 0,
        })
    }
    fn push(&mut self, pcm: &[u8]) -> io::Result<()> {
        self.child
            .stdin
            .as_mut()
            .ok_or_else(|| io_error("Opus encoder closed"))?
            .write_all(pcm)?;
        self.frames += pcm.len() as u64 / (4 * self.channels);
        Ok(())
    }
    fn finish(mut self, cancel: &AtomicBool) -> io::Result<Value> {
        self.child.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(io_error("Opus encoding cancelled or timed out"));
            }
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !status.success() {
            return Err(io_error(
                "FFmpeg could not encode this audio segment as Opus",
            ));
        }
        Ok(
            json!({"segment_id":self.id,"file":format!("{}.opus",self.id),"source_samples":self.frames.to_string(),
            "channels":self.channels,"rate_hz":48000,"target_bitrate":if self.channels==1 {48000} else {96000},
            "bytes":fs::metadata(&self.path)?.len().to_string()}),
        )
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Plan {
    spool: Spool,
    members: Vec<(String, u64)>,
    manifest: Vec<u8>,
}
fn pad(n: u64) -> u64 {
    (512 - n % 512) % 512
}
fn prepare(path: &FsPath, task: &Value, cancel: &AtomicBool) -> io::Result<(u64, Plan)> {
    if !available() {
        return Err(io_error(
            "Opus requires FFmpeg with the libopus encoder on the server",
        ));
    }
    let spool = Spool::new()?;
    let mut timeline = File::create(spool.path.join("frames.jsonl"))?;
    let mut encoder: Option<Encoder> = None;
    let mut segments = Vec::new();
    let mut total = 0;
    let mut lines = 0;
    let (_, _, hash) = audio::visit(path, task, cancel, |line, row, pcm, id, _| {
        lines += line.len() as u64 + 1;
        if lines > MAX_TIMELINE {
            return Err(io_error("Opus provenance exceeds the 64 MiB export limit"));
        }
        timeline.write_all(line)?;
        timeline.write_all(b"\n")?;
        if pcm.is_empty() {
            return Ok(());
        }
        if encoder.as_ref().is_some_and(|e| e.id != id) {
            let summary = encoder.take().unwrap().finish(cancel)?;
            total += number(&summary["bytes"]);
            segments.push(summary);
        }
        if encoder.is_none() {
            let source_frames = task["tracks"][0]["segments"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["segment_id"] == id)
                .map(|s| number(&s["samples"]))
                .ok_or_else(|| io_error("unknown Opus source segment"))?;
            encoder = Some(Encoder::new(&spool.path, id, row, source_frames)?);
        }
        let current = encoder.as_mut().unwrap();
        current.push(pcm)?;
        if total + fs::metadata(&current.path).map(|m| m.len()).unwrap_or(0) > MAX_SPOOL {
            return Err(io_error("Opus export exceeds the 2 GiB encoded-file limit"));
        }
        Ok(())
    })?;
    if let Some(encoder) = encoder {
        let summary = encoder.finish(cancel)?;
        total += number(&summary["bytes"]);
        segments.push(summary);
    }
    if total > MAX_SPOOL {
        return Err(io_error("Opus export exceeds the 2 GiB encoded-file limit"));
    }
    let manifest = serde_json::to_vec(
        &json!({"format":"websa-audio-opus-export-v1","task":task,"source":"published_audio",
        "encoding":"Ogg Opus; lossy VBR; resampled to 48000 Hz; 48 kb/s mono, 96 kb/s stereo",
        "original_recording_retained":true,"protocol_input":false,"local_controls":"browser volume/pan/mute/solo excluded",
        "time":"original per-frame source clocks in frames.jsonl; source segments remain separate; no gap filling",
        "source_sha512":hash.iter().map(|b|format!("{b:02x}")).collect::<String>(),"segments":segments}),
    )?;
    if manifest.len() > 2 * 1024 * 1024 {
        return Err(io_error("Opus manifest exceeds 2 MiB"));
    }
    let mut members = vec![("frames.jsonl".to_owned(), lines)];
    members.extend(
        segments
            .iter()
            .map(|s| (s["file"].as_str().unwrap().to_owned(), number(&s["bytes"]))),
    );
    let length = 1024
        + 512
        + manifest.len() as u64
        + pad(manifest.len() as u64)
        + members.iter().map(|(_, n)| 512 + n + pad(*n)).sum::<u64>();
    Ok((
        length,
        Plan {
            spool,
            members,
            manifest,
        },
    ))
}
fn emit(output: &mut dyn Write, plan: &Plan, cancel: &AtomicBool) -> io::Result<()> {
    use super::super::recording_export::header;
    header(output, "task.json", plan.manifest.len() as u64)?;
    output.write_all(&plan.manifest)?;
    output.write_all(&[0; 512][..pad(plan.manifest.len() as u64) as usize])?;
    let mut buffer = [0; 65536];
    for (name, bytes) in &plan.members {
        header(output, name, *bytes)?;
        let mut file = File::open(plan.spool.path.join(name))?;
        let mut left = *bytes;
        while left > 0 {
            if cancel.load(Ordering::Acquire) {
                return Err(io_error("Opus download cancelled"));
            }
            let count = file.read(&mut buffer[..left.min(65536) as usize])?;
            if count == 0 {
                return Err(io_error("truncated Opus export"));
            }
            output.write_all(&buffer[..count])?;
            left -= count as u64;
        }
        output.write_all(&[0; 512][..pad(*bytes) as usize])?;
    }
    output.write_all(&[0; 1024])
}
pub(crate) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !params.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    super::super::recording_export::download(
        app,
        task,
        "audio-opus.tar",
        prepare,
        |output, _, _, plan, cancel| emit(output, plan, cancel),
    )
    .await
}
