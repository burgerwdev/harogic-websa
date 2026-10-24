//! Bounded PFB workers with ordered coarse-bin IQ delivery.
use crate::pfb::Pfb;
use crate::timing::{Timing, TimingSnapshot};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Fixed worker count; task and result channels have bounded capacity.
pub const WORKERS: usize = 4;
pub const MAX_IN_FLIGHT: usize = WORKERS + 2;

struct Task {
    seq: u64,
    stamp: u64,
    input_index: u64,
    state: Pfb,
    bins: Arc<Vec<u32>>,
    iq: Vec<i16>,
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
    /// Worker completion to ordered receive, including caller scheduling delay.
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
    sender: Option<SyncSender<Task>>,
    receiver: Receiver<WorkerResult>,
    workers: Vec<JoinHandle<()>>,
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
        let (sender, task_rx) = sync_channel::<Task>(MAX_IN_FLIGHT);
        let task_rx = Arc::new(Mutex::new(task_rx));
        let (result_tx, receiver) = sync_channel::<WorkerResult>(MAX_IN_FLIGHT);
        let workers = (0..WORKERS)
            .map(|_| {
                let reader = task_rx.clone();
                let writer = result_tx.clone();
                thread::spawn(move || {
                    let mut output = vec![0f32; 2 * bin_count * (packet_samples / hop + 2)];
                    loop {
                        let item = reader.lock().unwrap().recv();
                        let Ok(mut task) = item else {
                            break;
                        };
                        let started = Instant::now();
                        let bin_count = task.bins.len();
                        output.resize(2 * bin_count * (packet_samples / hop + 2), 0.0);
                        let groups = task.state.process(&task.iq, &task.bins, &mut output);
                        let samples = if emit_iq {
                            output[..2 * groups * bin_count].to_vec()
                        } else {
                            Vec::new()
                        };
                        let result = WorkerResult {
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
                        };
                        if writer.send(result).is_err() {
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
            sender: Some(sender),
            receiver,
            workers,
            max_packet_samples: packet_samples,
            in_flight: 0,
            expected_submit: 0,
            expected_output: 0,
            pending: BTreeMap::new(),
            ready: VecDeque::new(),
            stats: PoolStats {
                task_capacity: MAX_IN_FLIGHT as u32,
                result_capacity: MAX_IN_FLIGHT as u32,
                ..Default::default()
            },
            timings: PoolTimings::default(),
            slow_ns_per_sample: 4.0 * 1e9 / fs,
            stopped: false,
        })
    }

    fn receive_one(&mut self) -> Result<(), ()> {
        let expected_seq = self.expected_output;
        let started = Instant::now();
        let result = self.receiver.recv().map_err(|_| ())?;
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
        self.in_flight -= 1;
        if self.pending.insert(result.meta.seq, result).is_some() {
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
        if self.stopped
            || seq != self.expected_submit
            || iq.len() & 1 != 0
            || iq.is_empty()
            || iq.len() / 2 > self.max_packet_samples
        {
            return Err(-1);
        }
        if self.ready.len() >= 3 {
            return Err(-2);
        } // drain metadata; never grow an unbounded queue
        while self.stats.submitted - self.stats.completed >= 3 {
            self.receive_admitted(seq, stamp).map_err(|_| -3)?;
        }
        if self.ready.len() >= 3 {
            return Err(-2);
        }
        let input_index = self.input_index;
        let threshold_ns = (((iq.len() / 2) as f64 * self.slow_ns_per_sample) as u64).max(1);
        let before = Instant::now();
        let mut state = self.controller.clone();
        let elapsed = before.elapsed();
        self.stats.clone_ns += elapsed.as_nanos() as u64;
        self.timings.observe(PoolTimingStage::Clone, elapsed, seq, stamp, None, threshold_ns);
        let before = Instant::now();
        if self.overlap_history {
            if !state.load_history(self.input_index, &self.tail) {
                return Err(-4);
            }
            let n = iq.len() / 2;
            let len = self.controller.history_len();
            if n >= len {
                self.tail.copy_from_slice(&iq[2 * (n - len)..]);
            } else {
                self.tail.copy_within(2 * n.., 0);
                self.tail[2 * (len - n)..].copy_from_slice(iq);
            }
        } else {
            self.controller.process(iq, &[], &mut []);
        }
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
            bins: self.bins.clone(),
            iq: iq.to_vec(),
        };
        let copy_elapsed = before.elapsed();
        let send_started = Instant::now();
        self.sender.as_ref().ok_or(-3)?.send(task).map_err(|_| -3)?;
        let send_elapsed = send_started.elapsed();
        self.stats.task_wait_ns += before.elapsed().as_nanos() as u64;
        self.timings.observe(PoolTimingStage::IqCopy, copy_elapsed, seq, stamp, None, threshold_ns);
        self.timings.observe(PoolTimingStage::TaskSend, send_elapsed, seq, stamp, None, threshold_ns);
        self.expected_submit += 1;
        self.stats.submitted += 1;
        self.in_flight += 1;
        if self.in_flight >= MAX_IN_FLIGHT {
            self.receive_admitted(seq, stamp).map_err(|_| -3)?;
        }
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
            return Ok(());
        }
        self.stopped = true;
        self.sender.take();
        while self.in_flight > 0 {
            self.receive_one()?;
        }
        for worker in self.workers.drain(..) {
            worker.join().map_err(|_| ())?;
        }
        if !self.pending.is_empty() || self.stats.submitted != self.stats.completed {
            return Err(());
        }
        Ok(())
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
        if self.pool.stopped {
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
        for seq in 0..3 {
            tx.send(WorkerResult {
                meta: PoolResult {
                    seq,
                    ..Default::default()
                },
                input_index: 0,
                iq: Vec::new(),
                packet_samples: 16240,
                completed_at: Instant::now(),
            })
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
                    MAX_IN_FLIGHT as u32,
                    MAX_IN_FLIGHT as u32
                )
            );
            assert!(stats.max_pending <= MAX_IN_FLIGHT as u32);
            assert!(stats.max_ready <= MAX_IN_FLIGHT as u32);
        }
    }
}
