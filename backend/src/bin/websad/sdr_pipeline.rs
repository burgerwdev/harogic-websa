use super::*;
use std::sync::mpsc::{self as sync, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use websa_channelizer::PfbOutput;
use websa_channelizer::timing::TimingSnapshot;

/// Admission reserve is documented in docs/en/ARCHITECTURE.md.
/// This admits fine processing/framing only; PFB and transport retain their own gates.
pub(super) const FINE_COST_RESERVE: f64 = 2.0;
pub(super) const FINE_COST_LIMIT_NS: f64 = 1_000_000_000.0;

#[cfg(not(test))]
fn calibrate_fine_cost() -> Result<websa_channelizer::FineCostBudget, String> {
    use websa_channelizer::{FineChannel, FineSpec};
    let mut costs = [0.0f64; 23];
    for decimate in 2..=24 {
        // OS2 uses bin_count / 2 input samples per coarse group. Exercise the
        // full-rate maximum selected-bin stride and the narrow-capture maximum
        // packet group count, with each stride paired with its valid input hop.
        for (bin_count, input_hop) in [(PFB_BINS as usize, 64usize), (2, 1)] {
            let groups = 16240usize.div_ceil(input_hop);
            let bins: Vec<_> = (0..bin_count as u32).collect();
            let mut channel = FineChannel::new(FineSpec { id:1, epoch:1, first_seq:0,
                bin:bin_count as u32 - 1, offset_hz:10_000.0, decimate,
                coarse_delay_samples:0.0, fused_fir:true }, &bins, COARSE_RATE, input_hop, groups)
                .ok_or("fine cost calibration channel unavailable")?;
            let mut framer = BasebandFramer::new(1, 1, 100e6, COARSE_RATE / decimate as f64)
                .ok_or("fine cost calibration framer unavailable")?;
            let iq = (0..groups * bins.len()).flat_map(|index| {
                let value = (index % 251) as f32;
                [1200.0 + value, 900.0 - value]
            }).collect();
            let mut packet = PfbOutput { seq:0, epoch:1, stamp:0, input_index:0,
                groups:groups as u32, process_ns:0, iq };
            // Each batch covers at least one complete 30 fps frame cycle, including
            // frame allocation, encoding and disposal. Discard the first warmup batch.
            let packets = (COARSE_RATE / 30.0).ceil() as usize / groups + 1;
            let mut samples = [0.0f64; 5];
            for batch in 0..6 {
                let started = Instant::now();
                for _ in 0..packets {
                    let output = channel.process(&packet).map_err(str::to_owned)?;
                    drop(std::hint::black_box(framer.push(&output).map_err(str::to_owned)?));
                    packet.seq += 1;
                    packet.input_index += (groups * input_hop) as u64;
                    packet.stamp = packet.input_index;
                }
                let cost = started.elapsed().as_secs_f64() * 1e9 / (packets * groups) as f64;
                if !cost.is_finite() || cost <= 0.0 { return Err("fine cost calibration invalid".into()); }
                if batch > 0 { samples[batch - 1] = cost; }
            }
            samples.sort_by(f64::total_cmp);
            costs[(decimate - 2) as usize] = costs[(decimate - 2) as usize].max(samples[2]);
        }
    }
    websa_channelizer::FineCostBudget::new(costs, FINE_COST_RESERVE, FINE_COST_LIMIT_NS)
        .ok_or_else(|| "fine cost calibration invalid".into())
}

pub(super) fn fine_cost_profile() -> Result<websa_channelizer::FineCostBudget, String> {
    #[cfg(not(test))]
    {
        static PROFILE: std::sync::LazyLock<Result<websa_channelizer::FineCostBudget, String>> = std::sync::LazyLock::new(calibrate_fine_cost);
        PROFILE.clone()
    }
    #[cfg(test)]
    { Ok(websa_channelizer::FineCostBudget::new([1.0; 23], FINE_COST_RESERVE, FINE_COST_LIMIT_NS).unwrap()) }
}

pub(super) fn check_plan_admission(profile: &websa_channelizer::FineCostBudget, fine: i32,
    count: usize, channel_budget: usize, output_rate_budget: f64) -> Result<(), String> {
    if count > channel_budget { return Err("fine channel budget exceeded".into()); }
    if !(2..=24).contains(&fine) { return Err("fine channel configuration rejected".into()); }
    let rate = COARSE_RATE / fine as f64 * count as f64;
    if !output_rate_budget.is_finite() || output_rate_budget <= 0.0
        || rate > output_rate_budget + output_rate_budget * 1e-12 {
        return Err("fine output rate budget exceeded".into());
    }
    let required = profile.required_ns_per_second(COARSE_RATE, std::iter::repeat_n(fine, count))
        .map_err(str::to_owned)?;
    if required > profile.limit_ns_per_second {
        return Err(format!("fine cost budget exceeded: required {required:.0} ns/s, available {:.0} ns/s (reserve {})",
            profile.limit_ns_per_second, profile.reserve));
    }
    Ok(())
}

#[derive(Clone, Copy, Default, serde::Serialize)]
struct SlowStage {
    seq:u64, stamp:u64, input_index:u64, period_ns:u64, fine_ns:u64, framer_ns:u64,
}

#[derive(Clone, Copy, Default, serde::Serialize)]
pub(super) struct SlowPipeline {
    stage: &'static str,
    seq: u64,
    stamp: u64,
    input_index: u64,
    period_ns: u64,
    elapsed_ns: u64,
}

const SLOW_STAGES: [&str; 4] = ["consumer_gap", "pfb_submit", "fine_handoff", "output_send"];

pub(super) struct PipelineTiming {
    pub submit: Timing,
    pub fine_handoff: Timing,
    pub output_send: Timing,
    pub consumer_gap: Timing,
    pub snapshot_total: Timing,
    pub received: Option<(Instant, u64, u64, u64)>,
    pub slow: [SlowPipeline; 4],
    pub period: Duration,
}
impl PipelineTiming {
    pub fn new(input_rate: f64) -> Self {
        Self { submit:Timing::default(), fine_handoff:Timing::default(), output_send:Timing::default(),
            consumer_gap:Timing::default(), snapshot_total:Timing::default(), received:None,
            slow:[SlowPipeline::default(); 4], period:Duration::from_secs_f64(16240.0/input_rate) }
    }
    pub fn observe_slow(&mut self, stage: &'static str, seq: u64, stamp: u64, input_index: u64, elapsed: Duration) {
        let Some(slot) = SLOW_STAGES.iter().position(|&name| name == stage) else { return };
        if elapsed > self.period * 4 {
            self.slow[slot] = SlowPipeline { stage, seq, stamp, input_index,
                period_ns:self.period.as_nanos().min(u64::MAX as u128) as u64,
                elapsed_ns:elapsed.as_nanos().min(u64::MAX as u128) as u64 };
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(super) struct ChannelRegistryEntry {
    pub channel: u32,
    pub channel_id: u32,
    pub epoch: u32,
    pub frequency_hz: f64,
    pub output_rate: f64,
    pub input_rate: f64,
    pub group_delay_samples: f64,
    pub state: &'static str,
}
/// Translate kernel IDs in place so sampling does not allocate another vector.
/// An unknown ID is a pipeline failure, never a product slot or an encode panic.
fn map_clocks(
    mut clocks: Vec<(u64, u64, u64)>,
    mut slot: impl FnMut(u64) -> Option<u32>,
) -> Result<Vec<(u64, u64, u64)>, String> {
    for (id, _, _) in &mut clocks {
        *id = u64::from(slot(*id).ok_or_else(|| format!("unknown fine channel ID {id}"))?);
    }
    Ok(clocks)
}


pub(super) struct FineState {
    pub bank: FineBank,
    pub framer: BasebandFramer,
    pub aux: BTreeMap<u32, (u64, f64, BasebandFramer)>,
    pub channel_id: u64,
    pub next_id: u64,
    fine_timing: Timing,
    framer_timing: Timing,
    /// Kernel counters mapped to product slots at the last worker or control boundary.
    pub clocks: Arc<[(u64, u64, u64)]>,
    pub registry: Arc<[ChannelRegistryEntry]>,
    period: Duration,
    slow: SlowStage,
    output_send: Timing,
    snapshot_publish: Timing,
    slow_io: SlowPipeline,
}
// The vendor handles move exclusively to one consumer and return only after join.
unsafe impl Send for FineState {}
impl FineState {
    pub fn new(bank: FineBank, framer: BasebandFramer, input_rate: f64, epoch: u32, frequency_hz: f64) -> Self {
        let (output_rate, group_delay_samples) = bank.channel_metadata(1).expect("primary channel configured");
        let registry = Arc::from([ChannelRegistryEntry { channel:1, channel_id:1, epoch,
            frequency_hz, output_rate, input_rate, group_delay_samples, state:"active" }]);
        Self::new_service(bank, framer, input_rate, registry)
    }
    pub fn new_service(bank: FineBank, framer: BasebandFramer, input_rate: f64,
        registry: Arc<[ChannelRegistryEntry]>) -> Self {
        Self { bank, framer, aux: BTreeMap::new(), channel_id: 1, next_id: 1, registry,
            fine_timing: Timing::default(), framer_timing: Timing::default(),
            clocks:Arc::from([]),
            output_send:Timing::default(), snapshot_publish:Timing::default(), slow_io:SlowPipeline::default(),
            period:Duration::from_secs_f64(16240.0/input_rate), slow:SlowStage::default() }
    }
    pub fn deliver(&mut self, packet: PfbOutput) -> Result<Vec<Vec<u8>>, String> {
        let mut frames = Vec::new();
        let started = Instant::now();
        let mut framing = Duration::ZERO;
        let mut unknown_id = None;
        self.bank.deliver(&packet, |fine| {
            let started = Instant::now();
            if fine.channel_id == self.channel_id {
                frames.extend(self.framer.push(fine)?);
            } else if let Some((_, (_, _, framer))) = self.aux.iter_mut().find(|(_, (id, _, _))| *id == fine.channel_id) {
                frames.extend(framer.push(fine)?);
            } else {
                unknown_id = Some(fine.channel_id);
                return Err("unknown fine channel ID");
            }
            let elapsed = started.elapsed();
            framing += elapsed;
            self.framer_timing.observe(elapsed);
            Ok(())
        }).map_err(|error| match unknown_id {
            Some(id) => format!("fine seq={} unknown fine channel ID {id}", packet.seq),
            None => format!("fine seq={} {error}", packet.seq),
        })?;
        let elapsed = started.elapsed();
        let fine = elapsed.saturating_sub(framing);
        self.fine_timing.observe(fine);
        if elapsed > self.period * 4 {
            self.slow = SlowStage { seq:packet.seq, stamp:packet.stamp, input_index:packet.input_index,
                period_ns:self.period.as_nanos() as u64, fine_ns:fine.as_nanos() as u64,
                framer_ns:framing.as_nanos() as u64 };
        }
        Ok(frames)
    }
}

#[derive(Clone, Default)]
struct FineMetrics {
    fine: TimingSnapshot,
    framer: TimingSnapshot,
    output_send: TimingSnapshot,
    snapshot_publish: TimingSnapshot,
    slow: SlowStage,
    slow_io: SlowPipeline,
    packets: u64,
    clocks: Arc<[(u64, u64, u64)]>,
}

impl FineState {
    fn metrics(&mut self) -> Result<FineMetrics, String> {
        self.sample_clocks()?;
        Ok(self.metrics_ref())
    }
    fn metrics_ref(&self) -> FineMetrics {
        FineMetrics { fine:self.fine_timing.snapshot(), framer:self.framer_timing.snapshot(),
            output_send:self.output_send.snapshot(), snapshot_publish:self.snapshot_publish.snapshot(),
            slow:self.slow, slow_io:self.slow_io,
            packets:self.bank.next_sequence(), clocks:self.clocks.clone() }
    }
    fn sample_clocks(&mut self) -> Result<(), String> {
        let primary = self.channel_id;
        let aux = &self.aux;
        let clocks = self.bank.clocks().map_err(|error| format!("fine clocks: {error}"))?;
        self.clocks = map_clocks(clocks, |id| if id == primary { Some(1) } else {
            aux.iter().find(|(_, (channel_id, _, _))| *channel_id == id)
                .map(|(&slot, _)| slot)
        })?.into();
        Ok(())
    }
    fn observe_io(&mut self, stage: &'static str, seq:u64, stamp:u64, input_index:u64, elapsed:Duration) {
        if elapsed > self.period * 4 {
            self.slow_io = SlowPipeline { stage, seq, stamp, input_index,
                period_ns:self.period.as_nanos().min(u64::MAX as u128) as u64,
                elapsed_ns:elapsed.as_nanos().min(u64::MAX as u128) as u64 };
        }
    }
}

pub(super) struct FineWorker {
    tx: Option<SyncSender<PfbOutput>>,
    rx: Receiver<Result<Vec<Vec<u8>>, String>>,
    join: Option<JoinHandle<FineState>>,
    timing: Arc<PLMutex<FineMetrics>>,
    /// Caller-side time blocked because the fine input queue is full.
    full_wait: Timing,
    /// Caller-side time spent collecting worker results.
    collect_timing: Timing,
}
impl FineWorker {
    fn start(mut state: FineState) -> Result<Self, String> {
        let (tx, input) = sync::sync_channel::<PfbOutput>(2);
        let (output, rx) = sync::sync_channel(2);
        let timing = Arc::new(PLMutex::new(state.metrics()?));
        let published = timing.clone();
        let join = thread::Builder::new().name("sdr-fine-framer".into()).spawn(move || {
            let mut refreshed = Instant::now();
            while let Ok(packet) = input.recv() {
                let (seq, stamp, input_index) = (packet.seq, packet.stamp, packet.input_index);
                let mut result = state.deliver(packet);
                if refreshed.elapsed() >= Duration::from_millis(50) {
                    let started = Instant::now();
                    match state.metrics() {
                        Ok(snapshot) => *published.lock() = snapshot,
                        Err(error) => if result.is_ok() { result = Err(error); },
                    }
                    let elapsed = started.elapsed();
                    state.observe_io("fine_snapshot_publish", seq, stamp, input_index, elapsed);
                    refreshed = Instant::now();
                }
                let failed = result.is_err();
                let has_output = result.as_ref().is_ok_and(|frames| !frames.is_empty());
                if failed || has_output {
                    let started = Instant::now();
                    let sent = output.send(result);
                    if has_output {
                        let elapsed = started.elapsed();
                        state.output_send.observe(elapsed);
                        state.observe_io("fine_output_send", seq, stamp, input_index, elapsed);
                    }
                    if sent.is_err() || failed { break; }
                }
            }
            state
        }).map_err(|e| e.to_string())?;
        Ok(Self { tx: Some(tx), rx, join: Some(join), timing, full_wait:Timing::default(),
            collect_timing:Timing::default() })
    }
    pub fn collect(&mut self, frames: &mut Vec<Vec<u8>>) -> Result<(), String> {
        let started = Instant::now();
        let result = (|| {
            while let Ok(result) = self.rx.try_recv() { frames.extend(result?); }
            Ok(())
        })();
        self.collect_timing.observe(started.elapsed());
        result
    }
    pub fn submit(&mut self, mut packet: PfbOutput, frames: &mut Vec<Vec<u8>>) -> Result<(), String> {
        let mut blocked: Option<Instant> = None;
        loop {
            self.collect(frames)?;
            match self.tx.as_ref().unwrap().try_send(packet) {
                Ok(()) => {
                    if let Some(started) = blocked { self.full_wait.observe(started.elapsed()); }
                    return Ok(());
                }
                Err(TrySendError::Full(value)) => {
                    packet = value;
                    blocked.get_or_insert_with(Instant::now);
                    thread::yield_now();
                }
                Err(TrySendError::Disconnected(_)) => return Err("fine consumer disconnected".into()),
            }
        }
    }
    fn full_wait(&self) -> TimingSnapshot { self.full_wait.snapshot() }
    fn collect_snapshot(&self) -> TimingSnapshot { self.collect_timing.snapshot() }
    fn finish(mut self, frames: &mut Vec<Vec<u8>>) -> Result<FineState, String> {
        self.tx.take();
        let mut failure = None;
        while let Ok(result) = self.rx.recv() {
            match result { Ok(output) => frames.extend(output), Err(e) => failure = Some(e) }
        }
        let state = self.join.take().unwrap().join().map_err(|_| "fine consumer panicked")?;
        if let Some(error) = failure { return Err(error); }
        Ok(state)
    }
}
impl Drop for FineWorker {
    fn drop(&mut self) {
        self.tx.take();
        while self.rx.recv().is_ok() {}
        if let Some(join) = self.join.take() { let _ = join.join(); }
    }
}


pub(super) struct PipelineConfig {
    pub center: f64, pub listen: f64, pub fine: i32, pub display_decimate: i32,
    pub front_end: FrontEnd, pub shim: String, pub taps: usize,
    pub aux: Vec<(u32, f64)>, pub channel_budget: usize, pub output_rate_budget: f64,
}
#[derive(Clone)]
pub(super) struct Snapshot {
    pub capture_center: f64, pub usable_bandwidth: f64, pub rate: f64,
    pub generation: u64,
    pub taps: usize, pub device_decimate: u64, pub aux: Arc<[(u32, f64)]>, pub actual: Value,
    front_end: Option<hardware_swp::FrontEndActual>,
    pub registry: Arc<[ChannelRegistryEntry]>,
    pub selected_bins: usize,
    pool: websa_channelizer::PoolStats,
    pool_timing: websa_channelizer::PoolTimingSnapshot,
    input_samples: u64,
    stamp_gaps: u64,
    fine: TimingSnapshot,
    framer: TimingSnapshot,
    clocks: Arc<[(u64, u64, u64)]>,
    fine_packets: u64, slow: SlowStage,
    pfb_submit: TimingSnapshot,
    fine_handoff: TimingSnapshot,
    output_send: TimingSnapshot,
    slow_pipeline: [SlowPipeline; 4],
    consumer_gap: TimingSnapshot,
    snapshot_total: TimingSnapshot,
    fine_output_send: TimingSnapshot,
    fine_snapshot_publish: TimingSnapshot,
    fine_full_wait: TimingSnapshot,
    fine_collect: TimingSnapshot,
    slow_fine_io: SlowPipeline,
}
impl Snapshot {
    /// Kernel clocks rendered against this snapshot's own registry.
    pub fn channel_clocks(&self) -> Result<Value, String> {
        self.clocks.iter().map(|&(channel, input_samples, output_samples)| {
            let entry = self.registry.iter().find(|entry| u64::from(entry.channel) == channel)
                .ok_or_else(|| format!("fine clock product slot {channel} missing from registry"))?;
            Ok(json!({ "channel":channel,"channel_id":entry.channel_id,"epoch":entry.epoch,
                "input_samples":input_samples,"output_samples":output_samples }))
        }).collect::<Result<Vec<_>, String>>().map(Value::Array)
    }
    /// Keep clocks and the producer input watermark from the same publication.
    pub fn write_channel_status(&self, actual: &mut Value) {
        actual["input_samples"] = json!(self.input_samples);
        actual["channel_registry"] = json!(&*self.registry);
        match self.channel_clocks() {
            Ok(clocks) => {
                actual["channel_clocks"] = clocks;
                if let Some(object) = actual.as_object_mut() { object.remove("channel_clock_error"); }
            }
            Err(error) => {
                actual["channel_clocks"] = json!([]);
                actual["channel_clock_error"] = json!(error);
            }
        }
    }
}
#[derive(Default)]
pub(super) struct PipelineOutput {
    pub frames: Vec<Vec<u8>>, pub pan: Vec<(Vec<f64>, Vec<f32>, i32, u64, u64)>,
}
impl PipelineOutput {
    fn collect(mut self, rx: &Receiver<Result<Self, String>>, failure: &mut Option<String>) -> Result<Self, String> {
        if failure.is_none() {
            for _ in 0..2 {
                match rx.try_recv() {
                    Ok(Ok(output)) => { self.frames.extend(output.frames); self.pan.extend(output.pan); }
                    Ok(Err(error)) => { *failure = Some(error); break; }
                    Err(sync::TryRecvError::Empty) => break,
                    Err(sync::TryRecvError::Disconnected) => {
                        *failure = Some("pipeline output disconnected".into());
                        break;
                    }
                }
            }
        }
        // Deliver every collected tail before reporting the terminal producer failure.
        if self.frames.is_empty() && self.pan.is_empty() {
            if let Some(error) = failure { return Err(error.clone()); }
        }
        Ok(self)
    }
    fn capture_boundary(&mut self, previous: (f64, u64, u64), next: &Snapshot) {
        if previous != (next.capture_center, next.device_decimate, next.generation) {
            // IQ tails retain their order; spectra from the old capture configuration expire.
            self.pan.clear();
        }
    }
}
type Change = Box<dyn FnOnce(&mut Ddc) -> Result<Vec<Vec<u8>>, String> + Send>;
struct Command {
    change: Option<Change>,
    reply: SyncSender<Result<(Snapshot, PipelineOutput), (String, PipelineOutput)>>,
    resume: Receiver<()>,
}
pub(super) struct Pipeline {
    tx: SyncSender<Command>,
    rx: Receiver<Result<PipelineOutput, String>>,
    shared: Arc<PLMutex<Snapshot>>,
    pub snapshot: Snapshot,
    pub device_info: Option<hardware_swp::DeviceInfo>,
    gnss: hardware_iqs::CaptureGnss,
    join: Option<JoinHandle<()>>,
    diagnostics: hardware_iqs::CaptureDiagnostics,
    terminal: Arc<PLMutex<Option<Value>>>,
    refreshed: Instant,
    pending_pan: Vec<(Vec<f64>, Vec<f32>, i32, u64, u64)>,
    pending_frames: Vec<Vec<u8>>,
    failure: Option<String>,
}
impl Ddc {
    pub(super) fn finish_framers(&mut self, frames: &mut Vec<Vec<u8>>) -> Result<(), String> {
        if self.service_configs.as_ref().is_none_or(|configs| configs[0].enabled) {
            frames.extend(self.framer.finish().map_err(str::to_owned)?);
        }
        for (_, _, framer) in self.aux.values_mut() {
            frames.extend(framer.finish().map_err(str::to_owned)?);
        }
        Ok(())
    }
    fn suspend_fine(&mut self) -> Result<Vec<Vec<u8>>, String> {
        self.stream.flush_pending().map_err(str::to_owned)?;
        let mut frames = self.drain()?;
        if let Some(worker) = self.fine_worker.take() { self.fine = Some(worker.finish(&mut frames)?); }
        Ok(frames)
    }
    fn resume_fine(&mut self) -> Result<(), String> {
        self.fine_worker = Some(FineWorker::start(self.fine.take().ok_or("fine stage absent")?)?);
        Ok(())
    }
    pub(super) fn channel_registry(&self) -> Arc<[ChannelRegistryEntry]> {
        self.registry.clone()
    }
    pub(super) fn refresh_registry(&mut self) {
        let epoch = u32::try_from(self.stream.epoch()).expect("validated DSP epoch");
        let input_rate = self.stream.input_sample_rate();
        let mut registry = Vec::with_capacity(1 + self.aux.len());
        for (channel, id, frequency_hz) in std::iter::once((1, self.channel_id, self.listen))
            .chain(self.aux.iter().map(|(&slot, &(id, frequency, _))| (slot, id, frequency))) {
            let (output_rate, group_delay_samples) = self.bank.channel_metadata(id).expect("active channel configured");
            registry.push(ChannelRegistryEntry { channel, channel_id:u32::try_from(id).expect("validated channel ID"),
                epoch, frequency_hz, output_rate, input_rate, group_delay_samples, state:"active" });
        }
        self.registry = registry.into();
    }
    fn pipeline_snapshot(&self, aux: Arc<[(u32, f64)]>) -> Snapshot {
        let metrics = if let Some(fine) = &self.fine { fine.metrics_ref() }
            else if let Some(worker) = &self.fine_worker { worker.timing.lock().clone() }
            else { FineMetrics::default() };
        Snapshot { capture_center:self.capture_center, usable_bandwidth:self.usable_bandwidth,
            generation:self.generation,
            rate:self.channel_rate(), taps:self.stream.taps_per_phase(), device_decimate:self.device_decimate,
            aux, actual:Value::Null, pool:self.stream.stats(), input_samples:self.input_pos,
            front_end:self.hardware.as_ref().and_then(|capture| capture.diagnostics().front_end()),
            registry:self.registry.clone(),
            selected_bins:self.stream.bins().len(),
            pool_timing:self.stream.timings(),
            stamp_gaps:self.stamp_gaps, fine:metrics.fine, framer:metrics.framer, clocks:metrics.clocks,
            fine_packets:metrics.packets, slow:metrics.slow,
            consumer_gap:self.pipeline_timing.consumer_gap.snapshot(),
            snapshot_total:self.pipeline_timing.snapshot_total.snapshot(),
            fine_output_send:metrics.output_send, fine_snapshot_publish:metrics.snapshot_publish,
            fine_full_wait:self.fine_worker.as_ref().map_or_else(TimingSnapshot::default, FineWorker::full_wait),
            fine_collect:self.fine_worker.as_ref().map_or_else(TimingSnapshot::default, FineWorker::collect_snapshot),
            slow_fine_io:metrics.slow_io,
            pfb_submit:self.pipeline_timing.submit.snapshot(),
            fine_handoff:self.pipeline_timing.fine_handoff.snapshot(),
            output_send:self.pipeline_timing.output_send.snapshot(),
            slow_pipeline:self.pipeline_timing.slow }
    }
}
impl Pipeline {
    pub fn presentation(&self) -> PipelinePresentation {
        PipelinePresentation { snapshot:self.snapshot.clone(), diagnostics:self.diagnostics.clone() }
    }
    pub fn gnss(&self) -> Result<hardware_swp::GnssInfo, String> {
        self.gnss.gnss()
    }
    pub fn clock(&self) -> (Option<f64>, Option<i32>) {
        self.diagnostics.clock()
    }
    pub fn front_end(&self) -> Option<hardware_swp::FrontEndActual> {
        self.diagnostics.front_end()
    }
    pub fn warning(&self) -> i32 {
        self.diagnostics.warning()
    }
    pub fn packet_observation(&self) -> (u64, i32) {
        self.diagnostics.packet_observation()
    }
    pub fn failure_diagnostics(&self) -> Value {
        self.terminal.lock().clone().unwrap_or_else(|| {
            PipelinePresentation { snapshot:self.shared.lock().clone(), diagnostics:self.diagnostics.clone() }.encode()
        })
    }
    pub fn encode_snapshot(&mut self) {
        self.snapshot.actual = self.presentation().encode();
    }
}
pub(super) struct PipelinePresentation {
    pub snapshot: Snapshot,
    diagnostics: hardware_iqs::CaptureDiagnostics,
}
impl PipelinePresentation {
    pub fn encode(&self) -> Value {
        let snapshot = &self.snapshot;
        let pool = snapshot.pool;
        let s = self.diagnostics.stats();
        let (refclk, refclk_src) = self.diagnostics.clock();
        let front_end = snapshot.front_end;
        let mut actual = json!({
            "refclk":refclk, "refclk_src":refclk_src,
            "ref":front_end.map(|f| f.ref_dbm), "atten":front_end.map(|f| f.atten),
            "preamp":front_end.map(|f| f.preamp), "ifgain":front_end.map(|f| f.ifgain),
            "iqs_stamp_gaps":snapshot.stamp_gaps,
            "fine":snapshot.fine,"framer":snapshot.framer,
            "last_slow_fine":snapshot.slow,
            "pfb_submit":snapshot.pfb_submit,"fine_handoff":snapshot.fine_handoff,
            "pipeline_output_send":snapshot.output_send,"last_slow_pipeline":snapshot.slow_pipeline,
            "pfb_submitted":pool.submitted,"pfb_completed":pool.completed,"pfb_groups":pool.groups,
            "fine_packets":snapshot.fine_packets,"fine_next_seq":snapshot.fine_packets,
            "pool_clone_ns":pool.clone_ns,"pool_history_ns":pool.advance_ns,
            "pool_task_wait_ns":pool.task_wait_ns,"pool_result_wait_ns":pool.result_wait_ns,
            "pool_timing":snapshot.pool_timing,
            "iqs_events":self.diagnostics.events(),
        });
        actual["consumer_gap"] = json!(snapshot.consumer_gap);
        snapshot.write_channel_status(&mut actual);
        actual["producer_snapshot_total"] = json!(snapshot.snapshot_total);
        actual["fine_output_send"] = json!(snapshot.fine_output_send);
        actual["fine_full_wait"] = json!(snapshot.fine_full_wait);
        actual["fine_collect"] = json!(snapshot.fine_collect);
        actual["pool_admission_wait"] = json!(snapshot.pool_timing.admission_wait);
        actual["fine_snapshot_publish"] = json!(snapshot.fine_snapshot_publish);
        actual["last_slow_fine_io"] = json!(snapshot.slow_fine_io);
        actual["iqs_capture"] = json!({
                "packets":s.packets,"host_seconds":s.host_seconds,"device_seconds":s.device_seconds,
                "warning":s.warning,
                "host_minus_device_ms":(s.host_seconds-s.device_seconds)*1e3,
                "fetch_max_ms":s.fetch_max_ms,"fail_calls":s.fail_calls,"fail_max_ms":s.fail_max_ms,
                "recovery_stall_ms":s.recovery_stall_ms,"queue_full":s.queue_full,
                "queue_wait_ms":s.queue_wait_ms,"queue_wait_max_ms":s.queue_wait_max_ms,
                "spectrum_frames":s.spectrum_frames,"skips":s.skips,"last_skip_status":s.last_skip_status,"recoveries":s.recoveries,
                "sdk_timing_available":s.sdk_timing_available,"sdk_entry_interval":s.sdk_entry_interval,
                "sdk_fetch":s.sdk_fetch,"sdk_memcpy":s.sdk_memcpy,"sdk_fft":s.sdk_fft,
                "sdk_failed_fetch":s.sdk_failed_fetch,"sdk_failed_fft":s.sdk_failed_fft,
                "fetch":s.fetch,"queue_wait":s.queue_wait,"queue_residence":s.queue_residence,
                "queue_serve":s.queue_serve,"control_serve":s.control_serve,
                "pfb_submitted":pool.submitted,"pfb_completed":pool.completed,
                "pfb_max_pending":pool.max_pending,"pfb_max_ready":pool.max_ready,
                "task_capacity":pool.task_capacity,"result_capacity":pool.result_capacity,
                "pool_clone_ns":pool.clone_ns,"pool_history_ns":pool.advance_ns,
                "pool_task_wait_ns":pool.task_wait_ns,"pool_result_wait_ns":pool.result_wait_ns,
        });
        actual
    }
}
impl Pipeline {
    pub fn start(config: PipelineConfig) -> Result<Self, String> {
        Self::start_inner(config, None)
    }
    pub fn start_service(config: PipelineConfig, channels: Vec<receiver_workers::ChannelConfig>) -> Result<Self,String> {
        Self::start_inner(config, Some(channels))
    }
    fn start_inner(config: PipelineConfig, channels: Option<Vec<receiver_workers::ChannelConfig>>) -> Result<Self, String> {
        if channels.is_none() {
            let profile = fine_cost_profile()?;
            check_plan_admission(&profile, config.fine, config.aux.len() + 1,
                config.channel_budget, config.output_rate_budget)?;
        }
        let (tx, commands) = sync::sync_channel::<Command>(1);
        let (output, rx) = sync::sync_channel(2);
        let (ready, init) = sync::sync_channel(1);
        let terminal = Arc::new(PLMutex::new(None));
        let failed_capture = terminal.clone();
        let join = thread::Builder::new().name("sdr-pfb".into()).spawn(move || {
            let setup = (|| {
                let frequencies: Vec<_> = config.aux.iter().map(|(_, f)| *f).collect();
                let device_decimate = capture_decimate(config.center, config.listen, &frequencies, config.display_decimate, config.taps);
                if let Some(channels)=&channels {
                    let fs=IQ_FS/device_decimate as f64;
                    let m=PFB_BINS.min((fs/BIN_SPACING).round() as i64).max(2);
                    let mut bins=Vec::new();
                    for c in channels.iter().filter(|c|c.enabled) {
                        let signed=signed_bin(c.freq,config.center);
                        for neighbour in signed-PLAN_NEIGHBOURS..=signed+PLAN_NEIGHBOURS {
                            if (-m/2..m/2).contains(&neighbour) {
                                let bin=neighbour.rem_euclid(m) as u32;
                                if !bins.contains(&bin){bins.push(bin);}
                            }
                        }
                    }
                    service_cost::admit_service(channels,fs,config.taps,bins.len().max(1))?;
                }
                let capture = HardwareCapture::start(config.shim, config.center, config.front_end, device_decimate)?;
                let g = capture.geometry;
                let fs = ((g.bandwidth / 0.8 / COARSE_RATE).round() * COARSE_RATE).max(COARSE_RATE);
                let ddc = if let Some(channels) = &channels {
                    Ddc::configured_service(g.center,g.bandwidth,&mut Some(capture),config.taps,fs,channels)?
                } else {
                    let mut ddc = Ddc::configured_with_budget(g.center, config.listen, config.fine, g.bandwidth,
                        &mut Some(capture), &frequencies, config.taps, fs, config.channel_budget, config.output_rate_budget)?;
                    for &(channel, frequency) in &config.aux { ddc.configure_aux(channel, true, frequency)?; }
                    ddc
                };
                Ok::<_, String>(ddc)
            })();
            let mut ddc = match setup { Ok(ddc) => ddc, Err(e) => { let _ = ready.send(Err(e)); return; } };
            let diagnostics = ddc.hardware.as_ref().unwrap().diagnostics();
            let device_info = ddc.hardware.as_ref().and_then(|capture| capture.device_info);
            let gnss = ddc.hardware.as_ref().unwrap().gnss_reader();
            let mut aux: Arc<[(u32, f64)]> = config.aux.into();
            if let Err(e) = ddc.resume_fine() { let _ = ready.send(Err(e)); return; }
            let shared = Arc::new(PLMutex::new(ddc.pipeline_snapshot(aux.clone())));
            if let Err(e) = ddc.hardware.as_mut().unwrap().arm() {
                let _ = ready.send(Err(e));
                return;
            }
            if ready.send(Ok((shared.clone(), diagnostics.clone(), device_info, gnss))).is_err() { return; }
            let mut refreshed = Instant::now();
            loop {
                match commands.try_recv() {
                    Ok(command) => {
                        let stop = command.change.is_none();
                        let mut frames = match ddc.suspend_fine() { Ok(frames) => frames, Err(e) => {
                            let _ = command.reply.send(Err((e, PipelineOutput::default())));
                            let _ = command.resume.recv();
                            break;
                        }};
                        let changed = match command.change {
                            Some(change) => change(&mut ddc).map(|tail| frames.extend(tail)),
                            None => ddc.finish_framers(&mut frames),
                        };
                        let clocks = ddc.sample_clocks();
                        aux = ddc.aux.iter().map(|(&ch, (_, f, _))| (ch, *f)).collect::<Vec<_>>().into();
                        let snapshot = ddc.pipeline_snapshot(aux.clone());
                        *shared.lock() = snapshot.clone();
                        let tail = PipelineOutput { frames, pan:std::mem::take(&mut ddc.pan) };
                        let reply = match changed.and(clocks) {
                            Ok(()) => Ok((snapshot, tail)),
                            Err(error) => Err((error, tail)),
                        };
                        let failed = ddc.capture_failed || ddc.fine.is_none();
                        let _ = command.reply.send(reply);
                        if command.resume.recv().is_err() { break; }
                        if stop || failed { break; }
                        if let Err(e) = ddc.resume_fine() { let _ = output.send(Err(e)); break; }
                    }
                    Err(sync::TryRecvError::Disconnected) => break,
                    Err(sync::TryRecvError::Empty) => (),
                }
                let result = ddc.packet().map(|mut frames| {
                    frames.splice(0..0, std::mem::take(&mut ddc.carry));
                    PipelineOutput { frames, pan:std::mem::take(&mut ddc.pan) }
                });
                let failed = result.is_err();
                if let Err(error) = &result {
                    let snapshot = ddc.pipeline_snapshot(aux.clone());
                    *shared.lock() = snapshot.clone();
                    let actual = PipelinePresentation { snapshot, diagnostics:diagnostics.clone() }.encode();
                    *failed_capture.lock() = Some(json!({
                        "error":error, "next_seq":ddc.next_seq, "last_stamp":ddc.last_stamp,
                        "input_index":ddc.input_pos, "capture_center":ddc.capture_center,
                        "device_decimate":ddc.device_decimate, "actual":actual,
                    }));
                }
                let has_output = result.as_ref().is_ok_and(|o| !o.frames.is_empty() || !o.pan.is_empty());
                let batch_frames = result.as_ref().map_or(0, |o| o.frames.len());
                if failed || has_output {
                    let started = Instant::now();
                    let sent = output.send(result);
                    if sent.is_ok() && has_output {
                        let completed_at = Instant::now();
                        let elapsed = completed_at.duration_since(started);
                        ddc.pipeline_timing.output_send.observe(elapsed);
                        let seq = ddc.next_seq.saturating_sub(1);
                        let stamp = ddc.last_stamp.unwrap_or(ddc.input_pos);
                        let input_index = ddc.input_pos.saturating_sub(16240);
                        ddc.pipeline_timing.observe_slow("output_send", seq, stamp, input_index, elapsed);
                        if let Some(capture) = &ddc.hardware {
                            capture.note_output(seq, input_index, batch_frames, completed_at);
                        }
                    }
                    if sent.is_err() { break; }
                }
                if failed { break; }
                if refreshed.elapsed() >= Duration::from_millis(50) {
                    let started = Instant::now();
                    let snapshot = ddc.pipeline_snapshot(aux.clone());
                    *shared.lock() = snapshot;
                    let elapsed = started.elapsed();
                    ddc.pipeline_timing.snapshot_total.observe(elapsed);
                    let (seq, stamp, input_index) = (ddc.next_seq.saturating_sub(1), ddc.last_stamp.unwrap_or(ddc.input_pos), ddc.input_pos.saturating_sub(16240));
                    ddc.pipeline_timing.observe_slow("producer_snapshot", seq, stamp, input_index, elapsed);
                    refreshed = Instant::now();
                }
            }
        }).map_err(|e| e.to_string())?;
        match init.recv().map_err(|_| "pipeline setup disconnected")? {
            Ok((shared, diagnostics, device_info, gnss)) => {
                let snapshot = shared.lock().clone();
                Ok(Self { tx, rx, shared, snapshot, device_info, gnss, diagnostics, terminal, refreshed:Instant::now(), pending_pan:Vec::new(), pending_frames:Vec::new(), failure:None, join:Some(join) })
            }
            Err(e) => { let _ = join.join(); Err(e) }
        }
    }
    pub fn poll(&mut self) -> Result<PipelineOutput, String> {
        let result = PipelineOutput {
            pan:std::mem::take(&mut self.pending_pan), frames:std::mem::take(&mut self.pending_frames),
        }.collect(&self.rx, &mut self.failure)?;
        if self.refreshed.elapsed() >= Duration::from_millis(50) {
            self.snapshot = self.shared.lock().clone();
            self.refreshed = Instant::now();
        }
        Ok(result)
    }
    fn boundary(&mut self, change: Option<Change>) -> Result<PipelineOutput, String> {
        let mut output = PipelineOutput::default();
        if let Err(error) = self.run_boundary(change, &mut output) {
            self.pending_frames.extend(output.frames);
            self.pending_pan.extend(output.pan);
            return Err(error);
        }
        Ok(output)
    }
    fn run_boundary(&mut self, change: Option<Change>, output: &mut PipelineOutput) -> Result<(), String> {
        let previous_capture = (self.snapshot.capture_center, self.snapshot.device_decimate, self.snapshot.generation);
        let (reply, rx) = sync::sync_channel(1);
        let (resume, resumed) = sync::sync_channel(0);
        let mut command = Command { change, reply, resume:resumed };
        loop {
            match self.tx.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return Err("pipeline disconnected".into()),
                Err(TrySendError::Full(value)) => command = value,
            }
            let queued = self.poll()?;
            output.frames.extend(queued.frames); output.pan.extend(queued.pan);
            thread::yield_now();
        }
        loop {
            let queued = self.poll()?;
            output.frames.extend(queued.frames); output.pan.extend(queued.pan);
            match rx.recv_timeout(Duration::from_millis(1)) {
                Ok(result) => {
                    // Everything already emitted precedes the boundary tail, even if it
                    // arrived between the last poll and the command acknowledgement.
                    while let Ok(queued) = self.rx.try_recv() {
                        let queued = queued?;
                        output.frames.extend(queued.frames); output.pan.extend(queued.pan);
                    }
                    let (snapshot, tail) = match result {
                        Ok(value) => value,
                        Err((error, tail)) => {
                            output.frames.extend(tail.frames); output.pan.extend(tail.pan);
                            let _ = resume.send(());
                            return Err(error);
                        }
                    };
                    output.frames.extend(tail.frames); output.pan.extend(tail.pan);
                    output.capture_boundary(previous_capture, &snapshot);
                    self.snapshot = snapshot;
                    let _ = resume.send(());
                    return Ok(());
                }
                Err(sync::RecvTimeoutError::Timeout) => (),
                Err(_) => return Err("pipeline command disconnected".into()),
            }
        }
    }
    pub fn command(&mut self, change: impl FnOnce(&mut Ddc) -> Result<Vec<Vec<u8>>, String> + Send + 'static) -> Result<Vec<Vec<u8>>, String> {
        let output = self.boundary(Some(Box::new(change)))?;
        self.pending_pan.extend(output.pan);
        Ok(output.frames)
    }
    pub fn stop(&mut self) -> Result<PipelineOutput, String> {
        if self.join.is_none() { return Ok(PipelineOutput::default()); }
        let mut result = self.boundary(None);
        while let Ok(queued) = self.rx.recv() {
            match (&mut result, queued) {
                (Ok(output), Ok(queued)) => { output.frames.extend(queued.frames); output.pan.extend(queued.pan); }
                (_, Err(error)) => result = Err(error),
                _ => (),
            }
        }
        let joined = self.join.take().unwrap().join().map_err(|_| "pipeline panicked".to_string());
        joined?;
        result
    }
}
impl Drop for Pipeline { fn drop(&mut self) { let _ = self.stop(); } }

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Ddc {
        Ddc::configured(100e6, 100e6, 3, 50e6, &mut None, &[], 16).unwrap()
    }

    #[test]
    fn presentation_keeps_snapshot_gain_after_capture_reconfigure() {
        let _guard = hardware_iqs::tests::TEST_IQS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for path in ["/tmp/websa_fake_iqs_fail.txt", "/tmp/websa_fake_iqs_open_fail.txt", "/tmp/websa_fake_iqs_reconfig_fail.txt"] {
            let _ = std::fs::remove_file(path);
        }
        let capture = HardwareCapture::start(hardware_iqs::tests::compile_fake_iqs_shim(), 100e6,
            hardware_iqs::tests::FRONT_END, 1).unwrap();
        let diagnostics = capture.diagnostics();
        let mut snapshot = plan().pipeline_snapshot(Arc::from([]));
        snapshot.front_end = capture.geometry.front_end;
        let presentation = PipelinePresentation { snapshot, diagnostics };
        capture.reconfigure(101e6, hardware_iqs::tests::FRONT_END, 1).unwrap();
        let actual = presentation.encode();
        assert_eq!(actual["ref"], -7.5);
        assert_eq!(actual["atten"], 12);
        assert_eq!(actual["preamp"], 1);
        assert_eq!(actual["ifgain"], 2);
        let latest = capture.diagnostics().front_end().unwrap();
        assert_eq!((latest.ref_dbm, latest.atten, latest.preamp, latest.ifgain), (-27.5, 18, 1, 3));
    }

    fn finish(ddc: &mut Ddc, frames: &mut Vec<Vec<u8>>) {
        frames.extend(ddc.suspend_fine().unwrap());
        ddc.finish_framers(frames).unwrap();
    }

    #[test]
    fn disabled_primary_recapture_and_stop_preserve_auxiliary_tail() {
        let mut configs:Vec<_> = (1..=3).map(receiver_workers::ChannelConfig::for_slot).collect();
        configs[0].freq = 100e6;
        configs[1].freq = 100e6 + BIN_SPACING;
        configs[1].enabled = true;
        let mut ddc = Ddc::configured_service(100e6, 50e6, &mut None, 16, IQ_FS, &configs).unwrap();
        for _ in 0..8 { ddc.packet().unwrap(); }
        configs[0].enabled = false;
        let retired = ddc.configure_service_channel(configs[0].clone()).unwrap();
        assert!(retired.iter().any(|frame| frame.starts_with(b"IQBF") && frame.len() > 56));
        let old_aux = ddc.channel_registry()[0].channel_id;
        for _ in 0..8 { ddc.packet().unwrap(); }
        let front_end = DevState::new(json!({"preset_defaults":{}}), json!({})).front_end(0.0);
        assert!(ddc.recapture_service_configs(100e6, 1, front_end, 3, &configs).is_err());
        // The next plan holds one enabled channel plus its five neighbour bins.
        service_cost::admit_service(&configs, IQ_FS, 16, 5).unwrap();
        let tail = ddc.recapture_service_configs(100e6, 1, front_end, 16, &configs).unwrap();
        assert!(tail.iter().any(|frame| frame.starts_with(b"IQAX") && frame.len() > 56
            && u32::from_le_bytes(frame[32..36].try_into().unwrap()) == old_aux));
        assert!(!ddc.capture_failed);
        assert_eq!(ddc.channel_registry().iter().map(|row| row.channel).collect::<Vec<_>>(), vec![2]);
        for _ in 0..8 { ddc.packet().unwrap(); }
        let mut stopped = Vec::new();
        finish(&mut ddc, &mut stopped);
        assert!(stopped.iter().any(|frame| frame.starts_with(b"IQAX") && frame.len() > 56));
        assert!(!stopped.iter().any(|frame| frame.starts_with(b"IQBF")));
    }

    #[test]
    fn complete_candidate_admission_checks_count_rate_and_exact_cost_boundary() {
        let profile = websa_channelizer::FineCostBudget::new([1.0; 23], 2.0, COARSE_RATE * 4.0).unwrap();
        assert_eq!(check_plan_admission(&profile, 3, 2, 1, COARSE_RATE).unwrap_err(), "fine channel budget exceeded");
        assert_eq!(check_plan_admission(&profile, 3, 2, 2, COARSE_RATE / 3.0).unwrap_err(), "fine output rate budget exceeded");
        check_plan_admission(&profile, 3, 2, 2, COARSE_RATE * 2.0 / 3.0).unwrap();
        assert!(check_plan_admission(&profile, 3, 3, 3, COARSE_RATE).unwrap_err().contains("fine cost budget exceeded"));
        let error = Ddc::configured_with_cost_budget(100e6, 100e6, 3, 50e6, &mut None,
            &[100e6 + BIN_SPACING, 100e6 - BIN_SPACING], 16, IQ_FS, 3, COARSE_RATE, profile)
            .err().unwrap();
        assert!(error.contains("fine cost budget exceeded"));
    }

    #[test]
    fn cost_rejections_preserve_plan_and_complete_stream_after_worker_resume() {
        let build = || {
            let mut costs = [1.0; 23];
            costs[0] = 4.0;
            let profile = websa_channelizer::FineCostBudget::new(costs, 2.0, COARSE_RATE * 4.0).unwrap();
            let mut ddc = Ddc::configured_with_cost_budget(100e6, 100e6, 3, 50e6, &mut None,
                &[100e6 + BIN_SPACING], 16, IQ_FS, 4, COARSE_RATE * 4.0, profile).unwrap();
            ddc.configure_aux(2, true, 100e6 + BIN_SPACING).unwrap();
            ddc
        };
        let mut expected = build();
        let mut expected_frames = Vec::new();
        for _ in 0..48 { expected_frames.extend(expected.packet().unwrap()); }
        finish(&mut expected, &mut expected_frames);
        for operation in 0..4 {
            let mut actual = build();
            actual.resume_fine().unwrap();
            let mut frames = Vec::new();
            for _ in 0..24 { frames.extend(actual.packet().unwrap()); }
            frames.extend(actual.suspend_fine().unwrap());
            let registry = actual.channel_registry();
            let clocks = actual.bank.clocks().unwrap();
            let before = (actual.stream.epoch(), actual.channel_id, actual.next_id, actual.next_seq,
                actual.listen, actual.decimate, actual.input_pos);
            let aux = [(2, 100e6 + BIN_SPACING)];
            let error = match operation {
                0 => actual.configure_aux(3, true, 100e6 - BIN_SPACING).unwrap_err(),
                1 => actual.tune(100e6 + 1000.0, 2).unwrap_err(),
                2 => actual.replan_with_aux(100e6, 2, &aux).unwrap_err(),
                _ => {
                    let front_end = DevState::new(json!({"preset_defaults":{}}), json!({})).front_end(0.0);
                    actual.recapture(100e6, 100e6, 2, 32, front_end, 16).unwrap_err()
                }
            };
            assert!(error.contains("fine cost budget exceeded"), "{operation}: {error}");
            assert_eq!((actual.stream.epoch(), actual.channel_id, actual.next_id, actual.next_seq,
                actual.listen, actual.decimate, actual.input_pos), before);
            assert!(Arc::ptr_eq(&registry, &actual.channel_registry()));
            assert_eq!(actual.bank.clocks().unwrap(), clocks);
            assert!(!actual.capture_failed);
            actual.resume_fine().unwrap();
            for _ in 0..24 { frames.extend(actual.packet().unwrap()); }
            finish(&mut actual, &mut frames);
            assert_eq!(frames.len(), expected_frames.len());
            for (frame, control) in frames.iter().zip(&expected_frames) {
                assert_eq!(&frame[..36], &control[..36]);
                assert_eq!(&frame[40..], &control[40..]);
                assert_eq!(u32::from_le_bytes(frame[36..40].try_into().unwrap()) as u64, before.0);
            }
            assert_eq!(actual.bank.clocks().unwrap(), expected.bank.clocks().unwrap());
        }
    }

    #[test]
    fn producer_disconnect_delivers_queued_tail_before_terminal_error() {
        let (tx, rx) = sync::sync_channel(2);
        assert!(tx.send(Ok(PipelineOutput { frames:vec![vec![2]], pan:Vec::new() })).is_ok());
        drop(tx);
        let mut failure = None;
        let pending = PipelineOutput { frames:vec![vec![1]], pan:Vec::new() };
        let output = pending.collect(&rx, &mut failure).unwrap();
        assert_eq!(output.frames, vec![vec![1], vec![2]]);
        assert_eq!(PipelineOutput::default().collect(&rx, &mut failure).err().as_deref(), Some("pipeline output disconnected"));
    }

    #[test]
    fn explicit_producer_error_preserves_tail_and_original_failure() {
        let (tx, rx) = sync::sync_channel(2);
        assert!(tx.send(Ok(PipelineOutput { frames:vec![vec![1]], pan:Vec::new() })).is_ok());
        assert!(tx.send(Err("SDK reconfigure failed".into())).is_ok());
        let mut failure = None;
        assert_eq!(PipelineOutput::default().collect(&rx, &mut failure).unwrap().frames, vec![vec![1]]);
        assert_eq!(PipelineOutput::default().collect(&rx, &mut failure).err().as_deref(), Some("SDK reconfigure failed"));
    }

    #[test]
    fn empty_live_producer_is_not_a_terminal_failure() {
        let (_tx, rx) = sync::sync_channel(2);
        let mut failure = None;
        let output = PipelineOutput::default().collect(&rx, &mut failure).unwrap();
        assert!(output.frames.is_empty());
        assert!(failure.is_none());
    }

    #[test]
    fn capture_grid_changes_expire_only_old_pan() {
        let mut snapshot = plan().pipeline_snapshot(Arc::from([]));
        let previous = (snapshot.capture_center, snapshot.device_decimate, snapshot.generation);
        let mut output = PipelineOutput { frames:vec![vec![1], vec![2]], pan:vec![(vec![100e6], vec![-40.0], 0, 0, 1)] };
        // A DSP-only change retains spectra on the same device frequency grid.
        snapshot.rate /= 2.0;
        output.capture_boundary(previous, &snapshot);
        assert_eq!(output.pan, vec![(vec![100e6], vec![-40.0], 0, 0, 1)]);
        snapshot.capture_center = 200e6;
        output.capture_boundary(previous, &snapshot);
        assert!(output.pan.is_empty());
        assert_eq!(output.frames, vec![vec![1], vec![2]]);
        snapshot.capture_center = previous.0;
        snapshot.device_decimate = previous.1 + 1;
        output.pan.push((vec![100e6], vec![-40.0], 0, 0, 1));
        output.capture_boundary(previous, &snapshot);
        assert!(output.pan.is_empty());
        assert_eq!(output.frames, vec![vec![1], vec![2]]);
        snapshot.device_decimate = previous.1;
        snapshot.generation += 1;
        output.pan.push((vec![100e6], vec![-40.0], -12, previous.2, 1));
        output.capture_boundary(previous, &snapshot);
        assert!(output.pan.is_empty());
    }

    #[test]
    fn rejected_recapture_after_async_drain_preserves_every_old_sample() {
        let mut expected = plan();
        let mut expected_frames = Vec::new();
        for _ in 0..48 { expected_frames.extend(expected.packet().unwrap()); }
        finish(&mut expected, &mut expected_frames);

        let mut actual = plan();
        actual.resume_fine().unwrap();
        let registry = actual.channel_registry();
        assert!(Arc::ptr_eq(&registry, &actual.pipeline_snapshot(Arc::from([])).registry));
        let mut frames = Vec::new();
        for _ in 0..24 { frames.extend(actual.packet().unwrap()); }
        frames.extend(actual.suspend_fine().unwrap());
        let before = (actual.capture_center, actual.listen, actual.stream.taps_per_phase(), actual.next_seq);
        let front_end = DevState::new(json!({"preset_defaults":{}}), json!({})).front_end(0.0);
        // Four-tap passband rejects the requested wide channel near its bin edge.
        assert!(actual.recapture(100e6, 100e6 + BIN_SPACING * 0.49, 3, 32, front_end, 4).is_err());
        assert_eq!((actual.capture_center, actual.listen, actual.stream.taps_per_phase(), actual.next_seq), before);
        assert!(Arc::ptr_eq(&registry, &actual.channel_registry()));
        assert!(!actual.capture_failed);
        actual.resume_fine().unwrap();
        for _ in 0..24 { frames.extend(actual.packet().unwrap()); }
        finish(&mut actual, &mut frames);
        assert_ne!(actual.stream.epoch(), expected.stream.epoch());
        assert_eq!(frames.len(), expected_frames.len());
        for (frame, expected_frame) in frames.iter().zip(&expected_frames) {
            assert_eq!(u32::from_le_bytes(frame[36..40].try_into().unwrap()) as u64, actual.stream.epoch());
            assert_eq!(u32::from_le_bytes(expected_frame[36..40].try_into().unwrap()) as u64, expected.stream.epoch());
            assert_eq!(&frame[..36], &expected_frame[..36]);
            assert_eq!(&frame[40..], &expected_frame[40..]);
        }
        assert_eq!(actual.bank.clocks().unwrap(), expected.bank.clocks().unwrap());
    }

    #[test]
    fn accepted_recapture_preserves_old_tail_before_new_sequence() {
        let mut ddc = plan();
        ddc.resume_fine().unwrap();
        let mut old_frames = Vec::new();
        for _ in 0..24 { old_frames.extend(ddc.packet().unwrap()); }
        old_frames.extend(ddc.suspend_fine().unwrap());
        let old_clock = ddc.bank.clocks().unwrap()[0].2;
        let old_epoch = ddc.stream.epoch();
        let front_end = DevState::new(json!({"preset_defaults":{}}), json!({})).front_end(0.0);
        ddc.recapture(100e6, 100e6, 20, 32, front_end, 8).unwrap();
        old_frames.append(&mut ddc.carry);
        let wire_samples: u64 = old_frames.iter().map(|frame| u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64).sum();
        assert_eq!(wire_samples, old_clock);
        assert!(old_frames.iter().all(|frame| u32::from_le_bytes(frame[36..40].try_into().unwrap()) as u64 == old_epoch));
        assert_ne!(ddc.stream.epoch(), old_epoch);
        ddc.resume_fine().unwrap();
        let mut new_frames = Vec::new();
        for _ in 0..24 { new_frames.extend(ddc.packet().unwrap()); }
        finish(&mut ddc, &mut new_frames);
        assert_eq!(u32::from_le_bytes(new_frames[0][8..12].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(new_frames[0][36..40].try_into().unwrap()) as u64, ddc.stream.epoch());
        assert_eq!(ddc.stream.taps_per_phase(), 8);
        assert_eq!(new_frames.iter().map(|frame| u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64).sum::<u64>(), ddc.bank.clocks().unwrap()[0].2);
    }

    #[test]
    fn unknown_bank_channel_is_reported_before_worker_start() {
        let mut ddc = plan();
        // A broken product mapping must report the real kernel identity, not slot 0.
        ddc.channel_id = 99;
        assert_eq!(ddc.resume_fine().unwrap_err(), "unknown fine channel ID 1");
    }

    #[test]
    fn snapshot_registry_mismatch_is_an_explicit_diagnostic() {
        let mut snapshot = plan().pipeline_snapshot(Arc::from([]));
        snapshot.clocks = Arc::from([(42, 1, 1)]);
        let mut actual = json!({});
        snapshot.write_channel_status(&mut actual);
        assert_eq!(actual["channel_clocks"], json!([]));
        assert_eq!(actual["channel_clock_error"], "fine clock product slot 42 missing from registry");
    }

    #[test]
    fn tune_receipt_clocks_follow_new_identity_through_worker_restart() {
        let mut ddc = plan();
        for _ in 0..24 { ddc.packet().unwrap(); }
        ddc.suspend_fine().unwrap();
        ddc.sample_clocks().unwrap();
        let before = ddc.pipeline_snapshot(Arc::from([]));
        ddc.tune(100e6 + 1000.0, 3).unwrap();
        ddc.sample_clocks().unwrap();
        let boundary = ddc.pipeline_snapshot(Arc::from([]));
        assert_ne!(before.registry[0].channel_id, boundary.registry[0].channel_id);
        let mut receipt_actual = json!({"input_samples":0,"bandwidth":50e6});
        boundary.write_channel_status(&mut receipt_actual);
        let clocks = receipt_actual["channel_clocks"].as_array().unwrap();
        assert_eq!(clocks.len(), boundary.registry.len());
        assert_eq!(clocks[0]["channel"], boundary.registry[0].channel);
        assert_eq!(clocks[0]["channel_id"], boundary.registry[0].channel_id);
        assert_eq!(clocks[0]["epoch"], boundary.registry[0].epoch);
        assert_eq!(receipt_actual["input_samples"], boundary.input_samples);
        assert!(boundary.input_samples > 0);
        assert_eq!(receipt_actual["bandwidth"], json!(50e6));
        ddc.resume_fine().unwrap();
        let restarted = ddc.pipeline_snapshot(Arc::from([]));
        assert_eq!(restarted.channel_clocks().unwrap(), receipt_actual["channel_clocks"]);
        assert_eq!(restarted.input_samples, boundary.input_samples);
        ddc.suspend_fine().unwrap();
    }
}
