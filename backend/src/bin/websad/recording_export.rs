//! Streaming single-segment SigMF export. One checked block and two transport chunks
//! stay resident; neither samples nor annotations accumulate with recording length.
use super::recording_read::{visit_range, Selection, READER};
use super::*;
use sha2::{Digest, Sha512};
use std::{
    io::{self, Write},
    path::Path as FsPath,
    time::Instant,
};

// POSIX ustar file sizes have 11 octal digits. Keep each tar member within that bound.
pub(super) const MAX_SAMPLES: u64 = (0o77777777777u64) / 8;
const CHUNK: usize = 64 * 1024;
const STALL: Duration = Duration::from_secs(2);
type Packet = io::Result<Bytes>;
fn invalid(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}

struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
pub(super) struct Plan {
    prefix: Vec<u8>,
    metadata_bytes: u64,
    raw_bytes: u64,
    digest: u32,
}
fn prefix(meta: &Value) -> io::Result<Vec<u8>> {
    Ok(format!(
        "{{\"global\":{},\"captures\":{},\"annotations\":[",
        serde_json::to_string(&meta["global"])?,
        serde_json::to_string(&meta["captures"])?
    )
    .into_bytes())
}
pub(super) fn prepare(
    path: &FsPath,
    task: &Value,
    selection: &Selection,
    cancel: &AtomicBool,
) -> io::Result<Plan> {
    let mut annotations = 0u64;
    let mut first = true;
    let mut digest = crc32fast::Hasher::new();
    let meta = visit_range(path, task, selection, cancel, |raw, annotation| {
        let json = serde_json::to_vec(annotation)?;
        annotations = annotations
            .checked_add(json.len() as u64 + u64::from(!first))
            .ok_or_else(|| invalid("metadata length overflow"))?;
        first = false;
        digest.update(raw);
        digest.update(&json);
        Ok(())
    })?;
    let prefix = prefix(&meta)?;
    let metadata_bytes = prefix.len() as u64 + annotations + 2;
    if metadata_bytes > 0o77777777777 {
        return Err(invalid("SigMF metadata exceeds POSIX tar size limit"));
    }
    Ok(Plan {
        prefix,
        metadata_bytes,
        raw_bytes: selection.count * 8,
        digest: digest.finalize(),
    })
}
fn padding(bytes: u64) -> u64 {
    (512 - bytes % 512) % 512
}
pub(super) fn total(plan: &Plan) -> u64 {
    2048 + plan.raw_bytes
        + padding(plan.raw_bytes)
        + plan.metadata_bytes
        + padding(plan.metadata_bytes)
}
pub(super) fn header(
    output: &mut (impl Write + ?Sized),
    name: &str,
    length: u64,
) -> io::Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_path(name)?;
    header.set_size(length);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    output.write_all(header.as_bytes())
}
/// Write metadata without retaining its annotations. Both preparation and emission use
/// these exact bytes for the collection's required SHA512 metadata-file hash.
pub(super) fn metadata(
    output: &mut (impl Write + ?Sized),
    path: &FsPath,
    task: &Value,
    selection: &Selection,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<()> {
    output.write_all(&plan.prefix)?;
    let mut first = true;
    let mut bytes = plan.prefix.len() as u64 + 2;
    let mut digest = crc32fast::Hasher::new();
    visit_range(path, task, selection, cancel, |raw, annotation| {
        let json = serde_json::to_vec(annotation)?;
        digest.update(raw);
        digest.update(&json);
        if !first {
            output.write_all(b",")?;
            bytes += 1;
        }
        first = false;
        output.write_all(&json)?;
        bytes += json.len() as u64;
        Ok(())
    })?;
    if bytes != plan.metadata_bytes || digest.finalize() != plan.digest {
        return Err(invalid("metadata changed during export"));
    }
    output.write_all(b"]}")
}

pub(super) struct Hashed<W> {
    pub(super) output: W,
    pub(super) hash: Sha512,
}
impl<W: Write> Write for Hashed<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
impl Plan {
    pub(super) fn retained_bytes(&self) -> usize {
        self.prefix.len()
    }
}

/// Two named members, without a tar terminator, so a collection can append records.
pub(super) fn members(
    output: &mut (impl Write + ?Sized),
    path: &FsPath,
    task: &Value,
    selection: &Selection,
    plan: &Plan,
    cancel: &AtomicBool,
    name: &str,
) -> io::Result<[u8; 64]> {
    header(output, &format!("{name}.sigmf-data"), plan.raw_bytes)?;
    let mut digest = crc32fast::Hasher::new();
    visit_range(path, task, selection, cancel, |raw, annotation| {
        digest.update(raw);
        digest.update(&serde_json::to_vec(annotation)?);
        output.write_all(raw)
    })?;
    if digest.finalize() != plan.digest {
        return Err(invalid("archive changed after export verification"));
    }
    output.write_all(&[0; 512][..padding(plan.raw_bytes) as usize])?;
    header(output, &format!("{name}.sigmf-meta"), plan.metadata_bytes)?;
    let mut hashed = Hashed {
        output: &mut *output,
        hash: Sha512::new(),
    };
    metadata(&mut hashed, path, task, selection, plan, cancel)?;
    let hash = hashed.hash.finalize().into();
    output.write_all(&[0; 512][..padding(plan.metadata_bytes) as usize])?;
    Ok(hash)
}
pub(super) fn archive(
    output: &mut (impl Write + ?Sized),
    path: &FsPath,
    task: &Value,
    selection: &Selection,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<()> {
    members(
        output,
        path,
        task,
        selection,
        plan,
        cancel,
        "recording/recording",
    )?;
    output.write_all(&[0; 1024])
}

struct Pipe {
    sender: tokio::sync::mpsc::Sender<Packet>,
    cancel: Arc<AtomicBool>,
}
fn response_body(
    receiver: tokio::sync::mpsc::Receiver<Packet>,
    permit: Arc<tokio::sync::SemaphorePermit<'static>>,
    cancel: Cancel,
) -> axum::body::Body {
    let stream = futures_util::stream::unfold(
        (receiver, permit, cancel),
        |(mut receiver, permit, cancel)| async move {
            receiver
                .recv()
                .await
                .map(|packet| (packet, (receiver, permit, cancel)))
        },
    );
    axum::body::Body::from_stream(stream)
}
impl Pipe {
    fn send(&self, mut packet: Packet) -> io::Result<()> {
        let deadline = Instant::now() + STALL;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                // write_all retries Interrupted; cancellation must terminate that loop.
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "export cancelled",
                ));
            }
            match self.sender.try_send(packet) {
                Ok(()) => return Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "download closed"))
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(value)) => packet = value,
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "export consumer stalled for 2 seconds",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Write for Pipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(CHUNK) {
            self.send(Ok(Bytes::copy_from_slice(chunk)))?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let error = |status, message: String| (status, Json(json!({"error":message}))).into_response();
    let selection = match Selection::parse_limit(&params, MAX_SAMPLES) {
        Ok(s) => s,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    download(
        app,
        task,
        "recording.sigmf",
        move |path, task, cancel| {
            let plan = prepare(path, task, &selection, cancel)?;
            Ok((total(&plan), (selection, plan)))
        },
        |output, path, task, (selection, plan), cancel| {
            archive(output, path, task, selection, plan, cancel)
        },
    )
    .await
}

/// Preparation and producer/body lifetimes share one reader lease with /range.
pub(super) async fn download<P: Send + 'static>(
    app: Arc<App>,
    task: String,
    filename: &'static str,
    prepare: impl FnOnce(&FsPath, &Value, &AtomicBool) -> io::Result<(u64, P)> + Send + 'static,
    emit: impl FnOnce(&mut dyn Write, &FsPath, &Value, &P, &AtomicBool) -> io::Result<()>
        + Send
        + 'static,
) -> Response {
    let error = |status, message: String| (status, Json(json!({"error":message}))).into_response();
    let (path, snapshot) = {
        let state = app.state.lock().unwrap();
        let Some(manager) = &state.recordings else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match manager.readable_task(&task) {
            Ok(t) => t,
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
        Ok(p) => Arc::new(p),
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "another IQ read or download is in progress".into(),
            )
        }
    };
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let worker_cancel = cancel.0.clone();
    let worker_permit = permit.clone();
    let (worker_path, worker_task) = (path.clone(), snapshot.clone());
    let (length, plan) = match tokio::task::spawn_blocking(move || {
        let _permit = worker_permit;
        prepare(&worker_path, &worker_task, &worker_cancel)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        Err(e) => return state_worker_failed(e).into_response(),
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let worker_cancel = cancel.0.clone();
    let worker_permit = permit.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = worker_permit;
        let mut output = Pipe {
            sender,
            cancel: worker_cancel,
        };
        let flag = output.cancel.clone();
        if let Err(e) = emit(&mut output, &path, &snapshot, &plan, &flag) {
            let _ = output.sender.try_send(Err(e));
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-tar")
        .header(header::CONTENT_LENGTH, length)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(response_body(receiver, permit, cancel))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dropping_http_body_cancels_pending_producer_and_releases_its_lease() {
        let permit = Arc::new(READER.try_acquire().unwrap());
        let flag = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let worker_flag = flag.clone();
        let worker_permit = permit.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = worker_permit;
            let mut pipe = Pipe {
                sender,
                cancel: worker_flag,
            };
            pipe.write_all(&vec![1; 1024 * 1024])
        });
        let mut body = response_body(receiver, permit, Cancel(flag.clone())).into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap().len(), CHUNK);
        drop(body);
        assert!(flag.load(Ordering::Acquire));
        let result = tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(READER.try_acquire().is_ok());
    }
    #[test]
    fn cancelled_or_slow_download_stops_without_an_unbounded_transport_queue() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut pipe = Pipe {
            sender,
            cancel: cancel.clone(),
        };
        assert_eq!(pipe.write(&vec![7; 2 * CHUNK]).unwrap(), 2 * CHUNK);
        assert_eq!(receiver.len(), 2);
        assert_eq!(
            pipe.write(&[1]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        cancel.store(true, Ordering::Release);
        assert_eq!(
            pipe.write(&[1]).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(receiver.try_recv().unwrap().unwrap().len(), CHUNK);
        assert_eq!(receiver.try_recv().unwrap().unwrap().len(), CHUNK);
        cancel.store(false, Ordering::Release);
        drop(receiver);
        assert_eq!(
            pipe.write(&[1]).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
