//! Independent fine-filter and framer owners. Fanout never waits for a lane.
//! Each coarse bin is deinterleaved once and retained immutably by its consumers.
use super::sdr_pipeline::{FineMetrics, FineState};
use super::*;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use websa_channelizer::{FineChannel, PfbOutput};

pub(super) use super::receiver_limits::INPUT_JOBS;
const BATCH_JOBS: usize = 8;
const BATCH_DELAY: Duration = Duration::from_millis(4);
const OUTPUT_JOBS: usize = 32;
const OUTPUT_BYTES: usize = 8 * 1024 * 1024;

struct Column {
    packet: PfbOutput,
    iq: Vec<f32>,
}
struct Raw {
    seq: u64,
    stamp: u64,
    input_index: u64,
    iq: Vec<i16>,
}
enum Input {
    Coarse(Arc<Column>),
    Raw(Arc<Raw>),
}
impl Input {
    fn bytes(&self) -> usize {
        match self {
            Self::Coarse(column) => column.iq.len() * 4,
            Self::Raw(raw) => raw.iq.len() * 2,
        }
    }
    fn index(&self) -> u64 {
        match self {
            Self::Coarse(column) => column.packet.input_index,
            Self::Raw(raw) => raw.input_index,
        }
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct FineFault {
    pub channel: u32,
    pub channel_id: u32,
    pub epoch: u32,
    pub error: String,
    pub kind: reception_recovery::FaultKind,
    pub first_unavailable_input: Option<u64>,
    pub last_completed_input: Option<u64>,
}
#[derive(Default, Clone)]
struct LaneStats {
    accepted: u64,
    started: u64,
    succeeded: u64,
    failed: u64,
    cancelled: u64,
    rejected: u64,
    held_bytes: usize,
    peak_bytes: usize,
    output_bytes: usize,
    queue_wait: Timing,
    fine: Timing,
    framer: Timing,
    clock: (u64, u64),
    last_completed_input: Option<u64>,
    cpu_ns: u64,
    cpu_available: bool,
    fault: Option<FineFault>,
}
struct Shared {
    stats: PLMutex<LaneStats>,
    input_bytes_limit: usize,
    slot: u32,
    id: u32,
    epoch: u32,
    #[cfg(test)]
    gate: PLMutex<Option<Arc<tests::Gate>>>,
}
impl Shared {
    fn fail(&self, error: impl Into<String>, first: Option<u64>) {
        let mut state = self.stats.lock();
        if state.fault.is_none() {
            let error=error.into();
            state.fault = Some(FineFault {
                channel: self.slot,
                channel_id: self.id,
                epoch: self.epoch,
                kind: reception_recovery::FaultKind::fine(&error),
                error,
                first_unavailable_input: first,
                last_completed_input: state.last_completed_input,
            });
        }
    }
}
struct Work {
    input: Input,
    shared: Arc<Shared>,
    queued: Instant,
    accepted: bool,
    failed: bool,
    done: bool,
}
impl Drop for Work {
    fn drop(&mut self) {
        let mut stats = self.shared.stats.lock();
        stats.held_bytes -= self.input.bytes();
        if self.accepted && !self.done {
            if self.failed || std::thread::panicking() {
                stats.failed += 1;
            } else {
                stats.cancelled += 1;
            }
            if let Some(fault) = &mut stats.fault {
                fault.first_unavailable_input = Some(
                    fault
                        .first_unavailable_input
                        .map_or(self.input.index(), |old| old.min(self.input.index())),
                );
            }
        }
    }
}
struct Output {
    frames: IqFrames,
    bytes: usize,
    shared: Arc<Shared>,
}
impl Drop for Output {
    fn drop(&mut self) {
        self.shared.stats.lock().output_bytes -= self.bytes;
    }
}
enum Execution {
    Fine {
        channel: FineChannel,
        framer: BasebandFramer,
    },
    Direct(sdr_pipeline::DirectChannel),
}
impl Execution {
    fn clock(&self) -> (u64, u64) {
        match self {
            Self::Fine { channel, .. } => channel.clock(),
            Self::Direct(route) => route.clock(),
        }
    }
}
// The native filter and framer have one owner; no handle/workspace is shared.
unsafe impl Send for Execution {}
struct Lane {
    bin: Option<u32>,
    tx: Option<SyncSender<Vec<Work>>>,
    pending: Vec<Work>,
    rx: Receiver<Output>,
    join: Option<JoinHandle<Execution>>,
    shared: Arc<Shared>,
}
impl Drop for Lane {
    fn drop(&mut self) {
        self.tx.take();
        // Disconnect outputs before joining so shutdown cannot await a consumer.
        let (_, empty) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.rx, empty));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
#[derive(Clone, serde::Serialize)]
pub(super) struct FineLaneSnapshot {
    channel: u32,
    channel_id: u32,
    epoch: u32,
    accepted: u64,
    started: u64,
    succeeded: u64,
    failed: u64,
    cancelled: u64,
    rejected: u64,
    in_flight: u64,
    held_bytes: usize,
    peak_bytes: usize,
    input_bytes_limit: usize,
    output_bytes: usize,
    fine: websa_channelizer::timing::TimingSnapshot,
    framer: websa_channelizer::timing::TimingSnapshot,
    queue_wait: websa_channelizer::timing::TimingSnapshot,
    thread_cpu_ns: Option<u64>,
    fault: Option<FineFault>,
}

fn flush_lane(lane: &mut Lane) {
    if lane.pending.is_empty() {
        return;
    }
    let batch = std::mem::take(&mut lane.pending);
    let result = match lane.tx.as_ref() {
        Some(tx) => tx.try_send(batch),
        None => Err(TrySendError::Disconnected(batch)),
    };
    if let Err(error) = result {
        let (batch, message) = match error {
            TrySendError::Full(batch) => (
                batch,
                "fine input queue full; branch stopped without skipping samples",
            ),
            TrySendError::Disconnected(batch) => (batch, "fine owner disconnected or retiring"),
        };
        lane.shared
            .fail(message, batch.first().map(|work| work.input.index()));
        for mut work in batch {
            work.accepted = false;
            {
                let mut stats = lane.shared.stats.lock();
                stats.accepted -= 1;
                stats.rejected += 1;
            }
            drop(work);
        }
    }
}
fn enqueue(lane: &mut Lane, mut work: Work) {
    let full = {
        let stats = lane.shared.stats.lock();
        stats
            .accepted
            .saturating_sub(stats.succeeded + stats.failed + stats.cancelled)
            > INPUT_JOBS as u64 + 1
    };
    if full {
        work.accepted = false;
        {
            let mut stats = lane.shared.stats.lock();
            stats.accepted -= 1;
            stats.rejected += 1;
        }
        lane.shared.fail(
            "fine input queue full; branch stopped without skipping samples",
            Some(work.input.index()),
        );
        return;
    }
    lane.pending.push(work);
    if lane.pending.len() >= BATCH_JOBS || lane.pending[0].queued.elapsed() >= BATCH_DELAY {
        flush_lane(lane);
    }
}

fn publish(frames: IqFrames, shared: &Arc<Shared>, tx: &SyncSender<Output>) -> bool {
    if frames.is_empty() {
        return true;
    }
    let bytes = frames
        .iter()
        .map(|frame| {
            frame
                .native_iq()
                .map_or_else(|| frame.len(), |iq| iq.samples.len() * 4 + 96)
        })
        .sum();
    {
        let mut stats = shared.stats.lock();
        if stats.output_bytes + bytes > OUTPUT_BYTES {
            drop(stats);
            shared.fail(&format!("fine output exceeds {OUTPUT_BYTES} bytes retained payload"), None);
            return false;
        }
        stats.output_bytes += bytes;
    }
    if tx
        .try_send(Output {
            frames,
            bytes,
            shared: shared.clone(),
        })
        .is_err()
    {
        shared.fail(
            "fine output queue full; branch stopped without skipping samples",
            None,
        );
        return false;
    }
    true
}
fn run(
    mut execution: Execution,
    input: Receiver<Vec<Work>>,
    output: SyncSender<Output>,
    shared: Arc<Shared>,
) -> Execution {
    websa_channelizer::capture_scheduling::normal();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        'batches: while let Ok(batch) = input.recv() {
            for mut work in batch {
                if shared.stats.lock().fault.is_some() {
                    continue;
                }
                let begin = Instant::now();
                let cpu = super::receiver_workers::cpu_now();
                {
                    let mut stats = shared.stats.lock();
                    stats.started += 1;
                    stats.queue_wait.observe(work.queued.elapsed());
                }
                #[cfg(test)]
                if let Some(gate) = work.shared.gate.lock().take() {
                    gate.hold();
                }
                if shared.stats.lock().fault.is_some() {
                    continue;
                }
                let result = match (&mut execution, &work.input) {
                    (Execution::Fine { channel, framer }, Input::Coarse(column)) => channel
                        .process_column(&column.packet, &column.iq)
                        .map_err(str::to_owned)
                        .and_then(|range| {
                            shared.stats.lock().fine.observe(begin.elapsed());
                            let start = Instant::now();
                            let frames = framer
                                .push_native(&channel.output_view(&column.packet, range))
                                .map_err(str::to_owned);
                            shared.stats.lock().framer.observe(start.elapsed());
                            frames
                        }),
                    (Execution::Direct(route), Input::Raw(raw)) => {
                        let frames = route.process(raw.seq, raw.stamp, raw.input_index, &raw.iq);
                        shared.stats.lock().fine.observe(begin.elapsed());
                        frames
                    }
                    _ => Err("fine owner received wrong source representation".into()),
                };
                if let Some((before, after)) = cpu.zip(super::receiver_workers::cpu_now()) {
                    let mut stats = shared.stats.lock();
                    stats.cpu_ns = stats.cpu_ns.saturating_add(after.saturating_sub(before));
                    stats.cpu_available = true;
                }
                match result {
                    Ok(frames) => {
                        if !publish(frames, &shared, &output) {
                            work.failed = true;
                            break 'batches;
                        }
                        let mut stats = shared.stats.lock();
                        stats.succeeded += 1;
                        stats.clock = execution.clock();
                        stats.last_completed_input = Some(work.input.index());
                        work.done = true;
                    }
                    Err(error) => {
                        work.failed = true;
                        shared.fail(error, Some(work.input.index()));
                        break 'batches;
                    }
                }
            }
        }
    }));
    if result.is_err() {
        shared.fail("fine owner panicked", None);
    }
    execution
}

pub(super) struct FineWorker {
    pub state: FineState,
    lanes: BTreeMap<u32, Lane>,
    pub timing: Arc<PLMutex<FineMetrics>>,
    collect_timing: Timing,
    refreshed: Instant,
    restored_direct: BTreeMap<u32, sdr_pipeline::DirectChannel>,
}
impl FineWorker {
    pub fn start(mut state: FineState) -> Result<Self, String> {
        state.sample_clocks()?;
        let mut worker = Self {
            state,
            lanes: BTreeMap::new(),
            timing: Arc::new(PLMutex::new(FineMetrics::default())),
            collect_timing: Timing::default(),
            refreshed: Instant::now(),
            restored_direct: BTreeMap::new(),
        };
        worker.sync_lanes()?;
        worker.refresh();
        Ok(worker)
    }
    pub fn sync_lanes(&mut self) -> Result<(), String> {
        // Called only at configuration boundaries; no lookup by scanning all framers per packet.
        let slots: BTreeMap<_, _> = self
            .state
            .registry
            .iter()
            .map(|row| (u64::from(row.channel_id), row.clone()))
            .collect();
        for id in self.state.bank.execution_ids() {
            let row = slots.get(&id).ok_or("fine execution missing registry")?;
            if self
                .state
                .faults
                .get(&row.channel)
                .is_some_and(|fault| fault.channel_id == row.channel_id && fault.epoch == row.epoch)
            {
                continue;
            }
            if self.lanes.contains_key(&row.channel) {
                return Err("old fine owner must retire before replacement".into());
            }
            let channel = self
                .state
                .bank
                .detach_execution(id)
                .ok_or("fine owner missing")?;
            let replacement = if row.channel == 1 {
                BasebandFramer::new(id, row.epoch as u64, row.frequency_hz, row.output_rate)
            } else {
                BasebandFramer::new_aux(id, row.epoch as u64, row.frequency_hz, row.output_rate)
            }
            .and_then(|framer| framer.with_source_rate(row.input_rate))
            .ok_or("fine framer unavailable")?;
            let framer = if row.channel == 1 {
                std::mem::replace(&mut self.state.framer, replacement)
            } else {
                std::mem::replace(
                    &mut self
                        .state
                        .aux
                        .get_mut(&row.channel)
                        .ok_or("fine framer missing")?
                        .2,
                    replacement,
                )
            };
            let shared = Arc::new(Shared {
                input_bytes_limit: super::receiver_limits::input_bytes_limit(channel.input_rate(), 8) as usize,
                stats: PLMutex::new(LaneStats {
                    clock: channel.clock(),
                    ..Default::default()
                }),
                slot: row.channel,
                id: row.channel_id,
                epoch: row.epoch,
                #[cfg(test)]
                gate: PLMutex::new(None),
            });
            let (tx, input) = mpsc::sync_channel(INPUT_JOBS);
            let (output, rx) = mpsc::sync_channel(OUTPUT_JOBS);
            let bin = Some(channel.bin());
            let owner = shared.clone();
            let execution = Execution::Fine { channel, framer };
            let join = thread::Builder::new()
                .name(format!("fine-C{}", row.channel))
                .spawn(move || run(execution, input, output, owner))
                .map_err(|error| format!("spawn fine owner: {error}"))?;
            self.lanes.insert(
                row.channel,
                Lane {
                    bin,
                    tx: Some(tx),
                    pending: Vec::new(),
                    rx,
                    join: Some(join),
                    shared,
                },
            );
        }
        self.refresh();
        Ok(())
    }
    pub fn sync_directs(
        &mut self,
        direct: &mut BTreeMap<u32, sdr_pipeline::DirectChannel>,
    ) -> Result<(), String> {
        for (&slot, route) in direct.iter_mut().filter(|(_, route)| !route.detached()) {
            if self.lanes.contains_key(&slot) {
                return Err("previous fine owner still present".into());
            }
            let row = self
                .state
                .registry
                .iter()
                .find(|row| row.channel == slot)
                .ok_or("direct registry missing")?;
            if self
                .state
                .faults
                .get(&slot)
                .is_some_and(|fault| fault.channel_id == row.channel_id && fault.epoch == row.epoch)
            {
                continue;
            }
            let shared = Arc::new(Shared {
                input_bytes_limit: super::receiver_limits::input_bytes_limit(row.input_rate, 4) as usize,
                stats: PLMutex::new(LaneStats {
                    clock: route.clock(),
                    ..Default::default()
                }),
                slot,
                id: row.channel_id,
                epoch: row.epoch,
                #[cfg(test)]
                gate: PLMutex::new(None),
            });
            let (tx, input) = mpsc::sync_channel(INPUT_JOBS);
            let (output, rx) = mpsc::sync_channel(OUTPUT_JOBS);
            let execution = Execution::Direct(route.detach(slot)?);
            let owner = shared.clone();
            let join = thread::Builder::new()
                .name(format!("fine-C{slot}"))
                .spawn(move || run(execution, input, output, owner))
                .map_err(|error| format!("spawn direct fine owner: {error}"))?;
            self.lanes.insert(
                slot,
                Lane {
                    bin: None,
                    tx: Some(tx),
                    pending: Vec::new(),
                    rx,
                    join: Some(join),
                    shared,
                },
            );
        }
        self.refresh();
        Ok(())
    }
    pub fn submit_raw(
        &mut self,
        seq: u64,
        stamp: u64,
        input_index: u64,
        raw: &[i16],
    ) -> Result<(), String> {
        if !self.lanes.values().any(|lane| lane.bin.is_none()) {
            return Ok(());
        }
        let raw = Arc::new(Raw {
            seq,
            stamp,
            input_index,
            iq: raw.to_vec(),
        });
        for lane in self.lanes.values_mut().filter(|lane| lane.bin.is_none()) {
            if lane.shared.stats.lock().fault.is_some() {
                continue;
            }
            let input = Input::Raw(raw.clone());
            let bytes = input.bytes();
            {
                let mut stats = lane.shared.stats.lock();
                if stats.held_bytes + bytes > lane.shared.input_bytes_limit {
                    stats.rejected += 1;
                    drop(stats);
                    lane.shared
                        .fail(&format!("direct fine input exceeds {} bytes", lane.shared.input_bytes_limit), Some(input_index));
                    continue;
                }
                stats.held_bytes += bytes;
                stats.peak_bytes = stats.peak_bytes.max(stats.held_bytes);
                stats.accepted += 1;
            }
            let work = Work {
                input,
                shared: lane.shared.clone(),
                queued: Instant::now(),
                accepted: true,
                failed: false,
                done: false,
            };
            enqueue(lane, work);
        }
        Ok(())
    }
    pub fn direct_clocks(&self) -> Vec<(u64, u64, u64)> {
        self.lanes
            .iter()
            .filter(|(_, lane)| lane.bin.is_none())
            .map(|(&slot, lane)| {
                let clock = lane.shared.stats.lock().clock;
                (slot as u64, clock.0, clock.1)
            })
            .collect()
    }
    pub fn take_direct(&mut self) -> BTreeMap<u32, sdr_pipeline::DirectChannel> {
        std::mem::take(&mut self.restored_direct)
    }
    fn restore(&mut self, slot: u32, execution: Execution) -> Result<(), String> {
        match execution {
            Execution::Fine { channel, framer } => {
                self.state
                    .bank
                    .restore_execution(channel)
                    .map_err(str::to_owned)?;
                if slot == 1 {
                    self.state.framer = framer;
                } else if let Some(entry) = self.state.aux.get_mut(&slot) {
                    entry.2 = framer;
                }
            }
            Execution::Direct(route) => {
                self.restored_direct.insert(slot, route);
            }
        }
        Ok(())
    }
    pub fn submit(&mut self, packet: &PfbOutput, frames: &mut IqFrames) -> Result<(), String> {
        self.state
            .bank
            .accept_shared(&packet)
            .map_err(str::to_owned)?;
        let bins = self.state.bank.bin_layout();
        if packet.iq.len() != 2 * packet.groups as usize * bins.len() {
            return Err("shared coarse input shape mismatch".into());
        }
        let mut columns = BTreeMap::<u32, Arc<Column>>::new();
        for lane in self.lanes.values_mut().filter(|lane| lane.bin.is_some()) {
            let bin = lane.bin.unwrap();
            if lane.shared.stats.lock().fault.is_some() {
                continue;
            }
            let column = columns
                .entry(bin)
                .or_insert_with(|| {
                    let at = bins
                        .iter()
                        .position(|bin| *bin == lane.bin.unwrap())
                        .expect("provisioned fine bin");
                    let mut iq = Vec::with_capacity(packet.groups as usize * 2);
                    for group in packet.iq.chunks_exact(2 * bins.len()) {
                        iq.extend_from_slice(&group[2 * at..2 * at + 2]);
                    }
                    Arc::new(Column {
                        packet: PfbOutput {
                            seq: packet.seq,
                            epoch: packet.epoch,
                            stamp: packet.stamp,
                            input_index: packet.input_index,
                            groups: packet.groups,
                            process_ns: packet.process_ns,
                            iq: Vec::new(),
                        },
                        iq,
                    })
                })
                .clone();
            let bytes = column.iq.len() * 4;
            {
                let mut stats = lane.shared.stats.lock();
                if stats.held_bytes + bytes > lane.shared.input_bytes_limit {
                    stats.rejected += 1;
                    drop(stats);
                    lane.shared.fail(
                        &format!("fine input exceeds {} bytes retained payload", lane.shared.input_bytes_limit),
                        Some(packet.input_index),
                    );
                    continue;
                }
                stats.held_bytes += bytes;
                stats.peak_bytes = stats.peak_bytes.max(stats.held_bytes);
                stats.accepted += 1;
            }
            let work = Work {
                input: Input::Coarse(column),
                shared: lane.shared.clone(),
                queued: Instant::now(),
                accepted: true,
                failed: false,
                done: false,
            };
            enqueue(lane, work);
        }
        self.collect(frames)
    }
    pub fn fail_delivery(&mut self, identity: (u32, u32), first: u64) {
        if let Some(lane) = self
            .lanes
            .values()
            .find(|lane| (lane.shared.id, lane.shared.epoch) == identity)
        {
            lane.shared.fail(
                "publisher IQ FIFO full; source branch stopped without skipping samples",
                Some(first),
            );
            if let Some(fault) = lane.shared.stats.lock().fault.clone() {
                self.state.faults.insert(fault.channel, fault);
            }
        }
    }
    pub fn collect(&mut self, frames: &mut IqFrames) -> Result<(), String> {
        let began = Instant::now();
        for lane in self.lanes.values_mut() {
            if lane
                .pending
                .first()
                .is_some_and(|work| work.queued.elapsed() >= BATCH_DELAY)
            {
                flush_lane(lane);
            }
            while let Ok(mut output) = lane.rx.try_recv() {
                frames.append(&mut output.frames);
            }
            let stats = lane.shared.stats.lock();
            if let Some(fault) = &stats.fault {
                self.state.faults.insert(fault.channel, fault.clone());
            }
            self.state
                .bank
                .update_clock(lane.shared.id as u64, stats.clock);
        }
        self.collect_timing.observe(began.elapsed());
        if self.refreshed.elapsed() >= Duration::from_millis(50) {
            self.refresh();
        }
        Ok(())
    }
    fn refresh(&mut self) {
        let mut fine = Timing::default();
        let mut framer = Timing::default();
        for lane in self.lanes.values() {
            let stats = lane.shared.stats.lock();
            fine.merge(&stats.fine);
            framer.merge(&stats.framer);
            if let Some(fault) = &stats.fault {
                self.state.faults.insert(fault.channel, fault.clone());
            }
        }
        self.state.fine_timing = fine;
        self.state.framer_timing = framer;
        if self.state.sample_clocks().is_ok() {
            *self.timing.lock() = self.state.metrics_ref();
        }
        self.refreshed = Instant::now();
    }
    pub fn snapshots(&self) -> Arc<[FineLaneSnapshot]> {
        self.lanes
            .iter()
            .map(|(&slot, lane)| {
                let s = lane.shared.stats.lock();
                FineLaneSnapshot {
                    channel: slot,
                    channel_id: lane.shared.id,
                    epoch: lane.shared.epoch,
                    accepted: s.accepted,
                    started: s.started,
                    succeeded: s.succeeded,
                    failed: s.failed,
                    cancelled: s.cancelled,
                    rejected: s.rejected,
                    in_flight: s
                        .accepted
                        .saturating_sub(s.succeeded + s.failed + s.cancelled),
                    held_bytes: s.held_bytes,
                    peak_bytes: s.peak_bytes,
                    input_bytes_limit: lane.shared.input_bytes_limit,
                    output_bytes: s.output_bytes,
                    fine: s.fine.snapshot(),
                    framer: s.framer.snapshot(),
                    queue_wait: s.queue_wait.snapshot(),
                    thread_cpu_ns: s.cpu_available.then_some(s.cpu_ns),
                    fault: s.fault.clone(),
                }
            })
            .collect::<Vec<_>>()
            .into()
    }
    pub fn full_wait(&self) -> websa_channelizer::timing::TimingSnapshot {
        Default::default()
    }
    pub fn collect_snapshot(&self) -> websa_channelizer::timing::TimingSnapshot {
        self.collect_timing.snapshot()
    }
    /// Wait only for this lane before closing its owner. Keep admission open
    /// until it is idle so a timed-out edit leaves the existing stream usable.
    pub fn pause_channel(&mut self, slot: u32, frames: &mut IqFrames) -> Result<(), String> {
        let Some(lane) = self.lanes.get_mut(&slot) else {
            return Ok(());
        };
        flush_lane(lane);
        let started = Instant::now();
        loop {
            // Other owners keep processing while this one drains. Collect their
            // ready output too, so a slow target cannot fill a healthy lane's FIFO.
            for lane in self.lanes.values_mut() {
                while let Ok(mut output) = lane.rx.try_recv() {
                    frames.append(&mut output.frames);
                }
            }
            let lane = &self.lanes[&slot];
            let idle = {
                let stats = lane.shared.stats.lock();
                stats.accepted == stats.succeeded + stats.failed + stats.cancelled
            };
            if idle || lane.join.as_ref().unwrap().is_finished() {
                break;
            }
            if started.elapsed() > Duration::from_millis(100) {
                return Err("fine channel busy; retry configuration shortly".into());
            }
            thread::sleep(Duration::from_micros(100));
        }
        // No native work remains. Closing the queue wakes the idle owner; joining
        // now cannot wait for a filter call or cancel an accepted source block.
        let mut lane = self.lanes.remove(&slot).unwrap();
        lane.tx.take();
        while let Ok(mut output) = lane.rx.try_recv() {
            frames.append(&mut output.frames);
        }
        let execution = lane
            .join
            .take()
            .unwrap()
            .join()
            .map_err(|_| "fine owner panicked")?;
        self.restore(slot, execution)?;
        Ok(())
    }
    #[cfg(test)]
    pub fn finish(self, frames: &mut IqFrames) -> Result<FineState, String> {
        self.finish_with_direct(frames).map(|(state, _)| state)
    }
    pub fn finish_with_direct(
        mut self,
        frames: &mut IqFrames,
    ) -> Result<(FineState, BTreeMap<u32, sdr_pipeline::DirectChannel>), String> {
        for lane in self.lanes.values_mut() {
            flush_lane(lane);
            lane.tx.take();
        }
        let slots: Vec<_> = self.lanes.keys().copied().collect();
        for slot in slots {
            let lane = self.lanes.get_mut(&slot).unwrap();
            while let Ok(mut output) = lane.rx.recv() {
                frames.append(&mut output.frames);
            }
            let execution = lane
                .join
                .take()
                .unwrap()
                .join()
                .map_err(|_| "fine owner panicked")?;
            self.restore(slot, execution)?;
        }
        self.refresh();
        Ok((self.state, self.restored_direct))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};
    pub(super) struct Gate {
        state: Mutex<(bool, bool)>,
        wake: Condvar,
    }
    impl Gate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new((false, false)),
                wake: Condvar::new(),
            })
        }
        pub(super) fn hold(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.wake.notify_all();
            while !state.1 {
                state = self.wake.wait(state).unwrap();
            }
        }
        fn entered(&self) {
            let state = self.state.lock().unwrap();
            let (state, timeout) = self
                .wake
                .wait_timeout_while(state, Duration::from_secs(5), |state| !state.0)
                .unwrap();
            assert!(state.0 && !timeout.timed_out());
        }
        fn release(&self) {
            self.state.lock().unwrap().1 = true;
            self.wake.notify_all();
        }
    }
    struct Release(Arc<Gate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    fn plan(count: u32) -> Ddc {
        let configs: Vec<_> = (1..=count)
            .map(|slot| {
                let mut config = receiver_workers::ChannelConfig::for_slot(slot);
                config.enabled = true;
                config.freq = 100e6
                    + if slot <= 2 {
                        0.0
                    } else {
                        (slot - 2) as f64 * BIN_SPACING
                    };
                config
            })
            .collect();
        Ddc::configured_service(100e6, 50e6, &mut None, 16, IQ_FS, &configs).unwrap()
    }
    fn packet(state: &FineState, seq: u64) -> PfbOutput {
        let groups = 1000;
        let columns = state.bank.bin_layout().len();
        PfbOutput {
            seq,
            epoch: state.registry[0].epoch as u64,
            stamp: seq * 64000 * 2,
            input_index: seq * 64000,
            groups,
            process_ns: 0,
            iq: (0..groups as usize * columns)
                .flat_map(|index| {
                    let phase = (seq as f64 * 1000.0 + (index / columns) as f64) * 0.031;
                    [
                        (phase.cos() * 10000.0) as f32,
                        (phase.sin() * 10000.0) as f32,
                    ]
                })
                .collect(),
        }
    }
    fn wait_count(worker: &mut FineWorker, slot: u32, count: u64, frames: &mut IqFrames) {
        let start = Instant::now();
        loop {
            worker.collect(frames).unwrap();
            let s = worker.lanes[&slot].shared.stats.lock();
            assert!(s.fault.is_none(), "{:?}", s.fault);
            if s.succeeded >= count {
                break;
            }
            drop(s);
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::yield_now();
        }
    }
    fn sample_bits(frames: &[IqFrame], id: u32) -> Vec<(u64, u32)> {
        frames
            .iter()
            .filter(|frame| u32::from_le_bytes(frame.header()[32..36].try_into().unwrap()) == id)
            .flat_map(|frame| {
                let index = u64::from_le_bytes(frame.header()[40..48].try_into().unwrap());
                let iq = frame.native_iq().unwrap();
                iq.samples.iter().map(move |value| (index, value.to_bits()))
            })
            .collect()
    }
    #[test]
    fn parallel_fine_matches_serial_samples_clocks_and_short_tails() {
        let mut a = plan(MAX_CHANNELS as u32);
        let mut b = plan(MAX_CHANNELS as u32);
        let mut serial = a.fine.take().unwrap();
        let mut worker = FineWorker::start(b.fine.take().unwrap()).unwrap();
        let (mut expected, mut actual) = (Vec::new(), Vec::new());
        for seq in 0..40 {
            expected.extend(serial.deliver(&packet(&serial, seq)).unwrap());
            worker
                .submit(&packet(&worker.state, seq), &mut actual)
                .unwrap();
            // Pace by completion, not a guessed machine-specific sleep.
            for slot in 1..=MAX_CHANNELS as u32 {
                wait_count(&mut worker, slot, seq + 1, &mut actual);
            }
        }
        let mut parallel = worker.finish(&mut actual).unwrap();
        expected.extend(serial.framer.finish_native().unwrap());
        actual.extend(parallel.framer.finish_native().unwrap());
        for (_, _, framer) in serial.aux.values_mut() {
            expected.extend(framer.finish_native().unwrap());
        }
        for (_, _, framer) in parallel.aux.values_mut() {
            actual.extend(framer.finish_native().unwrap());
        }
        for slot in 1..=MAX_CHANNELS as u32 {
            assert_eq!(
                sample_bits(&actual, slot),
                sample_bits(&expected, slot),
                "C{slot}"
            );
        }
        assert_eq!(
            parallel.bank.clocks().unwrap(),
            serial.bank.clocks().unwrap()
        );
    }
    #[test]
    fn local_retune_changes_only_target_and_preserves_other_native_samples() {
        let mut serial = plan(3);
        let mut parallel = plan(3);
        parallel.resume_fine().unwrap();
        let (mut expected, mut actual) = (Vec::new(), Vec::new());
        for seq in 0..60 {
            let raw: Vec<i16> = (0..16240)
                .flat_map(|i| {
                    let phase = (seq * 16240 + i) as f64 * 0.001;
                    [
                        (phase.cos() * 10000.0) as i16,
                        (phase.sin() * 10000.0) as i16,
                    ]
                })
                .collect();
            expected.extend(
                serial
                    .process_capture_packet(&raw, seq as u64 * 32480)
                    .unwrap(),
            );
            actual.extend(
                parallel
                    .process_capture_packet(&raw, seq as u64 * 32480)
                    .unwrap(),
            );
            if seq == 25 {
                let old_owner = parallel.fine_worker.as_ref().unwrap().lanes[&1]
                    .join
                    .as_ref()
                    .unwrap()
                    .thread()
                    .id();
                let mut config = parallel.service_configs.as_ref().unwrap()[2].clone();
                config.freq += 500.0;
                expected.extend(serial.configure_service_channel(config.clone()).unwrap());
                actual.extend(parallel.configure_service_channel(config).unwrap());
                assert_eq!(
                    old_owner,
                    parallel.fine_worker.as_ref().unwrap().lanes[&1]
                        .join
                        .as_ref()
                        .unwrap()
                        .thread()
                        .id()
                );
            }
        }
        expected.extend(serial.suspend_fine().unwrap());
        serial.finish_framers(&mut expected).unwrap();
        actual.extend(parallel.suspend_fine().unwrap());
        parallel.finish_framers(&mut actual).unwrap();
        for id in [1, 2, 3, 4] {
            assert_eq!(
                sample_bits(&actual, id),
                sample_bits(&expected, id),
                "identity {id}"
            );
        }
    }

    #[test]
    fn blocked_fine_owner_does_not_block_other_channels_or_local_retune() {
        let mut ddc = plan(3);
        let mut worker = FineWorker::start(ddc.fine.take().unwrap()).unwrap();
        let gate = Gate::new();
        *worker.lanes[&2].shared.gate.lock() = Some(gate.clone());
        let release = Release(gate.clone());
        let mut frames = Vec::new();
        worker
            .submit(&packet(&worker.state, 0), &mut frames)
            .unwrap();
        for lane in worker.lanes.values_mut() {
            flush_lane(lane);
        }
        gate.entered();
        wait_count(&mut worker, 1, 1, &mut frames);
        wait_count(&mut worker, 3, 1, &mut frames);
        // FineState's source sequence is one packet ahead of the idle test PFB.
        // Exercise the same targeted retirement used by the service control path.
        let began = Instant::now();
        worker.pause_channel(3, &mut frames).unwrap();
        assert!(began.elapsed() < Duration::from_millis(100));
        assert!(!worker.lanes[&2].join.as_ref().unwrap().is_finished());
        worker.sync_lanes().unwrap();
        assert!(worker
            .pause_channel(2, &mut frames)
            .unwrap_err()
            .contains("busy"));
        assert!(
            worker.lanes.contains_key(&2),
            "do not replace a still executing native owner"
        );
        assert!(worker.lanes[&2].tx.is_some());
        assert!(worker.lanes[&2].shared.stats.lock().fault.is_none());
        drop(release);
        wait_count(&mut worker, 2, 1, &mut frames);
        worker.submit(&packet(&worker.state, 1), &mut frames).unwrap();
        for lane in worker.lanes.values_mut() { flush_lane(lane); }
        wait_count(&mut worker, 2, 2, &mut frames);
        assert!(worker.lanes[&2].shared.stats.lock().fault.is_none());
        let _ = worker.finish(&mut frames).unwrap();
    }

    #[test]
    fn mode_switch_drains_a_slow_owner_without_faulting_or_losing_samples() {
        let mut ddc = plan(1);
        let mut worker = FineWorker::start(ddc.fine.take().unwrap()).unwrap();
        let gate = Gate::new();
        *worker.lanes[&1].shared.gate.lock() = Some(gate.clone());
        let mut frames = Vec::new();
        worker.submit(&packet(&worker.state, 0), &mut frames).unwrap();
        flush_lane(worker.lanes.get_mut(&1).unwrap());
        gate.entered();
        let shared = worker.lanes[&1].shared.clone();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(15));
            gate.release();
        });
        let paused = worker.pause_channel(1, &mut frames);
        release.join().unwrap();
        paused.unwrap();
        let stats = shared.stats.lock();
        assert_eq!(stats.succeeded, stats.accepted);
        assert_eq!(stats.cancelled, 0);
        assert!(stats.fault.is_none());
        drop(stats);
        worker.sync_lanes().unwrap();
        worker.submit(&packet(&worker.state, 1), &mut frames).unwrap();
        worker.finish(&mut frames).unwrap();
    }
    #[test]
    fn channel_deletion_drains_slow_owner_on_first_attempt_and_preserves_other_channel() {
        for target in [1, 2] {
            let mut serial = plan(2);
            let mut parallel = plan(2);
            parallel.resume_fine().unwrap();
            let healthy = 3 - target;
            let gate = Gate::new();
            let release_on_failure = Release(gate.clone());
            let worker = parallel.fine_worker.as_mut().unwrap();
            *worker.lanes[&target].shared.gate.lock() = Some(gate.clone());
            let shared = worker.lanes[&target].shared.clone();
            let healthy_owner = worker.lanes[&healthy].join.as_ref().unwrap().thread().id();
            let (mut expected, mut actual) = (Vec::new(), Vec::new());
            for seq in 0..8 {
                let raw: Vec<i16> = (0..SDR_PACKET_SAMPLES)
                    .flat_map(|i| {
                        let phase = (seq * SDR_PACKET_SAMPLES + i) as f64 * 0.001;
                        [(phase.cos() * 10000.0) as i16, (phase.sin() * 10000.0) as i16]
                    })
                    .collect();
                let stamp = (seq * SDR_PACKET_SAMPLES * 2) as u64;
                expected.extend(serial.process_capture_packet(&raw, stamp).unwrap());
                actual.extend(parallel.process_capture_packet(&raw, stamp).unwrap());
                if seq == 3 {
                    parallel.stream.flush_pending().unwrap();
                    actual.extend(parallel.drain().unwrap());
                    flush_lane(parallel.fine_worker.as_mut().unwrap().lanes.get_mut(&target).unwrap());
                    gate.entered();
                    let mut config = parallel.service_configs.as_ref().unwrap()[(target - 1) as usize].clone();
                    config.enabled = false;
                    expected.extend(serial.configure_service_channel(config.clone()).unwrap());
                    // Deletion must tolerate a native call taking longer than
                    // the old 2 ms retirement deadline, without a second command.
                    let release_gate = gate.clone();
                    let release = thread::spawn(move || {
                        thread::sleep(Duration::from_millis(15));
                        release_gate.release();
                    });
                    let deleted = parallel.configure_service_channel(config);
                    release.join().unwrap();
                    actual.extend(deleted.unwrap());
                    assert!(!parallel.registry.iter().any(|row| row.channel == target));
                    assert!(parallel.faults.is_empty());
                    let worker = parallel.fine_worker.as_ref().unwrap();
                    assert!(!worker.lanes.contains_key(&target));
                    assert_eq!(worker.lanes[&healthy].join.as_ref().unwrap().thread().id(), healthy_owner);
                    let stats = shared.stats.lock();
                    assert!(stats.accepted > 0);
                    assert_eq!(stats.succeeded, stats.accepted);
                    assert_eq!(stats.cancelled, 0);
                    assert!(stats.fault.is_none());
                }
            }
            expected.extend(serial.suspend_fine().unwrap());
            serial.finish_framers(&mut expected).unwrap();
            actual.extend(parallel.suspend_fine().unwrap());
            parallel.finish_framers(&mut actual).unwrap();
            for id in [1, 2] {
                let samples = sample_bits(&actual, id);
                assert!(!samples.is_empty());
                assert_eq!(samples, sample_bits(&expected, id), "delete C{target}, output C{id}");
            }
            drop(release_on_failure);
        }
    }

    #[test]
    fn source_recovery_with_unchanged_configuration_gets_new_iq_identity() {
        let mut ddc = plan(2);
        let epoch = ddc.registry[1].epoch;
        ddc.faults.insert(
            2,
            FineFault {
                channel: 2,
                channel_id: 2,
                epoch,
                error: "injected fine failure".into(), kind:reception_recovery::FaultKind::Internal,
                first_unavailable_input: Some(0),
                last_completed_input: None,
            },
        );
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.hardware_shim = Some("unit test does not open hardware".into());
        state.sdr_channels = ddc.service_configs.clone().unwrap();
        // Legacy compatibility fields are part of channel_configs' source of truth.
        state.sdr_aux = json!({"2":state.sdr_channels[1]});
        state.ddc = Some(SendDdc(ddc));
        state
            .apply_channel_config(&json!({"channel":2,"recover":true}))
            .unwrap();
        assert!(state.fine_faults().is_empty());
        assert_ne!(
            state
                .stream_registry()
                .iter()
                .find(|row| row.channel == 2)
                .unwrap()
                .channel_id,
            2
        );
        assert_eq!(
            state
                .stream_registry()
                .iter()
                .find(|row| row.channel == 1)
                .unwrap()
                .channel_id,
            1
        );
    }

    #[test]
    fn direct_input_retains_a_short_wideband_burst_without_dropping_samples() {
        let config=receiver_workers::ChannelConfig {freq:1_090e6,mode:"adsb".into(),if_bw:1_800_000.0,
            baseband:Some(receiver_workers::BasebandConfig {passband_hz:1_800_000.0,
                transition_hz:225_000.0,minimum_rate_hz:2_500_000.0}),
            ..receiver_workers::ChannelConfig::default()};
        let mut ddc=Ddc::configured_service(1_090e6,6_250_000.0,&mut None,16,7_812_500.0,&[config]).unwrap();
        let mut worker=FineWorker::start(ddc.fine.take().unwrap()).unwrap();
        worker.sync_directs(&mut ddc.direct).unwrap();
        let gate=Gate::new();
        *worker.lanes[&1].shared.gate.lock()=Some(gate.clone());
        let release=Release(gate.clone());
        let raw=vec![100_i16;SDR_PACKET_SAMPLES*2];
        worker.submit_raw(0,0,0,&raw).unwrap();
        flush_lane(worker.lanes.get_mut(&1).unwrap());gate.entered();
        for seq in 1..128 {
            let index=seq*SDR_PACKET_SAMPLES as u64;
            worker.submit_raw(seq,index*2,index,&raw).unwrap();
        }
        {
            let stats=worker.lanes[&1].shared.stats.lock();
            assert!(stats.held_bytes>4*1024*1024);
            assert!(stats.fault.is_none());
        }
        drop(release);
        let mut frames=Vec::new();
        wait_count(&mut worker,1,128,&mut frames);
        let (state,_)=worker.finish_with_direct(&mut frames).unwrap();
        assert!(state.faults.is_empty());
        assert!(frames.iter().any(|frame|frame.native_iq().is_some_and(|iq|!iq.samples.is_empty())));
    }

    #[test]
    fn fine_overflow_is_identity_scoped_and_counts_executing_references() {
        let mut ddc = plan(2);
        let mut worker = FineWorker::start(ddc.fine.take().unwrap()).unwrap();
        let gate = Gate::new();
        *worker.lanes[&2].shared.gate.lock() = Some(gate.clone());
        let release = Release(gate.clone());
        let mut frames = Vec::new();
        worker
            .submit(&packet(&worker.state, 0), &mut frames)
            .unwrap();
        for lane in worker.lanes.values_mut() {
            flush_lane(lane);
        }
        gate.entered();
        assert_eq!(worker.lanes[&2].shared.stats.lock().held_bytes, 1000 * 8);
        wait_count(&mut worker, 1, 1, &mut frames);
        let mut rejected = None;
        for seq in 1..=INPUT_JOBS as u64 + 1 {
            worker
                .submit(&packet(&worker.state, seq), &mut frames)
                .unwrap();
            wait_count(&mut worker, 1, seq + 1, &mut frames);
            if worker.state.faults.contains_key(&2) { rejected = Some(seq); break; }
        }
        let fault = worker.state.faults.get(&2).unwrap();
        assert_eq!(fault.channel_id, 2);
        assert_eq!(fault.kind, reception_recovery::FaultKind::Overload);
        assert_eq!(
            fault.first_unavailable_input,
            rejected.map(|seq| seq * 64000)
        );
        assert!(!worker.state.faults.contains_key(&1));
        drop(release);
        let state = worker.finish(&mut frames).unwrap();
        assert!(state.faults.contains_key(&2));
    }
}
