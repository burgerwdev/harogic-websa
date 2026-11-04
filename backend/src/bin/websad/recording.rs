//! Backend-owned IQ recording. Publication only admits shared immutable Bytes; all filesystem
//! operations, compression, checksums and recovery belong to these bounded worker lanes.
use super::*;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path as FsPath, PathBuf},
    sync::{Condvar, Mutex},
    thread::JoinHandle,
    time::Instant,
};
use websa_channelizer::iq_archive::{v2, MAX_BLOCK_BYTES};
#[path = "recording_results.rs"]
pub(super) mod results;
#[path = "recording_journal.rs"]
mod journal;
#[path = "recording_audio.rs"]
pub(super) mod audio;
#[path = "recording_flac.rs"]
pub(super) mod flac;
#[path = "recording_audio_read.rs"]
pub(super) mod audio_read;
#[path = "recording_opus.rs"]
pub(super) mod opus;
#[path = "recording_management.rs"]
pub(super) mod management;

const MAX_ACTIVE: usize = 3;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ITEMS: usize = 64;
const MAX_AGE: Duration = Duration::from_secs(2);
const BATCH_ITEMS: usize = 32;
const BATCH_BYTES: usize = 1024 * 1024;
const BATCH_WAIT: Duration = Duration::from_millis(50);
const MAX_SEGMENTS: usize = 4096;
const VOLUME_BYTES: u64 = 256 * 1024 * 1024;
const VOLUME_BLOCKS: u64 = 4096;

fn io_error(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}
fn millis() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
fn active(state: &str) -> bool {
    matches!(state, "preparing" | "waiting" | "recording" | "finalizing")
}
fn entry_file_hidden(path: &FsPath) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}
fn number(value: &Value) -> u64 {
    value.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}
fn add(value: &mut Value, amount: u64) {
    *value = json!((number(value) + amount).to_string());
}
fn persist(path: &FsPath, task: &Value) -> io::Result<()> {
    let temporary = path.join("task.json.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&serde_json::to_vec(task)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path.join("task.json"))?;
    File::open(path)?.sync_all()
}
fn set_fault(task: &mut Value, code: &str, message: &str) {
    task["state"] = json!("failed");
    task["tracks"][0]["state"] = json!("failed");
    task["fault"] = json!({"scope":"recording","code":code,"message":message,
        "receiver_uid":task["receiver_uid"],"task_id":task["task_id"],"retryable":false});
}
#[derive(Clone)]
pub(super) struct Input {
    pub bytes: Bytes,
    pub meta: v2::Meta,
}
struct Queued {
    input: WorkInput,
    at: Instant,
}
#[derive(Clone)]
enum WorkInput {
    Iq(Input),
    Results(Bytes),
    Audio(audio::Input),
}
impl WorkInput {
    fn bytes(&self) -> usize {
        match self { Self::Iq(input) => input.bytes.len(), Self::Results(bytes) => bytes.len(), Self::Audio(input) => input.bytes.len() + input.source.len() + 4 }
    }
    fn raw_bytes(&self) -> usize {
        match self { Self::Iq(input) => input.bytes.len() - 56, Self::Results(bytes) => bytes.len(), Self::Audio(input) => input.bytes.len().saturating_sub(96) }
    }
}
struct Queue {
    items: VecDeque<Queued>,
    bytes: usize,
    in_flight: Option<Instant>,
    in_flight_items: usize,
    sealed: bool,
    prepared: bool,
    failure: Option<(String, String)>,
    reason: String,
    accepted: u64,
    last_input: Instant,
}
struct Lane {
    uid: String,
    kind: String,
    task: Mutex<Value>,
    queue: Mutex<Queue>,
    wake: Condvar,
    done: AtomicBool,
}
impl Lane {
    fn fail(queue: &mut Queue, code: &str, message: impl Into<String>) {
        queue.sealed = true;
        if queue.failure.is_none() {
            queue.failure = Some((code.into(), message.into()));
        }
    }
    fn overdue(queue: &Queue) -> bool {
        queue
            .in_flight
            .or_else(|| queue.items.front().map(|item| item.at))
            .is_some_and(|at| at.elapsed() > MAX_AGE)
    }
    fn seal(&self, reason: &str, failure: Option<(&str, &str)>) {
        let mut queue = self.queue.lock().unwrap();
        queue.sealed = true;
        queue.reason = reason.into();
        if let Some((code, message)) = failure {
            Self::fail(&mut queue, code, message);
        }
        self.wake.notify_one();
    }
    fn next_batch(&self) -> Vec<Queued> {
        let mut queue = self.queue.lock().unwrap();
        while queue.items.is_empty() && !queue.sealed {
            queue = self.wake.wait(queue).unwrap();
        }
        // Small source packets otherwise require individual durability barriers.
        // Coalesce briefly without holding the queue lock; the original oldest-item
        // deadline and all byte/item reservations include the entire in-flight batch.
        if let Some(first) = queue.items.front() {
            let deadline = first.at + BATCH_WAIT;
            while !queue.sealed && queue.items.len() < BATCH_ITEMS && queue.bytes < BATCH_BYTES {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                queue = self.wake.wait_timeout(queue, remaining).unwrap().0;
            }
        }
        if Lane::overdue(&queue) {
            Lane::fail(
                &mut queue,
                "RECORDING_BACKLOG",
                "recording backlog exceeded 2 seconds",
            );
            return Vec::new();
        }
        let mut batch = Vec::new();
        let mut bytes = 0;
        while batch.len() < BATCH_ITEMS {
            let Some(next) = queue.items.front() else {
                break;
            };
            if !batch.is_empty() && bytes + next.input.bytes() > BATCH_BYTES {
                break;
            }
            let item = queue.items.pop_front().unwrap();
            bytes += item.input.bytes();
            batch.push(item);
        }
        queue.in_flight = batch.first().map(|item| item.at);
        queue.in_flight_items = batch.len();
        batch
    }
    fn complete_batch(&self, bytes: usize) {
        let mut queue = self.queue.lock().unwrap();
        queue.bytes -= bytes;
        queue.in_flight = None;
        queue.in_flight_items = 0;
    }
    fn snapshot(&self) -> Value {
        let mut value = self.task.lock().unwrap().clone();
        let queue = self.queue.lock().unwrap();
        value["accepted_bytes"] = json!(queue.accepted.to_string());
        value["queued_bytes"] = json!(queue.bytes.to_string());
        if !self.done.load(Ordering::Acquire) {
            if let Some((code, message)) = &queue.failure {
                set_fault(&mut value, code, message);
            } else if queue.sealed {
                value["state"] = json!("finalizing");
                value["tracks"][0]["state"] = json!("finalizing");
            }
        }
        value
    }
}
#[derive(Clone)]
pub(super) struct StartTicket {
    lane: Arc<Lane>,
}
impl StartTicket {
    /// API callers release acquisition state before waiting for durable preparation.
    pub(super) fn wait(&self) -> Result<(), String> {
        let deadline = Instant::now() + MAX_AGE;
        let mut queue = self.lane.queue.lock().unwrap();
        while !queue.prepared && queue.failure.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                Lane::fail(
                    &mut queue,
                    "RECORDING_PREPARATION",
                    "recording storage preparation exceeded 2 seconds",
                );
                self.lane.wake.notify_all();
                break;
            }
            queue = self.lane.wake.wait_timeout(queue, remaining).unwrap().0;
        }
        if let Some((_, message)) = &queue.failure {
            Err(message.clone())
        } else {
            Ok(())
        }
    }
}
struct Handle {
    lane: Arc<Lane>,
    thread: Option<JoinHandle<()>>,
}
pub(super) struct Manager {
    root: PathBuf,
    _lock: File,
    history: Vec<Value>,
    lanes: Vec<Handle>,
    next: u64,
}
impl Manager {
    pub(super) fn open(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".owner"))?;
        lock.try_lock()
            .map_err(|e| io_error(format!("recording directory already owned: {e}")))?;
        let mut history = Vec::new();
        for entry in fs::read_dir(&root)? {
            let path = entry?.path();
            if !path.is_dir() || entry_file_hidden(&path) {
                continue;
            }
            let manifest = path.join("task.json");
            if !manifest.exists() {
                continue;
            }
            let mut task: Value = serde_json::from_reader(File::open(&manifest)?)?;
            if task["storage_version"] != 2 {
                return Err(io_error(format!(
                    "unsupported recording manifest: {}",
                    manifest.display()
                )));
            }
            let recovered = match task["tracks"][0]["kind"].as_str() { Some("results") => results::recover(&path, &mut task), Some("audio") => audio::recover(&path, &mut task), _ => recover(&path, &mut task) };
            if let Err(error) = recovered {
                set_fault(&mut task, "RECORDING_RECOVERY", &error.to_string());
            }
            history.push(task);
        }
        history.sort_by_key(|task| task["task_id"].as_str().unwrap_or("").to_owned());
        Ok(Self {
            root,
            _lock: lock,
            history,
            lanes: Vec::new(),
            next: 0,
        })
    }
    pub(super) fn snapshots(&self) -> Vec<Value> {
        self.history
            .iter()
            .cloned()
            .chain(self.lanes.iter().map(|h| h.lane.snapshot()))
            .collect()
    }
    /// Finished tasks have immutable indexes. A failed live lane must finish draining first.
    pub(super) fn readable_task(&self, id: &str) -> Result<(PathBuf, Value), &'static str> {
        if let Some(task) = self.history.iter().find(|t| t["task_id"] == id) {
            return Ok((self.root.join(id), task.clone()));
        }
        if let Some(handle) = self.lanes.iter().find(|h| h.lane.task.lock().unwrap()["task_id"] == id) {
            if handle.lane.done.load(Ordering::Acquire) {
                return Ok((self.root.join(id), handle.lane.snapshot()));
            }
            return Err("recording must finish before reading");
        }
        Err("recording task not found")
    }
    pub(super) fn has_active(&self) -> bool {
        self.lanes.iter().any(|h| {
            let q = h.lane.queue.lock().unwrap();
            q.prepared && !q.sealed
        })
    }
    #[cfg(test)]
    pub(super) fn start(
        &mut self,
        uid: &str,
        service: &str,
        request: &str,
    ) -> Result<StartTicket, String> {
        self.start_kind(uid, service, request, "iq")
    }
    #[cfg(test)]
    pub(super) fn start_kind(&mut self, uid: &str, service: &str, request: &str, kind: &str) -> Result<StartTicket, String> {
        self.start_configured(uid, service, request, kind, "wav", "")
    }
    pub(super) fn start_configured(&mut self, uid: &str, service: &str, request: &str, kind: &str, audio_format: &str, name: &str) -> Result<StartTicket, String> {
        self.reap();
        if !matches!(kind, "iq" | "results" | "audio") { return Err("unsupported recording kind".into()); }
        if self.lanes.iter().any(|h| h.lane.uid == uid && h.lane.kind == kind) {
            return Err(format!("{kind} recording is already active or finalizing for this receiver"));
        }
        if self.lanes.len() >= MAX_ACTIVE {
            return Err("recording worker capacity reached".into());
        }
        self.next += 1;
        let id = format!("{kind}-{service}-{}", self.next);
        let track = format!("{id}-{kind}");
        let mut task = json!({"storage_version":2,"task_id":id,"receiver_uid":uid,"service_id":service,"request_id":request,
            "name":name,
            "created_at_ms":millis(),"updated_at_ms":millis(),"state":"preparing","fault":null,
            "tracks":[{"track_id":track,"kind":"iq","format":"cf32_le","source":"baseband",
                "compression":"zstd_auto_lossless","container":"websa-iq-v2","state":"preparing",
                "segments":[],"raw_bytes":"0","stored_bytes":"0","blocks":"0","volumes":[]}],
            "accepted_bytes":"0","queued_bytes":"0"});
        if kind == "results" {
            let t = &mut task["tracks"][0];
            t["kind"] = json!("results"); t["format"] = json!("jsonl");
            t["source"] = json!("decoder_results"); t["container"] = json!("websa-results-v1");
        }
        if kind == "audio" {
            task["audio_format"] = json!(audio_format);
            let t = &mut task["tracks"][0];
            t["kind"] = json!("audio"); t["format"] = json!("f32_le");
            t["source"] = json!("published_audio"); t["container"] = json!("websa-audio-v1");
        }
        let lane = Arc::new(Lane {
            uid: uid.into(),
            kind: kind.into(),
            task: Mutex::new(task),
            done: AtomicBool::new(false),
            wake: Condvar::new(),
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                bytes: 0,
                in_flight: None,
                in_flight_items: 0,
                sealed: false,
                prepared: false,
                failure: None,
                reason: "record_stopped".into(),
                accepted: 0,
                last_input: Instant::now(),
            }),
        });
        let path = self.root.join(&id);
        let owner = lane.clone();
        let thread = std::thread::Builder::new()
            .name(format!("{kind}-recording"))
            .spawn(move || run(owner, path))
            .map_err(|e| e.to_string())?;
        let ticket = StartTicket { lane: lane.clone() };
        self.lanes.push(Handle {
            lane,
            thread: Some(thread),
        });
        Ok(ticket)
    }
    pub(super) fn stop(&mut self, id: &str) -> Result<(), String> {
        if let Some(handle) = self
            .lanes
            .iter()
            .find(|h| h.lane.task.lock().unwrap()["task_id"] == id)
        {
            handle.lane.seal("record_stopped", None);
            return Ok(());
        }
        if self.history.iter().any(|task| task["task_id"] == id) {
            return Ok(());
        }
        Err("recording task not found".into())
    }
    pub(super) fn admit(&self, uid: &str, input: Input) {
        self.admit_input(uid, "iq", WorkInput::Iq(input));
    }
    pub(super) fn admit_result(&self, uid: &str, bytes: Bytes) {
        self.admit_input(uid, "results", WorkInput::Results(bytes));
    }
    pub(super) fn admit_audio(&self, uid: &str, input: audio::Input) {
        self.admit_input(uid, "audio", WorkInput::Audio(input));
    }
    pub(super) fn wants_audio(&self) -> bool {
        self.lanes.iter().any(|h| h.lane.kind == "audio" && !h.lane.queue.lock().unwrap().sealed)
    }
    pub(super) fn wants_results(&self) -> bool {
        self.lanes.iter().any(|h| h.lane.kind == "results" && !h.lane.queue.lock().unwrap().sealed)
    }
    fn admit_input(&self, uid: &str, kind: &str, input: WorkInput) {
        for handle in self.lanes.iter().filter(|h| h.lane.uid == uid && h.lane.kind == kind) {
            let lane = &handle.lane;
            let mut queue = lane.queue.lock().unwrap();
            if queue.sealed || !queue.prepared {
                continue;
            }
            let bytes = input.bytes();
            if Lane::overdue(&queue) {
                Lane::fail(
                    &mut queue,
                    "RECORDING_BACKLOG",
                    "recording backlog exceeded 2 seconds",
                );
            } else if bytes > (if kind == "iq" { MAX_BLOCK_BYTES + 56 } else { results::MAX_RECORD })
                || queue.bytes + bytes > MAX_BYTES
                || queue.items.len()
                    + queue.in_flight_items.max(usize::from(queue.in_flight.is_some()))
                    >= MAX_ITEMS
            {
                Lane::fail(
                    &mut queue,
                    "RECORDING_BACKLOG",
                    "recording queue byte/item budget exceeded",
                );
            } else {
                queue.bytes += bytes;
                queue.accepted += input.raw_bytes() as u64;
                queue.last_input = Instant::now();
                queue.items.push_back(Queued {
                    input: input.clone(),
                    at: Instant::now(),
                });
            }
            lane.wake.notify_one();
        }
    }
    pub(super) fn source_end(&self, uid: &str) {
        for handle in self.lanes.iter().filter(|h| h.lane.uid == uid) {
            handle.lane.seal("receiver_stopped", None);
        }
    }
    /// A quiet decoder may legitimately publish no events for arbitrarily long periods.
    /// Processing removal/failure is observed explicitly after its retiring FIFO is delivered.
    pub(super) fn processing_sources(&self, active: &[String], error: Option<&str>) {
        self.processing_kind_sources("results", active, error);
    }
    pub(super) fn processing_kind_sources(&self, kind: &str, active: &[String], error: Option<&str>) {
        for handle in self.lanes.iter().filter(|h| h.lane.kind == kind) {
            if handle.lane.queue.lock().unwrap().sealed { continue; }
            if let Some(error) = error {
                handle.lane.seal("processing_failed", Some(("PROCESSING_FAILED", error)));
            } else if !active.contains(&handle.lane.uid) {
                handle.lane.seal("processing_stopped", None);
            }
        }
    }
    pub(super) fn tick(&mut self, capture_error: Option<&str>) {
        for handle in &self.lanes {
            let mut queue = handle.lane.queue.lock().unwrap();
            if !queue.sealed {
                if let Some(error) = capture_error {
                    Lane::fail(&mut queue, "SOURCE_INTERRUPTED", error);
                } else if Lane::overdue(&queue) {
                    Lane::fail(
                        &mut queue,
                        "RECORDING_BACKLOG",
                        "recording backlog exceeded 2 seconds",
                    );
                } else if handle.lane.kind == "iq" && queue.last_input.elapsed() > Duration::from_secs(5) {
                    Lane::fail(
                        &mut queue,
                        "SOURCE_INTERRUPTED",
                        "IQ source stopped publishing for 5 seconds",
                    );
                }
                handle.lane.wake.notify_one();
            }
        }
        self.reap();
    }
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.lanes.len() {
            if self.lanes[i].lane.done.load(Ordering::Acquire) {
                let mut handle = self.lanes.remove(i);
                let _ = handle.thread.take().unwrap().join();
                self.history.push(handle.lane.snapshot());
            } else {
                i += 1;
            }
        }
    }
    fn managed(&mut self, id: &str, task: Option<Value>) {
        self.reap();
        self.history.retain(|t| t["task_id"] != id);
        if let Some(task) = task { self.history.push(task); }
        self.history.sort_by_key(|t| t["created_at_ms"].as_str().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0));
    }
    pub(super) fn shutdown(&mut self) {
        for handle in &self.lanes {
            if !handle.lane.queue.lock().unwrap().sealed {
                handle.lane.seal(
                    "service_shutdown",
                    Some(("SERVICE_INTERRUPTED", "service stopped during recording")),
                );
            }
        }
        for handle in &mut self.lanes {
            if let Some(thread) = handle.thread.take() {
                let _ = thread.join();
            }
        }
        self.history
            .extend(self.lanes.drain(..).map(|h| h.lane.snapshot()));
    }
}

fn update_block(task: &mut Value, index: &v2::Index) -> io::Result<()> {
    let track = &mut task["tracks"][0];
    let segments = track["segments"]
        .as_array_mut()
        .ok_or_else(|| io_error("recording segments absent"))?;
    if segments
        .last()
        .is_none_or(|s| s["segment_id"] != index.meta.segment_id)
    {
        if segments.len() >= MAX_SEGMENTS {
            return Err(io_error("recording segment limit reached"));
        }
        if let Some(last) = segments.last_mut() {
            last["end_reason"] = json!("reconfigured");
        }
        segments.push(json!({"segment_id":index.meta.segment_id,
            "stream":{"source_id":index.meta.clock.source_id,"epoch":index.meta.clock.epoch,"channel_id":index.meta.clock.channel_id},
            "first_input_index":index.meta.clock.first_input_index,"first_output_index":index.meta.clock.first_output_index,
            "fraction_num":index.meta.clock.fraction_num,"fraction_den":index.meta.clock.fraction_den,
            "input_rate_hz":index.meta.clock.input_rate_hz,"actual_rate_hz":index.meta.clock.actual_rate_hz,
            "delay_seconds":index.meta.clock.delay_seconds,"frequency_hz":index.meta.frequency_hz,
            "baseband":index.meta.baseband,"config_revision":index.meta.clock.config_revision,
            "samples":"0","end_reason":null,"utc_reliable":false}));
    }
    add(
        &mut segments.last_mut().unwrap()["samples"],
        u64::from(index.meta.clock.samples),
    );
    add(&mut track["raw_bytes"], u64::from(index.raw_bytes));
    add(&mut track["blocks"], 1);
    Ok(())
}
struct Volume {
    writer: Option<v2::Writer<File>>,
    index: File,
    blocks: u64,
    name: String,
}
impl Volume {
    fn new(path: &FsPath, ordinal: usize) -> io::Result<Self> {
        let name = format!("iq-{ordinal:06}");
        Ok(Self {
            writer: Some(v2::Writer::new(
                OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(path.join(format!("{name}.wbiq")))?,
            )?),
            index: OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path.join(format!("{name}.jsonl")))?,
            blocks: 0,
            name,
        })
    }
    fn append(&mut self, input: Input) -> io::Result<(v2::Index, u64)> {
        let index = self
            .writer
            .as_mut()
            .unwrap()
            .append(input.meta, &input.bytes[56..])?;
        self.index.write_all(&serde_json::to_vec(&index)?)?;
        self.index.write_all(b"\n")?;
        self.blocks += 1;
        Ok((index, self.writer.as_mut().unwrap().position()?))
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        self.writer.as_mut().unwrap().output_mut().sync_data()?;
        self.index.sync_data()
    }
    fn finish(&mut self) -> io::Result<u64> {
        let output = self.writer.take().unwrap().finish()?;
        output.sync_all()?;
        self.index.sync_all()?;
        Ok(output.metadata()?.len())
    }
}
fn run(lane: Arc<Lane>, path: PathBuf) {
    let result = match lane.kind.as_str() { "results" => results::work(&lane, &path), "audio" => audio::work(&lane, &path), _ => work(&lane, &path) };
    let mut task = lane.task.lock().unwrap().clone();
    let mut queue = lane.queue.lock().unwrap();
    // Seal immediately on worker exit, before any final manifest I/O can block.
    queue.sealed = true;
    if let Err(error) = &result {
        Lane::fail(&mut queue, "RECORDING_IO", error.to_string());
    }
    lane.wake.notify_all();
    let failure = queue.failure.clone();
    let reason = queue.reason.clone();
    task["accepted_bytes"] = json!(queue.accepted.to_string());
    task["queued_bytes"] = json!("0");
    drop(queue);
    if let Err(error) = result {
        set_fault(&mut task, "RECORDING_IO", &error.to_string());
    } else if let Some((code, message)) = failure {
        set_fault(&mut task, &code, &message);
    } else {
        task["state"] = json!("completed");
        task["tracks"][0]["state"] = json!("completed");
    }
    let failed = task["state"] == "failed";
    if let Some(last) = task["tracks"][0]["segments"]
        .as_array_mut()
        .and_then(|s| s.last_mut())
    {
        last["end_reason"] = json!(if failed { "failed" } else { reason.as_str() });
    }
    task["updated_at_ms"] = json!(millis());
    if let Err(error) = persist(&path, &task) {
        set_fault(
            &mut task,
            "RECORDING_IO",
            &format!("cannot persist task: {error}"),
        );
    }
    *lane.task.lock().unwrap() = task;
    let mut queue = lane.queue.lock().unwrap();
    queue.items.clear();
    queue.bytes = 0;
    queue.in_flight = None;
    queue.in_flight_items = 0;
    queue.sealed = true;
    drop(queue);
    lane.done.store(true, Ordering::Release);
}
fn work(lane: &Lane, path: &FsPath) -> io::Result<()> {
    fs::create_dir(path)?;
    File::open(
        path.parent()
            .ok_or_else(|| io_error("recording directory has no parent"))?,
    )?
    .sync_all()?;
    let mut task = lane.task.lock().unwrap().clone();
    persist(path, &task)?;
    let mut volume = Volume::new(path, 0)?;
    task["tracks"][0]["volumes"].as_array_mut().unwrap().push(json!({"file":format!("{}.wbiq",volume.name),"index":format!("{}.jsonl",volume.name),"complete":false}));
    volume.writer.as_mut().unwrap().output_mut().sync_all()?;
    volume.index.sync_all()?;
    persist(path, &task)?;
    *lane.task.lock().unwrap() = task.clone();
    {
        let mut queue = lane.queue.lock().unwrap();
        queue.prepared = true;
        queue.last_input = Instant::now();
        lane.wake.notify_all();
    }

    let mut previous: Option<v2::Index> = None;
    let mut finished_bytes = 0;
    loop {
        let batch = lane.next_batch();
        if batch.is_empty() {
            break;
        }
        let size: usize = batch.iter().map(|item| item.input.bytes()).sum();
        let outcome = (|| -> io::Result<()> {
            for item in batch {
                let WorkInput::Iq(mut input) = item.input else {
                    return Err(io_error("wrong recording lane input"));
                };
                input.meta.clock.track_id = task["tracks"][0]["track_id"].as_str().unwrap().into();
                if let Some(old) = &previous {
                    if old.meta.segment_id == input.meta.segment_id {
                        let expected = number(&json!(old.meta.sequence)) + 1;
                        let output = old
                            .meta
                            .clock
                            .first_output_index
                            .parse::<u64>()
                            .map_err(io_error)?
                            + u64::from(old.meta.clock.samples);
                        let step = (old.meta.clock.input_rate_hz / old.meta.clock.actual_rate_hz)
                            .round() as u64;
                        let next_input = old
                            .meta
                            .clock
                            .first_input_index
                            .parse::<u64>()
                            .map_err(io_error)?
                            .checked_add(
                                u64::from(old.meta.clock.samples)
                                    .checked_mul(step)
                                    .ok_or_else(|| io_error("IQ clock overflow"))?,
                            )
                            .ok_or_else(|| io_error("IQ clock overflow"))?;
                        if input.meta.clock.first_input_index != next_input.to_string()
                            || input.meta.clock.fraction_num != old.meta.clock.fraction_num
                            || input.meta.clock.fraction_den != old.meta.clock.fraction_den
                            || input.meta.frequency_hz != old.meta.frequency_hz
                            || input.meta.baseband != old.meta.baseband
                            || input.meta.sequence != expected.to_string()
                            || input.meta.clock.first_output_index != output.to_string()
                            || input.meta.clock.actual_rate_hz != old.meta.clock.actual_rate_hz
                        {
                            return Err(io_error(
                                "IQ sequence/clock discontinuity within recording segment",
                            ));
                        }
                    }
                }
                if previous
                    .as_ref()
                    .is_none_or(|old| old.meta.segment_id != input.meta.segment_id)
                {
                    volume.writer.as_mut().unwrap().event(&json!({"type":"segment_start","segment_id":input.meta.segment_id,
                "previous":previous.as_ref().map(|old|&old.meta.segment_id),"reason":if previous.is_some(){"reconfigured"}else{"record_started"}}))?;
                }
                let (index, position) = volume.append(input)?;
                update_block(&mut task, &index)?;
                previous = Some(index);
                task["state"] = json!("recording");
                task["tracks"][0]["state"] = json!("recording");
                task["tracks"][0]["stored_bytes"] = json!((finished_bytes + position).to_string());
                if position >= VOLUME_BYTES || volume.blocks >= VOLUME_BLOCKS {
                    let volume_bytes = volume.finish()?;
                    finished_bytes += volume_bytes;
                    let volumes = task["tracks"][0]["volumes"].as_array_mut().unwrap();
                    volumes.last_mut().unwrap()["complete"] = json!(true);
                    volumes.last_mut().unwrap()["valid_bytes"] = json!(volume_bytes.to_string());
                    volume = Volume::new(path, volumes.len())?;
                    volumes.push(json!({"file":format!("{}.wbiq",volume.name),"index":format!("{}.jsonl",volume.name),"complete":false}));
                }
            }
            Ok(())
        })();
        // Publish counters only after every block/index in this batch and its manifest
        // are durable. Volume rotation still finishes/syncs each old volume in order.
        if volume.writer.is_none() {
            // A failed volume finish/replacement has no open writer to sync.
            // Keep the last committed task; startup can inspect any checked tail.
            outcome?;
            return Err(io_error("recording volume was not replaced"));
        }
        volume.checkpoint()?;
        if outcome.is_ok() {
            task["tracks"][0]["stored_bytes"] =
                json!((finished_bytes + volume.writer.as_mut().unwrap().position()?).to_string());
        }
        task["updated_at_ms"] = json!(millis());
        task["accepted_bytes"] = json!(lane.queue.lock().unwrap().accepted.to_string());
        persist(path, &task)?;
        *lane.task.lock().unwrap() = task.clone();
        lane.complete_batch(size);
        // A bad later packet must not discard the checked, durable prefix of
        // this batch. Publish that prefix before surfacing the original error.
        outcome?;
    }
    task["state"] = json!("finalizing");
    task["tracks"][0]["state"] = json!("finalizing");
    *lane.task.lock().unwrap() = task.clone();
    persist(path, &task)?;
    let end = {
        let queue = lane.queue.lock().unwrap();
        json!({"type":"segment_end",
        "reason":queue.failure.as_ref().map(|(code,_)|code.as_str()).unwrap_or(&queue.reason),"drained":queue.items.is_empty()})
    };
    volume.writer.as_mut().unwrap().event(&end)?;
    let volume_bytes = volume.finish()?;
    finished_bytes += volume_bytes;
    task["tracks"][0]["stored_bytes"] = json!(finished_bytes.to_string());
    task["tracks"][0]["volumes"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["complete"] = json!(true);
    task["tracks"][0]["volumes"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["valid_bytes"] = json!(volume_bytes.to_string());
    *lane.task.lock().unwrap() = task;
    Ok(())
}

/// Rebuild indexes and counters from verified blocks, never from a reused receiver slot.
fn recover(path: &FsPath, task: &mut Value) -> io::Result<()> {
    let interrupted = active(task["state"].as_str().unwrap_or(""));
    let completed = task["state"] == "completed";
    let last_reason = task["tracks"][0]["segments"]
        .as_array()
        .and_then(|s| s.last())
        .map(|s| s["end_reason"].clone())
        .unwrap_or(Value::Null);
    let mut files = fs::read_dir(path)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "wbiq"))
        .collect::<Vec<_>>();
    files.sort();
    task["tracks"][0]["segments"] = json!([]);
    task["tracks"][0]["blocks"] = json!("0");
    task["tracks"][0]["raw_bytes"] = json!("0");
    task["tracks"][0]["stored_bytes"] = json!("0");
    task["tracks"][0]["volumes"] = json!([]);
    let mut failure = None;
    let mut incomplete = files.is_empty();
    for file in files {
        let temporary = file.with_extension("jsonl.tmp");
        let mut rebuilt = File::create(&temporary)?;
        let scanned = v2::scan(&mut File::open(&file)?, |index, _| {
            if index.meta.service_id != task["service_id"]
                || index.meta.clock.receiver_uid != task["receiver_uid"]
                || index.meta.clock.track_id != task["tracks"][0]["track_id"]
            {
                return Err(io_error("archive task identity mismatch"));
            }
            update_block(task, index)?;
            serde_json::to_writer(&mut rebuilt, index)?;
            rebuilt.write_all(b"\n")
        });
        rebuilt.sync_all()?;
        fs::rename(temporary, file.with_extension("jsonl"))?;
        let (complete, valid) = match scanned {
            Ok(s) => (s.complete, Some(s.valid_bytes.to_string())),
            Err(error) => {
                failure = Some(error.to_string());
                (false, None)
            }
        };
        incomplete |= !complete;
        add(
            &mut task["tracks"][0]["stored_bytes"],
            file.metadata()?.len(),
        );
        task["tracks"][0]["volumes"].as_array_mut().unwrap().push(json!({"file":file.file_name().unwrap().to_string_lossy(),
            "index":file.with_extension("jsonl").file_name().unwrap().to_string_lossy(),"complete":complete,"valid_bytes":valid}));
    }
    if let Some(error) = failure {
        set_fault(task, "ARCHIVE_CORRUPT", &error);
    } else if completed && incomplete {
        set_fault(
            task,
            "ARCHIVE_INCOMPLETE",
            "completed task contains an incomplete or missing archive volume",
        );
    } else if interrupted {
        set_fault(
            task,
            "SERVICE_INTERRUPTED",
            "service restarted during recording; verified prefix recovered, no automatic resume",
        );
    }
    if let Some(last) = task["tracks"][0]["segments"]
        .as_array_mut()
        .and_then(|s| s.last_mut())
    {
        last["end_reason"] = if interrupted {
            json!("interrupted")
        } else {
            last_reason
        };
    }
    task["accepted_bytes"] = json!(number(&task["accepted_bytes"])
        .max(number(&task["tracks"][0]["raw_bytes"]))
        .to_string());
    task["queued_bytes"] = json!("0");
    persist(path, task)
}

pub(super) async fn index(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let volume = params
        .get("volume")
        .cloned()
        .unwrap_or_else(|| "iq-000000.jsonl".into());
    let offset = params
        .get("offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // Clone only the root and validated task name under state, then perform the read outside it.
    let root = {
        let state = app.state.lock().unwrap();
        let Some(manager) = &state.recordings else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if !manager.snapshots().iter().any(|t| t["task_id"] == task) {
            return StatusCode::NOT_FOUND.into_response();
        }
        manager.root.clone()
    };
    match tokio::task::spawn_blocking(move|| {
        if volume.contains('/')||volume.contains('\\')||volume.contains("..")||!volume.ends_with(".jsonl")||offset>10_000_000 {return Err("invalid index request".to_string());}
        let reader=BufReader::new(File::open(root.join(task).join(volume)).map_err(|e|e.to_string())?);
        let entries:Result<Vec<Value>,_>=reader.lines().skip(offset).take(128).map(|line|line.and_then(|s|serde_json::from_str(&s).map_err(io_error))).collect();
        entries.map(|entries|json!({"offset":offset.to_string(),"next_offset":(offset+entries.len()).to_string(),"entries":entries})).map_err(|e|e.to_string())
    }).await {Ok(Ok(value))=>Json(value).into_response(),Ok(Err(error))=>(StatusCode::BAD_REQUEST,Json(json!({"error":error}))).into_response(),Err(error)=>state_worker_failed(error).into_response()}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "websa-recording-{}-{}-{}",
            std::process::id(),
            millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn input(sequence: u32, epoch: u32) -> Input {
        let raw = vec![0u8; 64];
        let bytes = websa_channelizer::encode_baseband(
            websa_channelizer::BasebandMagic::Primary,
            sequence + 1,
            62500000.0 / 10368.0,
            100e6,
            u64::from(epoch),
            u64::from(epoch),
            9007199254740993 + u64::from(sequence) * 8 * 10368,
            70.25,
            &[0.0; 16],
        )
        .unwrap();
        assert_eq!(&bytes[56..], raw);
        Input {
            bytes: bytes.into(),
            meta: v2::Meta {
                service_id: "service-A".into(),
                segment_id: epoch.to_string(),
                sequence: sequence.to_string(),
                frequency_hz: 100e6,
                baseband: json!({"frequency_hz":100e6,"passband_hz":3000}),
                clock: websa_channelizer::iq_archive::IqBlockMeta {
                    receiver_uid: "uid-A".into(),
                    source_id: "primary".into(),
                    track_id: String::new(),
                    epoch: epoch.to_string(),
                    channel_id: epoch.to_string(),
                    config_revision: "3".into(),
                    first_input_index: (9007199254740993 + u64::from(sequence) * 8 * 10368)
                        .to_string(),
                    first_output_index: (u64::from(sequence) * 8).to_string(),
                    fraction_num: "1".into(),
                    fraction_den: "3".into(),
                    samples: 8,
                    sample_format: "cf32_le".into(),
                    amplitude_scale: 1.0,
                    input_rate_hz: 62500000.0,
                    actual_rate_hz: 62500000.0 / 10368.0,
                    delay_seconds: Some(0.01),
                },
            },
        }
    }
    fn wait(manager: &mut Manager) {
        let until = Instant::now() + Duration::from_secs(10);
        while manager
            .lanes
            .iter()
            .any(|h| !h.lane.done.load(Ordering::Acquire))
        {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        manager.reap();
    }
    #[test]
    fn drain_identity_segments_persistent_history_and_no_slot_rebind() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        assert!(
            Manager::open(root.clone()).is_err(),
            "storage ownership is exclusive"
        );
        manager
            .start("uid-A", "service-A", "start-A")
            .unwrap()
            .wait()
            .unwrap();
        assert!(manager.start("uid-A", "service-A", "duplicate").is_err());
        for seq in 3..8 {
            manager.admit("uid-A", input(seq, 7));
        }
        for seq in 0..3 {
            manager.admit("uid-A", input(seq, 8));
        }
        let id = manager.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        manager.stop(&id).unwrap();
        manager.admit("uid-A", input(3, 8));
        wait(&mut manager);
        let task = &manager.snapshots()[0];
        assert_eq!(task["state"], "completed");
        assert_eq!(task["tracks"][0]["raw_bytes"], "512");
        assert_eq!(task["accepted_bytes"], "512");
        assert_eq!(task["tracks"][0]["segments"].as_array().unwrap().len(), 2);
        assert_eq!(
            task["tracks"][0]["segments"][0]["first_input_index"],
            "9007199254989825"
        );
        let saved = task.clone();
        drop(manager);
        fs::write(
            root.join(&id).join("iq-000000.jsonl"),
            b"damaged completed index",
        )
        .unwrap();
        let mut restarted = Manager::open(root.clone()).unwrap();
        assert_eq!(restarted.snapshots(), vec![saved]);
        assert!(!restarted.has_active());
        restarted
            .start("new-uid-same-slot", "service-B", "new")
            .unwrap()
            .wait()
            .unwrap();
        assert_ne!(
            restarted.snapshots()[1]["service_id"],
            restarted.snapshots()[0]["service_id"]
        );
        restarted.shutdown();
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn batch_keeps_every_in_flight_item_in_the_admission_budget() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        // Model a worker holding 32 inputs while another 32 are queued. There is
        // deliberately no worker thread to race the admission boundary.
        let items: VecDeque<_> = (32..64)
            .map(|seq| Queued {
                input: WorkInput::Iq(input(seq, 7)),
                at: Instant::now(),
            })
            .collect();
        let bytes = (0..64).map(|seq| input(seq, 7).bytes.len()).sum();
        let lane = Arc::new(Lane {
            uid: "uid-batch".into(),
            kind: "iq".into(),
            task: Mutex::new(json!({"task_id":"batch", "receiver_uid":"uid-batch",
                "state":"recording", "tracks":[{"state":"recording"}]})),
            queue: Mutex::new(Queue {
                items,
                bytes,
                in_flight: Some(Instant::now()),
                in_flight_items: 32,
                sealed: false,
                prepared: true,
                failure: None,
                reason: "record_stopped".into(),
                accepted: 64 * 8 * 8,
                last_input: Instant::now(),
            }),
            wake: Condvar::new(),
            done: AtomicBool::new(false),
        });
        manager.lanes.push(Handle {
            lane: lane.clone(),
            thread: None,
        });
        manager.admit("uid-batch", input(64, 7));
        let queue = lane.queue.lock().unwrap();
        assert!(queue.sealed);
        assert_eq!(queue.items.len(), 32);
        assert_eq!(queue.in_flight_items, 32);
        assert_eq!(queue.accepted, 64 * 8 * 8);
        assert_eq!(queue.failure.as_ref().unwrap().0, "RECORDING_BACKLOG");
        drop(queue);
        manager.shutdown();
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn byte_and_age_backlog_fail_only_the_lane_without_unbounded_queue() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start("uid-A", "service-A", "A")
            .unwrap()
            .wait()
            .unwrap();
        let lane = manager.lanes[0].lane.clone();
        {
            let mut queue = lane.queue.lock().unwrap();
            queue.in_flight = Some(Instant::now() - MAX_AGE - Duration::from_millis(1));
        }
        manager.admit("uid-A", input(0, 7));
        assert_eq!(manager.snapshots()[0]["fault"]["code"], "RECORDING_BACKLOG");
        manager
            .start("uid-B", "service-A", "B")
            .unwrap()
            .wait()
            .unwrap();
        // Model a full reservation while the worker is waiting, then admit an ordinary
        // valid frame. This exercises the aggregate byte budget rather than block size.
        manager
            .lanes
            .iter()
            .find(|h| h.lane.uid == "uid-B")
            .unwrap()
            .lane
            .queue
            .lock()
            .unwrap()
            .bytes = MAX_BYTES;
        manager.admit("uid-B", input(0, 7));
        assert!(manager
            .snapshots()
            .iter()
            .any(|t| t["receiver_uid"] == "uid-B" && t["fault"]["code"] == "RECORDING_BACKLOG"));
        manager
            .start("uid-C", "service-A", "C")
            .unwrap()
            .wait()
            .unwrap();
        assert!(manager.has_active());
        assert!(manager
            .snapshots()
            .iter()
            .any(|t| t["receiver_uid"] == "uid-C" && t["fault"].is_null()));
        manager.shutdown();
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_manifest_recovers_checked_blocks_and_partial_tail_without_resume() {
        let root = directory();
        let mut manager = Manager::open(root.clone()).unwrap();
        manager
            .start("uid-A", "service-A", "A")
            .unwrap()
            .wait()
            .unwrap();
        manager.admit("uid-A", input(0, 7));
        manager.admit("uid-A", input(1, 7));
        let id = manager.snapshots()[0]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        manager.stop(&id).unwrap();
        wait(&mut manager);
        let mut task = manager.snapshots()[0].clone();
        drop(manager);
        task["state"] = json!("recording");
        task["tracks"][0]["raw_bytes"] = json!("0");
        persist(&root.join(&id), &task).unwrap();
        let path = root.join(&id).join("iq-000000.wbiq");
        let length = path.metadata().unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(length - 10)
            .unwrap();
        fs::write(path.with_extension("jsonl"), b"torn index").unwrap();
        let recovered = Manager::open(root.clone()).unwrap();
        let task = &recovered.snapshots()[0];
        assert_eq!(task["fault"]["code"], "SERVICE_INTERRUPTED");
        assert_eq!(task["tracks"][0]["raw_bytes"], "128");
        assert_eq!(task["tracks"][0]["segments"][0]["fraction_num"], "1");
        assert_eq!(task["tracks"][0]["volumes"][0]["complete"], false);
        assert!(!recovered.has_active());
        assert_eq!(
            BufReader::new(File::open(path.with_extension("jsonl")).unwrap())
                .lines()
                .count(),
            2
        );
        drop(recovered);
        fs::remove_dir_all(root).unwrap();
    }
}
