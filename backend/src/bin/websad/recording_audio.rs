//! Persistent exact published Float32 PCM. Protocol-input audio is a separate source;
//! browser gain/pan/solo never enter this lane. WAV exports retain each configuration boundary.
use super::*;
use sha2::{Digest, Sha512};
const MAGIC: &[u8; 8] = b"WBAU0001";
const MAX_VOLUMES: usize = 4096;
const MAX_SOURCE: usize = 16 * 1024;
const MAX_PLAN: usize = 2 * 1024 * 1024;
const MAX_WAV_BYTES: u64 = u32::MAX as u64 - 50;
use super::journal::Writer;
#[derive(Clone)]
pub(crate) struct Input {
    pub bytes: Bytes,
    pub source: Bytes,
}
fn writer(path: &FsPath, ordinal: usize) -> io::Result<Writer> {
    Writer::new(&path.join(format!("audio-{ordinal:06}.wba")), MAGIC)
}
fn scan(
    path: &FsPath,
    limit: u64,
    visit: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<super::journal::Scan> {
    super::journal::scan(path, MAGIC, limit, visit)
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn f64_at(b: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn decode(raw: &[u8]) -> io::Result<(Value, &[u8])> {
    if raw.len() < 4 {
        return Err(io_error("short audio record"));
    }
    let n = u32_at(raw, 0) as usize;
    if n == 0 || n > MAX_SOURCE || raw.len() < 4 + n + 96 {
        return Err(io_error("audio metadata exceeds bound"));
    }
    let source: Value = serde_json::from_slice(&raw[4..4 + n])?;
    let b = &raw[4 + n..];
    let (seq, frames, rate, frequency, channels) = (
        u32_at(b, 8),
        u32_at(b, 12),
        f64_at(b, 16),
        f64_at(b, 24),
        u32_at(b, 56),
    );
    let (input_rate, delay, num, den) =
        (f64_at(b, 72), f64_at(b, 48), u64_at(b, 80), u64_at(b, 88));
    let flags = u32_at(b, 60);
    if &b[..4] != b"AUD2"
        || u32_at(b, 4) != 1
        || !(1..=2).contains(&channels)
        || !(8000.0..=192000.0).contains(&rate)
        || rate.fract() != 0.0
        || !frequency.is_finite()
        || !input_rate.is_finite()
        || input_rate <= 0.0
        || !delay.is_finite()
        || delay < 0.0
        || u64_at(b, 40) == u64::MAX
        || den == 0
        || num >= den
        || u32_at(b, 32) == 0
        || u32_at(b, 36) == 0
        || flags & !0x30000 != 1
        || (seq == 0) != (frames == 0)
        || b.len() != 96 + frames as usize * channels as usize * 4
        || b[96..]
            .chunks_exact(4)
            .any(|s| !f32::from_le_bytes(s.try_into().unwrap()).is_finite())
        || source["config"]["mode"].as_str().is_none()
        || source["baseband_stream"].is_null()
    {
        return Err(io_error("invalid published audio frame/source"));
    }
    let row = json!({"version":1,"source":source,"audio":{
        "stream":{"source_id":"primary","epoch":u32_at(b,36).to_string(),"channel_id":u32_at(b,32).to_string()},
        "sequence":seq,"frames":frames,"rate_hz":rate,"channels":channels,"frequency_hz":frequency,
        "audio_index":u64_at(b,64).to_string(),"input_index":u64_at(b,40).to_string(),
        "input_fraction_num":num.to_string(),"input_fraction_den":den.to_string(),"input_rate_hz":input_rate,
        "delay_seconds":delay,"delay_known":flags & 0x10000 == 0,"causal_presentation":flags & 0x20000 != 0},
        "original_aud2_header_hex":b[..96].iter().map(|b|format!("{b:02x}")).collect::<String>()});
    Ok((row, &b[96..]))
}
fn update(task: &mut Value, row: &Value, bytes: usize) -> io::Result<String> {
    let a = &row["audio"];
    let source = &row["source"];
    if source["receiver_uid"] != task["receiver_uid"] || source["service_id"] != task["service_id"]
    {
        return Err(io_error("audio source ownership mismatch"));
    }
    let segments = task["tracks"][0]["segments"]
        .as_array_mut()
        .ok_or_else(|| io_error("missing audio segments"))?;
    let mut boundary = segments.is_empty() || a["sequence"] == 0;
    if let Some(last) = segments.last() {
        let same = last["stream"] == a["stream"];
        if same
            && a["sequence"] != 0
            && (last["last_sequence"]
                .as_u64()
                .and_then(|n| n.checked_add(1))
                != a["sequence"].as_u64()
                || number(&last["next_audio_index"]) != number(&a["audio_index"]))
        {
            return Err(io_error("audio sequence/sample discontinuity"));
        }
        let format_changed =
            last["actual_rate_hz"] != a["rate_hz"] || last["channels"] != a["channels"];
        if same && format_changed && a["sequence"] != 0 {
            return Err(io_error("audio format changed without start"));
        }
        boundary |= !same || format_changed || last["source"] != *source;
        // Split long continuous sources before the RIFF 32-bit length limit.
        boundary |= number(&last["samples"]) * last["channels"].as_u64().unwrap_or(0) * 4
            + bytes as u64
            > MAX_WAV_BYTES;
    }
    if boundary {
        if segments.len() >= MAX_SEGMENTS {
            return Err(io_error("audio segment limit reached"));
        }
        if let Some(last) = segments.last_mut() {
            last["end_reason"] = json!("audio_source_changed");
        }
        let id = format!("audio-{:06}", segments.len());
        segments.push(json!({"segment_id":id,"stream":a["stream"],"first_input_index":a["input_index"],
            "fraction_num":a["input_fraction_num"],"fraction_den":a["input_fraction_den"],"samples":"0",
            "actual_rate_hz":a["rate_hz"],"frequency_hz":a["frequency_hz"],"channels":a["channels"],
            "source":source,"first_audio_index":a["audio_index"],"next_audio_index":a["audio_index"],
            "prefix_missing":a["sequence"] != 0,"last_sequence":null,"end_reason":null,"utc_reliable":false}));
        if serde_json::to_vec(&segments)?.len() > MAX_PLAN {
            return Err(io_error("audio segment metadata exceeds bound"));
        }
    }
    let last = segments.last_mut().unwrap();
    last["last_sequence"] = a["sequence"].clone();
    last["next_audio_index"] = json!(number(&a["audio_index"])
        .checked_add(a["frames"].as_u64().unwrap())
        .ok_or_else(|| io_error("audio sample clock overflow"))?
        .to_string());
    add(&mut last["samples"], a["frames"].as_u64().unwrap());
    let id = last["segment_id"].as_str().unwrap().to_string();
    add(&mut task["tracks"][0]["raw_bytes"], bytes as u64);
    add(&mut task["tracks"][0]["blocks"], 1);
    Ok(id)
}
fn volume(task: &mut Value, ordinal: usize) {
    task["tracks"][0]["volumes"].as_array_mut().unwrap().push(json!({"file":format!("audio-{ordinal:06}.wba"),"index":null,"complete":false,"valid_bytes":"8"}));
}
fn saved(task: &mut Value, finished: u64, position: u64) {
    task["tracks"][0]["stored_bytes"] = json!((finished + position).to_string());
    let last = task["tracks"][0]["volumes"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    last["valid_bytes"] = json!(position.to_string());
}
pub(super) fn work(lane: &Lane, path: &FsPath) -> io::Result<()> {
    fs::create_dir(path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    let mut task = lane.task.lock().unwrap().clone();
    persist(path, &task)?;
    let mut writer = writer(path, 0)?;
    volume(&mut task, 0);
    task["state"] = json!("waiting");
    task["tracks"][0]["state"] = json!("waiting");
    persist(path, &task)?;
    *lane.task.lock().unwrap() = task.clone();
    {
        let mut q = lane.queue.lock().unwrap();
        q.prepared = true;
        lane.wake.notify_all();
    }
    let mut finished = 0;
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
                let mut raw = Vec::with_capacity(input.source.len() + input.bytes.len() + 4);
                raw.extend_from_slice(&(input.source.len() as u32).to_le_bytes());
                raw.extend_from_slice(&input.source);
                raw.extend_from_slice(&input.bytes);
                let (row, pcm) = decode(&raw)?;
                // Validate and stage counters before writing. Only publish them after durable append.
                let mut next = task.clone();
                update(&mut next, &row, pcm.len())?;
                let position = writer.append(&raw)?;
                task = next;
                saved(&mut task, finished, position);
                task["state"] = json!("recording");
                task["tracks"][0]["state"] = json!("recording");
                if position >= VOLUME_BYTES || writer.records >= VOLUME_BLOCKS {
                    let position = writer.finish()?;
                    saved(&mut task, finished, position);
                    finished += position;
                    task["tracks"][0]["volumes"]
                        .as_array_mut()
                        .unwrap()
                        .last_mut()
                        .unwrap()["complete"] = json!(true);
                    let ordinal = task["tracks"][0]["volumes"].as_array().unwrap().len();
                    if ordinal >= MAX_VOLUMES {
                        return Err(io_error("audio volume limit reached"));
                    }
                    writer = self::writer(path, ordinal)?;
                    volume(&mut task, ordinal);
                }
            }
            Ok(())
        })();
        let position = writer.checkpoint()?;
        if outcome.is_ok() {
            saved(&mut task, finished, position);
        }
        task["updated_at_ms"] = json!(millis());
        persist(path, &task)?;
        *lane.task.lock().unwrap() = task.clone();
        lane.complete_batch(size);
        // A bad later packet must not discard the checked, durable prefix of
        // this batch. Publish that prefix before surfacing the original error.
        outcome?;
    }
    let position = writer.finish()?;
    saved(&mut task, finished, position);
    task["tracks"][0]["volumes"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["complete"] = json!(true);
    *lane.task.lock().unwrap() = task;
    Ok(())
}

pub(super) fn recover(path: &FsPath, task: &mut Value) -> io::Result<()> {
    let interrupted = active(task["state"].as_str().unwrap_or(""));
    let completed = task["state"] == "completed";
    let last_reason = task["tracks"][0]["segments"]
        .as_array()
        .and_then(|s| s.last())
        .map(|s| s["end_reason"].clone())
        .filter(|r| !r.is_null())
        .unwrap_or(json!("record_stopped"));
    let mut files = fs::read_dir(path)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    files.retain(|p| p.extension().is_some_and(|e| e == "wba"));
    files.sort();
    task["tracks"][0]["segments"] = json!([]);
    task["tracks"][0]["volumes"] = json!([]);
    for key in ["blocks", "raw_bytes", "stored_bytes"] {
        task["tracks"][0][key] = json!("0");
    }
    let mut fault = None;
    for (ordinal, file) in files.iter().enumerate() {
        if file.file_name().unwrap() != format!("audio-{ordinal:06}.wba").as_str() {
            fault = Some("audio volume order mismatch".into());
            break;
        }
        let size = file.metadata()?.len();
        let scanned = scan(file, size, |raw| {
            let (row, pcm) = decode(raw)?;
            let mut next = task.clone();
            update(&mut next, &row, pcm.len())?;
            *task = next;
            Ok(())
        })?;
        volume(task, ordinal);
        let finished = number(&task["tracks"][0]["stored_bytes"]);
        saved(task, finished, size);
        let last = task["tracks"][0]["volumes"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        last["valid_bytes"] = json!(scanned.valid.to_string());
        last["complete"] = json!(scanned.complete);
        if let Some(error) = scanned.error {
            fault = Some(error);
            break;
        }
        if !scanned.complete {
            fault = Some("audio volume has no checked footer".into());
            break;
        }
    }
    if interrupted {
        set_fault(task,"SERVICE_INTERRUPTED","service restarted during audio recording; verified prefix recovered, no automatic resume");
    } else if let Some(error) = fault {
        if task["state"] == "failed" && task["fault"].is_object() {
            // A torn write is evidence of the original failure, not a replacement cause.
            // Keep this diagnostic stable across repeated startup scans.
            let message = task["fault"]["message"].as_str().unwrap_or("");
            if !message.contains(&error) {
                task["fault"]["message"] = json!(format!("{message}; recovery: {error}"));
            }
        } else {
            set_fault(task, "ARCHIVE_CORRUPT", &error);
        }
    } else if completed && files.is_empty() {
        set_fault(task, "ARCHIVE_INCOMPLETE", "missing audio archive");
    }
    let reason = if task["state"] == "failed" {
        json!("failed")
    } else {
        last_reason
    };
    for s in task["tracks"][0]["segments"].as_array_mut().unwrap() {
        if s["end_reason"].is_null() {
            s["end_reason"] = reason.clone();
        }
    }
    task["accepted_bytes"] = json!(number(&task["accepted_bytes"])
        .max(number(&task["tracks"][0]["raw_bytes"]))
        .to_string());
    task["queued_bytes"] = json!("0");
    persist(path, task)
}

/// Validate every record in order, including the exact recovered prefix; retain only
/// bounded segment summaries and the current PCM block.
pub(crate) fn visit(
    path: &FsPath,
    task: &Value,
    cancel: &AtomicBool,
    mut output: impl FnMut(&[u8], &Value, &[u8], &str, u64) -> io::Result<()>,
) -> io::Result<(Value, u64, [u8; 64])> {
    if task["tracks"][0]["kind"] != "audio" {
        return Err(io_error("not an audio recording"));
    }
    let mut checked = task.clone();
    checked["tracks"][0]["segments"] = json!([]);
    checked["tracks"][0]["blocks"] = json!("0");
    checked["tracks"][0]["raw_bytes"] = json!("0");
    let volumes = task["tracks"][0]["volumes"]
        .as_array()
        .ok_or_else(|| io_error("audio volumes missing"))?;
    if volumes.len() > MAX_VOLUMES {
        return Err(io_error("audio volume limit exceeded"));
    }
    let mut hash = Sha512::new();
    let mut lines = 0u64;
    for (ordinal, volume) in volumes.iter().enumerate() {
        if volume["file"] != format!("audio-{ordinal:06}.wba") {
            return Err(io_error("invalid audio volume name"));
        }
        let limit = number(&volume["valid_bytes"]);
        let scanned = scan(&path.join(volume["file"].as_str().unwrap()), limit, |raw| {
            if cancel.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "audio download cancelled",
                ));
            }
            let (mut row, pcm) = decode(raw)?;
            let id = update(&mut checked, &row, pcm.len())?;
            let segment = checked["tracks"][0]["segments"]
                .as_array()
                .unwrap()
                .last()
                .unwrap();
            let offset = number(&segment["samples"]) - row["audio"]["frames"].as_u64().unwrap();
            row["segment_id"] = json!(id);
            row["segment_sample_start"] = json!(offset.to_string());
            let line = serde_json::to_vec(&row)?;
            lines = lines
                .checked_add(line.len() as u64 + 1)
                .ok_or_else(|| io_error("audio metadata length overflow"))?;
            hash.update(raw);
            output(&line, &row, pcm, &id, offset)
        })?;
        if let Some(error) = scanned.error {
            return Err(io_error(error));
        }
        if scanned.valid != limit || (volume["complete"] == true && !scanned.complete) {
            return Err(io_error("audio checked prefix mismatch"));
        }
    }
    for key in ["blocks", "raw_bytes"] {
        if checked["tracks"][0][key] != task["tracks"][0][key] {
            return Err(io_error("audio archive count mismatch"));
        }
    }
    // End reasons are task lifecycle state; all sample/source summaries must match.
    let mut declared = task["tracks"][0]["segments"].clone();
    let mut observed = checked["tracks"][0]["segments"].clone();
    for segments in [&mut declared, &mut observed] {
        for segment in segments
            .as_array_mut()
            .ok_or_else(|| io_error("missing audio segments"))?
        {
            segment["end_reason"] = Value::Null;
        }
    }
    if declared != observed {
        return Err(io_error("audio segment/source summary mismatch"));
    }
    Ok((checked, lines, hash.finalize().into()))
}
fn pad(n: u64) -> usize {
    ((512 - n % 512) % 512) as usize
}
/// WAVEFORMATEX IEEE Float32 with the required fact sample count, no quantization.
pub(super) fn wav(output: &mut (impl Write + ?Sized), segment: &Value) -> io::Result<u64> {
    let channels = segment["channels"]
        .as_u64()
        .ok_or_else(|| io_error("audio channels missing"))?;
    let rate = segment["actual_rate_hz"]
        .as_f64()
        .ok_or_else(|| io_error("audio rate missing"))? as u32;
    let frames = number(&segment["samples"]);
    let bytes = frames
        .checked_mul(channels * 4)
        .filter(|n| *n <= MAX_WAV_BYTES)
        .ok_or_else(|| io_error("WAV RIFF size limit exceeded"))?;
    let mut h = Vec::with_capacity(58);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&((bytes + 50) as u32).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&18u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes());
    h.extend_from_slice(&(channels as u16).to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * channels as u32 * 4).to_le_bytes());
    h.extend_from_slice(&(channels as u16 * 4).to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(b"fact");
    h.extend_from_slice(&4u32.to_le_bytes());
    h.extend_from_slice(&(frames as u32).to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(bytes as u32).to_le_bytes());
    output.write_all(&h)?;
    Ok(bytes + 58)
}
pub(crate) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !params.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    super::super::recording_export::download(app,task,"audio.tar",|path,task,cancel| {
        let (_,lines,hash) = visit(path,task,cancel,|_,_,_,_,_|Ok(()))?;
        if lines > 0o77777777777 { return Err(io_error("audio timeline exceeds POSIX tar limit")); }
        let manifest = serde_json::to_vec(&json!({"format":"websa-audio-export-v1","task":task,
            "source":"published_audio; includes backend gain/AGC/NR/squelch or codec output as configured",
            "encoding":"IEEE Float32 LE WAV; original sample bits, no resampling or quantization",
            "local_controls":"browser volume/pan/mute/solo excluded","protocol_input":false,
            "time":"original AUD2 per-frame provenance in frames.jsonl; no implied absolute UTC or gap filling"}))?;
        if manifest.len() > MAX_PLAN + 1024*1024 { return Err(io_error("audio manifest exceeds bound")); }
        let mut length = 2048 + manifest.len() as u64 + pad(manifest.len() as u64) as u64 + lines + pad(lines) as u64;
        for segment in task["tracks"][0]["segments"].as_array().unwrap() {
            if number(&segment["samples"]) == 0 {continue;}
            let n = wav(&mut io::sink(),segment)?;
            length = length.checked_add(512+n+pad(n) as u64).ok_or_else(||io_error("audio tar length overflow"))?;
        }
        Ok((length,(manifest,lines,hash)))
    },|output,path,task,(manifest,lines,hash),cancel| {
        use super::super::recording_export::header;
        header(output,"task.json",manifest.len() as u64)?; output.write_all(manifest)?;output.write_all(&[0;512][..pad(manifest.len() as u64)])?;
        header(output,"frames.jsonl",*lines)?;
        let (_,actual,digest) = visit(path,task,cancel,|line,_,_,_,_|{output.write_all(line)?;output.write_all(b"\n")})?;
        if actual != *lines || digest != *hash {return Err(io_error("audio timeline changed during export"));}
        output.write_all(&[0;512][..pad(*lines)])?;
        let mut current = String::new(); let mut written = 0u64; let mut expected = 0u64;
        let (_,_,digest) = visit(path,task,cancel,|_,_,pcm,id,_| {
            if pcm.is_empty() {return Ok(());}
            if current != id {
                if !current.is_empty() {
                    if written != expected {return Err(io_error("audio segment length changed"));}
                    output.write_all(&[0;512][..pad(written+58)])?;
                }
                let segment = task["tracks"][0]["segments"].as_array().unwrap().iter().find(|s|s["segment_id"]==id).ok_or_else(||io_error("unknown audio segment"))?;
                let n = wav(&mut io::sink(),segment)?;
                header(output,&format!("{id}.wav"),n)?;wav(output,segment)?;
                current = id.to_owned(); written = 0; expected = n-58;
            }
            output.write_all(pcm)?;written += pcm.len() as u64;Ok(())
        })?;
        if digest != *hash || written != expected {return Err(io_error("audio changed after export verification"));}
        if !current.is_empty() {output.write_all(&[0;512][..pad(written+58)])?;}
        output.write_all(&[0;1024])
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "websa-audio-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn input(seq: u32, index: u64, channels: u32, rate: f64, gain: f64) -> Input {
        let samples = if seq == 0 {
            vec![]
        } else {
            vec![0.25, -0.0, f32::from_bits(1), -0.75].repeat(channels as usize)
        };
        let bytes = websa_channelizer::encode_audio(
            websa_channelizer::AudioFrameMetadata {
                seq,
                rate,
                frequency_hz: 100e6,
                channel_id: 45,
                epoch: 46,
                input_index: 9007199254740999 + index,
                delay_seconds: 0.001,
                channels,
                audio_index: index,
                input_rate: 48840.130763116205,
                input_fraction_num: 1,
                input_fraction_den: 3,
                delay_known: false,
                causal_presentation: true,
            },
            &samples,
        )
        .unwrap()
        .into();
        let source=serde_json::to_vec(&json!({"receiver_uid":"uid","service_id":"service",
            "config":{"mode":"ft8","volume":gain},"baseband_stream":{"epoch":"1","channel_id":"2"}})).unwrap().into();
        Input { bytes, source }
    }
    fn finish(manager: &mut Manager) -> Value {
        let id = manager.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_string();
        manager.stop(&id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !manager.lanes[0].lane.done.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        manager.reap();
        manager.snapshots()[0].clone()
    }
    #[test]
    fn processing_failure_is_scoped_to_uid_and_healthy_audio_remains_readable() {
        let root=directory();let mut manager=Manager::open(root.clone()).unwrap();
        for (uid,kind) in [("uid","audio"),("healthy","audio"),("uid","iq")] {
            manager.start_kind(uid,"service",&format!("{uid}-{kind}"),kind).unwrap().wait().unwrap();
        }
        let healthy=|sequence,index| {
            let mut block=input(sequence,index,1,48000.0,1.0);
            let mut source:Value=serde_json::from_slice(&block.source).unwrap();source["receiver_uid"]=json!("healthy");
            block.source=serde_json::to_vec(&source).unwrap().into();block
        };
        manager.admit_audio("uid",input(1,0,1,48000.0,1.0));
        manager.admit_audio("healthy",healthy(1,0));
        manager.processing_failed("uid","C3 controlled inference failure");
        manager.admit_audio("healthy",healthy(2,4));
        let tasks=manager.snapshots();
        assert_eq!(tasks.iter().find(|task|task["receiver_uid"]=="uid"&&task["tracks"][0]["kind"]=="audio").unwrap()["fault"]["code"],"PROCESSING_FAILED");
        assert!(tasks.iter().filter(|task|task["receiver_uid"]=="healthy"||task["tracks"][0]["kind"]=="iq")
            .all(|task|task["fault"].is_null()));
        manager.shutdown();let tasks=manager.snapshots();
        let task=tasks.iter().find(|task|task["receiver_uid"]=="healthy").unwrap();
        let mut samples=Vec::new();
        visit(&root.join(task["task_id"].as_str().unwrap()),task,&AtomicBool::new(false),|_,_,pcm,_,_| {
            samples.extend_from_slice(pcm);Ok(())
        }).unwrap();
        let bits:Vec<_>=samples.chunks_exact(4).map(|sample|u32::from_le_bytes(sample.try_into().unwrap())).collect();
        assert_eq!(bits,vec![0.25_f32.to_bits(),(-0.0_f32).to_bits(),1,(-0.75_f32).to_bits()].repeat(2));
        drop(manager);fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_audio_range_preserves_stereo_bits_clocks_and_immutable_snapshot() {
        use super::super::audio_read;
        use super::super::super::recording_read::Selection;
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager.start_kind("uid", "service", "range", "audio").unwrap().wait().unwrap();
        let origin = 9007199254741011;
        let blocks = [input(11, origin, 2, 32000.0, 0.5), input(12, origin + 4, 2, 32000.0, 0.5), input(13, origin + 8, 2, 32000.0, 0.25)];
        for block in &blocks { manager.admit_audio("uid", block.clone()); }
        let task = finish(&mut manager);
        let path = root.join(task["task_id"].as_str().unwrap());
        let mut selection = Selection { segment: "audio-000000".into(), start: 3, count: 4 };
        let flag = AtomicBool::new(false);
        let snapshot = audio_read::prepare(&path, &task, &selection, &flag).unwrap();
        let mut data = Vec::new(); snapshot.emit(&mut data, &flag).unwrap();
        assert_eq!(data.len() as u64, snapshot.length());
        let mut files = HashMap::new();
        for entry in tar::Archive::new(data.as_slice()).entries().unwrap() {
            use std::io::Read;
            let mut entry = entry.unwrap(); let name = entry.path().unwrap().to_str().unwrap().to_owned();
            let mut bytes = Vec::new(); entry.read_to_end(&mut bytes).unwrap(); files.insert(name, bytes);
        }
        let expected: Vec<u8> = blocks[..2].iter().flat_map(|b| b.bytes[96..].iter().copied()).collect();
        assert_eq!(&files["audio.wav"][58..], &expected[24..56]);
        assert_eq!(u32_at(&files["audio.wav"], 46), 4);
        let manifest: Value = serde_json::from_slice(&files["selection.json"]).unwrap();
        assert_eq!(manifest["selection"]["first_audio_index"], (origin + 3).to_string());
        assert_eq!(manifest["task"], task);
        let rows: Vec<Value> = String::from_utf8(files["frames.jsonl"].clone()).unwrap().lines().map(|s|serde_json::from_str(s).unwrap()).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["selection"]["record_skip_frames"], "3");
        assert_eq!(rows[1]["selection"]["range_sample_start"], "1");
        assert_eq!(rows[0]["audio"]["audio_index"], origin.to_string());
        assert_eq!(rows[0]["audio"]["input_fraction_num"], "1");
        assert_eq!(rows[0]["audio"]["input_fraction_den"], "3");
        assert_eq!(rows[0]["original_aud2_header_hex"].as_str().unwrap(), blocks[0].bytes[..96].iter().map(|b|format!("{b:02x}")).collect::<String>());
        selection.count = 6; // Must not spill into the gain/configuration boundary.
        assert!(audio_read::prepare(&path, &task, &selection, &flag).is_err());
        selection.count = 4;
        assert!(audio_read::prepare(&path, &task, &selection, &AtomicBool::new(true)).is_err());
        let file = path.join("audio-000000.wba"); let mut bytes = fs::read(&file).unwrap(); bytes[30] ^= 1; fs::write(file,bytes).unwrap();
        assert!(audio_read::prepare(&path, &task, &selection, &flag).is_err());
        let mut same = Vec::new(); snapshot.emit(&mut same, &flag).unwrap(); assert_eq!(same,data);
        drop(manager); fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn exact_pcm_config_boundaries_and_same_processing_format_restart_survive_recovery() {
        let root = directory();
        let mut m = Manager::open(root.clone()).unwrap();
        m.start_kind("uid", "service", "audio", "audio")
            .unwrap()
            .wait()
            .unwrap();
        let inputs = vec![
            input(12, 80, 1, 48000.0, 0.5),
            input(13, 84, 1, 48000.0, 0.25),
            input(0, 0, 2, 24000.0, 0.25),
            input(1, 0, 2, 24000.0, 0.25),
        ];
        for block in &inputs {
            m.admit_audio("uid", block.clone());
        }
        let t = finish(&mut m);
        assert_eq!(t["state"], "completed");
        assert_eq!(t["accepted_bytes"], "64");
        let segments = t["tracks"][0]["segments"].as_array().unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0]["prefix_missing"], true);
        assert_eq!(segments[0]["first_input_index"], "9007199254741079");
        assert_eq!(segments[0]["fraction_num"], "1");
        assert_eq!(segments[2]["actual_rate_hz"], 24000.0);
        assert_eq!(segments[2]["samples"], "4");
        let path = root.join(t["task_id"].as_str().unwrap());
        let mut actual = Vec::new();
        visit(&path, &t, &AtomicBool::new(false), |_, _, pcm, _, _| {
            actual.extend_from_slice(pcm);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            actual,
            inputs
                .iter()
                .flat_map(|i| i.bytes[96..].iter().copied())
                .collect::<Vec<_>>()
        );
        let mut wave = Vec::new();
        wav(&mut wave, &segments[2]).unwrap();
        assert_eq!(&wave[..4], b"RIFF");
        assert_eq!(u32_at(&wave, 4), 82);
        assert_eq!(&wave[38..42], b"fact");
        assert_eq!(u32_at(&wave, 46), 4);
        drop(m);
        let m = Manager::open(root.clone()).unwrap();
        assert_eq!(m.snapshots()[0]["tracks"], t["tracks"]);
        drop(m);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn flac_export_preserves_source_boundaries_and_detects_corruption_and_cancellation() {
        let root = directory();
        let mut m = Manager::open(root.clone()).unwrap();
        m.start_kind("uid", "service", "flac", "audio").unwrap().wait().unwrap();
        let empty = m.snapshots()[0].clone();
        let path = root.join(empty["task_id"].as_str().unwrap());
        let cancel = AtomicBool::new(false);
        let (n, plan) = super::super::flac::prepare(&path, &empty, &cancel).unwrap();
        let mut bytes = Vec::new();
        super::super::flac::emit(&mut bytes, &path, &empty, &plan, &cancel).unwrap();
        assert_eq!(n, bytes.len() as u64);
        assert_eq!(tar::Archive::new(&bytes[..]).entries().unwrap().count(), 2);
        m.admit_audio("uid", input(1, 0, 1, 48000.0, 0.5));
        m.admit_audio("uid", input(0, 0, 2, 24000.0, 0.25));
        m.admit_audio("uid", input(1, 0, 2, 24000.0, 0.25));
        let task = finish(&mut m);
        let file = path.join("audio-000000.wba");
        let original = fs::read(&file).unwrap();
        let (n, plan) = super::super::flac::prepare(&path, &task, &cancel).unwrap();
        bytes.clear();
        super::super::flac::emit(&mut bytes, &path, &task, &plan, &cancel).unwrap();
        assert_eq!(n, bytes.len() as u64);
        let mut archive = tar::Archive::new(&bytes[..]);
        let names: Vec<_> = archive.entries().unwrap().map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["task.json", "frames.jsonl", "audio-000000.flac", "audio-000001.flac"]);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(super::super::flac::prepare(&path, &task, &AtomicBool::new(true)).is_err());
        let mut damaged = original; damaged[24] ^= 1; fs::write(&file, damaged).unwrap();
        assert!(super::super::flac::prepare(&path, &task, &cancel).is_err());
        assert!(super::super::flac::emit(&mut io::sink(), &path, &task, &plan, &cancel).is_err());
        drop(m); fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn corrupt_tail_exports_only_verified_original_prefix_after_restart() {
        let root = directory();
        let mut m = Manager::open(root.clone()).unwrap();
        m.start_kind("uid", "service", "a", "audio")
            .unwrap()
            .wait()
            .unwrap();
        m.admit_audio("uid", input(8, 0, 1, 48000.0, 0.5));
        m.admit_audio("uid", input(9, 4, 1, 48000.0, 0.5));
        let t = finish(&mut m);
        let path = root.join(t["task_id"].as_str().unwrap());
        let file = path.join("audio-000000.wba");
        let mut bytes = fs::read(&file).unwrap();
        let next = 8 + 16 + u32_at(&bytes, 16) as usize;
        bytes[next + 16] ^= 1;
        fs::write(&file, bytes).unwrap();
        assert!(visit(&path, &t, &AtomicBool::new(false), |_, _, _, _, _| Ok(())).is_err());
        drop(m);
        let m = Manager::open(root.clone()).unwrap();
        let recovered = m.snapshots()[0].clone();
        assert_eq!(recovered["state"], "failed");
        assert_eq!(recovered["tracks"][0]["blocks"], "1");
        assert_eq!(recovered["service_id"], "service");
        visit(
            &path,
            &recovered,
            &AtomicBool::new(false),
            |_, _, _, _, _| Ok(()),
        )
        .unwrap();
        drop(m);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn silent_codec_can_wait_and_processing_failure_leaves_iq_running_with_shared_worker_limit() {
        let root = directory();
        let mut m = Manager::open(root.clone()).unwrap();
        for kind in ["audio", "iq", "results"] {
            m.start_kind("uid", "service", kind, kind)
                .unwrap()
                .wait()
                .unwrap();
        }
        for index in 3..MAX_ACTIVE_TASKS {
            m.start_kind(&format!("other-{index}"),"service",&format!("fill-{index}"),"iq")
                .unwrap().wait().unwrap();
        }
        assert!(m
            .start_kind("other", "service", "overflow", "audio")
            .is_err());
        m.lanes[0].lane.queue.lock().unwrap().last_input = Instant::now() - Duration::from_secs(9);
        m.tick(None);
        assert!(m.snapshots()[0]["fault"].is_null());
        m.processing_kind_sources("audio", &[], Some("codec failed"));
        assert_eq!(m.snapshots()[0]["fault"]["code"], "PROCESSING_FAILED");
        assert!(m.snapshots()[1]["fault"].is_null());
        assert!(m.snapshots()[2]["fault"].is_null());
        m.shutdown();
        drop(m);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sample_gap_fails_the_audio_lane_and_durable_empty_start_recovers_original_identity() {
        let root = directory();
        let mut m = Manager::open(root.clone()).unwrap();
        m.start_kind("uid", "service", "a", "audio")
            .unwrap()
            .wait()
            .unwrap();
        m.admit_audio("uid", input(9, 0, 1, 48000.0, 0.5));
        m.admit_audio("uid", input(10, 5, 1, 48000.0, 0.5));
        let t = finish(&mut m);
        assert_eq!(t["state"], "failed");
        assert_eq!(t["tracks"][0]["blocks"], "1");
        let path = root.join(t["task_id"].as_str().unwrap());
        visit(&path, &t, &AtomicBool::new(false), |_, _, _, _, _| Ok(())).unwrap();
        drop(m);
        fs::remove_dir_all(root).unwrap();
        let root = directory();
        let mut m = Manager::open(root.clone()).unwrap();
        m.start_kind("uid", "service", "empty", "audio")
            .unwrap()
            .wait()
            .unwrap();
        let mut t = m.snapshots()[0].clone();
        let path = root.join(t["task_id"].as_str().unwrap());
        assert_eq!(fs::read(path.join("audio-000000.wba")).unwrap(), MAGIC);
        m.shutdown();
        drop(m);
        t["state"] = json!("waiting");
        persist(&path, &t).unwrap();
        let m = Manager::open(root.clone()).unwrap();
        let recovered = m.snapshots()[0].clone();
        assert_eq!(recovered["fault"]["code"], "SERVICE_INTERRUPTED");
        assert!(!m.has_active());
        assert_eq!(recovered["tracks"][0]["blocks"], "0");
        drop(m);
        fs::remove_dir_all(root).unwrap();
    }
}
