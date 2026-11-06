use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const QUEUE: usize = 32;
const LINE_LIMIT: usize = 64 * 1024;
const BLOCK_LIMIT: usize = 1024 * 1024;
const FINISH_TIMEOUT: Duration = Duration::from_secs(10);
#[repr(C)]
struct PollFd { fd: i32, events: i16, revents: i16 }
unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
}
fn nonblocking(fd: i32) -> Result<(), String> {
    let flags = unsafe { fcntl(fd, 3) };
    if flags < 0 || unsafe { fcntl(fd, 4, flags | 0o4000) } < 0 {
        return Err(format!("nonblocking decoder pipe: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}
#[derive(Default)]
struct InputStats { bytes:AtomicU64, executing:AtomicU64, blocks:AtomicU64, written:AtomicU64 }
struct InputReservation { stats:Arc<InputStats>, remaining:u64, executing:bool }
impl Drop for InputReservation {
    fn drop(&mut self) {
        self.stats.bytes.fetch_sub(self.remaining,Ordering::Relaxed);
        self.stats.blocks.fetch_sub(1,Ordering::Relaxed);
        if self.executing {self.stats.executing.fetch_sub(self.remaining,Ordering::Relaxed);}
    }
}
enum Input { Data(Vec<u8>,InputReservation), Finish, Retire }
pub struct Process {
    input: mpsc::SyncSender<Input>,
    output: mpsc::Receiver<String>,
    recycled: mpsc::Receiver<Vec<u8>>,
    error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stats:Arc<InputStats>,
}
impl Process {
    pub fn spawn(path: &Path, args: &[String]) -> Result<Self, String> { Self::start(path, args, None, Duration::ZERO) }
    fn start(path: &Path, args: &[String], probe_exit: Option<i32>, probe_timeout: Duration) -> Result<Self, String> {
        let probe = probe_exit.is_some();
        let mut command = Command::new(path);
        command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Spawn on the supervising thread: Linux ties PDEATHSIG to the creating thread.
        unsafe { command.pre_exec(|| {
            if prctl(1, 9usize, 0usize, 0usize, 0usize) != 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        }); }
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let (out_tx, out_rx) = mpsc::sync_channel(if probe { 256 } else { QUEUE });
        let (recycle_tx, recycle_rx) = mpsc::sync_channel(QUEUE);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let error = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (worker_error, worker_stop, worker_done) = (error.clone(), stop.clone(), done.clone());
        let stats=Arc::new(InputStats::default());
        let worker_stats=stats.clone();
        let thread = thread::Builder::new().name("decoder-process".into()).spawn(move || {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => { let _ = ready_tx.send(Err(format!("starting decoder: {error}"))); return; }
            };
            let mut stdin = child.stdin.take();
            let mut stdout = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let setup = nonblocking(stdin.as_ref().unwrap().as_raw_fd())
                .and_then(|_| nonblocking(stdout.as_raw_fd())).and_then(|_| nonblocking(stderr.as_raw_fd()));
            if let Err(error) = setup {
                let _ = child.kill(); let _ = child.wait(); let _ = ready_tx.send(Err(error)); return;
            }
            if ready_tx.send(Ok(())).is_err() { let _ = child.kill(); let _ = child.wait(); return; }
            let mut pending: Option<(Vec<u8>, usize, InputReservation)> = None;
            let mut line = Vec::new();
            let mut stderr_line = Vec::new();
            let discovery_started = Instant::now();
            let mut diagnostics = VecDeque::with_capacity(16 * 1024);
            let mut scratch = [0u8; 8192];
            let mut finishing = None;
            let mut retiring = false;
            let mut stdout_eof = false;
            let mut stderr_eof = false;
            let result = (|| -> Result<(), String> {
                loop {
                    if worker_stop.load(Ordering::Acquire) { return Ok(()); }
                    if probe && discovery_started.elapsed() >= probe_timeout {
                        return Err(format!("decoder capability probe exceeded {} seconds", probe_timeout.as_secs_f64()));
                    }
                    let mut progress = false;
                    let mut written = 0;
                    // Bound each input turn, then service output and cancellation.
                    // Never impose a timer delay on a pipe that can accept IQ.
                    for _ in 0..8 {
                        if pending.is_none() && finishing.is_none() && !retiring {
                            match rx.try_recv() {
                                Ok(Input::Data(bytes,mut reservation)) => {worker_stats.executing.store(bytes.len() as u64,Ordering::Relaxed);reservation.executing=true;pending = Some((bytes, 0,reservation));},
                                Ok(Input::Finish) => { stdin.take(); finishing = Some(Instant::now()); }
                                Ok(Input::Retire) => { stdin.take(); retiring = true; }
                                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                                Err(mpsc::TryRecvError::Empty) => {}
                            }
                        }
                        let Some((bytes, offset,reservation)) = pending.as_mut() else { break; };
                        let end = bytes.len().min(*offset + BLOCK_LIMIT - written);
                        match stdin.as_mut().ok_or("decoder input closed")?.write(&bytes[*offset..end]) {
                            Ok(0) => return Err("decoder stdin closed while writing".into()),
                            Ok(count) => { *offset += count; written += count; progress = true;
                                reservation.remaining-=count as u64;
                                worker_stats.bytes.fetch_sub(count as u64,Ordering::Relaxed);
                                worker_stats.executing.fetch_sub(count as u64,Ordering::Relaxed);
                                worker_stats.written.fetch_add(count as u64,Ordering::Relaxed); }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(error) => return Err(format!("decoder input: {error}")),
                        }
                        if *offset == bytes.len() {
                            let (mut bytes, _,_reservation) = pending.take().unwrap(); bytes.clear();
                            let _ = recycle_tx.try_send(bytes);
                        }
                        if written == BLOCK_LIMIT { break; }
                    }
                    // Bounded work per turn keeps cancellation responsive even for noisy children.
                    if !stdout_eof { for _ in 0..8 {
                        match stdout.read(&mut scratch) {
                            Ok(0) => { stdout_eof = true; break; }
                            Ok(count) => { progress = true; append_lines(&scratch[..count], &mut line, &out_tx)?; }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(error) => return Err(format!("decoder stdout: {error}")),
                        }
                    } }
                    if !stderr_eof { for _ in 0..8 {
                        match stderr.read(&mut scratch) {
                            Ok(0) => { stderr_eof = true; break; }
                            Ok(count) => {
                                progress = true;
                                if probe { append_lines(&scratch[..count], &mut stderr_line, &out_tx)?; }
                                for byte in &scratch[..count] {
                                    if diagnostics.len() == 16 * 1024 { diagnostics.pop_front(); }
                                    diagnostics.push_back(*byte);
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(error) => return Err(format!("decoder stderr: {error}")),
                        }
                    } }
                    // A live boundary has written every admitted input block.
                    // Preserve complete available records without waiting for EOF.
                    if retiring { return Ok(()); }
                    if let Some(status) = child.try_wait().map_err(|e| format!("decoder wait: {e}"))? {
                        if !stdout_eof || !stderr_eof { continue; }
                        if !line.is_empty() { emit_line(&mut line, &out_tx)?; }
                        if probe && !stderr_line.is_empty() { emit_line(&mut stderr_line, &out_tx)?; }
                        let accepted = if let Some(code) = probe_exit { status.code() == Some(code) } else { status.success() };
                        if (!probe && finishing.is_none()) || !accepted {
                            return Err(format!("decoder exited {status}: {}", String::from_utf8_lossy(&diagnostics.into_iter().collect::<Vec<_>>())));
                        }
                        return Ok(());
                    }
                    if finishing.is_some_and(|start| start.elapsed() >= FINISH_TIMEOUT) { return Err("decoder did not finish within 10 seconds of EOF".into()); }
                    if !progress {
                        let mut fds = [
                            PollFd { fd: if pending.is_some() { stdin.as_ref().map_or(-1, AsRawFd::as_raw_fd) } else { -1 }, events: 4, revents: 0 },
                            PollFd { fd: if stdout_eof { -1 } else { stdout.as_raw_fd() }, events: 1, revents: 0 },
                            PollFd { fd: if stderr_eof { -1 } else { stderr.as_raw_fd() }, events: 1, revents: 0 },
                        ];
                        // Readiness wakes blocked writes immediately. The short timeout
                        // also discovers newly queued blocks and cancellation while idle.
                        if unsafe { poll(fds.as_mut_ptr(), fds.len(), 1) } < 0 {
                            let error = std::io::Error::last_os_error();
                            if error.kind() != std::io::ErrorKind::Interrupted {
                                return Err(format!("decoder pipe readiness: {error}"));
                            }
                        }
                    }
                }
            })();
            let _ = child.kill(); let _ = child.wait();
            if let Err(reason) = result { *worker_error.lock().unwrap() = Some(reason); }
            worker_done.store(true, Ordering::Release);
        }).map_err(|e| format!("starting decoder supervisor: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { input: tx, output: out_rx, recycled: recycle_rx, error, stop, done, thread: Some(thread),stats }),
            outcome => { stop.store(true, Ordering::Release); let _ = thread.join();
                Err(outcome.ok().and_then(Result::err).unwrap_or_else(|| "decoder supervisor stopped during startup".into())) }
        }
    }
    pub fn buffer_status(&self)->serde_json::Value {
        let load=|v:&AtomicU64|v.load(Ordering::Relaxed);
        serde_json::json!({"input_held_bytes":load(&self.stats.bytes),"input_executing_bytes":load(&self.stats.executing),
            "input_blocks":load(&self.stats.blocks),"written_bytes":load(&self.stats.written),
            "done":self.done.load(Ordering::Acquire),"max_input_blocks":QUEUE,"max_input_block_bytes":BLOCK_LIMIT})
    }
    pub fn buffer(&self) -> Vec<u8> { self.recycled.try_recv().unwrap_or_default() }
    fn check(&self) -> Result<(), String> {
        if let Some(error) = self.error.lock().unwrap().clone() { Err(error) } else { Ok(()) }
    }
    pub fn push(&mut self, bytes: Vec<u8>) -> Result<Vec<String>, String> {
        self.check()?;
        if bytes.len() > BLOCK_LIMIT { return Err("decoder input block exceeds 1 MiB".into()); }
        let result = self.drain();
        if !bytes.is_empty() {
            let size=bytes.len() as u64;
            self.stats.bytes.fetch_add(size,Ordering::Relaxed);self.stats.blocks.fetch_add(1,Ordering::Relaxed);
            self.input.try_send(Input::Data(bytes,InputReservation {stats:self.stats.clone(),remaining:size,executing:false})).map_err(|e| {
                match e {
                mpsc::TrySendError::Full(_) => "decoder input backlog exceeded 32 blocks".to_string(),
                mpsc::TrySendError::Disconnected(_) => "decoder input process stopped".to_string(),
            }})?;
        }
        self.check()?;
        Ok(result)
    }
    fn drain(&self) -> Vec<String> { self.output.try_iter().collect() }
    pub fn retire(&mut self) -> Result<Vec<String>, String> {
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut result = self.drain();
        let mut admitted = false;
        while !self.done.load(Ordering::Acquire) {
            self.check()?;
            if !admitted {
                match self.input.try_send(Input::Retire) {
                    Ok(()) => admitted = true,
                    Err(mpsc::TrySendError::Full(_)) => {},
                    Err(_) => return Err("decoder stopped before live retirement".into()),
                }
            }
            result.extend(self.drain());
            if result.len() > 4096 { return Err("decoder retirement result backlog exceeded 4096 records".into()); }
            if Instant::now() >= deadline {
                self.stop.store(true, Ordering::Release);
                return Err("decoder live retirement input flush stalled; admitted IQ was not fully written".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        result.extend(self.drain());
        self.check()?;
        if let Some(thread) = self.thread.take() { thread.join().map_err(|_| "decoder supervisor panicked")?; }
        Ok(result)
    }

    pub fn finish(&mut self) -> Result<Vec<String>, String> {
        let mut result = self.drain();
        let deadline = Instant::now() + FINISH_TIMEOUT + Duration::from_secs(1);
        loop {
            self.check()?;
            match self.input.try_send(Input::Finish) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(_)) => {
                    result.extend(self.drain());
                    if Instant::now() >= deadline { return Err("decoder EOF input backlog timeout".into()); }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) if self.done.load(Ordering::Acquire) => break,
                Err(_) => return Err("decoder stopped before EOF".into()),
            }
        }
        while !self.done.load(Ordering::Acquire) {
            result.extend(self.drain());
            if result.len() > 4096 { return Err("decoder EOF result backlog exceeded 4096 records".into()); }
            if Instant::now() >= deadline { return Err("decoder EOF timeout".into()); }
            thread::sleep(Duration::from_millis(1));
        }
        result.extend(self.drain()); self.check()?;
        if let Some(thread) = self.thread.take() { thread.join().map_err(|_| "decoder supervisor panicked")?; }
        Ok(result)
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}
fn append_lines(bytes: &[u8], line: &mut Vec<u8>, tx: &mpsc::SyncSender<String>) -> Result<(), String> {
    for byte in bytes {
        if *byte == b'\n' { emit_line(line, tx)?; }
        else { if line.len() >= LINE_LIMIT { return Err("decoder output line exceeded 64 KiB".into()); } line.push(*byte); }
    }
    Ok(())
}
fn emit_line(line: &mut Vec<u8>, tx: &mpsc::SyncSender<String>) -> Result<(), String> {
    let text = String::from_utf8(std::mem::take(line)).map_err(|_| "decoder emitted invalid UTF-8")?;
    if !text.trim().is_empty() { tx.try_send(text).map_err(|_| "decoder output backlog exceeded 32 records")?; }
    Ok(())
}
pub(super) fn probe(path: &Path, argument: &str, expected_exit: i32) -> Result<String, String> {
    probe_with_timeout(path, argument, expected_exit, Duration::from_secs(2))
}
pub(super) fn probe_with_timeout(path: &Path, argument: &str, expected_exit: i32, timeout: Duration) -> Result<String, String> {
    let mut process = Process::start(path, &[argument.into()], Some(expected_exit), timeout)?;
    Ok(process.finish()?.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_limits_fail_without_truncating_a_protocol_record() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut line = Vec::new();
        append_lines(b"message\n", &mut line, &tx).unwrap();
        assert!(append_lines(b"overflow\n", &mut line, &tx).is_err());
        assert_eq!(rx.recv().unwrap(), "message");
        assert!(append_lines(&vec![b'a'; LINE_LIMIT + 1], &mut Vec::new(), &tx).is_err());
    }
    #[test]
    fn eof_drains_and_reaps_process() {
        let mut process = Process::spawn(Path::new("/usr/bin/cat"), &[]).unwrap();
        let mut records = process.push(b"first\nlast".to_vec()).unwrap();
        records.extend(process.finish().unwrap());
        assert_eq!(records, ["first", "last"]);
        assert!(process.thread.is_none());
    }
    #[test]
    fn live_retirement_writes_queued_input_without_waiting_for_child_eof() {
        // The child consumes stdin but deliberately never exits after EOF.
        let mut process = Process::spawn(Path::new("/usr/bin/python3"), &["-c".into(),
            "import os,time\nwhile os.read(0,4096): pass\ntime.sleep(30)".into()]).unwrap();
        let blocks = 8;
        for _ in 0..blocks { process.push(vec![42; 8192]).unwrap(); }
        let started = Instant::now();
        assert!(process.retire().unwrap().is_empty());
        assert!(started.elapsed() < Duration::from_secs(1), "live replacement waited for EOF completion");
        // Recycling happens only after every byte of a queued block was written.
        assert_eq!(process.recycled.try_iter().count(), blocks);
        assert_eq!(process.buffer_status()["input_held_bytes"],0);
        assert_eq!(process.buffer_status()["input_blocks"],0);
        assert_eq!(process.buffer_status()["written_bytes"],blocks*8192);
        assert!(process.done.load(Ordering::Acquire));
        assert!(process.thread.is_none(), "retirement must reap its bounded owner");
    }

    #[test]
    fn drop_stops_and_reaps_a_waiting_child() {
        let process = Process::spawn(Path::new("/usr/bin/cat"), &[]).unwrap();
        let done = process.done.clone();
        drop(process);
        assert!(done.load(Ordering::Acquire));
    }
    #[test]
    fn abnormal_exit_is_an_explicit_error() {
        let mut process = Process::spawn(Path::new("/usr/bin/false"), &[]).unwrap();
        assert!(process.finish().is_err());
    }
    #[test]
    fn capability_probe_requires_the_expected_exit_status() {
        assert!(probe(Path::new("/usr/bin/false"), "-V", 0).is_err());
        assert!(probe(Path::new("/usr/bin/true"), "-h", 2).is_err());
        assert!(probe(Path::new("/usr/bin/true"), "-V", 0).is_ok());
    }
    #[test]
    fn separate_pipe_fragments_never_form_a_false_capability_line() {
        let (tx, rx) = mpsc::sync_channel(4);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        append_lines(b"Available demo", &mut stdout, &tx).unwrap();
        append_lines(b"unrelated diagnostic\n", &mut stderr, &tx).unwrap();
        append_lines(b"dulators: POCSAG1200\n", &mut stdout, &tx).unwrap();
        assert_eq!(rx.try_iter().collect::<Vec<_>>(),
            ["unrelated diagnostic", "Available demodulators: POCSAG1200"]);
    }
}
