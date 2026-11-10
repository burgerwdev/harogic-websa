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
            || decimal_revision(&data["revision"]).is_none()
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
            if !names.insert(name) || !valid_settings(&preset["settings"]) {
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
        let revision = decimal_revision(&request["expected_revision"])
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
            if !valid_settings(&settings) {
                return Err(fail("invalid preset settings"));
            }
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
        // A stale path or symlink is not ours. Only a successfully created
        // temporary file may be written or removed on a failed transaction.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
        let result = (|| -> std::io::Result<()> {
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

fn decimal_revision(value: &Value) -> Option<u64> {
    let text = value.as_str()?;
    (text.len() <= 20 && !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Stored editor data must be well formed independently of the current device,
/// provider catalog, model availability or channel admission budget. Optional
/// controls from older files and unknown mode names remain round-trippable.
fn valid_settings(settings: &Value) -> bool {
    let finite = |value: &Value| value.as_f64().is_some_and(f64::is_finite);
    if !settings.is_object()
        || !matches!(
            settings["purpose"].as_str(),
            Some("iq" | "listen" | "digital")
        )
        || settings
            .get("continuity")
            .is_some_and(|value| !matches!(value.as_str(), Some("continuous" | "strict")))
    {
        return false;
    }
    let baseband = &settings["baseband"];
    if !baseband.is_object()
        || [
            "frequency_hz",
            "passband_hz",
            "transition_hz",
            "minimum_rate_hz",
        ]
        .iter()
        .any(|key| {
            !baseband[*key]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n > 0.0)
        })
    {
        return false;
    }
    let Some(processors) = settings["processors"].as_array() else {
        return false;
    };
    if processors.len() > 1 {
        return false;
    }
    processors.iter().all(|processor| {
        processor.is_object()
            && ["id", "mode"]
                .iter()
                .all(|key| processor[*key].as_str().is_some_and(|s| !s.is_empty()))
            && matches!(processor["kind"].as_str(), Some("demodulator" | "decoder"))
            && processor["minimum_rate_hz"]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= 0.0)
            && [
                "bandwidth_hz",
                "pcm_gain",
                "squelch_dbm",
                "nr_strength",
                "nr_atten_db",
                "pitch",
                "deemph_us",
                "radiofax_phase_pixels",
                "radiofax_clock_ppm",
            ]
            .iter()
            .all(|key| processor.get(*key).is_none_or(finite))
            && ["agc", "nr", "am_sync", "stereo", "rf_snr"]
                .iter()
                .all(|key| processor.get(*key).is_none_or(Value::is_boolean))
            && [
                "nr_algorithm",
                "radiofax_acquisition",
                "radiofax_ioc",
                "radiofax_lpm",
                "radiofax_modulation",
                "radiofax_polarity",
                "radiofax_clock_recovery",
            ]
            .iter()
            .all(|key| processor.get(*key).is_none_or(Value::is_string))
            && processor.get("audio_rate").is_none_or(|value| {
                value
                    .as_u64()
                    .is_some_and(|rate| u32::try_from(rate).is_ok())
            })
            && processor
                .get("sstv_mode")
                .is_none_or(|value| value.is_null() || value.as_u64().is_some())
    })
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name == name.trim()
        && name.len() <= 128
        && !name.chars().any(char::is_control)
}

pub(super) async fn list(State(app): State<Arc<App>>) -> Response {
    let store = app.state.lock_async().await.unwrap().receiver_presets.clone();
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
        let mut state = app.state.lock_async().await.unwrap();
        let Some(store) = state.receiver_presets.clone() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let snapshot = state.receiver_live_snapshot();
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

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "websa-preset-validation-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn saved_settings() -> Value {
        json!({"purpose":"listen","baseband":{"frequency_hz":100e6,
            "passband_hz":6000,"transition_hz":1000,"minimum_rate_hz":8000},
            "processors":[{"id":"primary","kind":"demodulator","mode":"am","minimum_rate_hz":0,
                "bandwidth_hz":6000,"pcm_gain":1,"agc":true,"squelch_dbm":-100,"nr":false,
                "nr_algorithm":"wiener","nr_strength":0.5,"nr_atten_db":18,
                "rf_snr":false,"am_sync":false,"pitch":700,"deemph_us":-1,"stereo":true,"audio_rate":48000,
                "sstv_mode":null,"radiofax_acquisition":"auto","radiofax_ioc":"auto",
                "radiofax_lpm":"auto","radiofax_modulation":"fm","radiofax_polarity":"normal",
                "radiofax_clock_recovery":"auto","radiofax_phase_pixels":0,"radiofax_clock_ppm":0}],
            "continuity":"strict"})
    }

    fn saved_file(settings: Value, revision: &str) -> Value {
        json!({"version":1,"revision":revision,"presets":[{"name":"saved","settings":settings}]})
    }

    #[test]
    fn preset_storage_rejects_malformed_nested_settings_without_touching_original() {
        let root = TestDirectory::new();
        let mut cases: Vec<(String, Value)> = vec![
            ("/baseband", json!({})),
            ("/baseband/frequency_hz", json!("100000000")),
            ("/baseband/frequency_hz", json!(0)),
            ("/baseband/passband_hz", json!(-1)),
            ("/baseband/transition_hz", Value::Null),
            ("/baseband/minimum_rate_hz", json!(false)),
            ("/processors/0", json!("am")),
            ("/processors/0/id", json!("")),
            ("/processors/0/kind", json!("unknown-kind")),
            ("/processors/0/mode", Value::Null),
            ("/processors/0/minimum_rate_hz", json!(-1)),
            ("/processors/0/audio_rate", json!(48000.5)),
            ("/processors/0/audio_rate", json!(u64::from(u32::MAX) + 1)),
            ("/processors/0/sstv_mode", json!(-1)),
            ("/processors/0/sstv_mode", json!(1.25)),
        ]
        .into_iter()
        .map(|(path, value)| (path.to_owned(), value))
        .collect();
        for field in [
            "bandwidth_hz",
            "pcm_gain",
            "squelch_dbm",
            "nr_strength",
            "nr_atten_db",
            "pitch",
            "deemph_us",
            "radiofax_phase_pixels",
            "radiofax_clock_ppm",
        ] {
            cases.push((format!("/processors/0/{field}"), json!("invalid")));
        }
        for field in [
            "agc",
            "nr",
            "am_sync",
            "rf_snr",
            "stereo",
            "nr_algorithm",
            "radiofax_acquisition",
            "radiofax_ioc",
            "radiofax_lpm",
            "radiofax_modulation",
            "radiofax_polarity",
            "radiofax_clock_recovery",
        ] {
            cases.push((format!("/processors/0/{field}"), json!(7)));
        }
        cases.push((
            "/processors".into(),
            json!(vec![saved_settings()["processors"][0].clone(); 2]),
        ));
        for (path, bad) in cases {
            let mut settings = saved_settings();
            *settings.pointer_mut(&path).unwrap() = bad;
            let bytes = serde_json::to_vec(&saved_file(settings, "7")).unwrap();
            std::fs::write(root.0.join("receiver-presets.json"), &bytes).unwrap();
            assert!(
                Store::open(root.0.clone()).is_err(),
                "accepted malformed {path}"
            );
            assert_eq!(
                std::fs::read(root.0.join("receiver-presets.json")).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn preset_storage_keeps_legacy_optional_fields_and_defers_mode_and_rf_admission() {
        let root = TestDirectory::new();
        for mode in ["am", "drmplus", "future-provider-mode"] {
            let mut settings = saved_settings();
            settings["baseband"]["frequency_hz"] = json!(1e15);
            settings["processors"] =
                json!([{"id":"primary","kind":"decoder","mode":mode,"minimum_rate_hz":0}]);
            settings.as_object_mut().unwrap().remove("continuity");
            let bytes =
                serde_json::to_vec(&saved_file(settings.clone(), "9007199254740993")).unwrap();
            std::fs::write(root.0.join("receiver-presets.json"), bytes).unwrap();
            let mut store = Store::open(root.0.clone())
                .expect("storage does not require an available mode/device");
            assert_eq!(store.data["presets"][0]["settings"], settings);
            let saved = store
                .change(
                    &json!({"name":"copy","expected_revision":"9007199254740993"}),
                    Some(settings),
                )
                .unwrap();
            assert_eq!(saved["revision"], "9007199254740994");
            assert_eq!(Store::open(root.0.clone()).unwrap().data, saved);
        }
    }

    #[test]
    fn preset_storage_revision_is_decimal_u64_and_cannot_wrap() {
        let root = TestDirectory::new();
        for revision in ["+0", "-0", "", " 0", "18446744073709551616"] {
            let bytes = serde_json::to_vec(&saved_file(saved_settings(), revision)).unwrap();
            std::fs::write(root.0.join("receiver-presets.json"), &bytes).unwrap();
            assert!(
                Store::open(root.0.clone()).is_err(),
                "accepted revision {revision:?}"
            );
            assert_eq!(
                std::fs::read(root.0.join("receiver-presets.json")).unwrap(),
                bytes
            );
        }
        let bytes =
            serde_json::to_vec(&saved_file(saved_settings(), &u64::MAX.to_string())).unwrap();
        std::fs::write(root.0.join("receiver-presets.json"), &bytes).unwrap();
        let mut store = Store::open(root.0.clone()).unwrap();
        let previous = store.data.clone();
        assert!(store
            .change(
                &json!({"name":"saved","expected_revision":u64::MAX.to_string()}),
                None
            )
            .is_err());
        assert_eq!(store.data, previous);
        assert_eq!(
            std::fs::read(root.0.join("receiver-presets.json")).unwrap(),
            bytes
        );
    }

    #[test]
    fn preset_storage_rejects_invalid_change_before_replacing_the_saved_file() {
        let root = TestDirectory::new();
        let bytes = serde_json::to_vec(&saved_file(saved_settings(), "1")).unwrap();
        std::fs::write(root.0.join("receiver-presets.json"), &bytes).unwrap();
        let mut store = Store::open(root.0.clone()).unwrap();
        let previous = store.data.clone();
        assert!(store
            .change(
                &json!({"name":"saved","expected_revision":"1"}),
                Some(json!({"purpose":"listen","baseband":{},"processors":[null]}))
            )
            .is_err());
        assert_eq!(store.data, previous);
        assert_eq!(
            std::fs::read(root.0.join("receiver-presets.json")).unwrap(),
            bytes
        );
        assert!(store
            .change(&json!({"name":"saved","expected_revision":"+1"}), None)
            .is_err());
        assert_eq!(
            std::fs::read(root.0.join("receiver-presets.json")).unwrap(),
            bytes
        );
    }

    #[test]
    fn preset_storage_temp_collision_cannot_follow_or_remove_an_existing_file() {
        let root = TestDirectory::new();
        let path = root.0.join("receiver-presets.json");
        let original = serde_json::to_vec(&saved_file(saved_settings(), "1")).unwrap();
        std::fs::write(&path, &original).unwrap();
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        std::os::unix::fs::symlink(&path, &temporary).unwrap();
        let mut store = Store::open(root.0.clone()).unwrap();
        let previous = store.data.clone();
        let result = store.change(&json!({"name":"saved","expected_revision":"1"}), None);
        assert!(
            result.is_err(),
            "pre-existing temporary path must not be overwritten"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "atomic writes cannot modify the original through a symlink"
        );
        assert_eq!(store.data, previous);
        assert_eq!(
            std::fs::read_link(&temporary).unwrap(),
            path,
            "cleanup cannot delete someone else's path"
        );
    }

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
        let settings = json!({"purpose":"iq","baseband":{"frequency_hz":100e6,
            "passband_hz":6000,"transition_hz":1000,"minimum_rate_hz":8000},"processors":[]});
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
