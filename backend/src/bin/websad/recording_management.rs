//! Finished recordings share the archive reader lease with destructive operations.
//! Filesystem work runs outside acquisition state; a tombstone makes deletion durable.
use super::*;
static MANAGEMENT: Mutex<()> = Mutex::new(());

pub(crate) async fn command(app: Arc<App>, request: Value, id: String, preview: bool) -> Response {
    match tokio::task::spawn_blocking(move || {
        let _management = MANAGEMENT.lock().unwrap();
        let mut state = app.state.lock().unwrap();
        if let Some((_, old, reply)) = state.receivers.replies.iter().find(|(key, _, _)| key == &id) {
            if old == &request { return reply.clone(); }
            return json!({"request_id":id,"snapshot":state.receiver_snapshot(),"fault":{
                "scope":"request","code":"IDENTITY_CONFLICT","message":"request_id reused with different command","retryable":false}});
        }
        drop(state);
        let result = perform(&app, &request, preview);
        let mut state = app.state.lock().unwrap();
        if preview {
            return json!({"accepted":result.is_ok(),"faults":result.err().map(|message|vec![json!({
                "scope":"recording","code":"RECORDING_MANAGEMENT","message":message,"retryable":true})]).unwrap_or_default(),
                "capture_change":false,"affected_receivers":[],"affected_tracks":[],"candidate_center_hz":state.capture_center(),"estimated_cf32_bytes_per_second":0});
        }
        let reply = json!({"request_id":id,"snapshot":state.receiver_snapshot(),"fault":result.err().map(|message|json!({
            "scope":"recording","code":"RECORDING_MANAGEMENT","message":message,"task_id":request["task_id"],"retryable":true}))});
        state.receivers.replies.push_back((id, request, reply.clone()));
        if state.receivers.replies.len() > 128 { state.receivers.replies.pop_front(); }
        reply
    }).await {
        Ok(reply) => Json(reply).into_response(),
        Err(error) => state_worker_failed(error).into_response(),
    }
}

fn perform(app: &Arc<App>, request: &Value, preview: bool) -> Result<(), String> {
    let mut state = app.state.lock().unwrap();
    if request["service_id"]
        .as_str()
        .is_some_and(|s| s != state.receivers.boot)
    {
        return Err("source service restarted; refresh before retrying".into());
    }
    let id = request["task_id"].as_str().ok_or("task_id required")?;
    let manager = state
        .recordings
        .as_ref()
        .ok_or("backend recording storage unavailable")?;
    let (path, mut task) = manager.readable_task(id).map_err(str::to_owned)?;
    if !matches!(task["state"].as_str(), Some("completed" | "failed")) {
        return Err("stop the recording before managing its files".into());
    }
    let deleting = request["type"] == "record_delete";
    if !deleting {
        let name = request["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| {
                !s.is_empty() && s.chars().count() <= 128 && !s.chars().any(char::is_control)
            })
            .ok_or("recording name must contain 1 to 128 characters without control characters")?;
        task["name"] = json!(name);
    }
    if preview {
        return Ok(());
    }
    // Acquire while state is held so another management request cannot pass lookup
    // until this operation has reserved the immutable archive.
    let _permit = super::super::recording_read::READER
        .try_acquire()
        .map_err(|_| "another recording download or file operation is in progress")?;
    let tombstone = path.with_file_name(format!(
        ".deleted-{}",
        path.file_name().unwrap().to_string_lossy()
    ));
    // Release acquisition state before performing filesystem operations.
    drop(state);
    let operation = if deleting {
        fs::rename(&path, &tombstone)
    } else {
        persist(&path, &task)
    };
    // Commit the tombstone before reporting success; a power loss must not
    // resurrect a deleted recording during the next metadata-only startup.
    let durable = if deleting && operation.is_ok() {
        File::open(path.parent().unwrap()).and_then(|directory| directory.sync_all())
    } else { Ok(()) };
    state = app.state.lock().unwrap();
    operation.map_err(|e| e.to_string())?;
    state
        .recordings
        .as_mut()
        .ok_or("recording storage closed")?
        .managed(id, (!deleting).then_some(task));
    if deleting {
        drop(state);
        // Hidden tombstones are never recovered, even if cleanup is interrupted.
        let _ = fs::remove_dir_all(tombstone);
    }
    durable.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deletion_invalidates_lookup_and_index_and_never_reappears_on_restart() {
        let root = std::env::temp_dir().join(format!("websa-delete-{}-{}", std::process::id(), millis()));
        let mut manager = Manager::open(root.clone()).unwrap();
        manager.start_configured("uid", "service", "delete-test", "iq", "wav", "Station 14 MHz").unwrap().wait().unwrap();
        let id = manager.snapshots()[0]["task_id"].as_str().unwrap().to_owned();
        manager.stop(&id).unwrap();
        manager.shutdown();
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.recordings = Some(manager);
        let app = Arc::new(App {
            state: StateMutex::new(state), clients: PLMutex::new(Vec::new()),
            next_client: AtomicU64::new(0), epoch: Arc::new(AtomicU64::new(0)),
            display_version: Arc::new(AtomicU64::new(0)), identities: channel_identities(),
        });
        let request = json!({"type":"record_delete","task_id":id});
        perform(&app, &json!({"type":"record_rename","task_id":id,"name":"Evening session"}), false).unwrap();
        let saved:Value = serde_json::from_slice(&fs::read(root.join(&id).join("task.json")).unwrap()).unwrap();
        assert_eq!(saved["name"], "Evening session");
        assert_eq!(saved["receiver_name"], "Station 14 MHz");
        let response = super::super::index(State(app.clone()), Path(id.clone()), Query(HashMap::new())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        fs::write(root.join(&id).join("iq-000000.jsonl"), vec![b'x'; 256*1024+1]).unwrap();
        let response = super::super::index(State(app.clone()), Path(id.clone()), Query(HashMap::new())).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let lease = super::super::super::recording_read::READER.try_acquire().unwrap();
        assert!(perform(&app, &request, false).unwrap_err().contains("in progress"));
        assert!(root.join(&id).exists());
        drop(lease);
        perform(&app, &request, false).unwrap();
        {
            let state = app.state.lock().unwrap();
            let manager = state.recordings.as_ref().unwrap();
            assert!(manager.snapshots().is_empty());
            assert_eq!(manager.readable_task(&id).unwrap_err(), "recording task not found");
        }
        let response = super::super::index(State(app.clone()), Path(id.clone()), Query(HashMap::new())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!root.join(&id).exists());
        drop(app);
        // A crash during cleanup can leave a complete manifest in the tombstone.
        let tombstone = root.join(format!(".deleted-{id}"));
        fs::create_dir(&tombstone).unwrap();
        fs::write(tombstone.join("task.json"), b"not even a valid manifest").unwrap();
        let manager = Manager::open(root.clone()).unwrap();
        assert!(manager.snapshots().is_empty());
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
}
