//! Checked, independently compressed published-result journal. Image updates retain their
//! original pixel payloads; recording never creates a decoder or consumes a browser snapshot.
use super::*;

pub(super) const MAX_RECORD: usize = 3 * 1024 * 1024;
const MAX_VOLUMES: usize = 4096;
const MAGIC: &[u8; 8] = b"WBRS0001";

use super::journal::Writer;
fn writer(path: &FsPath, ordinal: usize) -> io::Result<Writer> {
    Writer::new(&path.join(format!("results-{ordinal:06}.wbr")), MAGIC)
}
fn scan(
    path: &FsPath,
    limit: u64,
    mut visit: impl FnMut(&[u8], &Value) -> io::Result<()>,
) -> io::Result<super::journal::Scan> {
    super::journal::scan(path, MAGIC, limit, |raw| {
        visit(raw, &serde_json::from_slice(raw)?)
    })
}
fn validate(task: &Value, row: &Value) -> io::Result<()> {
    let e = &row["envelope"];
    if row["version"] != 1
        || e["receiver_uid"] != task["receiver_uid"]
        || e["service_id"] != task["service_id"]
        || e["channel_id"].as_u64().is_none()
        || e["epoch"].as_u64().is_none()
        || e["seq"].as_u64().is_none()
        || e["event"]["type"].as_str().is_none()
        || e["mode"].as_str().is_none()
        || e["input_index"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none()
        || e["input_fraction_num"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none()
        || e["input_fraction_den"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none_or(|n| n == 0)
        || !row["source"]["actual_rate_hz"]
            .as_f64()
            .is_some_and(|r| r.is_finite() && r > 0.0)
    {
        return Err(io_error("result record identity/clock mismatch"));
    }
    Ok(())
}
fn update(task: &mut Value, row: &Value, raw: usize) -> io::Result<()> {
    validate(task, row)?;
    let e = &row["envelope"];
    let source = &row["source"];
    let id = format!("{}-{}", e["epoch"], e["channel_id"]);
    let segments = task["tracks"][0]["segments"]
        .as_array_mut()
        .ok_or_else(|| io_error("missing result segments"))?;
    let index = if let Some(index) = segments.iter().position(|s| s["segment_id"] == id) {
        index
    } else {
        if segments.len() >= MAX_SEGMENTS {
            return Err(io_error("result segment limit reached"));
        }
        if let Some(old) = segments.last_mut() {
            old["end_reason"] = json!("processing_changed");
        }
        segments.push(json!({"segment_id":id,"stream":{"source_id":"primary","epoch":e["epoch"].as_u64().unwrap().to_string(),"channel_id":e["channel_id"].as_u64().unwrap().to_string()},
            "first_input_index":e["input_index"],"fraction_num":e["input_fraction_num"],"fraction_den":e["input_fraction_den"],
            "samples":"0","events":"0","actual_rate_hz":source["actual_rate_hz"],"frequency_hz":source["frequency_hz"],
            "mode":e["mode"],"prefix_missing":e["event"]["type"] != "start" || e["seq"] != 0,
            "last_sequence":null,"end_reason":null,"utc_reliable":false}));
        segments.len() - 1
    };
    let segment = &mut segments[index];
    if segment["last_sequence"]
        .as_u64()
        .is_some_and(|seq| seq.checked_add(1) != e["seq"].as_u64())
        || segment["mode"] != e["mode"]
        || segment["actual_rate_hz"] != source["actual_rate_hz"]
        || segment["frequency_hz"] != source["frequency_hz"]
    {
        return Err(io_error(
            "result sequence/configuration changed within processing stream",
        ));
    }
    segment["last_sequence"] = e["seq"].clone();
    add(&mut segment["events"], 1);
    add(&mut task["tracks"][0]["raw_bytes"], raw as u64);
    add(&mut task["tracks"][0]["blocks"], 1);
    Ok(())
}
fn volume(task: &mut Value, ordinal: usize) {
    task["tracks"][0]["volumes"].as_array_mut().unwrap().push(json!({"file":format!("results-{ordinal:06}.wbr"),"index":null,"complete":false,"valid_bytes":"8"}));
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
                let WorkInput::Results(raw) = item.input else {
                    return Err(io_error("wrong result lane input"));
                };
                let row: Value = serde_json::from_slice(&raw)?;
                // Validate and stage counters before writing. Only publish them after durable append.
                let mut next = task.clone();
                update(&mut next, &row, raw.len())?;
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
                        return Err(io_error("result volume limit reached"));
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
    files.retain(|p| p.extension().is_some_and(|e| e == "wbr"));
    files.sort();
    task["tracks"][0]["segments"] = json!([]);
    task["tracks"][0]["volumes"] = json!([]);
    for key in ["blocks", "raw_bytes", "stored_bytes"] {
        task["tracks"][0][key] = json!("0");
    }
    let mut fault = None;
    for (ordinal, file) in files.iter().enumerate() {
        if file.file_name().unwrap() != format!("results-{ordinal:06}.wbr").as_str() {
            fault = Some("result volume order mismatch".into());
            break;
        }
        let size = file.metadata()?.len();
        let scanned = scan(file, size, |raw, row| update(task, row, raw.len()))?;
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
            fault = Some("result volume has no checked footer".into());
            break;
        }
    }
    if interrupted {
        set_fault(task,"SERVICE_INTERRUPTED","service restarted during result recording; verified prefix recovered, no automatic resume");
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
        set_fault(task, "ARCHIVE_INCOMPLETE", "missing result archive");
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

fn visit(
    path: &FsPath,
    task: &Value,
    cancel: &AtomicBool,
    mut output: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<(u64, [u8; 64])> {
    use sha2::{Digest, Sha512};
    if task["tracks"][0]["kind"] != "results" {
        return Err(io_error("not a results recording"));
    }
    let volumes = task["tracks"][0]["volumes"]
        .as_array()
        .ok_or_else(|| io_error("result volumes missing"))?;
    if volumes.len() > MAX_VOLUMES {
        return Err(io_error("result volume limit exceeded"));
    }
    let mut count = 0;
    let mut length = 0;
    let mut hash = Sha512::new();
    for (ordinal, vol) in volumes.iter().enumerate() {
        if vol["file"] != format!("results-{ordinal:06}.wbr") {
            return Err(io_error("invalid result volume name"));
        }
        let file = path.join(vol["file"].as_str().unwrap());
        let limit = number(&vol["valid_bytes"]);
        let scanned = scan(&file, limit, |raw, row| {
            if cancel.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "result download cancelled",
                ));
            }
            validate(task, row)?;
            output(raw)?;
            output(b"\n")?;
            hash.update(raw);
            hash.update(b"\n");
            count += 1;
            length += raw.len() as u64 + 1;
            Ok(())
        })?;
        if let Some(error) = scanned.error {
            return Err(io_error(error));
        }
        if scanned.valid != limit || (vol["complete"] == true && !scanned.complete) {
            return Err(io_error("result volume prefix mismatch"));
        }
    }
    if count != number(&task["tracks"][0]["blocks"]) {
        return Err(io_error("result archive event count mismatch"));
    }
    Ok((length, hash.finalize().into()))
}

/// Tar keeps the task's failure/prefix semantics alongside the independently readable JSONL.
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
        "results.tar",
        |path, task, cancel| {
            let (length, hash) = visit(path, task, cancel, |_| Ok(()))?;
            if length > 0o77777777777 {
                return Err(io_error("result JSONL exceeds POSIX tar size limit"));
            }
            let manifest =
                serde_json::to_vec(&json!({"format":"websa-results-export-v1","task":task,
            "capture_policy":"published_events_from_record_start","includes_prior_history":false,
            "images":"original_incremental_pixel_payloads; missing prefixes/rows remain partial",
            "time":"received_at_ms is host receipt time; source indices do not imply UTC"}))?;
            let pad = |n: u64| (512 - n % 512) % 512;
            Ok((
                2048 + manifest.len() as u64 + pad(manifest.len() as u64) + length + pad(length),
                (manifest, length, hash),
            ))
        },
        |output, path, task, (manifest, length, hash), cancel| {
            let pad = |n: u64| (512 - n % 512) % 512;
            super::super::recording_export::header(output, "task.json", manifest.len() as u64)?;
            output.write_all(manifest)?;
            output.write_all(&[0; 512][..pad(manifest.len() as u64) as usize])?;
            super::super::recording_export::header(output, "events.jsonl", *length)?;
            let (actual, digest) = visit(path, task, cancel, |raw| output.write_all(raw))?;
            if actual != *length || digest != *hash {
                return Err(io_error("result archive changed during export"));
            }
            output.write_all(&[0; 512][..pad(*length) as usize])?;
            output.write_all(&[0; 1024])
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "websa-results-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn row(epoch: u32, seq: u32) -> Bytes {
        serde_json::to_vec(&json!({"version":1,"received_at_ms":"1793548800000",
            "source":{"frequency_hz":100e6,"actual_rate_hz":48840.130763116205},
            "envelope":{"receiver_uid":"uid","service_id":"service","channel_id":epoch,"epoch":epoch,"seq":seq,
                "input_index":"18446744073709551600","input_fraction_num":"1","input_fraction_den":"3",
                "delay_known":false,"timing_kind":"causal_presentation","mode":"sstv",
                "event":{"type":"sstv-image","width":2,"height":3,"row":1,"rows":1,"rgb":[0,255,128,4,5,6],"meta":{"complete":false}}}})).unwrap().into()
    }
    fn finish(manager: &mut Manager, id: &str) {
        if manager.lanes.iter().any(|h| {
            h.lane.task.lock().unwrap()["task_id"] == id && !h.lane.queue.lock().unwrap().sealed
        }) {
            manager.stop(id).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager
            .lanes
            .iter()
            .any(|h| !h.lane.done.load(Ordering::Acquire))
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        manager.reap();
    }
    #[test]
    fn durable_results_preserve_pixels_clocks_interleaved_processing_tails_and_coexist_with_iq() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start_kind("uid", "service", "results", "results")
            .unwrap()
            .wait()
            .unwrap();
        manager
            .start("uid", "service", "iq")
            .unwrap()
            .wait()
            .unwrap();
        assert!(manager
            .start_kind("uid", "service", "duplicate", "results")
            .is_err());
        let id = manager.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let items = vec![row(4, 42), row(5, 0), row(4, 43), row(5, 1)];
        for raw in &items {
            manager.admit_result("uid", raw.clone());
        }
        manager.processing_sources(&["uid".into()], None);
        let iq = manager.snapshots()[1]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        manager.stop(&iq).unwrap();
        manager.processing_sources(&[], None);
        finish(&mut manager, &id);
        let task = manager
            .snapshots()
            .into_iter()
            .find(|t| t["task_id"] == id)
            .unwrap();
        assert_eq!(task["state"], "completed");
        assert_eq!(task["accepted_bytes"], task["tracks"][0]["raw_bytes"]);
        let segments = task["tracks"][0]["segments"].as_array().unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0]["events"], "2");
        assert_eq!(segments[0]["first_input_index"], "18446744073709551600");
        assert_eq!(segments[0]["fraction_num"], "1");
        assert_eq!(segments[0]["prefix_missing"], true);
        let mut output = Vec::new();
        visit(&root.join(&id), &task, &AtomicBool::new(false), |raw| {
            output.extend_from_slice(raw);
            Ok(())
        })
        .unwrap();
        let expected = items
            .iter()
            .flat_map(|r| r.iter().copied().chain([b'\n']))
            .collect::<Vec<_>>();
        assert_eq!(output, expected);
        drop(manager);
        let manager = Manager::open(root.clone()).unwrap();
        let recovered = manager
            .snapshots()
            .into_iter()
            .find(|t| t["task_id"] == id)
            .unwrap();
        assert_eq!(recovered["tracks"][0], task["tracks"][0]);
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn completed_content_is_checked_on_read_and_interrupted_content_is_recovered() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start_kind("uid", "service", "results", "results")
            .unwrap()
            .wait()
            .unwrap();
        let id = manager.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        manager.admit_result("uid", row(4, 10));
        manager.admit_result("uid", row(4, 11));
        finish(&mut manager, &id);
        let task = manager.snapshots()[0].clone();
        let path = root.join(&id);
        let file = path.join("results-000000.wbr");
        let mut raw = fs::read(&file).unwrap();
        let next = 8 + 16 + u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
        raw[next + 16] ^= 1;
        fs::write(&file, raw).unwrap();
        assert!(visit(&path, &task, &AtomicBool::new(false), |_| Ok(())).is_err());
        drop(manager);
        let manager = Manager::open(root.clone()).unwrap();
        assert_eq!(manager.snapshots()[0], task);
        assert!(visit(&path, &manager.snapshots()[0], &AtomicBool::new(false), |_| Ok(())).is_err());
        drop(manager);
        let mut interrupted = task;
        interrupted["state"] = json!("finalizing");
        persist(&path, &interrupted).unwrap();
        let manager = Manager::open(root.clone()).unwrap();
        let recovered = manager.snapshots()[0].clone();
        assert_eq!(recovered["state"], "failed");
        assert_eq!(recovered["fault"]["code"], "SERVICE_INTERRUPTED");
        assert_eq!(recovered["tracks"][0]["blocks"], "1");
        assert_eq!(recovered["service_id"], "service");
        let mut out = Vec::new();
        visit(&path, &recovered, &AtomicBool::new(false), |r| {
            out.extend_from_slice(r);
            Ok(())
        })
        .unwrap();
        assert!(out.starts_with(&row(4, 10)));
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn recovery_keeps_original_write_failure_and_a_stable_tail_diagnostic() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start_kind("uid", "service", "results", "results")
            .unwrap()
            .wait()
            .unwrap();
        let id = manager.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        manager.admit_result("uid", row(4, 10));
        finish(&mut manager, &id);
        let mut task = manager.snapshots()[0].clone();
        drop(manager);
        set_fault(&mut task, "RECORDING_IO", "File too large (os error 27)");
        let path = root.join(&id);
        persist(&path, &task).unwrap();
        let file = path.join("results-000000.wbr");
        let length = file.metadata().unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(file)
            .unwrap()
            .set_len(length - 6)
            .unwrap();
        let manager = Manager::open(root.clone()).unwrap();
        let first = manager.snapshots()[0].clone();
        drop(manager);
        assert_eq!(first["fault"]["code"], "RECORDING_IO");
        assert!(first["fault"]["message"]
            .as_str()
            .unwrap()
            .starts_with("File too large (os error 27); recovery:"));
        assert_eq!(first["tracks"][0]["blocks"], "1");
        let manager = Manager::open(root.clone()).unwrap();
        assert_eq!(manager.snapshots()[0]["fault"], first["fault"]);
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn quiet_results_do_not_time_out_and_processing_failure_seals_only_results() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start_kind("uid", "service", "results", "results")
            .unwrap()
            .wait()
            .unwrap();
        manager
            .start("uid", "service", "iq")
            .unwrap()
            .wait()
            .unwrap();
        manager.lanes[0].lane.queue.lock().unwrap().last_input =
            Instant::now() - Duration::from_secs(9);
        manager.tick(None);
        assert!(manager.snapshots()[0]["fault"].is_null());
        manager.processing_sources(&["uid".into()], Some("decoder failed"));
        assert_eq!(manager.snapshots()[0]["fault"]["code"], "PROCESSING_FAILED");
        assert!(manager.snapshots()[1]["fault"].is_null());
        manager.shutdown();
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn acknowledged_empty_results_and_torn_tails_recover_without_automatic_resume() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start_kind("uid", "service", "results", "results")
            .unwrap()
            .wait()
            .unwrap();
        let task = manager.snapshots()[0].clone();
        let path = root.join(task["task_id"].as_str().unwrap());
        // A durable preparation receipt already has a readable manifest and first volume.
        assert!(path.join("task.json").exists());
        assert_eq!(fs::read(path.join("results-000000.wbr")).unwrap(), MAGIC);
        manager.shutdown();
        drop(manager);
        let mut interrupted = task;
        interrupted["state"] = json!("recording");
        persist(&path, &interrupted).unwrap();
        OpenOptions::new()
            .append(true)
            .open(path.join("results-000000.wbr"))
            .unwrap()
            .write_all(b"EVZ")
            .unwrap();
        let manager = Manager::open(root.clone()).unwrap();
        let recovered = manager.snapshots()[0].clone();
        assert_eq!(recovered["fault"]["code"], "SERVICE_INTERRUPTED");
        assert!(!manager.has_active());
        assert_eq!(recovered["tracks"][0]["blocks"], "0");
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
}
