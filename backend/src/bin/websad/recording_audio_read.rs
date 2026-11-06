//! Bounded original published PCM snapshot. Validate the complete journal before
//! returning selected sample frames; never attach a historical UID to a live slot.
use super::super::recording_read::Selection;
use super::*;
use sha2::{Digest, Sha256};

const MAX_FRAMES: u64 = 1_048_576;
const MAX_SECONDS: f64 = 60.0;
const MAX_TIMELINE: usize = 4 * 1024 * 1024;
const MAX_MANIFEST: usize = 2 * 1024 * 1024;

pub(super) struct Snapshot {
    wav: Vec<u8>,
    timeline: Vec<u8>,
    manifest: Vec<u8>,
}
impl Snapshot {
    fn members(&self) -> [(&str, &[u8]); 3] {
        [
            ("selection.json", &self.manifest),
            ("frames.jsonl", &self.timeline),
            ("audio.wav", &self.wav),
        ]
    }
    pub(super) fn length(&self) -> u64 {
        1024 + self
            .members()
            .iter()
            .map(|(_, b)| 512 + (b.len() as u64).div_ceil(512) * 512)
            .sum::<u64>()
    }
    pub(super) fn emit(&self, output: &mut dyn Write, cancel: &AtomicBool) -> io::Result<()> {
        for (name, bytes) in self.members() {
            if cancel.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "audio range cancelled",
                ));
            }
            super::super::recording_export::header(output, name, bytes.len() as u64)?;
            output.write_all(bytes)?;
            output.write_all(&[0; 512][..(512 - bytes.len() % 512) % 512])?;
        }
        output.write_all(&[0; 1024])
    }
}

pub(super) fn prepare(
    path: &FsPath,
    task: &Value,
    selection: &Selection,
    cancel: &AtomicBool,
) -> io::Result<Snapshot> {
    if task["tracks"][0]["kind"] != "audio" {
        return Err(io_error("audio range requires an audio recording"));
    }
    let segment = task["tracks"][0]["segments"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["segment_id"] == selection.segment))
        .ok_or_else(|| io_error("unknown audio segment"))?;
    let end = selection
        .start
        .checked_add(selection.count)
        .ok_or_else(|| io_error("audio range overflow"))?;
    let rate = segment["actual_rate_hz"]
        .as_f64()
        .ok_or_else(|| io_error("missing audio rate"))?;
    let channels = segment["channels"]
        .as_u64()
        .ok_or_else(|| io_error("missing audio channels"))?;
    if !(1..=MAX_FRAMES).contains(&selection.count)
        || end > number(&segment["samples"])
        || !rate.is_finite()
        || !(8000.0..=192000.0).contains(&rate)
        || rate.fract() != 0.0
        || !(1..=2).contains(&channels)
        || selection.count as f64 / rate > MAX_SECONDS
    {
        return Err(io_error(
            "select 1..1048576 sample frames within one segment and at most 60 seconds",
        ));
    }
    let stride = channels as usize * 4;
    let mut wav = Vec::with_capacity(selection.count as usize * stride + 58);
    let mut selected = segment.clone();
    selected["samples"] = json!(selection.count.to_string());
    audio::wav(&mut wav, &selected)?;
    let mut timeline = Vec::new();
    let mut next = selection.start;
    let mut records = 0;
    let mut hash = Sha256::new();
    let direct = super::audio_files::is_direct(task);
    let saved_pcm = if direct {
        super::audio_files::verify(path, task, cancel)?;
        super::audio_files::read_pcm(path, task, segment, selection.start, selection.count, cancel)?
    } else { Vec::new() };
    // Existing journals have no random-access audio index. Stream-check every record
    // and summary (including later segments), retaining only the chosen PCM/timeline.
    let mut collect = |row: &Value, pcm: &[u8], id: &str, offset: u64| -> io::Result<()> {
        if id != selection.segment {
            return Ok(());
        }
        let stop = offset
            .checked_add(row["audio"]["frames"].as_u64().unwrap())
            .ok_or_else(|| io_error("audio clock overflow"))?;
        let from = offset.max(selection.start);
        let to = stop.min(end);
        if from >= to {
            return Ok(());
        }
        if from != next {
            return Err(io_error("audio range discontinuity"));
        }
        let skip = from - offset;
        let count = to - from;
        let bytes = if direct {
            &saved_pcm[(from - selection.start) as usize * stride..(to - selection.start) as usize * stride]
        } else { &pcm[skip as usize * stride..(skip + count) as usize * stride] };
        hash.update(bytes);
        wav.extend_from_slice(bytes);
        let mut selected_row = row.clone();
        selected_row["selection"] = json!({"record_skip_frames":skip.to_string(),"frames":count.to_string(),"range_sample_start":(from-selection.start).to_string()});
        let line = serde_json::to_vec(&selected_row)?;
        records += 1;
        if records > 4096 || timeline.len() + line.len() + 1 > MAX_TIMELINE {
            return Err(io_error(
                "audio range provenance exceeds 4096 records / 4 MiB",
            ));
        }
        timeline.extend_from_slice(&line);
        timeline.push(b'\n');
        next = to;
        Ok(())
    };
    if direct {
        super::audio_files::visit_timeline(path, task, cancel, |row| {
            let id = row["segment_id"].as_str().ok_or_else(|| io_error("audio timeline segment missing"))?;
            collect(&row, &[], id, number(&row["segment_sample_start"]))
        })?;
    } else { audio::visit(path, task, cancel, |_, row, pcm, id, offset| collect(row, pcm, id, offset))?; }
    if next != end || wav.len() != 58 + selection.count as usize * stride {
        return Err(io_error("audio range missing samples"));
    }
    let first_audio_index = number(&segment["first_audio_index"])
        .checked_add(selection.start)
        .ok_or_else(|| io_error("audio index overflow"))?;
    let manifest = serde_json::to_vec(&json!({"format":"websa-audio-range-v1","task":task,
        "selection":{"segment":selection.segment,"start":selection.start.to_string(),"count":selection.count.to_string(),"first_audio_index":first_audio_index.to_string()},
        "audio":{"source":"published_audio","sample_format":"f32_le","rate_hz":rate,"channels":channels,"frames":selection.count.to_string(),"pcm_sha256":format!("{:x}",hash.finalize()),"float32_bit_exact":!direct || task["audio_format"]=="wav"},
        "processing":{"resampled":direct && task["audio_format"]=="opus","quantized":direct && task["audio_format"]=="flac","lossy":direct && task["audio_format"]=="opus","gain_applied":false,"protocol_input":false,"local_controls_included":false},
        "time":{"playback_origin":"selected first audio sample; relative seconds","original_clock":"unchanged AUD2 rows/headers plus exact record_skip_frames; input index/fraction not interpolated","utc_reliable":false},
        "provenance":{"file":"frames.jsonl","records":records,"sha256":format!("{:x}",Sha256::digest(&timeline))}}))?;
    if manifest.len() > MAX_MANIFEST {
        return Err(io_error("audio range manifest exceeds 2 MiB"));
    }
    Ok(Snapshot {
        wav,
        timeline,
        manifest,
    })
}

pub(crate) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let selection = if params
        .keys()
        .any(|k| !matches!(k.as_str(), "segment" | "start" | "count"))
    {
        Err(io_error("unknown audio range parameter"))
    } else {
        Selection::parse_limit(&params, MAX_FRAMES)
    };
    let selection = match selection {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":e.to_string()})),
            )
                .into_response()
        }
    };
    super::super::recording_export::download(
        app,
        task,
        "audio-range.tar",
        move |path, task, cancel| {
            let snapshot = prepare(path, task, &selection, cancel)?;
            Ok((snapshot.length(), snapshot))
        },
        |output, _, _, snapshot, cancel| snapshot.emit(output, cancel),
    )
    .await
}
