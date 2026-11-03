//! A task's independent source segments packaged as a standard SigMF Collection.
//! Rates, frequencies and clocks stay separate; no samples bridge a source boundary.
use super::recording_export::{self as stream, Hashed};
use super::recording_read::Selection;
use super::*;
use sha2::{Digest, Sha512};
use std::{
    io::{self, Write},
    path::Path as FsPath,
};

pub(super) const MAX_SEGMENTS: usize = 4096;
const MAX_PLAN_BYTES: usize = 4 * 1024 * 1024;

fn invalid(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}
struct Record {
    name: String,
    selection: Selection,
    plan: stream::Plan,
    metadata_hash: [u8; 64],
}
pub(super) struct Plan {
    records: Vec<Record>,
    collection: Vec<u8>,
    length: u64,
}

/// Validate limits before any archive I/O. A bad later segment rejects the whole
/// request before success; no partial subset is silently substituted.
fn selections(task: &Value) -> io::Result<Vec<Selection>> {
    let segments = task["tracks"][0]["segments"]
        .as_array()
        .ok_or_else(|| invalid("recording segments absent"))?;
    if segments.len() > MAX_SEGMENTS {
        return Err(invalid("collection exceeds 4096 segments"));
    }
    let mut selections = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut samples = 0u64;
    for segment in segments {
        let id = segment["segment_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("segment identity absent"))?;
        if !seen.insert(id) {
            return Err(invalid("duplicate recording segment"));
        }
        let count = segment["samples"]
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| invalid("invalid segment sample count"))?;
        samples = samples
            .checked_add(count)
            .filter(|n| *n <= stream::MAX_SAMPLES)
            .ok_or_else(|| {
                invalid("collection exceeds sample limit; download individual ranges")
            })?;
        if count > 0 {
            selections.push(Selection {
                segment: id.to_owned(),
                start: 0,
                count,
            });
        }
    }
    if selections.is_empty() {
        return Err(invalid("collection contains no verified samples"));
    }
    Ok(selections)
}
pub(super) fn prepare(path: &FsPath, task: &Value, cancel: &AtomicBool) -> io::Result<Plan> {
    let mut records = Vec::new();
    let mut streams = Vec::new();
    let mut retained = 0usize;
    let mut length = 1024u64; // A single tar terminator for the entire collection.
    for selection in selections(task)? {
        let plan = stream::prepare(path, task, &selection, cancel)?;
        retained += plan.retained_bytes() + selection.segment.len();
        if retained > MAX_PLAN_BYTES / 2 {
            return Err(invalid("collection plan exceeds 2 MiB"));
        }
        let mut hash = Hashed {
            output: io::sink(),
            hash: Sha512::new(),
        };
        stream::metadata(&mut hash, path, task, &selection, &plan, cancel)?;
        let metadata_hash: [u8; 64] = hash.hash.finalize().into();
        // Generated ordinals keep every path portable and independent of source IDs.
        let name = format!("segment-{:06}", records.len());
        let hex: String = metadata_hash.iter().map(|b| format!("{b:02x}")).collect();
        streams.push(json!({"name":name,"hash":hex}));
        length = length
            .checked_add(stream::total(&plan) - 1024)
            .ok_or_else(|| invalid("collection length overflow"))?;
        records.push(Record {
            name,
            selection,
            plan,
            metadata_hash,
        });
    }
    let collection = serde_json::to_vec(&json!({"collection":{
        "core:version":"1.2.6", "core:streams":streams,
        "core:description":"Verified segments in recorded order; no continuity or time alignment between records is implied.",
        "core:extensions":[{"name":"websa","version":"1.0.0","optional":true}],
        "websa:task_id":task["task_id"],"websa:service_id":task["service_id"],"websa:receiver_uid":task["receiver_uid"],
        "websa:task_state":task["state"],"websa:task_fault":task["fault"],"websa:utc_reliable":false
    }}))?;
    if collection.len() > MAX_PLAN_BYTES / 2 {
        return Err(invalid("collection manifest exceeds 2 MiB"));
    }
    length = length
        .checked_add(512 + (collection.len() as u64).next_multiple_of(512))
        .ok_or_else(|| invalid("collection length overflow"))?;
    Ok(Plan {
        records,
        collection,
        length,
    })
}
pub(super) fn archive(
    output: &mut (impl Write + ?Sized),
    path: &FsPath,
    task: &Value,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<()> {
    for record in &plan.records {
        let hash = stream::members(
            output,
            path,
            task,
            &record.selection,
            &record.plan,
            cancel,
            &record.name,
        )?;
        if hash != record.metadata_hash {
            return Err(invalid("collection metadata changed after verification"));
        }
    }
    // Publish the collection only after every member has passed its emission check.
    stream::header(
        output,
        "recording.sigmf-collection",
        plan.collection.len() as u64,
    )?;
    output.write_all(&plan.collection)?;
    let padding = (512 - plan.collection.len() % 512) % 512;
    output.write_all(&[0; 512][..padding])?;
    output.write_all(&[0; 1024])
}

pub(super) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !params.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"collection exports all verified segments; no range parameters accepted"}))).into_response();
    }
    stream::download(
        app,
        task,
        "recording-collection.sigmf",
        |path, task, cancel| {
            let plan = prepare(path, task, cancel)?;
            Ok((plan.length, plan))
        },
        |output, path, task, plan, cancel| archive(output, path, task, plan, cancel),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collection_limits_reject_duplicates_overflow_empty_and_oversized_tasks() {
        for segments in [
            json!([]),
            json!([{"segment_id":"a","samples":"0"}]),
            json!([{"segment_id":"a","samples":"1"},{"segment_id":"a","samples":"2"}]),
            json!([{"segment_id":"a","samples":"1073741823"},{"segment_id":"b","samples":"1"}]),
            json!([{"segment_id":"a","samples":"18446744073709551616"}]),
            json!([{"segment_id":"a","samples":"-1"}]),
        ] {
            assert!(selections(&json!({"tracks":[{"segments":segments}]})).is_err());
        }
        let valid = json!({"tracks":[{"segments":[{"segment_id":"0","samples":"0"},
            {"segment_id":"a","samples":"123"},{"segment_id":"b","samples":"456"}]}]});
        let result = selections(&valid).unwrap();
        assert_eq!(
            result
                .iter()
                .map(|s| s.segment.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(result.iter().map(|s| s.count).sum::<u64>(), 579);
    }
}
