//! Bounded, checked reads of immutable IQ tasks. Never attaches to a receiver or holds
//! acquisition state while opening, seeking, decompressing or packaging a recording.
use super::*;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::Path as FsPath,
};
use websa_channelizer::iq_archive::v2;

const MAX_SAMPLES: u64 = 1_048_576;
const MAX_BLOCKS: usize = 4096;
const MAX_METADATA: usize = 4 * 1024 * 1024;
pub(super) static READER: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

fn invalid(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}
fn integer(value: &str) -> io::Result<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("expected unsigned decimal integer"));
    }
    value.parse().map_err(invalid)
}

#[derive(Clone)]
pub(super) struct Selection {
    pub(super) segment: String,
    pub(super) start: u64,
    pub(super) count: u64,
}
impl Selection {
    fn parse(params: &HashMap<String, String>) -> io::Result<Self> {
        Self::parse_limit(params, MAX_SAMPLES)
    }
    pub(super) fn parse_limit(params: &HashMap<String, String>, limit: u64) -> io::Result<Self> {
        let segment = params
            .get("segment")
            .ok_or_else(|| invalid("segment is required"))?
            .clone();
        let start = integer(
            params
                .get("start")
                .ok_or_else(|| invalid("start is required"))?,
        )?;
        let count = integer(
            params
                .get("count")
                .ok_or_else(|| invalid("count is required"))?,
        )?;
        if count == 0 || count > limit || start.checked_add(count).is_none() {
            return Err(invalid(format!(
                "range must contain 1..{limit} samples without overflow"
            )));
        }
        Ok(Self {
            segment,
            start,
            count,
        })
    }
}

#[derive(Debug)]
struct Selected {
    raw: Vec<u8>,
    metadata: Value,
}

pub(super) fn visit_range(
    path: &FsPath,
    task: &Value,
    selection: &Selection,
    cancelled: &AtomicBool,
    mut visit: impl FnMut(&[u8], &Value) -> io::Result<()>,
) -> io::Result<Value> {
    let track = &task["tracks"][0];
    let segment = track["segments"]
        .as_array()
        .and_then(|items| items.iter().find(|s| s["segment_id"] == selection.segment))
        .ok_or_else(|| invalid("unknown recording segment"))?;
    let samples = integer(
        segment["samples"]
            .as_str()
            .ok_or_else(|| invalid("segment sample count absent"))?,
    )?;
    if selection
        .start
        .checked_add(selection.count)
        .is_none_or(|end| end > samples)
    {
        return Err(invalid("range exceeds the verified segment"));
    }
    let first_output = integer(
        segment["first_output_index"]
            .as_str()
            .ok_or_else(|| invalid("segment origin absent"))?,
    )?;
    let begin = first_output
        .checked_add(selection.start)
        .ok_or_else(|| invalid("output index overflow"))?;
    let end = begin
        .checked_add(selection.count)
        .ok_or_else(|| invalid("output index overflow"))?;
    let mut next = begin;
    'volumes: for volume in track["volumes"]
        .as_array()
        .ok_or_else(|| invalid("volumes absent"))?
    {
        let name = volume["file"]
            .as_str()
            .ok_or_else(|| invalid("volume filename absent"))?;
        // Only generated basenames from the manifest are accepted, never query-supplied paths.
        if !name.starts_with("iq-")
            || !name.ends_with(".wbiq")
            || name.contains(['/', '\\'])
            || name.contains("..")
        {
            return Err(invalid("invalid archive filename"));
        }
        let file_path = path.join(name);
        let mut archive = File::open(&file_path)?;
        let mut reader = BufReader::new(File::open(file_path.with_extension("jsonl"))?);
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "range request cancelled",
                ));
            }
            // Take bounds the read even if an index has been damaged after startup recovery.
            // Actual record and CF32 CRCs are checked by read_at, not trusted from sidecars.
            let mut line = Vec::new();
            let length = (&mut reader)
                .take(256 * 1024 + 1)
                .read_until(b'\n', &mut line)?;
            if length == 0 {
                break;
            }
            if length > 256 * 1024 {
                return Err(invalid("index entry exceeds bounds"));
            }
            let index: v2::Index = serde_json::from_slice(&line)?;
            if index.meta.segment_id != selection.segment {
                continue;
            }
            let clock = &index.meta.clock;
            let block_start = integer(&clock.first_output_index)?;
            let block_end = block_start
                .checked_add(u64::from(clock.samples))
                .ok_or_else(|| invalid("block index overflow"))?;
            if block_end <= begin {
                continue;
            }
            if block_start >= end {
                break 'volumes;
            }
            let from = block_start.max(begin);
            let to = block_end.min(end);
            if from != next
                || index.meta.service_id != task["service_id"]
                || clock.receiver_uid != task["receiver_uid"]
                || clock.track_id != track["track_id"]
                || clock.epoch != segment["stream"]["epoch"]
                || clock.channel_id != segment["stream"]["channel_id"]
                || clock.source_id != segment["stream"]["source_id"]
                || json!(clock.actual_rate_hz) != segment["actual_rate_hz"]
                || json!(clock.input_rate_hz) != segment["input_rate_hz"]
                || json!(index.meta.frequency_hz) != segment["frequency_hz"]
                || index.meta.baseband != segment["baseband"]
            {
                return Err(invalid(
                    "range contains a gap, overlap or changed source configuration",
                ));
            }
            let bytes = v2::read_at(&mut archive, &index)?;
            let skip = from - block_start;
            // Keep the exact block origin and the trim offset separately: no f64 arithmetic
            // on u64 source clocks, no invented UTC or assumed rational resampler cadence.
            let annotation = json!({"core:sample_start":from-begin,"core:sample_count":to-from,
                "websa:clock":clock,"websa:skip_samples":skip.to_string(),"websa:sequence":index.meta.sequence,
                "websa:config_revision":clock.config_revision});
            visit(
                &bytes[skip as usize * 8..(to - block_start) as usize * 8],
                &annotation,
            )?;
            next = to;
            if next == end {
                break 'volumes;
            }
        }
    }
    if next != end {
        return Err(invalid("verified range is incomplete"));
    }
    Ok(json!({
        "global":{"core:datatype":"cf32_le","core:version":"1.2.6","core:sample_rate":segment["actual_rate_hz"],
            "core:recorder":"HAROGIC WebSA","core:description":"Selected verified IQ segment; source UTC is unknown.",
            "core:extensions":[{"name":"websa","version":"1.0.0","optional":true}],
            "websa:task_id":task["task_id"],"websa:service_id":task["service_id"],"websa:receiver_uid":task["receiver_uid"],
            "websa:segment_id":selection.segment,"websa:range_start":selection.start.to_string(),
            "websa:sample_count":selection.count.to_string(),"websa:task_state":task["state"],"websa:task_fault":task["fault"],
            "websa:utc_reliable":false,"websa:baseband":segment["baseband"]},
        "captures":[{"core:sample_start":0,"core:frequency":segment["frequency_hz"]}],"annotations":[]
    }))
}

fn read_range(path: &FsPath, task: &Value, selection: &Selection) -> io::Result<Selected> {
    let mut raw = Vec::with_capacity(selection.count as usize * 8);
    let mut annotations = Vec::new();
    let mut metadata_bytes = 0;
    let mut metadata = visit_range(
        path,
        task,
        selection,
        &AtomicBool::new(false),
        |bytes, annotation| {
            if annotations.len() >= MAX_BLOCKS {
                return Err(invalid("range exceeds 4096 blocks; choose a smaller range"));
            }
            metadata_bytes += serde_json::to_vec(annotation)?.len();
            if metadata_bytes > MAX_METADATA {
                return Err(invalid(
                    "range metadata exceeds 4 MiB; choose a smaller range",
                ));
            }
            raw.extend_from_slice(bytes);
            annotations.push(annotation.clone());
            Ok(())
        },
    )?;
    metadata["annotations"] = json!(annotations);
    if serde_json::to_vec(&metadata)?.len() > MAX_METADATA {
        return Err(invalid(
            "range metadata exceeds 4 MiB; choose a smaller range",
        ));
    }
    Ok(Selected { raw, metadata })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use std::time::SystemTime;
    use websa_channelizer::iq_archive::IqBlockMeta;

    #[tokio::test]
    async fn slow_or_aborted_response_retains_and_releases_the_reader_budget() {
        let response = download(
            vec![1; 128 * 1024],
            "application/octet-stream",
            "recording.sigmf-data",
            READER.try_acquire().unwrap(),
        );
        let mut stream = response.into_body().into_data_stream();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.len(), 64 * 1024);
        assert!(READER.try_acquire().is_err());
        drop(stream);
        assert!(READER.try_acquire().is_ok());
    }

    fn fixture() -> (std::path::PathBuf, Value, Vec<u8>) {
        let path = std::env::temp_dir().join(format!(
            "websa-range-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let special = [
            0u32, 0x80000000, 0x7fc01234, 0x7f800000, 0xff800000, 1, 0x3f800001, 0xffc01234,
        ];
        let raw: Vec<u8> = special
            .into_iter()
            .cycle()
            .take(600)
            .flat_map(u32::to_le_bytes)
            .collect();
        let origin = u64::MAX - 1000;
        let mut volumes = Vec::new();
        for volume in 0..2 {
            let name = format!("iq-{volume:06}.wbiq");
            let mut writer = v2::Writer::new(File::create(path.join(&name)).unwrap()).unwrap();
            let mut sidecar = File::create(path.join(&name).with_extension("jsonl")).unwrap();
            for block in 0..3 {
                let position = volume * 150 + block * 50;
                let meta = v2::Meta {
                    service_id: "boot".into(),
                    segment_id: "7-9".into(),
                    sequence: (position / 50).to_string(),
                    frequency_hz: 100e6,
                    baseband: json!({"passband_hz":3000}),
                    clock: IqBlockMeta {
                        receiver_uid: "old-uid".into(),
                        source_id: "primary".into(),
                        track_id: "track".into(),
                        epoch: "7".into(),
                        channel_id: "9".into(),
                        config_revision: "9007199254740993".into(),
                        first_input_index: (origin + position).to_string(),
                        first_output_index: (origin + position).to_string(),
                        fraction_num: "1".into(),
                        fraction_den: "3".into(),
                        samples: 50,
                        sample_format: "cf32_le".into(),
                        amplitude_scale: 1.0,
                        input_rate_hz: 6028.163580246914,
                        actual_rate_hz: 6028.163580246914,
                        delay_seconds: Some(0.0123),
                    },
                };
                let index = writer
                    .append(
                        meta,
                        &raw[position as usize * 8..(position as usize + 50) * 8],
                    )
                    .unwrap();
                serde_json::to_writer(&mut sidecar, &index).unwrap();
                sidecar.write_all(b"\n").unwrap();
            }
            writer.finish().unwrap();
            volumes.push(json!({"file":name}));
        }
        let task = json!({"task_id":"task","service_id":"boot","receiver_uid":"old-uid","state":"failed","fault":{"code":"SERVICE_INTERRUPTED"},
            "tracks":[{"track_id":"track","volumes":volumes,"segments":[{"segment_id":"7-9","samples":"300","first_output_index":origin.to_string(),
                "stream":{"epoch":"7","channel_id":"9","source_id":"primary"},"frequency_hz":100e6,"baseband":{"passband_hz":3000},
                "actual_rate_hz":6028.163580246914,"input_rate_hz":6028.163580246914}]}]});
        (path, task, raw)
    }
    #[test]
    fn ranges_cross_blocks_and_volumes_preserving_cf32_bits_and_exact_clocks() {
        let (path, task, raw) = fixture();
        let selected = read_range(
            &path,
            &task,
            &Selection {
                segment: "7-9".into(),
                start: 137,
                count: 126,
            },
        )
        .unwrap();
        assert_eq!(selected.raw, raw[137 * 8..263 * 8]);
        assert_eq!(
            selected.metadata["global"]["core:sample_rate"],
            6028.163580246914
        );
        assert!(selected.metadata["captures"][0]
            .get("core:datetime")
            .is_none());
        assert_eq!(
            selected.metadata["annotations"][0]["websa:skip_samples"],
            "37"
        );
        assert_eq!(
            selected.metadata["annotations"][0]["websa:clock"]["fraction_den"],
            "3"
        );
        assert_eq!(
            selected.metadata["annotations"][0]["websa:clock"]["first_input_index"],
            (u64::MAX - 900).to_string()
        );
        let packed = sigmf(selected).unwrap();
        let mut archive = tar::Archive::new(packed.as_slice());
        let mut entries = archive.entries().unwrap();
        assert_eq!(
            entries
                .next()
                .unwrap()
                .unwrap()
                .path()
                .unwrap()
                .to_str()
                .unwrap(),
            "recording/recording.sigmf-meta"
        );
        let mut bytes = Vec::new();
        entries
            .next()
            .unwrap()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, raw[137 * 8..263 * 8]);
        assert!(entries.next().is_none());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn streaming_export_preserves_checked_ranges_and_refuses_changed_archives() {
        use super::super::recording_export;
        let (path, task, raw) = fixture();
        let selection = Selection {
            segment: "7-9".into(),
            start: 137,
            count: 126,
        };
        let cancel = AtomicBool::new(false);
        let plan = recording_export::prepare(&path, &task, &selection, &cancel).unwrap();
        let mut output = Vec::new();
        recording_export::archive(&mut output, &path, &task, &selection, &plan, &cancel).unwrap();
        let mut archive = tar::Archive::new(output.as_slice());
        let mut entries = archive.entries().unwrap();
        let mut first = entries.next().unwrap().unwrap();
        assert_eq!(
            first.path().unwrap().to_str().unwrap(),
            "recording/recording.sigmf-data"
        );
        let mut bytes = Vec::new();
        first.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, raw[137 * 8..263 * 8]);
        let meta: Value = serde_json::from_reader(entries.next().unwrap().unwrap()).unwrap();
        assert_eq!(meta["annotations"][0]["websa:skip_samples"], "37");
        assert_eq!(meta["global"]["core:sample_rate"], 6028.163580246914);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path.join("iq-000001.wbiq"))
            .unwrap();
        file.seek(SeekFrom::Start(30)).unwrap();
        file.write_all(b"X").unwrap();
        let mut failed = Vec::new();
        assert!(
            recording_export::archive(&mut failed, &path, &task, &selection, &plan, &cancel)
                .is_err()
        );
        assert!(failed.len() < output.len());
        cancel.store(true, Ordering::Release);
        assert_eq!(
            recording_export::prepare(&path, &task, &selection, &cancel)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn collection_keeps_distinct_segments_exact_and_rejects_corrupt_later_members() {
        use super::super::recording_collection;
        use sha2::{Digest, Sha512};
        let (path, mut task, raw) = fixture();
        let first: v2::Index = serde_json::from_str(
            std::fs::read_to_string(path.join("iq-000000.jsonl"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let mut meta = first.meta;
        meta.segment_id = "8-10".into();
        meta.clock.epoch = "8".into();
        meta.clock.channel_id = "10".into();
        meta.clock.actual_rate_hz *= 2.0;
        meta.clock.input_rate_hz *= 2.0;
        meta.frequency_hz += 12345.0;
        let mut writer =
            v2::Writer::new(File::create(path.join("iq-000002.wbiq")).unwrap()).unwrap();
        let index = writer.append(meta, &raw[..400]).unwrap();
        writer.finish().unwrap();
        std::fs::write(
            path.join("iq-000002.jsonl"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        task["tracks"][0]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"file":"iq-000002.wbiq"}));
        let mut segment = task["tracks"][0]["segments"][0].clone();
        segment["segment_id"] = json!("8-10");
        segment["samples"] = json!("50");
        segment["stream"]["epoch"] = json!("8");
        segment["stream"]["channel_id"] = json!("10");
        segment["actual_rate_hz"] = json!(index.meta.clock.actual_rate_hz);
        segment["input_rate_hz"] = json!(index.meta.clock.input_rate_hz);
        segment["frequency_hz"] = json!(index.meta.frequency_hz);
        task["tracks"][0]["segments"]
            .as_array_mut()
            .unwrap()
            .push(segment);
        let cancel = AtomicBool::new(false);
        let plan = recording_collection::prepare(&path, &task, &cancel).unwrap();
        let mut output = Vec::new();
        recording_collection::archive(&mut output, &path, &task, &plan, &cancel).unwrap();
        let mut archive = tar::Archive::new(output.as_slice());
        let files: BTreeMap<String, Vec<u8>> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let name = entry.path().unwrap().to_str().unwrap().to_owned();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (name, bytes)
            })
            .collect();
        assert_eq!(files.len(), 5);
        assert_eq!(files["segment-000000.sigmf-data"], raw);
        assert_eq!(files["segment-000001.sigmf-data"], raw[..400]);
        let collection: Value =
            serde_json::from_slice(&files["recording.sigmf-collection"]).unwrap();
        for (i, record) in collection["collection"]["core:streams"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let name = record["name"].as_str().unwrap();
            let bytes = &files[&format!("{name}.sigmf-meta")];
            assert_eq!(record["hash"], format!("{:x}", Sha512::digest(bytes)));
            let meta: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(
                meta["global"]["core:sample_rate"],
                task["tracks"][0]["segments"][i]["actual_rate_hz"]
            );
            assert_eq!(meta["global"]["websa:task_state"], "failed");
            assert_eq!(meta["global"]["websa:service_id"], "boot");
            assert!(meta["captures"][0].get("core:datetime").is_none());
            assert_eq!(
                meta["annotations"][0]["websa:clock"]["first_input_index"],
                (u64::MAX - 1000).to_string()
            );
            assert_eq!(meta["annotations"][0]["websa:clock"]["fraction_den"], "3");
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path.join("iq-000002.wbiq"))
            .unwrap();
        file.seek(SeekFrom::Start(30)).unwrap();
        file.write_all(b"X").unwrap();
        let mut changed = Vec::new();
        assert!(recording_collection::archive(&mut changed, &path, &task, &plan, &cancel).is_err());
        assert!(changed.len() < output.len());
        assert!(recording_collection::prepare(&path, &task, &cancel).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn rejects_ranges_gaps_identity_changes_and_damaged_payload_before_success() {
        let (path, mut task, _) = fixture();
        let selection = Selection {
            segment: "7-9".into(),
            start: 137,
            count: 126,
        };
        assert!(read_range(
            &path,
            &task,
            &Selection {
                segment: "7-9".into(),
                start: 300,
                count: 1
            }
        )
        .is_err());
        task["receiver_uid"] = json!("reused-slot");
        assert!(read_range(&path, &task, &selection).is_err());
        task["receiver_uid"] = json!("old-uid");
        let index_path = path.join("iq-000001.jsonl");
        let original = std::fs::read_to_string(&index_path).unwrap();
        std::fs::write(
            &index_path,
            original.lines().skip(1).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        assert!(read_range(&path, &task, &selection)
            .unwrap_err()
            .to_string()
            .contains("gap"));
        std::fs::write(index_path, original).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path.join("iq-000001.wbiq"))
            .unwrap();
        file.seek(SeekFrom::Start(30)).unwrap();
        file.write_all(b"X").unwrap();
        assert!(read_range(&path, &task, &selection)
            .unwrap_err()
            .to_string()
            .contains("CRC32"));
        for (start, count) in [
            ("0", "0"),
            ("0", "1048577"),
            ("18446744073709551615", "1"),
            ("-1", "1"),
            ("1.0", "1"),
        ] {
            assert!(Selection::parse(&HashMap::from([
                ("segment".into(), "7-9".into()),
                ("start".into(), start.into()),
                ("count".into(), count.into())
            ]))
            .is_err());
        }
        std::fs::remove_dir_all(path).unwrap();
    }
}

fn sigmf(selected: Selected) -> io::Result<Vec<u8>> {
    let metadata = serde_json::to_vec(&selected.metadata)?;
    let mut tar = tar::Builder::new(Vec::new());
    // SigMF archives use a single directory with a matching meta/data pair.
    for (name, bytes) in [
        ("recording/recording.sigmf-meta", metadata.as_slice()),
        ("recording/recording.sigmf-data", selected.raw.as_slice()),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes)?;
    }
    tar.into_inner()
}

fn download(
    bytes: Vec<u8>,
    content_type: &str,
    name: &str,
    permit: tokio::sync::SemaphorePermit<'static>,
) -> Response {
    let length = bytes.len();
    // The permit travels from blocking work into the response body. A stalled socket
    // cannot accumulate another range buffer; dropping the response releases its budget.
    let stream = futures_util::stream::unfold(
        (Bytes::from(bytes), permit),
        |(mut bytes, permit)| async move {
            if bytes.is_empty() {
                return None;
            }
            let chunk = bytes.split_to(bytes.len().min(64 * 1024));
            Some((Ok::<_, io::Error>(chunk), (bytes, permit)))
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, length)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from_stream(stream))
        .unwrap()
}

pub(super) async fn range(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let error = |status, message: String| (status, Json(json!({"error":message}))).into_response();
    let selection = match Selection::parse(&params) {
        Ok(s) => s,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let format = params.get("format").map(String::as_str).unwrap_or("sigmf");
    if !matches!(format, "sigmf" | "cf32" | "meta") {
        return error(StatusCode::BAD_REQUEST, "unknown range format".into());
    }
    let format = format.to_owned();
    let (path, snapshot) = {
        let state = app.state.lock_async().await.unwrap();
        let Some(manager) = &state.recordings else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match manager.readable_task(&task) {
            Ok(value) => value,
            Err(e) => {
                return error(
                    if e == "recording task not found" {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::CONFLICT
                    },
                    e.into(),
                )
            }
        }
    };
    let permit = match READER.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "another IQ range read is in progress".into(),
            )
        }
    };
    match tokio::task::spawn_blocking(
        move || -> io::Result<(Vec<u8>, &'static str, &'static str, tokio::sync::SemaphorePermit<'static>)> {
            let selected = read_range(&path, &snapshot, &selection)?;
            let (bytes, content_type, name) = match format.as_str() {
                "meta" => (
                    serde_json::to_vec(&selected.metadata)?,
                    "application/json",
                    "recording.sigmf-meta",
                ),
                "cf32" => (
                    selected.raw,
                    "application/octet-stream",
                    "recording.sigmf-data",
                ),
                _ => (sigmf(selected)?, "application/x-tar", "recording.sigmf"),
            };
            Ok((bytes,content_type,name,permit))
        },
    )
    .await
    {
        Ok(Ok((bytes, content_type, name, permit))) => download(bytes,content_type,name,permit),
        Ok(Err(e)) => error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        Err(e) => state_worker_failed(e).into_response(),
    }
}
