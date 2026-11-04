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
    Ok(())
}
