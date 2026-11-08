//! Bounded PFB workers with ordered coarse-bin IQ delivery.
use crate::pfb::Pfb;
use crate::timing::{Timing, TimingSnapshot};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Fixed worker count; task and result channels have bounded capacity.
pub const WORKERS: usize = 4;
pub const TASK_CAPACITY: usize = WORKERS;
pub const MAX_IN_FLIGHT: usize = WORKERS + 2;

struct Task {
    seq: u64,
    stamp: u64,
    input_index: u64,
    state: Option<Pfb>,
    history: Vec<i16>,
    bins: Arc<Vec<u32>>,
    iq: Vec<i16>,
    #[cfg(test)]
    execution_gate: Option<Arc<ExecutionGate>>,
}

#[cfg(test)]
struct ExecutionGate {
    entered: std::sync::mpsc::Sender<u64>,
    release: std::sync::atomic::AtomicBool,
    held_seq: Option<u64>,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PoolResult {
    pub seq: u64,
    pub stamp: u64,
    pub groups: u32,
    pub first_re: f32,
    pub process_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PoolStats {
    pub submitted: u64,
    pub completed: u64,
    pub groups: u64,
    pub clone_ns: u64,
    pub advance_ns: u64,
    pub task_wait_ns: u64,
    pub result_wait_ns: u64,
    pub max_pending: u32,
    pub max_ready: u32,
    pub task_capacity: u32,
    pub result_capacity: u32,
}

/// Stable slot order for `PoolTimingSnapshot::max_events`.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolTimingStage {
    Clone,
    History,
    IqCopy,
    TaskSend,
    ResultWait,
    Reorder,
    /// Blocking receive inside `submit`, exclusively from admission.
    AdmissionWait,
    /// Worker PFB processing, measured inside the worker.
    Process,
    /// Worker completion to controller receive, including caller scheduling delay.
    ResultAge,
}

/// Maximum observed event for a stage, including events below the slow threshold.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct PoolSlowEvent {
    pub stage: PoolTimingStage,
    /// Submitted sequence, or actual received sequence for result_wait/reorder.
    pub seq: u64,
    pub stamp: u64,
    /// Next ordered sequence at receive entry; absent for submission stages.
    pub expected_seq: Option<u64>,
    pub elapsed_ns: u64,
    /// Four times this packet's input duration, rounded down, minimum 1 ns.
    pub threshold_ns: u64,
    pub is_slow: bool,
    /// Time from worker completion to receive; only set on result_wait events.
    pub age_ns: Option<u64>,
}

/// Rust-only diagnostics; the C `PoolStats` layout and counters are unchanged.
#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
pub struct PoolTimingSnapshot {
    pub clone: TimingSnapshot,
    pub history: TimingSnapshot,
    pub iq_copy: TimingSnapshot,
    pub task_send: TimingSnapshot,
    pub result_wait: TimingSnapshot,
    pub reorder: TimingSnapshot,
    pub admission_wait: TimingSnapshot,
    pub process: TimingSnapshot,
    pub result_age: TimingSnapshot,
    /// Slots follow PoolTimingStage declaration order; ties retain the first event.
    pub max_events: [Option<PoolSlowEvent>; 9],
}

#[derive(Default)]
struct PoolTimings {
    stages: [Timing; 9],
    max_events: [Option<PoolSlowEvent>; 9],
}

impl PoolTimings {
    fn observe(&mut self, stage: PoolTimingStage, elapsed: Duration, seq: u64,
        stamp: u64, expected_seq: Option<u64>, threshold_ns: u64) {
        let slot = stage as usize;
        let elapsed_ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.stages[slot].observe(elapsed);
        if self.max_events[slot].map_or(true, |event| elapsed_ns > event.elapsed_ns) {
            self.max_events[slot] = Some(PoolSlowEvent {
                stage, seq, stamp, expected_seq, elapsed_ns, threshold_ns,
                is_slow: elapsed_ns >= threshold_ns, age_ns: None,
            });
        }
    }

    fn observe_result_wait(&mut self, elapsed: Duration, seq: u64, stamp: u64,
        expected_seq: u64, threshold_ns: u64, age: Duration) {
        let elapsed_ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.observe(PoolTimingStage::ResultWait, elapsed, seq, stamp, Some(expected_seq), threshold_ns);
        if let Some(event) = self.max_events[PoolTimingStage::ResultWait as usize].as_mut() {
            if event.seq == seq && event.elapsed_ns == elapsed_ns {
                event.age_ns = Some(age.as_nanos().min(u64::MAX as u128) as u64);
            }
        }
    }

    fn snapshot(&self) -> PoolTimingSnapshot {
        PoolTimingSnapshot {
            clone: self.stages[0].snapshot(),
            history: self.stages[1].snapshot(),
            iq_copy: self.stages[2].snapshot(),
            task_send: self.stages[3].snapshot(),
            result_wait: self.stages[4].snapshot(),
            reorder: self.stages[5].snapshot(),
            admission_wait: self.stages[6].snapshot(),
            process: self.stages[7].snapshot(),
            result_age: self.stages[8].snapshot(),
            max_events: self.max_events,
        }
    }
}

struct WorkerResult {
    worker_id: usize,
    meta: PoolResult,
    input_index: u64,
    iq: Vec<f32>,
    packet_samples: usize,
    completed_at: Instant,
}

pub struct PfbOutput {
    pub seq: u64,
    pub stamp: u64,
    pub input_index: u64,
    pub epoch: u64,
    pub groups: u32,
    pub process_ns: u64,
    /// Time-major: each group contains the selected bins in request order, interleaved I/Q.
    pub iq: Vec<f32>,
}

pub struct PfbStream {
    pool: PfbBenchPool,
    epoch: u64,
    pub sample_rate: f64,
    pub delay_samples: f64,
}

pub struct PfbBenchPool {
    controller: Pfb,
    bins: Arc<Vec<u32>>,
    overlap_history: bool,
    tail: Vec<i16>,
    input_index: u64,
    sender: Option<Vec<SyncSender<Task>>>,
    receiver: Receiver<Result<WorkerResult, ()>>,
    workers: Vec<JoinHandle<()>>,
    idle_workers: VecDeque<usize>,
    max_packet_samples: usize,
    in_flight: usize,
    expected_submit: u64,
    expected_output: u64,
    pending: BTreeMap<u64, WorkerResult>,
    ready: VecDeque<WorkerResult>,
    stats: PoolStats,
    timings: PoolTimings,
    slow_ns_per_sample: f64,
    stopped: bool,
    failed: bool,
    #[cfg(test)]
    execution_gate: Option<Arc<ExecutionGate>>,
}

impl PfbBenchPool {
    fn new(fs: f64, packet_samples: usize, bins: Vec<u32>, strategy: u32) -> Option<Self> {
        Self::new_with_output(fs, packet_samples, bins, strategy, false, 16)
    }

    fn new_with_output(
        fs: f64,
        packet_samples: usize,
        bins: Vec<u32>,
        strategy: u32,
        emit_iq: bool,
        taps_per_phase: usize,
    ) -> Option<Self> {
        let mut controller = Pfb::with_taps(fs, 2, taps_per_phase)?;
        if !controller.avx2
            || strategy > 1
            || bins.is_empty()
            || bins.len() > controller.m
            || bins.iter().any(|&bin| bin as usize >= controller.m)
            || packet_samples == 0
            || packet_samples > 65536
        {
            return None;
        }
        controller.fir_kernel = 1;
        if emit_iq {
            let mut sorted = bins.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.len() != bins.len() {
                return None;
            }
        }
        let tail = if strategy == 1 {
            vec![0i16; 2 * controller.history_len()]
        } else {
            Vec::new()
        };
        let bin_count = bins.len();
        let hop = controller.hop;
        let bins = Arc::new(bins);
        let mut senders = Vec::with_capacity(WORKERS);
        let (result_tx, receiver) = sync_channel::<Result<WorkerResult, ()>>(MAX_IN_FLIGHT);
        let workers = (0..WORKERS)
            .map(|worker_id| {
                // Admit one outstanding packet per worker. A private
                // mailbox wakes only the selected worker, without a second
                // wakeup to transfer ownership of a shared blocking receiver.
                let (sender, reader) = sync_channel::<Task>(1);
                senders.push(sender);
                let writer = result_tx.clone();
                let mut reusable = controller.clone();
                thread::spawn(move || {
                    let mut output = vec![0f32; 2 * bin_count * (packet_samples / hop + 2)];
                    loop {
                        let item = reader.recv();
                        let Ok(mut task) = item else {
                            break;
                        };
                        // Every admitted packet must complete or explicitly fail.
                        // A dropped writer alone cannot wake the owner while the
                        // other workers still hold this shared result channel.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            #[cfg(test)]
                            if let Some(gate) = &task.execution_gate {
                                let _ = gate.entered.send(task.seq);
                                while gate.held_seq.is_none_or(|seq| seq == task.seq)
                                    && !gate.release.load(std::sync::atomic::Ordering::Acquire) {
                                    thread::sleep(Duration::from_millis(1));
                                }
                            }
                            let started = Instant::now();
                            let restore_history = task.state.is_none();
                            let state = task.state.as_mut().unwrap_or(&mut reusable);
                            if restore_history {
                                assert!(state.load_history(task.input_index, &task.history));
                            }
                            let bin_count = task.bins.len();
                            output.resize(2 * bin_count * (packet_samples / hop + 2), 0.0);
                            let groups = state.process(&task.iq, &task.bins, &mut output);
                            let samples = if emit_iq {
                                output[..2 * groups * bin_count].to_vec()
                            } else {
                                Vec::new()
                            };
                            WorkerResult {
                                worker_id,
                                meta: PoolResult {
                                    seq: task.seq,
                                    stamp: task.stamp,
                                    groups: groups as u32,
                                    first_re: if groups > 0 { output[0] } else { 0.0 },
                                    process_ns: started.elapsed().as_nanos() as u64,
                                },
                                input_index: task.input_index,
                                packet_samples: task.iq.len() / 2,
                                iq: samples,
                                completed_at: Instant::now(),
                            }
                        })).map_err(|_| ());
                        let failed = result.is_err();
                        if writer.send(result).is_err() || failed {
                            break;
                        }
                    }
                })
            })
            .collect();
        drop(result_tx);
        Some(Self {
            controller,
            bins,
            overlap_history: strategy == 1,
            tail,
            input_index: 0,
            sender: Some(senders),
            receiver,
            workers,
            idle_workers: (0..WORKERS).collect(),
            max_packet_samples: packet_samples,
            in_flight: 0,
            expected_submit: 0,
            expected_output: 0,
            pending: BTreeMap::new(),
            ready: VecDeque::new(),
            stats: PoolStats {
                task_capacity: TASK_CAPACITY as u32,
                result_capacity: MAX_IN_FLIGHT as u32,
                ..Default::default()
            },
            timings: PoolTimings::default(),
            slow_ns_per_sample: 4.0 * 1e9 / fs,
            stopped: false,
            failed: false,
            #[cfg(test)]
            execution_gate: None,
        })
    }

    fn receive_one(&mut self) -> Result<(), ()> {
        let expected_seq = self.expected_output;
        let started = Instant::now();
        let result = match self.receiver.recv() {
            Ok(result) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                match result {
                    Ok(result) => result,
                    Err(()) => { self.failed = true; return Err(()); }
                }
            }
            Err(_) => {
                self.failed = true;
                self.in_flight = 0;
                return Err(());
            }
        };
        self.idle_workers.push_back(result.worker_id);
        let received = Instant::now();
        let elapsed = received.duration_since(started);
        self.stats.result_wait_ns += elapsed.as_nanos() as u64;
        let seq = result.meta.seq;
        let stamp = result.meta.stamp;
        let threshold_ns = ((result.packet_samples as f64 * self.slow_ns_per_sample) as u64).max(1);
        self.timings.observe(PoolTimingStage::Process, Duration::from_nanos(result.meta.process_ns),
            seq, stamp, None, threshold_ns);
        let age = received.duration_since(result.completed_at);
        self.timings.observe(PoolTimingStage::ResultAge, age,
            seq, stamp, Some(expected_seq), threshold_ns);
        self.timings.observe_result_wait(elapsed, seq, stamp, expected_seq, threshold_ns, age);
        let started = Instant::now();
        if self.pending.insert(result.meta.seq, result).is_some() {
            self.failed = true;
            return Err(());
        }
        self.stats.max_pending = self.stats.max_pending.max(self.pending.len() as u32);
        while let Some(item) = self.pending.remove(&self.expected_output) {
            self.stats.completed += 1;
            self.stats.groups += item.meta.groups as u64;
            self.expected_output += 1;
            self.ready.push_back(item);
        }
        self.stats.max_ready = self.stats.max_ready.max(self.ready.len() as u32);
        self.timings.observe(PoolTimingStage::Reorder, started.elapsed(), seq, stamp,
            Some(expected_seq), threshold_ns);
        Ok(())
    }

    fn submit(&mut self, seq: u64, stamp: u64, iq: &[i16]) -> Result<(), i32> {
        if self.stopped || self.failed
            || seq != self.expected_submit
            || iq.len() & 1 != 0
            || iq.is_empty()
            || iq.len() / 2 > self.max_packet_samples
        {
            return Err(-1);
        }
        // Execution owners are independent of ordered results: an earlier slow
        // packet must not strand workers that have finished later packets.
        // Every submitted packet still occupies the existing retained budget
        // until the caller takes it, whether executing, pending or ready.
        loop {
            if self.ready.len() >= 3 {
                return Err(-2);
            }
            let retained = self.in_flight + self.pending.len() + self.ready.len();
            if retained >= MAX_IN_FLIGHT && !self.ready.is_empty() {
                return Err(-2);
            }
            if !self.idle_workers.is_empty() && retained < MAX_IN_FLIGHT {
                break;
            }
            self.receive_admitted(seq, stamp).map_err(|_| -3)?;
        }
        let input_index = self.input_index;
        let threshold_ns = (((iq.len() / 2) as f64 * self.slow_ns_per_sample) as u64).max(1);
        let before = Instant::now();
        // Overlap jobs carry only the causal input tail. Each worker retains
        // its filters/FFT workspace instead of cloning them for every packet.
        let state = (!self.overlap_history).then(|| self.controller.clone());
        let elapsed = before.elapsed();
        self.stats.clone_ns += elapsed.as_nanos() as u64;
        self.timings.observe(PoolTimingStage::Clone, elapsed, seq, stamp, None, threshold_ns);
        let before = Instant::now();
        let history = if self.overlap_history {
            let history = self.tail.clone();
            let n = iq.len() / 2;
            let len = self.controller.history_len();
            if n >= len {
                self.tail.copy_from_slice(&iq[2 * (n - len)..]);
            } else {
                self.tail.copy_within(2 * n.., 0);
                self.tail[2 * (len - n)..].copy_from_slice(iq);
            }
            history
        } else {
            self.controller.process(iq, &[], &mut []);
            Vec::new()
        };
        let elapsed = before.elapsed();
        self.stats.advance_ns += elapsed.as_nanos() as u64;
        self.timings.observe(PoolTimingStage::History, elapsed, seq, stamp, None, threshold_ns);
        self.input_index += (iq.len() / 2) as u64;
        let before = Instant::now();
        let task = Task {
            seq,
            stamp,
            input_index,
            state,
            history,
            bins: self.bins.clone(),
            iq: iq.to_vec(),
            #[cfg(test)]
            execution_gate: self.execution_gate.clone(),
        };
        let copy_elapsed = before.elapsed();
        let send_started = Instant::now();
        let worker = self.idle_workers.pop_front().ok_or(-3)?;
        if self.sender.as_ref().ok_or(-3)?[worker].send(task).is_err() {
            self.failed = true;
            return Err(-3);
        }
        let send_elapsed = send_started.elapsed();
        self.stats.task_wait_ns += before.elapsed().as_nanos() as u64;
        self.timings.observe(PoolTimingStage::IqCopy, copy_elapsed, seq, stamp, None, threshold_ns);
        self.timings.observe(PoolTimingStage::TaskSend, send_elapsed, seq, stamp, None, threshold_ns);
        self.expected_submit += 1;
        self.stats.submitted += 1;
        self.in_flight += 1;
        Ok(())
    }

    fn receive_admitted(&mut self, seq: u64, stamp: u64) -> Result<(), ()> {
        let threshold_ns = ((self.max_packet_samples as f64 * self.slow_ns_per_sample) as u64).max(1);
        let started = Instant::now();
        let result = self.receive_one();
        self.timings.observe(PoolTimingStage::AdmissionWait, started.elapsed(), seq, stamp,
            None, threshold_ns);
        result
    }

    fn finish(&mut self) -> Result<(), ()> {
        if self.stopped {
            return if self.failed { Err(()) } else { Ok(()) };
        }
        self.stopped = true;
        self.sender.take();
        while self.in_flight > 0 {
            // Keep collecting the valid prefix before the lost sequence and
            // let every admitted owner finish, even after an earlier failure.
            let _ = self.receive_one();
        }
        // No admitted results remain. Release the receive endpoint before joins
        // so a failed owner cannot leave a worker blocked on a final send.
        let (_, closed) = sync_channel(0);
        self.receiver = closed;
        for worker in self.workers.drain(..) {
            if worker.join().is_err() { self.failed = true; }
        }
        if !self.pending.is_empty() || self.stats.submitted != self.stats.completed {
            self.failed = true;
        }
        self.pending.clear();
        if self.failed { Err(()) } else { Ok(()) }
    }
}

impl Drop for PfbBenchPool {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

impl PfbStream {
    pub fn new(fs: f64, packet_samples: usize, bins: Vec<u32>, epoch: u64) -> Option<Self> {
        Self::with_taps(fs, packet_samples, bins, epoch, 16)
    }

    pub fn with_taps(fs: f64, packet_samples: usize, bins: Vec<u32>, epoch: u64, taps_per_phase: usize) -> Option<Self> {
        let pool = PfbBenchPool::new_with_output(fs, packet_samples, bins, 1, true, taps_per_phase)?;
        let sample_rate = fs / pool.controller.hop as f64;
        let delay_samples =
            (pool.controller.history_len() - 1) as f64 / (2.0 * pool.controller.hop as f64);
        Some(Self {
            pool,
            epoch,
            sample_rate,
            delay_samples,
        })
    }

    pub fn bins(&self) -> &[u32] {
        &self.pool.bins
    }

    /// Replace the output columns at a fully drained control boundary. Flush pending
    /// work and consume every old output before calling; then remap fine-bank columns
    /// before submitting more IQ. FIR history, sample clocks, epoch and sequence survive.
    /// Invalid selections and undrained streams are left unchanged.
    pub fn replace_bins(&mut self, bins: Vec<u32>) -> Result<(), &'static str> {
        if bins.is_empty()
            || bins.len() > self.bin_count()
            || bins.iter().any(|&bin| bin as usize >= self.bin_count())
            || bins.iter().enumerate().any(|(column, bin)| bins[..column].contains(bin))
        {
            return Err("PFB selection must contain distinct in-range bins");
        }
        if self.pool.stopped || self.pool.failed {
            return Err("PFB stream is stopped");
        }
        if self.pool.in_flight != 0
            || !self.pool.pending.is_empty()
            || !self.pool.ready.is_empty()
            || self.pool.expected_submit != self.pool.expected_output
            || self.pool.stats.submitted != self.pool.stats.completed
        {
            return Err("PFB selection change requires all outputs to be collected");
        }
        if self.bins() != bins.as_slice() {
            self.pool.bins = Arc::new(bins);
        }
        Ok(())
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn bin_count(&self) -> usize {
        self.pool.controller.m
    }

    pub fn taps_per_phase(&self) -> usize {
        self.pool.controller.taps_per_phase
    }

    pub fn input_hop(&self) -> usize {
        self.pool.controller.hop
    }

    pub fn input_sample_rate(&self) -> f64 {
        self.sample_rate * self.input_hop() as f64
    }

    /// -2 means the caller must drain ordered outputs before retrying the same input.
    pub fn submit(&mut self, seq: u64, stamp: u64, iq: &[i16]) -> Result<(), i32> {
        self.pool.submit(seq, stamp, iq)
    }

    pub fn take(&mut self) -> Option<PfbOutput> {
        let item = self.pool.ready.pop_front()?;
        Some(PfbOutput {
            seq: item.meta.seq,
            stamp: item.meta.stamp,
            input_index: item.input_index,
            epoch: self.epoch,
            groups: item.meta.groups,
            process_ns: item.meta.process_ns,
            iq: item.iq,
        })
    }

    /// Wait for already submitted work without stopping the stream. Control changes can
    /// then take effect after every old packet has been delivered in order.
    pub fn flush_pending(&mut self) -> Result<(), &'static str> {
        if self.pool.failed { return Err("PFB worker stopped before ordered output"); }
        while self.pool.in_flight > 0 {
            self.pool
                .receive_one()
                .map_err(|_| "PFB worker stopped before ordered output")?;
        }
        if !self.pool.pending.is_empty() {
            return Err("PFB ordered output missing at control boundary");
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), &'static str> {
        self.pool
            .finish()
            .map_err(|_| "PFB worker stopped before ordered output")
    }
    pub fn stats(&self) -> PoolStats {
        self.pool.stats
    }

    /// Cumulative fixed-memory stage distributions and maximum-event packet keys.
    pub fn timings(&self) -> PoolTimingSnapshot {
        self.pool.timings.snapshot()
    }
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_open(
    fs: f64,
    packet_samples: u32,
    bins: *const u32,
    count: u32,
) -> *mut PfbBenchPool {
    if bins.is_null() || count == 0 || count > 1024 {
        return std::ptr::null_mut();
    }
    let selected = std::slice::from_raw_parts(bins, count as usize).to_vec();
    PfbBenchPool::new(fs, packet_samples as usize, selected, 0)
        .map_or(std::ptr::null_mut(), |pool| Box::into_raw(Box::new(pool)))
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_open_strategy(
    fs: f64,
    packet_samples: u32,
    bins: *const u32,
    count: u32,
    strategy: u32,
) -> *mut PfbBenchPool {
    if bins.is_null() || count == 0 || count > 1024 {
        return std::ptr::null_mut();
    }
    let selected = std::slice::from_raw_parts(bins, count as usize).to_vec();
    PfbBenchPool::new(fs, packet_samples as usize, selected, strategy)
        .map_or(std::ptr::null_mut(), |pool| Box::into_raw(Box::new(pool)))
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_submit(
    ptr: *mut PfbBenchPool,
    seq: u64,
    stamp: u64,
    iq: *const i16,
    n: u32,
) -> i32 {
    if ptr.is_null() || iq.is_null() || n == 0 || n > 65536 {
        return -1;
    }
    let iq = std::slice::from_raw_parts(iq, n as usize * 2);
    (*ptr).submit(seq, stamp, iq).err().unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_take(ptr: *mut PfbBenchPool, out: *mut PoolResult) -> i32 {
    if ptr.is_null() || out.is_null() {
        return -1;
    }
    match (*ptr).ready.pop_front() {
        Some(item) => {
            *out = item.meta;
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_finish(ptr: *mut PfbBenchPool) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    (*ptr).finish().map(|_| 0).unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_stats(ptr: *const PfbBenchPool, out: *mut PoolStats) -> i32 {
    if ptr.is_null() || out.is_null() {
        return -1;
    }
    *out = (*ptr).stats;
    0
}

#[no_mangle]
pub unsafe extern "C" fn pfb_pool_close(ptr: *mut PfbBenchPool) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pfb_owner_boundary_worker_panic_wakes_flush_and_preserves_ordered_prefix() {
        let mut stream = PfbStream::new(62_500_000.0, 64, vec![0], 17).unwrap();
        stream.submit(0, 100, &[0; 128]).unwrap();
        stream.flush_pending().unwrap();
        // One real worker unwinds in Pfb::process; the other result senders stay live.
        stream.pool.bins = Arc::new(vec![u32::MAX]);
        let rescue = stream.pool.sender.as_ref().unwrap().clone();
        stream.submit(1, 228, &[0; 128]).unwrap();
        let (noticed, notice) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let owner = thread::spawn(move || {
            let failed = stream.flush_pending().is_err();
            let _ = noticed.send(failed);
            let _ = released.recv();
            let rejected = stream.submit(2, 356, &[0; 128]).is_err();
            let stopped = stream.finish().is_err();
            let still_failed = stream.finish().is_err() && stream.flush_pending().is_err();
            let joined = stream.pool.workers.is_empty();
            let prefix = stream.take().map(|p| (p.seq, p.stamp, p.epoch, p.input_index));
            (failed, rejected, stopped, still_failed, joined, prefix, stream.take().is_none())
        });
        let early = notice.recv_timeout(Duration::from_secs(1));
        // Also free the old implementation: close every remaining result sender by
        // giving its real worker the same fault. Never leave a failing test blocked.
        for sender in rescue {
            let _ = sender.send(Task {
                seq: 99, stamp: 0, input_index: 0,
                state: Some(Pfb::new(62_500_000.0, 2).unwrap()), history: Vec::new(),
                bins: Arc::new(vec![u32::MAX]), iq: vec![0; 128], execution_gate: None,
            });
        }
        let _ = release.send(());
        let (failed, rejected, stopped, still_failed, joined, prefix, empty) = owner.join().unwrap();
        assert_eq!(early, Ok(true), "one lost worker left flush waiting on healthy result senders");
        assert!(failed && rejected && stopped && still_failed && joined, "fatal pool must reject more input and join every worker");
        assert_eq!(prefix, Some((0, 100, 17, 0)));
        assert!(empty, "failed or later packets cannot cross the missing ordered boundary");
        let mut reopened = PfbStream::new(62_500_000.0, 64, vec![0], 18).unwrap();
        reopened.submit(0, 500, &[0; 128]).unwrap();
        reopened.finish().unwrap();
        let next = reopened.take().unwrap();
        assert_eq!((next.seq, next.stamp, next.epoch, next.input_index), (0, 500, 18, 0));
    }

    #[test]
    fn reused_worker_history_matches_serial_bits_at_partial_boundaries() {
        for fs in [62_500_000.0, 976_562.5] {
            for taps in [4, 8, 16] {
                let mut reference = Pfb::with_taps(fs, 2, taps).unwrap();
                reference.fir_kernel = 1;
                let bins = vec![0, (reference.m - 1) as u32];
                let mut stream = PfbStream::with_taps(fs, 16240, bins.clone(), 7, taps).unwrap();
                let mut seed = 20261105u32;
                let mut index = 0;
                for seq in 0..137 {
                    let n = [17, 16240, 4096, 121, 27][seq as usize % 5];
                    let iq: Vec<i16> = (0..2 * n).map(|_| {
                        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
                        (seed >> 8) as i16
                    }).collect();
                    let mut expected = vec![0.0; 2 * bins.len() * (n / reference.hop + 2)];
                    let groups = reference.process(&iq, &bins, &mut expected);
                    let stamp = 1000 + 2 * index;
                    stream.submit(seq, stamp, &iq).unwrap();
                    stream.flush_pending().unwrap();
                    let actual = stream.take().unwrap();
                    assert_eq!((actual.seq, actual.stamp, actual.input_index, actual.epoch, actual.groups),
                        (seq, stamp, index, 7, groups as u32));
                    assert_eq!(actual.iq.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                        expected[..2 * groups * bins.len()].iter().map(|v| v.to_bits()).collect::<Vec<_>>());
                    assert!(stream.take().is_none());
                    index += n as u64;
                }
                stream.finish().unwrap();
            }
        }
    }
    #[test]
    fn stream_reselects_only_at_drained_boundary_without_losing_history() {
        let mut stream = PfbStream::new(62_500_000.0, 16240, vec![5, 31], 7).unwrap();
        let mut reference = Pfb::new(62_500_000.0, 2).unwrap();
        reference.fir_kernel = 1;
        let mut seed = 20261019u32;
        let mut input_index = 0;
        for (seq, n) in [17, 16240, 121, 4096].into_iter().enumerate() {
            if seq == 1 {
                stream.replace_bins(vec![31, 127, 5]).unwrap();
            } else if seq == 2 {
                stream.replace_bins(vec![127, 31]).unwrap();
            }
            let selected = stream.bins().to_vec();
            for invalid in [vec![], vec![31, 31], vec![stream.bin_count() as u32]] {
                assert!(stream.replace_bins(invalid).is_err());
                assert_eq!(stream.bins(), selected);
            }
            let iq: Vec<i16> = (0..2 * n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 8) as i16
                })
                .collect();
            // The reference retains all bins across each partial-hop boundary.
            let reference_bins = [5, 31, 127];
            let mut expected = vec![0.0; 2 * reference_bins.len() * (n / reference.hop + 2)];
            let groups = reference.process(&iq, &reference_bins, &mut expected);
            let stamp = 1000 + 2 * input_index;
            stream.submit(seq as u64, stamp, &iq).unwrap();
            assert!(stream.replace_bins(vec![0]).is_err());
            assert_eq!(stream.bins(), selected);
            stream.flush_pending().unwrap();
            assert!(stream.replace_bins(vec![0]).is_err());
            assert_eq!(stream.bins(), selected);
            let item = stream.take().unwrap();
            assert_eq!(
                (item.seq, item.stamp, item.input_index, item.epoch, item.groups),
                (seq as u64, stamp, input_index, 7, groups as u32)
            );
            assert_eq!(item.iq.len(), 2 * groups * selected.len());
            for group in 0..groups {
                for (column, bin) in selected.iter().enumerate() {
                    let reference_column = reference_bins.iter().position(|value| value == bin).unwrap();
                    let actual = 2 * (group * selected.len() + column);
                    let wanted = 2 * (group * reference_bins.len() + reference_column);
                    assert_eq!(&item.iq[actual..actual + 2], &expected[wanted..wanted + 2]);
                }
            }
            assert!(stream.take().is_none());
            input_index += n as u64;
        }
        stream.finish().unwrap();
        assert!(stream.replace_bins(vec![0]).is_err());
    }

    #[test]
    fn pool_keeps_one_clock_and_bounded_ordered_results() {
        for strategy in [0, 1] {
            let fs = 62_500_000.0;
            let bins = [0, 5, 31, 127];
            let mut pool = PfbBenchPool::new(fs, 16240, bins.to_vec(), strategy).unwrap();
            let mut reference = Pfb::new(fs, 2).unwrap();
            reference.fir_kernel = 1;
            let mut seed = 20261019u32;
            let mut expected = Vec::new();
            let mut seq_seen = 0;
            for seq in 0..130 {
                let iq: Vec<i16> = (0..2 * 16240)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        (seed >> 8) as i16
                    })
                    .collect();
                let mut out = vec![0f32; 2 * bins.len() * (16240 / reference.hop + 2)];
                let groups = reference.process(&iq, &bins, &mut out);
                expected.push((groups as u32, out[0]));
                loop {
                    match pool.submit(seq, 1000 + seq * 32480, &iq) {
                        Ok(()) => break,
                        Err(-2) => {
                            let item = pool.ready.pop_front().unwrap();
                            assert_eq!(item.meta.seq, seq_seen);
                            assert_eq!(item.meta.stamp, 1000 + seq_seen * 32480);
                            assert_eq!(
                                (item.meta.groups, item.meta.first_re),
                                expected[seq_seen as usize]
                            );
                            seq_seen += 1;
                        }
                        err => panic!("submit: {err:?}"),
                    }
                }
                while let Some(item) = pool.ready.pop_front() {
                    assert_eq!(item.meta.seq, seq_seen);
                    assert_eq!(item.meta.stamp, 1000 + seq_seen * 32480);
                    assert_eq!(
                        (item.meta.groups, item.meta.first_re),
                        expected[seq_seen as usize]
                    );
                    seq_seen += 1;
                }
            }
            assert!(pool.submit(0, 0, &[0, 0]).is_err());
            pool.finish().unwrap();
            while let Some(item) = pool.ready.pop_front() {
                assert_eq!(item.meta.seq, seq_seen);
                assert_eq!(
                    (item.meta.groups, item.meta.first_re),
                    expected[seq_seen as usize]
                );
                seq_seen += 1;
            }
            assert_eq!(seq_seen, 130);
            assert_eq!(pool.stats.completed, 130);
            assert!(pool.stats.max_pending <= MAX_IN_FLIGHT as u32);
            assert!(pool.stats.max_ready <= MAX_IN_FLIGHT as u32);
        }
    }

    #[test]
    fn every_worker_can_start_before_earlier_jobs_finish() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (entered, events) = std::sync::mpsc::channel();
        let gate = Arc::new(ExecutionGate { entered, release: AtomicBool::new(false), held_seq: None });
        let producer_gate = gate.clone();
        let producer = thread::spawn(move || {
            let mut pool = PfbBenchPool::new(62_500_000.0, 16240, vec![0], 1).unwrap();
            pool.execution_gate = Some(producer_gate);
            for seq in 0..WORKERS as u64 {
                pool.submit(seq, 2 * seq * 16240, &vec![0; 2 * 16240]).unwrap();
            }
            pool.finish().unwrap();
            (pool.ready.iter().map(|r| r.meta.seq).collect::<Vec<_>>(), pool.stats)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut started = Vec::new();
        for _ in 0..WORKERS {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(seq) => started.push(seq),
                Err(_) => break,
            }
        }
        // Release before asserting, including on the old three-job deadlock.
        gate.release.store(true, Ordering::Release);
        let (ordered, stats) = producer.join().unwrap();
        started.sort_unstable();
        assert_eq!(started, (0..WORKERS as u64).collect::<Vec<_>>());
        assert_eq!(ordered, started);
        assert!(stats.max_ready <= MAX_IN_FLIGHT as u32);
        assert!(stats.max_pending <= MAX_IN_FLIGHT as u32);
    }

    #[test]
    fn stalled_oldest_reuses_idle_workers_within_retained_limit() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for held_seq in [0, 1] {
            let (entered, events) = std::sync::mpsc::channel();
            let gate = Arc::new(ExecutionGate {
                entered, release: AtomicBool::new(false), held_seq: Some(held_seq),
            });
            let bins = vec![127, 5, 0];
            let mut reference = Pfb::new(62_500_000.0, 2).unwrap();
            reference.fir_kernel = 1;
            let mut seed = 20261107u32;
            let mut input_index = 0;
            let mut expected = Vec::new();
            let blocks: Vec<Vec<i16>> = [17, 16240, 4096, 121, 27, 16240, 61, 4096]
                .into_iter().map(|n| {
                    let iq: Vec<i16> = (0..2 * n).map(|_| {
                        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
                        (seed >> 8) as i16
                    }).collect();
                    let mut output = vec![0.0; 2 * bins.len() * (n / reference.hop + 2)];
                    let groups = reference.process(&iq, &bins, &mut output);
                    output.truncate(2 * groups * bins.len());
                    expected.push((input_index, groups as u32, output));
                    input_index += n as u64;
                    iq
                }).collect();
            let producer_gate = gate.clone();
            let (attempted, attempts) = std::sync::mpsc::channel();
            let started_at = Instant::now();
            let producer = thread::spawn(move || {
                let mut stream = PfbStream::new(62_500_000.0, 16240, bins, 29).unwrap();
                stream.pool.execution_gate = Some(producer_gate);
                let mut delivered = Vec::new();
                let mut input_index = 0;
                let mut peak_retained = 0;
                for (seq, iq) in blocks.iter().enumerate() {
                    attempted.send(seq as u64).unwrap();
                    loop {
                        match stream.submit(seq as u64, 1000 + 2 * input_index, iq) {
                            Ok(()) => break,
                            Err(-2) => delivered.push(stream.take().expect("backpressure needs output")),
                            result => panic!("submit: {result:?}"),
                        }
                    }
                    let retained = stream.pool.in_flight + stream.pool.pending.len() + stream.pool.ready.len();
                    peak_retained = peak_retained.max(retained);
                    assert!(retained <= MAX_IN_FLIGHT);
                    assert!(stream.pool.in_flight <= WORKERS);
                    // The second case keeps a deliverable prefix until admission
                    // asks for space, while a later packet remains blocked.
                    if held_seq == 0 {
                        while let Some(output) = stream.take() { delivered.push(output); }
                    }
                    input_index += (iq.len() / 2) as u64;
                }
                stream.finish().unwrap();
                while let Some(output) = stream.take() { delivered.push(output); }
                assert!(stream.pool.workers.is_empty());
                (delivered, peak_retained, stream.stats())
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut started = Vec::new();
            let expected_started = MAX_IN_FLIGHT + held_seq as usize;
            while started.len() < expected_started {
                match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(seq) => started.push(seq),
                    Err(_) => break,
                }
            }
            let gate_elapsed = started_at.elapsed();
            let mut next_attempted = false;
            if started.len() == expected_started {
                while let Ok(seq) = attempts.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    if seq == expected_started as u64 { next_attempted = true; break; }
                }
            }
            let beyond_limit = events.recv_timeout(Duration::from_millis(50)).ok();
            // Unblock before any assertions so even an old implementation failure joins cleanly.
            gate.release.store(true, Ordering::Release);
            let (delivered, peak_retained, stats) = producer.join().unwrap();
            started.sort_unstable();
            eprintln!("stalled_seq{held_seq} entered_before_release={started:?} elapsed_us={} retained_peak={peak_retained}", gate_elapsed.as_micros());
            assert_eq!(started, (0..expected_started as u64).collect::<Vec<_>>(),
                "an unfinished oldest packet must not strand idle owners while retained slots remain");
            assert!(next_attempted, "producer must exercise the full retained boundary");
            assert_eq!(beyond_limit, None, "no seventh retained packet can start before the stalled packet is released");
            assert_eq!(peak_retained, MAX_IN_FLIGHT);
            assert_eq!(delivered.len(), expected.len());
            for (seq, (actual, (index, groups, values))) in delivered.iter().zip(&expected).enumerate() {
                assert_eq!((actual.seq, actual.stamp, actual.epoch, actual.input_index, actual.groups),
                    (seq as u64, 1000 + 2 * index, 29, *index, *groups));
                assert_eq!(actual.iq.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    values.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
            }
            assert_eq!(stats.submitted, expected.len() as u64);
            assert_eq!(stats.completed, stats.submitted);
            assert!(stats.max_ready <= MAX_IN_FLIGHT as u32);
            assert!(stats.max_pending <= MAX_IN_FLIGHT as u32);
        }
    }

    #[test]
    fn ready_results_cap_rejects_submit() {
        let mut pool = PfbBenchPool::new(62_500_000.0, 16240, vec![0], 1).unwrap();
        pool.sender.take();
        for worker in pool.workers.drain(..) {
            worker.join().unwrap();
        }
        let (tx, rx) = sync_channel(MAX_IN_FLIGHT);
        pool.receiver = rx;
        pool.stats.submitted = 3;
        pool.expected_submit = 3;
        pool.in_flight = 3;
        pool.idle_workers = VecDeque::from([3]);
        for seq in 0..3 {
            tx.send(Ok(WorkerResult {
                worker_id: seq as usize,
                meta: PoolResult {
                    seq,
                    ..Default::default()
                },
                input_index: 0,
                iq: Vec::new(),
                packet_samples: 16240,
                completed_at: Instant::now(),
            }))
            .unwrap();
        }
        for _ in 0..3 {
            pool.receive_one().unwrap();
        }
        assert_eq!(
            pool.ready.iter().map(|r| r.meta.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(pool.submit(3, 0, &[0, 0]), Err(-2));
        assert_eq!(pool.stats.completed, 3);
        assert_eq!(pool.in_flight, 0);
        pool.ready.clear();
        pool.finish().unwrap();
    }

    #[test]
    fn stream_delivers_exact_bins_clock_and_epoch_at_partial_boundaries() {
        assert!(PfbStream::new(62_500_000.0, 16240, vec![5, 5], 7).is_none());
        for bins in [
            vec![5],
            vec![0, 5, 31, 127],
            vec![127, 5, 0],
            (0..32).collect(),
        ] {
            let mut stream = PfbStream::new(62_500_000.0, 16240, bins.clone(), 7).unwrap();
            let mut reference = Pfb::new(62_500_000.0, 2).unwrap();
            reference.fir_kernel = 1;
            assert_eq!(stream.bins(), bins);
            assert_eq!(stream.sample_rate, 62_500_000.0 / reference.hop as f64);
            assert!(stream.delay_samples > 0.0);
            assert_eq!(stream.submit(0, 1000, &vec![0; 2 * 16241]), Err(-1));
            let mut expected = Vec::new();
            let mut seed = 20261019u32;
            let mut index = 0u64;
            let mut seen = 0usize;
            for (seq, n) in [17, 16240, 4096, 121, 16240, 27, 16240]
                .into_iter()
                .enumerate()
            {
                let iq: Vec<i16> = (0..2 * n)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        (seed >> 8) as i16
                    })
                    .collect();
                let stamp = 1000 + index * 2;
                let mut out = vec![0f32; 2 * bins.len() * (n / reference.hop + 2)];
                let groups = reference.process(&iq, &bins, &mut out);
                expected.push((
                    index,
                    stamp,
                    groups as u32,
                    out[..2 * groups * bins.len()].to_vec(),
                ));
                loop {
                    match stream.submit(seq as u64, stamp, &iq) {
                        Ok(()) => break,
                        Err(-2) => {
                            let item = stream.take().unwrap();
                            let (ix, ts, count, values) = &expected[seen];
                            assert_eq!(
                                (
                                    item.seq,
                                    item.epoch,
                                    item.input_index,
                                    item.stamp,
                                    item.groups
                                ),
                                (seen as u64, 7, *ix, *ts, *count)
                            );
                            assert_eq!(&item.iq, values);
                            seen += 1;
                        }
                        err => panic!("submit: {err:?}"),
                    }
                }
                while let Some(item) = stream.take() {
                    let (ix, ts, count, values) = &expected[seen];
                    assert_eq!(
                        (
                            item.seq,
                            item.epoch,
                            item.input_index,
                            item.stamp,
                            item.groups
                        ),
                        (seen as u64, 7, *ix, *ts, *count)
                    );
                    assert_eq!(&item.iq, values);
                    seen += 1;
                }
                index += n as u64;
            }
            stream.finish().unwrap();
            while let Some(item) = stream.take() {
                let (ix, ts, count, values) = &expected[seen];
                assert_eq!(
                    (
                        item.seq,
                        item.epoch,
                        item.input_index,
                        item.stamp,
                        item.groups
                    ),
                    (seen as u64, 7, *ix, *ts, *count)
                );
                assert_eq!(&item.iq, values);
                seen += 1;
            }
            assert_eq!(seen, expected.len());
            let stats = stream.stats();
            assert_eq!(
                (
                    stats.submitted,
                    stats.completed,
                    stats.task_capacity,
                    stats.result_capacity
                ),
                (
                    seen as u64,
                    seen as u64,
                    TASK_CAPACITY as u32,
                    MAX_IN_FLIGHT as u32
                )
            );
            assert!(stats.max_pending <= MAX_IN_FLIGHT as u32);
            assert!(stats.max_ready <= MAX_IN_FLIGHT as u32);
        }
    }
}
