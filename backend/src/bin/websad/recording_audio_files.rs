//! Encode on the recording worker. Downloads only verify and copy saved media.
//! Independently finalized minute-sized segments bound interrupted recording loss.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};

const MAX_LINE: u64 = 64 * 1024;
const MAX_TIMELINE: u64 = 0o77777777777;
pub(super) fn is_direct(task: &Value) -> bool {
    task["tracks"][0]["container"] == "websa-audio-files-v1"
}
pub(super) fn task_is_direct(app: &App, id: &str) -> bool {
    app.state
        .lock()
        .unwrap()
        .recordings
        .as_ref()
        .and_then(|m| m.readable_task(id).ok())
        .is_some_and(|(_, task)| is_direct(&task))
}
fn cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io_error("audio operation cancelled"))
    } else {
        Ok(())
    }
}
// The child must never hold a recording lane or archive reader indefinitely.
pub(super) fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
pub(super) fn write_pipe(pipe: &mut impl Write, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !bytes.is_empty() {
        match pipe.write(bytes) {
            Ok(0) => return Err(io_error("audio encoder pipe closed")),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn copy_checked(
    path: &FsPath,
    bytes: u64,
    expected: Option<&str>,
    cancel: &AtomicBool,
    out: &mut dyn Write,
) -> io::Result<String> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() < bytes {
        return Err(io_error("truncated recorded audio file"));
    }
    let mut hash = Sha256::new();
    let mut left = bytes;
    let mut buffer = [0; 65536];
    while left > 0 {
        cancelled(cancel)?;
        let n = left.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..n])?;
        hash.update(&buffer[..n]);
        out.write_all(&buffer[..n])?;
        left -= n as u64;
    }
    let digest = format!("{:x}", hash.finalize());
    if expected.is_some_and(|expected| digest != expected) {
        return Err(io_error("recorded audio checksum mismatch"));
    }
    Ok(digest)
}
enum Codec {
    Wav(File),
    Flac(File, flac::Encoder),
    Opus {
        encoder: Option<opus::Encoder>,
        pending: Vec<u8>,
        row: Value,
    },
}
struct Writer {
    root: PathBuf,
    id: String,
    file: String,
    channels: u64,
    codec: Codec,
}
impl Writer {
    fn new(path: &FsPath, format: &str, segment: &Value, row: &Value) -> io::Result<Self> {
        let id = segment["segment_id"].as_str().unwrap().to_owned();
        let name = format!("{id}.{format}");
        let codec = match format {
            "wav" => {
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path.join(&name))?;
                let mut empty = segment.clone();
                empty["samples"] = json!("0");
                audio::wav(&mut file, &empty)?;
                Codec::Wav(file)
            }
            "flac" => {
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path.join(&name))?;
                // Reserve the exact STREAMINFO header; rewrite its counts/MD5 at close.
                let header = flac::Encoder::new(&id, &row["audio"])?
                    .finish(&mut io::sink())?
                    .header;
                file.write_all(&header)?;
                Codec::Flac(file, flac::Encoder::new(&id, &row["audio"])?)
            }
            "opus" => Codec::Opus {
                encoder: None,
                pending: Vec::new(),
                row: row.clone(),
            },
            _ => return Err(io_error("unsupported audio recording format")),
        };
        Ok(Self {
            root: path.into(),
            id,
            file: name,
            channels: row["audio"]["channels"].as_u64().unwrap(),
            codec,
        })
    }
    fn push(&mut self, pcm: &[u8]) -> io::Result<()> {
        match &mut self.codec {
            Codec::Wav(file) => file.write_all(pcm),
            Codec::Flac(file, encoder) => encoder.push(pcm, file),
            Codec::Opus {
                encoder,
                pending,
                row,
            } => {
                if let Some(encoder) = encoder {
                    return encoder.push(pcm);
                }
                // Tiny sources need a known length to flush the resampler without
                // adding silence to the saved duration. Retain at most one packet.
                pending.extend_from_slice(pcm);
                if pending.len() >= 512 * self.channels as usize * 4 {
                    let mut live = opus::Encoder::live(&self.root, &self.id, row)?;
                    live.push(pending)?;
                    pending.clear();
                    *encoder = Some(live);
                }
                Ok(())
            }
        }
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        match &mut self.codec {
            Codec::Wav(file) | Codec::Flac(file, _) => file.sync_data(),
            Codec::Opus { .. } => Ok(()),
        }
    }
    fn finish(self, segment: &mut Value) -> io::Result<()> {
        match self.codec {
            Codec::Wav(mut file) => {
                file.seek(SeekFrom::Start(0))?;
                let length = audio::wav(&mut file, segment)?;
                if file.metadata()?.len() != length {
                    return Err(io_error("recorded WAV length mismatch"));
                }
                file.sync_all()?;
            }
            Codec::Flac(mut file, encoder) => {
                let result = encoder.finish(&mut file)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(&result.header)?;
                file.sync_all()?;
                segment["clipped_values"] = json!(result.clipped.to_string());
            }
            Codec::Opus {
                encoder,
                pending,
                row,
            } => {
                let encoder = match encoder {
                    Some(encoder) => encoder,
                    None => {
                        let mut encoder = opus::Encoder::new(
                            &self.root,
                            &self.id,
                            &row,
                            number(&segment["samples"]),
                        )?;
                        encoder.push(&pending)?;
                        encoder
                    }
                };
                let encoded = encoder.finish(&AtomicBool::new(false))?;
                segment["encoded_rate_hz"] = encoded["rate_hz"].clone();
                segment["target_bitrate"] = encoded["target_bitrate"].clone();
                File::open(self.root.join(&self.file))?.sync_all()?;
            }
        }
        let path = self.root.join(&self.file);
        let bytes = path.metadata()?.len();
        let hash = copy_checked(&path, bytes, None, &AtomicBool::new(false), &mut io::sink())?;
        segment["file"] = json!(self.file);
        segment["file_bytes"] = json!(bytes.to_string());
        segment["sha256"] = json!(hash);
        segment["file_complete"] = json!(true);
        Ok(())
    }
}
fn finish_segment(writer: &mut Option<Writer>, task: &mut Value) -> io::Result<()> {
    if let Some(writer) = writer.take() {
        let segment = task["tracks"][0]["segments"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        writer.finish(segment)?;
    }
    Ok(())
}
fn stored(path: &FsPath, task: &mut Value) -> io::Result<()> {
    let mut total = number(&task["tracks"][0]["timeline_bytes"]);
    for segment in task["tracks"][0]["segments"].as_array().unwrap() {
        if number(&segment["samples"]) == 0 {
            continue;
        }
        let file = format!(
            "{}.{}",
            segment["segment_id"].as_str().unwrap(),
            task["audio_format"].as_str().unwrap()
        );
        match fs::metadata(path.join(file)) {
            Ok(meta) => total += meta.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {} // Opus's short initial packet is buffered.
            Err(e) => return Err(e),
        }
    }
    task["tracks"][0]["stored_bytes"] = json!(total.to_string());
    Ok(())
}
pub(super) fn work(lane: &Lane, path: &FsPath) -> io::Result<()> {
    let mut task = lane.task.lock().unwrap().clone();
    let format = task["audio_format"].as_str().unwrap().to_owned();
    let mut timeline = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path.join("frames.jsonl"))?;
    let mut timeline_hash = Sha256::new();
    task["state"] = json!("waiting");
    task["tracks"][0]["state"] = json!("waiting");
    persist(path, &task)?;
    *lane.task.lock().unwrap() = task.clone();
    {
        let mut q = lane.queue.lock().unwrap();
        q.prepared = true;
        lane.wake.notify_all();
    }
    let mut writer: Option<Writer> = None;
    let result = (|| -> io::Result<()> {
        loop {
            let batch = lane.next_batch();
            if batch.is_empty() {
                break;
            }
            let size = batch.iter().map(|item| item.input.bytes()).sum();
            let outcome = (|| -> io::Result<()> {
                for item in batch {
                    let WorkInput::Audio(input) = item.input else {
                        return Err(io_error("wrong audio lane input"));
                    };
                    let mut raw = Vec::with_capacity(4 + input.source.len() + input.bytes.len());
                    raw.extend_from_slice(&(input.source.len() as u32).to_le_bytes());
                    raw.extend_from_slice(&input.source);
                    raw.extend_from_slice(&input.bytes);
                    let (mut row, pcm) = audio::decode(&raw)?;
                    let mut next = task.clone();
                    let id = audio::update(&mut next, &row, pcm.len())?;
                    if task["tracks"][0]["segments"]
                        .as_array()
                        .unwrap()
                        .last()
                        .is_some_and(|s| s["segment_id"] != id)
                    {
                        finish_segment(&mut writer, &mut task)?;
                        next = task.clone();
                        audio::update(&mut next, &row, pcm.len())?;
                    }
                    let segment = next["tracks"][0]["segments"]
                        .as_array_mut()
                        .unwrap()
                        .last_mut()
                        .unwrap();
                    row["segment_id"] = json!(id);
                    row["segment_sample_start"] = json!((number(&segment["samples"])
                        - row["audio"]["frames"].as_u64().unwrap())
                    .to_string());
                    let mut line = serde_json::to_vec(&row)?;
                    line.push(b'\n');
                    let end = number(&task["tracks"][0]["timeline_bytes"]) + line.len() as u64;
                    if line.len() as u64 > MAX_LINE || end > MAX_TIMELINE {
                        return Err(io_error("audio timeline limit reached"));
                    }
                    if !pcm.is_empty() {
                        if writer.is_none() {
                            writer = Some(Writer::new(path, &format, segment, &row)?);
                        }
                        writer.as_mut().unwrap().push(pcm)?;
                    }
                    timeline.write_all(&line)?;
                    timeline_hash.update(&line);
                    segment["timeline_end"] = json!(end.to_string());
                    next["tracks"][0]["timeline_bytes"] = json!(end.to_string());
                    task = next;
                    task["state"] = json!("recording");
                    task["tracks"][0]["state"] = json!("recording");
                }
                Ok(())
            })();
            if let Some(writer) = &mut writer {
                writer.checkpoint()?;
            }
            timeline.sync_data()?;
            stored(path, &mut task)?;
            task["updated_at_ms"] = json!(millis());
            persist(path, &task)?;
            *lane.task.lock().unwrap() = task.clone();
            lane.complete_batch(size);
            outcome?;
        }
        Ok(())
    })();
    // A bad input packet still leaves a finalized, playable preceding prefix.
    let finished = finish_segment(&mut writer, &mut task);
    timeline.sync_all()?;
    task["tracks"][0]["timeline_sha256"] = json!(format!("{:x}", timeline_hash.finalize()));
    stored(path, &mut task)?;
    if let Err(error) = &finished {
        set_fault(&mut task, "RECORDING_IO", &error.to_string());
        recover(path, &mut task)?;
    }
    *lane.task.lock().unwrap() = task;
    result.and(finished)
}

fn media_name(task: &Value, segment: &Value, index: usize) -> io::Result<String> {
    let format = task["audio_format"]
        .as_str()
        .filter(|f| matches!(*f, "wav" | "flac" | "opus"))
        .ok_or_else(|| io_error("invalid saved audio format"))?;
    let id = format!("audio-{index:06}");
    let file = format!("{id}.{format}");
    if segment["segment_id"] != id || segment["file"] != file || segment["file_complete"] != true {
        return Err(io_error("audio file is not finalized"));
    }
    Ok(file)
}
pub(super) fn metadata_intact(path: &FsPath, task: &Value) -> bool {
    let Some(segments) = task["tracks"][0]["segments"].as_array() else {
        return false;
    };
    if segments.len() > MAX_SEGMENTS {
        return false;
    }
    let timeline_bytes = number(&task["tracks"][0]["timeline_bytes"]);
    if !fs::metadata(path.join("frames.jsonl"))
        .is_ok_and(|m| m.is_file() && m.len() == timeline_bytes)
        || task["tracks"][0]["timeline_sha256"].as_str().is_none()
    {
        return false;
    }
    let mut bytes = timeline_bytes;
    for (index, segment) in segments.iter().enumerate() {
        if number(&segment["samples"]) == 0 {
            continue;
        }
        let Ok(name) = media_name(task, segment, index) else {
            return false;
        };
        let n = number(&segment["file_bytes"]);
        if !fs::metadata(path.join(name)).is_ok_and(|m| m.is_file() && m.len() == n)
            || segment["sha256"].as_str().is_none()
        {
            return false;
        }
        bytes += n;
    }
    bytes == number(&task["tracks"][0]["stored_bytes"])
}
pub(super) fn verify(path: &FsPath, task: &Value, cancel: &AtomicBool) -> io::Result<()> {
    cancelled(cancel)?;
    if !is_direct(task) {
        return Err(io_error("not a saved audio file recording"));
    }
    for (i, s) in task["tracks"][0]["segments"]
        .as_array()
        .ok_or_else(|| io_error("audio segments missing"))?
        .iter()
        .enumerate()
    {
        if number(&s["samples"]) == 0 {
            continue;
        }
        let name = media_name(task, s, i)?;
        if fs::metadata(path.join(&name))?.len() != number(&s["file_bytes"]) {
            return Err(io_error("recorded audio file length mismatch"));
        }
        let expected = s["sha256"]
            .as_str()
            .ok_or_else(|| io_error("audio checksum missing"))?;
        copy_checked(
            &path.join(name),
            number(&s["file_bytes"]),
            Some(expected),
            cancel,
            &mut io::sink(),
        )?;
    }
    let track = &task["tracks"][0];
    let hash = track["timeline_sha256"]
        .as_str()
        .ok_or_else(|| io_error("audio timeline checksum missing"))?;
    copy_checked(
        &path.join("frames.jsonl"),
        number(&track["timeline_bytes"]),
        Some(hash),
        cancel,
        &mut io::sink(),
    )?;
    Ok(())
}
pub(super) fn recover(path: &FsPath, task: &mut Value) -> io::Result<()> {
    let interrupted = active(task["state"].as_str().unwrap_or(""));
    let original_bytes = number(&task["tracks"][0]["timeline_bytes"]);
    let original_hash = task["tracks"][0]["timeline_sha256"]
        .as_str()
        .map(str::to_owned);
    let mut retained = Vec::new();
    let mut error = None;
    for (i, s) in task["tracks"][0]["segments"]
        .as_array()
        .ok_or_else(|| io_error("audio segments missing"))?
        .iter()
        .enumerate()
    {
        let checked = (|| -> io::Result<()> {
            if number(&s["samples"]) == 0 {
                return Ok(());
            }
            let file = media_name(task, s, i)?;
            let hash = s["sha256"]
                .as_str()
                .ok_or_else(|| io_error("audio checksum missing"))?;
            copy_checked(
                &path.join(file),
                number(&s["file_bytes"]),
                Some(hash),
                &AtomicBool::new(false),
                &mut io::sink(),
            )?;
            Ok(())
        })();
        if let Err(e) = checked {
            error = Some(e);
            break;
        }
        retained.push(s.clone());
    }
    if interrupted || error.is_some() {
        let bytes = retained
            .last()
            .map(|s| number(&s["timeline_end"]))
            .unwrap_or(0);
        task["tracks"][0]["segments"] = json!(retained);
        task["tracks"][0]["timeline_bytes"] = json!(bytes.to_string());
        task["tracks"][0]["raw_bytes"] = json!(retained
            .iter()
            .map(|s| number(&s["samples"]) * s["channels"].as_u64().unwrap_or(0) * 4)
            .sum::<u64>()
            .to_string());
        let code = if interrupted {
            "SERVICE_INTERRUPTED"
        } else {
            "ARCHIVE_CORRUPT"
        };
        if task["state"] != "failed" {
            set_fault(
                task,
                code,
                "recording interrupted or damaged; only finalized audio files were recovered",
            );
        }
        for s in task["tracks"][0]["segments"].as_array_mut().unwrap() {
            if s["end_reason"].is_null() {
                s["end_reason"] = json!("interrupted");
            }
        }
    }
    let bytes = number(&task["tracks"][0]["timeline_bytes"]);
    let hash = copy_checked(
        &path.join("frames.jsonl"),
        bytes,
        None,
        &AtomicBool::new(false),
        &mut io::sink(),
    )?;
    if !interrupted
        && bytes == original_bytes
        && original_hash.is_some_and(|expected| hash != expected)
    {
        return Err(io_error("audio timeline checksum mismatch during recovery"));
    }
    task["tracks"][0]["timeline_sha256"] = json!(hash);
    task["accepted_bytes"] = json!(number(&task["accepted_bytes"])
        .max(number(&task["tracks"][0]["raw_bytes"]))
        .to_string());
    task["queued_bytes"] = json!("0");
    stored(path, task)?;
    let mut blocks = 0u64;
    visit_timeline(path, task, &AtomicBool::new(false), |_| {
        blocks += 1;
        Ok(())
    })?;
    task["tracks"][0]["blocks"] = json!(blocks.to_string());
    persist(path, task)
}

pub(super) struct Plan {
    manifest: Vec<u8>,
    members: Vec<(String, u64, String)>,
}
pub(super) fn prepare(
    path: &FsPath,
    task: &Value,
    format: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<(u64, Plan)> {
    if format.is_some_and(|f| task["audio_format"] != f) {
        return Err(io_error("download must use the recorded audio format"));
    }
    verify(path, task, cancel)?;
    let track = &task["tracks"][0];
    let mut members = vec![(
        "frames.jsonl".into(),
        number(&track["timeline_bytes"]),
        track["timeline_sha256"].as_str().unwrap().into(),
    )];
    for (i, s) in track["segments"].as_array().unwrap().iter().enumerate() {
        if number(&s["samples"]) > 0 {
            members.push((
                media_name(task, s, i)?,
                number(&s["file_bytes"]),
                s["sha256"].as_str().unwrap().into(),
            ));
        }
    }
    let manifest = serde_json::to_vec(&json!({"format":"websa-audio-files-v1","task":task,
        "encoding":task["audio_format"],"encoded_during_recording":true,"transcoded_on_download":false,
        "source":"published_audio","local_controls_included":false,"utc_reliable":false,
        "flac_quantization":"round away from zero at 2^23, saturate signed-24; no dither or gain",
        "opus":"lossy VBR, 48000 Hz, mono 48000 / stereo 96000 bit/s"}))?;
    if manifest.len() > 3 * 1024 * 1024 {
        return Err(io_error("audio manifest too large"));
    }
    let length = 1024
        + 512
        + (manifest.len() as u64).div_ceil(512) * 512
        + members
            .iter()
            .map(|(_, n, _)| 512 + n.div_ceil(512) * 512)
            .sum::<u64>();
    Ok((length, Plan { manifest, members }))
}
pub(super) fn emit(
    out: &mut dyn Write,
    path: &FsPath,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<()> {
    use super::super::recording_export::header;
    header(out, "task.json", plan.manifest.len() as u64)?;
    out.write_all(&plan.manifest)?;
    out.write_all(&[0; 512][..(512 - plan.manifest.len() % 512) % 512])?;
    for (file, bytes, hash) in &plan.members {
        header(out, file, *bytes)?;
        copy_checked(&path.join(file), *bytes, Some(hash), cancel, out)?;
        out.write_all(&[0; 512][..((512 - bytes % 512) % 512) as usize])?;
    }
    cancelled(cancel)?;
    out.write_all(&[0; 1024])
}
pub(super) async fn export(app: Arc<App>, id: String, format: Option<&'static str>) -> Response {
    super::super::recording_export::download(
        app,
        id,
        "audio-recording.tar",
        move |path, task, cancel| prepare(path, task, format, cancel),
        |out, path, _, plan, cancel| emit(out, path, plan, cancel),
    )
    .await
}

struct Decoder(Child);
impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub(super) fn read_pcm(
    path: &FsPath,
    task: &Value,
    segment: &Value,
    start: u64,
    count: u64,
    cancel: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let index = task["tracks"][0]["segments"]
        .as_array()
        .unwrap()
        .iter()
        .position(|s| s["segment_id"] == segment["segment_id"])
        .unwrap();
    let path = path.join(media_name(task, segment, index)?);
    let channels = segment["channels"].as_u64().unwrap();
    let bytes = count * channels * 4;
    if bytes > 8 * 1024 * 1024 {
        return Err(io_error("audio preview exceeds 8 MiB"));
    }
    if task["audio_format"] == "wav" {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(58 + start * channels * 4))?;
        let mut pcm = vec![0; bytes as usize];
        file.read_exact(&mut pcm)?;
        return Ok(pcm);
    }
    let rate = segment["actual_rate_hz"].as_f64().unwrap() as u32;
    let padding = if task["audio_format"] == "opus" {
        "apad=pad_len=64,"
    } else {
        ""
    };
    let filter = format!(
        "{padding}aresample={rate},atrim=start_sample={start}:end_sample={}",
        start + count
    );
    let mut decoder = Decoder(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(path)
            .args([
                "-af",
                &filter,
                "-f",
                "f32le",
                "-c:a",
                "pcm_f32le",
                "-threads",
                "1",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut output = decoder.0.stdout.take().unwrap();
    nonblocking(&output)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut pcm = Vec::with_capacity(bytes as usize);
    let mut buffer = [0; 65536];
    loop {
        cancelled(cancel)?;
        if Instant::now() >= deadline {
            return Err(io_error("audio decoding timed out"));
        }
        match output.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if pcm.len() + n > bytes as usize {
                    return Err(io_error("audio decoder exceeded requested range"));
                }
                pcm.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) => return Err(e),
        }
    }
    loop {
        cancelled(cancel)?;
        if Instant::now() >= deadline {
            return Err(io_error("audio decoding timed out"));
        }
        if let Some(status) = decoder.0.try_wait()? {
            if !status.success() {
                return Err(io_error("cannot decode saved audio"));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if pcm.len() != bytes as usize {
        return Err(io_error("saved audio has fewer samples than requested"));
    }
    Ok(pcm)
}
pub(super) fn visit_timeline(
    path: &FsPath,
    task: &Value,
    cancel: &AtomicBool,
    mut visit: impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let mut reader = BufReader::new(
        File::open(path.join("frames.jsonl"))?.take(number(&task["tracks"][0]["timeline_bytes"])),
    );
    loop {
        cancelled(cancel)?;
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if n as u64 > MAX_LINE || line.last() != Some(&b'\n') {
            return Err(io_error("invalid audio timeline row"));
        }
        visit(serde_json::from_slice(&line)?)?;
    }
    Ok(())
}
