//! Saved tuning/processing settings. Loading still uses normal receiver admission.
use super::*;
use std::io::{Read, Write};
use std::path::PathBuf;

const MAX_PRESETS: usize = 32;
const MAX_BYTES: usize = 256 * 1024;

pub(super) struct Store {
    path: PathBuf,
    data: Value,
}

impl Store {
    pub(super) fn open(root: PathBuf) -> Result<Self, String> {
        let path = root.join("receiver-presets.json");
        let data = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() > MAX_BYTES {
                    return Err("preset file exceeds 256 KiB".into());
                }
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                json!({"version":1,"revision":"0","presets":[]})
            }
            Err(error) => return Err(error.to_string()),
        };
        let presets = data["presets"].as_array().ok_or("invalid preset list")?;
        if data["version"] != 1
            || data["revision"]
                .as_str()
                .and_then(|r| r.parse::<u64>().ok())
                .is_none()
            || presets.len() > MAX_PRESETS
        {
            return Err("unsupported preset file".into());
        }
        let mut names = std::collections::HashSet::new();
        for preset in presets {
            let name = preset["name"]
                .as_str()
                .filter(|s| valid_name(s))
                .ok_or("invalid preset name")?;
            if !names.insert(name)
                || !preset["settings"]["baseband"].is_object()
                || !preset["settings"]["processors"].is_array()
                || preset["settings"].get("continuity").is_some_and(|c| !matches!(c.as_str(), Some("continuous" | "strict")))
                || !matches!(
                    preset["settings"]["purpose"].as_str(),
                    Some("iq" | "listen" | "digital")
                )
            {
                return Err("invalid preset settings".into());
            }
        }
        Ok(Self { path, data })
    }

    fn change(
        &mut self,
        request: &Value,
        settings: Option<Value>,
    ) -> Result<Value, (StatusCode, String)> {
        let fail = |message: &str| (StatusCode::BAD_REQUEST, message.to_owned());
        let name = request["name"]
            .as_str()
            .filter(|s| valid_name(s))
            .ok_or_else(|| fail("invalid preset name"))?;
        let revision = request["expected_revision"]
            .as_str()
            .and_then(|r| r.parse::<u64>().ok())
            .ok_or_else(|| fail("expected_revision must be a decimal u64 string"))?;
        let current = self.data["revision"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        if revision != current {
            return Err((
                StatusCode::CONFLICT,
                "preset list changed; refresh before retrying".into(),
            ));
        }
        let next_revision = current
            .checked_add(1)
            .ok_or_else(|| fail("preset revision exhausted"))?;
        let mut next = self.data.clone();
        let presets = next["presets"].as_array_mut().unwrap();
        let index = presets.iter().position(|p| p["name"] == name);
        if let Some(settings) = settings {
            let entry = json!({"name":name,"settings":settings});
            if let Some(index) = index {
                presets[index] = entry;
            } else {
                if presets.len() == MAX_PRESETS {
                    return Err(fail("at most 32 receiver presets"));
                }
                presets.push(entry);
            }
        } else if let Some(index) = index {
            presets.remove(index);
        } else {
            return Err((StatusCode::NOT_FOUND, "preset not found".into()));
        }
        next["revision"] = json!(next_revision.to_string());
        let bytes = serde_json::to_vec(&next).map_err(|e| fail(&e.to_string()))?;
        if bytes.len() > MAX_BYTES {
            return Err(fail("preset file exceeds 256 KiB"));
        }
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)?;
            // Publish the renamed state even if directory sync reports a durability error.
            self.data = next;
            std::fs::File::open(self.path.parent().unwrap())?.sync_all()
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(temporary);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()));
        }
        Ok(self.data.clone())
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name == name.trim()
        && name.len() <= 128
        && !name.chars().any(char::is_control)
}

pub(super) async fn list(State(app): State<Arc<App>>) -> Response {
    let store = app.state.lock().unwrap().receiver_presets.clone();
    match store {
        Some(store) => {
            match tokio::task::spawn_blocking(move || store.lock().unwrap().data.clone()).await {
                Ok(value) => ([("cache-control", "no-store")], Json(value)).into_response(),
                Err(error) => state_worker_failed(error).into_response(),
            }
        }
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn change(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !browser_access(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.len() > 4096 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let request: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let (store, settings) = {
        let mut state = app.state.lock().unwrap();
        let Some(store) = state.receiver_presets.clone() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let snapshot = state.receiver_snapshot();
        if request["service_id"] != snapshot["service_id"] {
            return (
                StatusCode::CONFLICT,
                Json(error_body(
                    "source service restarted; refresh before retrying",
                    "preset_conflict",
                )),
            )
                .into_response();
        }
        let settings = match request["type"].as_str() {
            Some("save") => {
                let receiver = snapshot["receivers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["receiver_uid"] == request["receiver_uid"]);
                let Some(receiver) = receiver else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                if receiver["revision"] != request["receiver_revision"] {
                    return (
                        StatusCode::CONFLICT,
                        Json(error_body(
                            "receiver changed; refresh before saving",
                            "preset_conflict",
                        )),
                    )
                        .into_response();
                }
                // Resolve omitted fields to the source's actual settings. A missing
                // AGC/gain value must not inherit a different target's old value.
                let channel = &state.channel_configs()
                    [(receiver["display_number"].as_u64().unwrap() - 1) as usize];
                let config = multichannel::config_value(channel, Some(&receiver["confirmed"]));
                Some(
                    json!({"purpose":config["purpose"],"baseband":config["baseband"],"processors":config["processors"],"continuity":config["continuity"]}),
                )
            }
            Some("delete") => None,
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        (store, settings)
    };
    // Only the preset mutex covers disk I/O; capture and recording remain independent.
    match tokio::task::spawn_blocking(move || store.lock().unwrap().change(&request, settings))
        .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err((status, message))) => {
            (status, Json(error_body(&message, "preset_error"))).into_response()
        }
        Err(error) => state_worker_failed(error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_presets_conflict_and_failed_write_keep_saved_settings() {
        let root = std::env::temp_dir().join(format!(
            "websa-presets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let settings = json!({"purpose":"iq","baseband":{"frequency_hz":100e6},"processors":[]});
        let request = json!({"name":"AM / 频点","expected_revision":"0"});
        let mut store = Store::open(root.clone()).unwrap();
        store.change(&request, Some(settings.clone())).unwrap();
        let mut reopened = Store::open(root.clone()).unwrap();
        assert_eq!(reopened.data["presets"][0]["settings"], settings);
        assert_eq!(
            reopened.change(&request, Some(json!({}))).unwrap_err().0,
            StatusCode::CONFLICT
        );
        let previous = reopened.data.clone();
        reopened.path = root.join("missing/presets.json");
        assert!(reopened
            .change(
                &json!({"name":"other","expected_revision":"1"}),
                Some(settings)
            )
            .is_err());
        assert_eq!(reopened.data, previous);
        store
            .change(&json!({"name":"AM / 频点","expected_revision":"1"}), None)
            .unwrap();
        assert!(Store::open(root.clone()).unwrap().data["presets"]
            .as_array()
            .unwrap()
            .is_empty());
        std::fs::write(root.join("receiver-presets.json"), b"broken").unwrap();
        assert!(Store::open(root.clone()).is_err());
        assert_eq!(
            std::fs::read(root.join("receiver-presets.json")).unwrap(),
            b"broken"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
