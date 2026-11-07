use super::*;
use std::collections::VecDeque;
use std::sync::mpsc::{self as sync, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use websa_channelizer::PfbOutput;
use websa_channelizer::timing::TimingSnapshot;

/// Benchmark-only prediction parameters. Product startup does not calibrate or
/// install this budget, and check_plan_admission enforces geometry only.
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

pub(super) fn check_plan_admission(_profile:&websa_channelizer::FineCostBudget,fine:i32,
    count:usize,channel_budget:usize,_output_rate_budget:f64)->Result<(),String> {
    check_plan_geometry(fine,count,channel_budget)
}
pub(super) fn check_plan_geometry(fine:i32,count:usize,channel_budget:usize)->Result<(),String> {
    if count>channel_budget {return Err("fine channel budget exceeded".into());}
    if !(2..=24).contains(&fine) {return Err("fine channel configuration rejected".into());}
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

/// One raw-capture route. The native cascade owns NCO/FIR history; scratch is reused.
pub(super) struct DirectChannel {
    handle: *mut websa_channelizer::Channel,
    pub id: u64,
    pub rate: f64,
    pub delay: f64,
    decimate: u64,
    epoch: u64,
    origin: u64,
    next_seq: u64,
    input: u64,
    output: u64,
    scratch: Vec<f32>,
    pub framer: BasebandFramer,
}
unsafe impl Send for DirectChannel {}
impl Drop for DirectChannel {
    fn drop(&mut self) { if !self.handle.is_null() {unsafe { websa_channelizer::channel_close(self.handle); }} }
}
impl DirectChannel {
    pub fn new(slot:u32,id:u64,epoch:u64,fs:f64,center:f64,frequency:f64,
        plan:&websa_channelizer::rate_plan::RatePlan,origin:u64,seq:u64)->Result<Self,String> {
        let framer = if slot==1 {BasebandFramer::new(id,epoch,frequency,plan.output_rate)}
            else {BasebandFramer::new_aux(id,epoch,frequency,plan.output_rate)}
            .and_then(|framer|framer.with_source_rate(fs)).ok_or("direct framer rejected")?;
        let handle=websa_channelizer::channel_open_planned(fs,center-frequency,plan);
        if handle.is_null() {return Err("direct channel filter rejected".into());}
        let delay=unsafe {websa_channelizer::channel_delay(handle)};
        Ok(Self {handle,id,rate:plan.output_rate,delay,decimate:plan.decimate as u64,
            epoch,origin,next_seq:seq,input:0,output:0,
            scratch:vec![0.0;2*(16240/plan.decimate as usize+2)],framer})
    }
    pub(super) fn detach(&mut self,slot:u32)->Result<Self,String> {
        let framer=if slot==1 {BasebandFramer::new(self.id,self.epoch,0.0,self.rate)}
            else {BasebandFramer::new_aux(self.id,self.epoch,0.0,self.rate)}.ok_or("direct placeholder rejected")?;
        Ok(std::mem::replace(self,Self {handle:std::ptr::null_mut(),id:self.id,rate:self.rate,delay:self.delay,
            decimate:self.decimate,epoch:self.epoch,origin:self.origin,next_seq:self.next_seq,input:self.input,
            output:self.output,scratch:Vec::new(),framer}))
    }
    pub(super) fn detached(&self)->bool {self.handle.is_null()}
    pub(super) fn clock(&self)->(u64,u64) {(self.input,self.output)}
    pub fn process(&mut self,seq:u64,stamp:u64,input_index:u64,raw:&[i16])->Result<IqFrames,String> {
        if self.handle.is_null() || seq!=self.next_seq || input_index!=self.origin+self.input || raw.len()!=2*16240 {
            return Err("direct source sequence/clock mismatch".into());
        }
        let first=self.origin.checked_add((self.output+1).checked_mul(self.decimate)
            .ok_or("direct sample clock overflow")?).and_then(|n|n.checked_sub(1))
            .ok_or("direct sample clock overflow")?;
        let count=unsafe {websa_channelizer::channel_process(self.handle,raw.as_ptr(),
            16240,self.scratch.as_mut_ptr(),(self.scratch.len()/2) as i32)};
        if count<0 {return Err("direct channel processing failed".into());}
        let frames=self.framer.push_native(&websa_channelizer::FineOutput {channel_id:self.id,epoch:self.epoch,
            bin:0,seq,stamp,input_index:first,output_index:self.output,output_rate:self.rate,
            center_offset_hz:None,delay_samples:self.delay,iq:&self.scratch[..2*count as usize]})
            .map_err(str::to_owned)?;
        self.input+=16240;
        self.output+=count as u64;
        self.next_seq=self.next_seq.checked_add(1).ok_or("direct sequence overflow")?;
        Ok(frames)
    }
}


pub(super) struct FineState {
    pub bank: FineBank,
    pub framer: BasebandFramer,
    pub aux: BTreeMap<u32, (u64, f64, BasebandFramer)>,
    pub channel_id: u64,
    pub next_id: u64,
    pub faults:BTreeMap<u32,super::fine_workers::FineFault>,
    pub(super) fine_timing: Timing,
    pub(super) framer_timing: Timing,
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
        Self { bank, framer, aux: BTreeMap::new(), channel_id: 1, next_id: 1, registry, faults:BTreeMap::new(),
            fine_timing: Timing::default(), framer_timing: Timing::default(),
            clocks:Arc::from([]),
            output_send:Timing::default(), snapshot_publish:Timing::default(), slow_io:SlowPipeline::default(),
            period:Duration::from_secs_f64(16240.0/input_rate), slow:SlowStage::default() }
    }
    pub fn deliver(&mut self, packet: PfbOutput) -> Result<IqFrames, String> {
        let mut frames = Vec::new();
        let started = Instant::now();
        let mut framing = Duration::ZERO;
        let mut unknown_id = None;
        self.bank.deliver(&packet, |fine| {
            let started = Instant::now();
            if fine.channel_id == self.channel_id {
                frames.extend(self.framer.push_native(fine)?);
            } else if let Some((_, (_, _, framer))) = self.aux.iter_mut().find(|(_, (id, _, _))| *id == fine.channel_id) {
                frames.extend(framer.push_native(fine)?);
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
pub(super) struct FineMetrics {
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
    pub(super) fn metrics_ref(&self) -> FineMetrics {
        FineMetrics { fine:self.fine_timing.snapshot(), framer:self.framer_timing.snapshot(),
            output_send:self.output_send.snapshot(), snapshot_publish:self.snapshot_publish.snapshot(),
            slow:self.slow, slow_io:self.slow_io,
            packets:self.bank.next_sequence(), clocks:self.clocks.clone() }
    }
    pub(super) fn sample_clocks(&mut self) -> Result<(), String> {
        let primary = self.channel_id;
        let slots:BTreeMap<_,_>=self.aux.iter().map(|(&slot,(id,_,_))|(*id,slot)).collect();
        let clocks = self.bank.clocks().map_err(|error| format!("fine clocks: {error}"))?;
        self.clocks = map_clocks(clocks, |id| if id == primary { Some(1) } else {
            slots.get(&id).copied()
        })?.into();
        Ok(())
    }

}

pub(super) use super::fine_workers::FineWorker;

pub(super) struct PipelineConfig {
    pub center: f64, pub listen: f64, pub fine: i32, pub display_decimate: i32,
    pub front_end: FrontEnd, pub shim: String, pub taps: usize,
    pub aux: Vec<(u32, f64)>, pub channel_budget: usize, pub output_rate_budget: f64,
}
#[derive(Clone)]
pub(super) struct Snapshot {
    pub capture_center: f64, pub usable_bandwidth: f64, pub rate: f64,
    pub input_rate: f64, pub epoch: u64,
    pub generation: u64,
    pub batch_packets: usize,
    pub display_fft: Value,
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
    fine_lanes:Arc<[super::fine_workers::FineLaneSnapshot]>,
    pub fine_faults:Arc<[super::fine_workers::FineFault]>,
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
        actual["ddc_batch"] = json!(self.batch_packets);
        actual["display_fft"] = self.display_fft.clone();
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
    pub frames: IqFrames, pub pan: Vec<(Vec<f64>, Vec<f32>, i32, u64, u64, usize)>,
}
impl PipelineOutput {
    fn capture_boundary(&mut self, previous: (f64, u64, u64), next: &Snapshot) {
        if previous != (next.capture_center, next.device_decimate, next.generation) {
            // IQ tails retain their order; spectra from the old capture configuration expire.
            self.pan.clear();
        }
    }
}
/// Publisher stalls retain bounded per-identity FIFOs, never the capture thread.
struct DeliveryQueue {
    lanes:BTreeMap<(u32,u32),(usize,VecDeque<IqFrame>)>,
    pan:Vec<(Vec<f64>,Vec<f32>,i32,u64,u64,usize)>,
    failure:Option<String>,closed:bool,identities:usize,failed:std::collections::BTreeSet<(u32,u32)>,
}
impl DeliveryQueue {
    fn new(channels:usize)->Self {Self {lanes:BTreeMap::new(),pan:Vec::new(),failure:None,closed:false,identities:channels.saturating_mul(2),failed:Default::default()}}
    fn push(&mut self,result:Result<PipelineOutput,String>)->Vec<((u32,u32),u64)> {
        let mut rejected=Vec::new();
        let output=match result {Ok(output)=>output,Err(error)=>{self.failure=Some(error);return rejected;}};
        for frame in output.frames {
            let header=frame.header();
            let key=(u32::from_le_bytes(header[32..36].try_into().unwrap()),u32::from_le_bytes(header[36..40].try_into().unwrap()));
            if self.failed.contains(&key) {continue;}
            let index=u64::from_le_bytes(header[40..48].try_into().unwrap());
            let bytes=frame.native_iq().map_or_else(||frame.len(),|iq|iq.samples.len()*4+96);
            if !self.lanes.contains_key(&key) && self.lanes.len()>=self.identities {self.failed.insert(key);rejected.push((key,index));continue;}
            let lane=self.lanes.entry(key).or_default();
            if lane.1.len()>=32 || lane.0+bytes>8*1024*1024 {self.failed.insert(key);rejected.push((key,index));continue;}
            lane.0+=bytes;lane.1.push_back(frame);
        }
        // Display frames are replaceable, unlike IQ. Keep the newest per FFT kind.
        for pan in output.pan {
            if let Some(old)=self.pan.iter_mut().find(|old|old.5==pan.5) {*old=pan;}
            else {self.pan.push(pan);}
        }
        rejected
    }
    fn take(&mut self)->PipelineOutput {
        PipelineOutput {frames:std::mem::take(&mut self.lanes).into_values().flat_map(|(_,frames)|frames).collect(),
            pan:std::mem::take(&mut self.pan)}
    }
    fn collect(&mut self,mut pending:PipelineOutput,failure:&mut Option<String>)->Result<PipelineOutput,String> {
        let output=self.take();pending.frames.extend(output.frames);pending.pan.extend(output.pan);
        if failure.is_none() {*failure=self.failure.clone().or_else(||self.closed.then(||"pipeline output disconnected".into()));}
        if pending.frames.is_empty() && pending.pan.is_empty() {if let Some(error)=failure {return Err(error.clone());}}
        Ok(pending)
    }
}
fn owner_panic_error(panic: Box<dyn std::any::Any + Send>) -> String {
    let detail = panic.downcast_ref::<String>().map(String::as_str)
        .or_else(||panic.downcast_ref::<&str>().copied()).unwrap_or("non-string panic payload");
    format!("PFB owner panicked: {detail}")
}

type Change = Box<dyn FnOnce(&mut Ddc) -> Result<IqFrames, String> + Send>;
struct Command {
    local:bool,
    change: Option<Change>,
    reply: SyncSender<Result<(Snapshot, PipelineOutput), (String, PipelineOutput)>>,
    resume: Receiver<()>,
}
pub(super) struct Pipeline {
    tx: SyncSender<Command>,
    rx: Arc<PLMutex<DeliveryQueue>>,
    shared: Arc<PLMutex<Snapshot>>,
    pub snapshot: Snapshot,
    pub device_info: Option<hardware_swp::DeviceInfo>,
    gnss: hardware_iqs::CaptureGnss,
    join: Option<JoinHandle<()>>,
    diagnostics: hardware_iqs::CaptureDiagnostics,
    terminal: Arc<PLMutex<Option<Value>>>,
    refreshed: Instant,
    pending_pan: Vec<(Vec<f64>, Vec<f32>, i32, u64, u64, usize)>,
    pending_frames: IqFrames,
    failure: Option<String>,
}
impl Ddc {
    pub(super) fn display_fft_status(&self)->Value {
        let mut value=self.display_fft.as_ref().map(|a|a.status(self.stream.input_sample_rate()))
            .unwrap_or_else(||json!({"size":0,"fft_points":16240,"window":"FlatTop"}));
        value["available"]=json!(self.hardware.is_some());
        value["error"]=json!(self.display_fft_error);value
    }
    pub(super) fn finish_framers(&mut self, frames: &mut IqFrames) -> Result<(), String> {
        if !self.direct.contains_key(&1) && self.service_configs.as_ref().is_none_or(|configs| configs[0].enabled) {
            frames.extend(self.framer.finish_native().map_err(str::to_owned)?);
        }
        for (_, _, framer) in self.aux.values_mut() {
            frames.extend(framer.finish_native().map_err(str::to_owned)?);
        }
        for route in self.direct.values_mut() {
            frames.extend(route.framer.finish_native().map_err(str::to_owned)?);
        }
        Ok(())
    }
    pub(super) fn suspend_fine(&mut self) -> Result<IqFrames, String> {
        self.stream.flush_pending().map_err(str::to_owned)?;
        let mut frames = self.drain()?;
        if let Some(worker) = self.fine_worker.take() {
            let (state,direct)=worker.finish_with_direct(&mut frames)?;self.fine=Some(state);self.direct.extend(direct);
        }
        Ok(frames)
    }
    pub(super) fn resume_fine(&mut self) -> Result<(), String> {
        if let Some(worker)=&mut self.fine_worker {worker.sync_lanes()?;return worker.sync_directs(&mut self.direct);}
        let mut state=self.fine.take().ok_or("fine stage absent")?;
        state.registry=self.registry.clone();
        let mut worker=FineWorker::start(state)?;worker.sync_directs(&mut self.direct)?;
        self.fine_worker = Some(worker);
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
        let mut clocks=metrics.clocks.to_vec();
        if let Some(worker)=&self.fine_worker {clocks.extend(worker.direct_clocks());}
        else {clocks.extend(self.direct.iter().map(|(&slot,route)| (u64::from(slot),route.input,route.output)));}
        Snapshot { capture_center:self.capture_center, usable_bandwidth:self.usable_bandwidth,
            input_rate:self.stream.input_sample_rate(), epoch:self.stream.epoch(),
            generation:self.generation,
            batch_packets:self.capture_batch_packets(),
            display_fft:self.display_fft_status(),
            rate:self.channel_rate(), taps:self.stream.taps_per_phase(), device_decimate:self.device_decimate,
            aux, actual:Value::Null, pool:self.stream.stats(), input_samples:self.input_pos,
            front_end:self.hardware.as_ref().and_then(|capture| capture.diagnostics().front_end()),
            registry:self.registry.clone(),
            selected_bins:self.stream.bins().len(),
            pool_timing:self.stream.timings(),
            stamp_gaps:self.stamp_gaps, fine:metrics.fine, framer:metrics.framer, clocks:clocks.into(),
            fine_packets:metrics.packets, slow:metrics.slow,
            fine_lanes:self.fine_worker.as_ref().map_or_else(||Arc::from([]),|worker|worker.snapshots()),
            fine_faults:self.faults.values().cloned().collect::<Vec<_>>().into(),
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
    pub fn terminal_failure(&self) -> Option<Value> {
        self.terminal.lock().clone()
    }
    pub fn failure_diagnostics(&self) -> Value {
        self.terminal_failure().unwrap_or_else(|| {
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
        actual["fine_owners"] = json!({"input_jobs_per_channel":64,"input_bytes_per_channel":4194304,
            "output_jobs_per_channel":32,"output_bytes_per_channel":8388608,
            "shared_coarse_columns":true,"channels":&*snapshot.fine_lanes});
        actual["fine_faults"] = json!(&*snapshot.fine_faults);
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
            check_plan_geometry(config.fine,config.aux.len()+1,config.channel_budget)?;
        }
        let (tx, commands) = sync::sync_channel::<Command>(1);
        let rx=Arc::new(PLMutex::new(DeliveryQueue::new(config.channel_budget)));
        let output=rx.clone();
        let (ready, init) = sync::sync_channel(1);
        let terminal = Arc::new(PLMutex::new(None));
        let failed_capture = terminal.clone();
        let join = thread::Builder::new().name("sdr-pfb".into()).spawn(move || {
            // Publish a terminal result before dropping owners that may still
            // need to join native work. Never resume DDC state after a panic.
            let mut owned_ddc = None;
            let mut last_published = None;
            let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let setup = (|| {
                    let frequencies: Vec<_> = config.aux.iter().map(|(_, f)| *f).collect();
                    let device_decimate = if let Some(channels)=&channels {
                        service_ddc::service_capture_decimate(config.center,config.display_decimate,config.taps,channels)?
                    } else {capture_decimate(config.center, config.listen, &frequencies, config.display_decimate, config.taps)};
                    if let Some(channels)=&channels {
                        let fs=IQ_FS/device_decimate as f64;
                        let m=PFB_BINS.min((fs/BIN_SPACING).round() as i64).max(2);
                        let mut bins=Vec::new();
                        for c in channels.iter().filter(|c|c.enabled && !service_ddc::is_direct(c)) {
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
                    let fs = g.sample_rate;
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
                let ddc = match setup { Ok(ddc) => owned_ddc.insert(ddc), Err(e) => { let _ = ready.send(Err(e)); return; } };
                let diagnostics = ddc.hardware.as_ref().unwrap().diagnostics();
                let device_info = ddc.hardware.as_ref().and_then(|capture| capture.device_info);
                let gnss = ddc.hardware.as_ref().unwrap().gnss_reader();
                let mut aux: Arc<[(u32, f64)]> = config.aux.into();
                if let Err(e) = ddc.resume_fine() { let _ = ready.send(Err(e)); return; }
                let shared = Arc::new(PLMutex::new(ddc.pipeline_snapshot(aux.clone())));
                last_published = Some((shared.clone(), diagnostics.clone()));
                if let Err(e) = ddc.hardware.as_mut().unwrap().arm() {
                    let _ = ready.send(Err(e));
                    return;
                }
                if ready.send(Ok((shared.clone(), diagnostics.clone(), device_info, gnss))).is_err() { return; }
                let mut refreshed = Instant::now();
                let mut control_failure = None;
                loop {
                    match commands.try_recv() {
                        Ok(command) => {
                            let Command { local, change, reply, resume } = command;
                            let stop = change.is_none();
                            // Keep the reply gate and already-drained output outside
                            // unwind. A panic is fatal, but cannot discard this prefix
                            // or masquerade as a disconnected command mailbox.
                            let mut frames = Vec::new();
                            let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                frames.extend(if local {
                                    ddc.stream.flush_pending().map_err(str::to_owned).and_then(|_|ddc.drain())
                                } else { ddc.suspend_fine() }?);
                                let changed = match change {
                                    Some(change) => change(ddc).map(|tail| frames.extend(tail)),
                                    None => ddc.finish_framers(&mut frames),
                                };
                                let clocks = ddc.sample_clocks();
                                aux = ddc.registry.iter().filter(|r|r.channel!=1)
                                    .map(|r|(r.channel,r.frequency_hz)).collect::<Vec<_>>().into();
                                output.lock().failed.retain(|&(id,epoch)|ddc.registry.iter().any(|row|row.channel_id==id&&row.epoch==epoch));
                                let snapshot = ddc.pipeline_snapshot(aux.clone());
                                *shared.lock() = snapshot.clone();
                                let failed = ddc.capture_failed || (ddc.fine.is_none() && ddc.fine_worker.is_none());
                                Ok::<_, String>((snapshot, changed.and(clocks), failed))
                            }));
                            let (snapshot, changed, failed) = match applied {
                                Ok(Ok(value)) => value,
                                failure => {
                                    let error = match failure {
                                        Ok(Err(error)) => error,
                                        Err(panic) => owner_panic_error(panic),
                                        Ok(Ok(_)) => unreachable!(),
                                    };
                                    control_failure = Some(error.clone());
                                    let _ = reply.send(Err((error, PipelineOutput { frames, pan:Vec::new() })));
                                    let _ = resume.recv();
                                    break;
                                }
                            };
                            let tail = PipelineOutput { frames, pan:std::mem::take(&mut ddc.pan) };
                            let result = match changed {
                                Ok(()) => Ok((snapshot, tail)),
                                Err(error) => Err((error, tail)),
                            };
                            if failed { control_failure = result.as_ref().err().map(|(error, _)| error.clone()); }
                            let _ = reply.send(result);
                            if resume.recv().is_err() { break; }
                            if stop || failed { break; }
                            if let Err(error) = ddc.resume_fine() { control_failure = Some(error); break; }
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
                        let rejected=output.lock().push(result);
                        if let Some(worker)=ddc.fine_worker.as_mut() {
                            for (identity,index) in rejected {worker.fail_delivery(identity,index);}
                        }
                        if has_output {
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
                if let Some(error) = control_failure {
                    // Commands return their accepted old tails before releasing the
                    // resume gate. Only then publish the fatal error to polling, so
                    // it cannot overtake that reply or collapse into disconnection.
                    let snapshot = shared.lock().clone();
                    let actual = PipelinePresentation { snapshot, diagnostics:diagnostics.clone() }.encode();
                    *failed_capture.lock() = Some(json!({
                        "error":&error, "snapshot_state":"last_published", "actual":actual,
                    }));
                    output.lock().push(Err(error));
                }
            }));
            if let Err(panic) = completed {
                let error = owner_panic_error(panic);
                // Diagnostics use the last published snapshot, not potentially
                // damaged DDC state. The retained DDC is only dropped below.
                let actual = last_published.map(|(shared, diagnostics)| {
                    let snapshot = shared.lock().clone();
                    PipelinePresentation { snapshot, diagnostics }.encode()
                });
                let mut terminal = failed_capture.lock();
                let first = terminal.get_or_insert_with(||json!({
                    "error":&error, "snapshot_state":"last_published", "actual":actual,
                }));
                let cause = first["error"].as_str().unwrap_or(&error).to_owned();
                drop(terminal);
                if cause != error { eprintln!("WebSA cleanup after {cause}: {error}"); }
                output.lock().push(Err(cause.clone()));
                // Startup panics must also retain their cause. A full ready slot
                // already holds the live pipeline; its delivery queue owns failure.
                let _ = ready.try_send(Err(cause));
            }
            output.lock().closed=true;
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
        let pending = PipelineOutput {
            pan:std::mem::take(&mut self.pending_pan), frames:std::mem::take(&mut self.pending_frames),
        };
        let result=self.rx.lock().collect(pending,&mut self.failure)?;
        if self.refreshed.elapsed() >= Duration::from_millis(50) {
            self.snapshot = self.shared.lock().clone();
            self.refreshed = Instant::now();
        }
        Ok(result)
    }
    fn boundary(&mut self, change: Option<Change>, local:bool) -> Result<PipelineOutput, String> {
        let mut output = PipelineOutput::default();
        if let Err(error) = self.run_boundary(change, &mut output, local) {
            self.pending_frames.extend(output.frames);
            self.pending_pan.extend(output.pan);
            return Err(error);
        }
        Ok(output)
    }
    fn run_boundary(&mut self, change: Option<Change>, output: &mut PipelineOutput, local:bool) -> Result<(), String> {
        let previous_capture = (self.snapshot.capture_center, self.snapshot.device_decimate, self.snapshot.generation);
        let (reply, rx) = sync::sync_channel(1);
        let (resume, resumed) = sync::sync_channel(0);
        let mut command = Command { local, change, reply, resume:resumed };
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
                    let queued=self.rx.lock().take();
                    output.frames.extend(queued.frames);output.pan.extend(queued.pan);
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
    pub fn command(&mut self, change: impl FnOnce(&mut Ddc) -> Result<IqFrames, String> + Send + 'static) -> Result<IqFrames, String> {
        let output = self.boundary(Some(Box::new(change)),false)?;
        self.pending_pan.extend(output.pan);
        Ok(output.frames)
    }
    pub fn command_channel(&mut self, change: impl FnOnce(&mut Ddc)->Result<IqFrames,String>+Send+'static)->Result<IqFrames,String> {
        let output=self.boundary(Some(Box::new(change)),true)?;
        self.pending_pan.extend(output.pan);Ok(output.frames)
    }
    pub fn stop(&mut self) -> Result<PipelineOutput, String> {
        if self.join.is_none() { return Ok(PipelineOutput::default()); }
        let mut result = self.boundary(None,false);
        let joined = self.join.take().unwrap().join().map_err(|_| "pipeline panicked".to_string());
        joined?;
        let mut queue=self.rx.lock();
        let queued=queue.take();
        if let Ok(output)=&mut result {output.frames.extend(queued.frames);output.pan.extend(queued.pan);}
        if let Some(error)=queue.failure.take() {result=Err(error);}
        result
    }
}
impl Drop for Pipeline { fn drop(&mut self) { let _ = self.stop(); } }

#[cfg(test)]
#[path = "sdr_capture_contract_tests.rs"]
mod capture_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_fifo_overflow_keeps_other_identities_and_reports_terminal_after_prefix() {
        let frame=|id:u64,seq:u32|websa_channelizer::IqFrame::native(websa_channelizer::BasebandMagic::Aux,
            seq+1,48000.0,100e6,id,9,seq as u64*10,0.0,62500000.0,vec![0.25,0.5]).unwrap();
        let mut queue=DeliveryQueue::new(2);
        for seq in 0..32 {assert!(queue.push(Ok(PipelineOutput {frames:vec![frame(1,seq)],pan:vec![]})).is_empty());}
        let rejected=queue.push(Ok(PipelineOutput {frames:vec![frame(1,32),frame(2,0)],pan:vec![]}));
        assert_eq!(rejected,vec![((1,9),320)]);
        queue.push(Err("actual capture failure".into()));let mut failure=None;
        let prefix=queue.collect(PipelineOutput::default(),&mut failure).unwrap();
        assert_eq!(prefix.frames.len(),33);
        assert_eq!(prefix.frames.last().unwrap().header()[32],2);
        assert!(queue.push(Ok(PipelineOutput {frames:vec![frame(1,33)],pan:vec![]})).is_empty());
        assert!(queue.take().frames.is_empty(),"never resume a failed identity after a gap");
        assert_eq!(queue.collect(PipelineOutput::default(),&mut failure).err().as_deref(),Some("actual capture failure"));
    }

    fn plan() -> Ddc {
        Ddc::configured(100e6, 100e6, 3, 50e6, &mut None, &[], 16).unwrap()
    }

    const DIRECT_TEST_FS: f64 = 7_812_500.0;
    const DIRECT_TEST_PACKET: u64 = 16_240;

    fn direct_test_plan() -> websa_channelizer::rate_plan::RatePlan {
        // GPS L1 C/A occupied width, without depending on an installed decoder/catalog entry.
        websa_channelizer::rate_plan::RatePlan::new(DIRECT_TEST_FS, "am", 2_046_000.0).unwrap()
    }

    fn direct_test_tone(first: u64, frequency: f64, amplitude: f64) -> Vec<i16> {
        (first..first + DIRECT_TEST_PACKET).flat_map(|index| {
            let phase = std::f64::consts::TAU * frequency * index as f64 / DIRECT_TEST_FS;
            [(amplitude * phase.cos()).round() as i16, (amplitude * phase.sin()).round() as i16]
        }).collect()
    }

    fn direct_test_iq(frames: &[IqFrame]) -> Vec<[f64; 2]> {
        frames.iter().flat_map(|frame| frame[56..].chunks_exact(8).map(|sample| {
            [f32::from_le_bytes(sample[..4].try_into().unwrap()) as f64,
                f32::from_le_bytes(sample[4..].try_into().unwrap()) as f64]
        })).collect()
    }

    fn assert_direct_test_clocks(channel: &DirectChannel, input: u64, output: u64) {
        let (mut native_input, mut native_output) = (0, 0);
        assert_eq!(unsafe { websa_channelizer::channel_clock(channel.handle,
            &mut native_input, &mut native_output) }, 0);
        assert_eq!((native_input, native_output), (input, output));
        assert_eq!((channel.input, channel.output), (input, output));
    }

    #[test]
    fn direct_raw_i16_nco_sign_and_wire_full_scale_are_preserved() {
        let plan = direct_test_plan();
        for (offset, residual, amplitude) in [(600_000.0, 25_000.0, 16_384.0),
            (-600_000.0, -25_000.0, 8_192.0)] {
            let mut channel = DirectChannel::new(1, 7, 11, DIRECT_TEST_FS, 100e6,
                100e6 + offset, &plan, 0, 0).unwrap();
            let mut frames = Vec::new();
            for packet in 0..5 {
                let first = packet * DIRECT_TEST_PACKET;
                frames.extend(channel.process(packet, first, first,
                    &direct_test_tone(first, offset + residual, amplitude)).unwrap());
            }
            frames.extend(channel.framer.finish_native().unwrap());
            let iq = direct_test_iq(&frames);
            let settled = &iq[(2.0 * channel.delay).ceil() as usize + 32..];
            let rms = (settled.iter().map(|s| s[0] * s[0] + s[1] * s[1]).sum::<f64>()
                / settled.len() as f64).sqrt();
            assert!((rms - amplitude / 32768.0).abs() < 0.002, "offset {offset}: rms {rms}");
            let (mut re, mut im) = (0.0, 0.0);
            for pair in settled.windows(2) {
                re += pair[0][0] * pair[1][0] + pair[0][1] * pair[1][1];
                im += pair[0][0] * pair[1][1] - pair[0][1] * pair[1][0];
            }
            let measured = im.atan2(re) * plan.output_rate / std::f64::consts::TAU;
            assert!((measured - residual).abs() < 2.0,
                "offset {offset}: expected residual {residual}, measured {measured}");
        }
    }

    #[test]
    fn direct_gps_occupied_edges_survive_and_stopband_is_attenuated() {
        let plan = direct_test_plan();
        let edge = plan.occupied_bandwidth_hz / 2.0;
        for (probe, pass) in [(edge, true), (-edge, true),
            (plan.stopband_hz + 50_000.0, false), (-plan.stopband_hz - 50_000.0, false)] {
            let mut channel = DirectChannel::new(2, 9, 12, DIRECT_TEST_FS, 100e6,
                100e6 + 350_000.0, &plan, 0, 0).unwrap();
            let mut frames = Vec::new();
            for packet in 0..5 {
                let first = packet * DIRECT_TEST_PACKET;
                frames.extend(channel.process(packet, first, first,
                    &direct_test_tone(first, 350_000.0 + probe, 16_384.0)).unwrap());
            }
            frames.extend(channel.framer.finish_native().unwrap());
            let iq = direct_test_iq(&frames);
            let settled = &iq[(2.0 * channel.delay).ceil() as usize + 32..];
            let gain = (settled.iter().map(|s| s[0] * s[0] + s[1] * s[1]).sum::<f64>()
                / settled.len() as f64).sqrt() / 0.5;
            if pass {
                assert!((0.97..1.03).contains(&gain), "occupied edge {probe}: gain {gain}");
            } else {
                assert!(gain < 0.001, "stopband {probe}: gain {gain}");
            }
        }
    }

    #[test]
    fn direct_packet_counts_and_wire_source_indices_survive_retune_origin() {
        let plan = direct_test_plan();
        assert_eq!(plan.decimate, 3, "exercise a decimation phase crossing fixed packet boundaries");
        let decimate = plan.decimate as u64;
        for (slot, id, epoch, origin, first_seq) in [(1, 7, 11, 0, 0),
            (2, 8, 12, 9 * DIRECT_TEST_PACKET + 17, 103)] {
            let mut channel = DirectChannel::new(slot, id, epoch, DIRECT_TEST_FS, 100e6,
                100e6 + 250_000.0, &plan, origin, first_seq).unwrap();
            let mut frames = Vec::new();
            for packet in 0..31 {
                let consumed = packet * DIRECT_TEST_PACKET;
                frames.extend(channel.process(first_seq + packet, 1_000_000 + packet,
                    origin + consumed, &direct_test_tone(consumed, 250_000.0, 16_384.0)).unwrap());
                let input = consumed + DIRECT_TEST_PACKET;
                assert_direct_test_clocks(&channel, input, input / decimate);
                assert_eq!(channel.next_seq, first_seq + packet + 1);
            }
            frames.extend(channel.framer.finish_native().unwrap());
            assert!(frames.len() >= 4, "exercise multiple wire blocks and a flushed tail");
            let mut delivered = 0;
            for (seq, frame) in frames.iter().enumerate() {
                assert_eq!(&frame[..4], if slot == 1 { b"IQBF" } else { b"IQAX" });
                assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize, seq);
                let count = u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64;
                assert_eq!(count == 0, seq == 0);
                assert_eq!(frame.len(), 56 + count as usize * 8);
                assert_eq!(f64::from_le_bytes(frame[16..24].try_into().unwrap()), plan.output_rate);
                assert_eq!(f64::from_le_bytes(frame[24..32].try_into().unwrap()), 100e6 + 250_000.0);
                assert_eq!(u32::from_le_bytes(frame[32..36].try_into().unwrap()) as u64, id);
                assert_eq!(u32::from_le_bytes(frame[36..40].try_into().unwrap()) as u64, epoch);
                assert_eq!(u64::from_le_bytes(frame[40..48].try_into().unwrap()),
                    origin + (delivered + 1) * decimate - 1);
                assert_eq!(f64::from_le_bytes(frame[48..56].try_into().unwrap()), channel.delay);
                delivered += count;
            }
            assert_eq!(delivered, 31 * DIRECT_TEST_PACKET / decimate);
        }
    }

    #[test]
    fn direct_source_gaps_are_rejected_without_consuming_filter_or_framer_state() {
        let plan = direct_test_plan();
        let origin = 7 * DIRECT_TEST_PACKET + 1;
        let make = || DirectChannel::new(2, 8, 12, DIRECT_TEST_FS, 100e6,
            100e6 + 250_000.0, &plan, origin, 91).unwrap();
        let mut actual = make();
        let mut expected = make();
        let raw = direct_test_tone(0, 260_000.0, 16_384.0);
        assert_eq!(actual.process(91, 100, origin, &raw).unwrap(),
            expected.process(91, 100, origin, &raw).unwrap());
        let next = origin + DIRECT_TEST_PACKET;
        for (seq, input_index, len) in [(93, next, raw.len()), (91, next, raw.len()),
            (92, next + 1, raw.len()), (92, next - 1, raw.len()), (92, next, raw.len() - 2)] {
            assert_eq!(actual.process(seq, 101, input_index, &raw[..len]).unwrap_err(),
                "direct source sequence/clock mismatch");
            assert_direct_test_clocks(&actual, DIRECT_TEST_PACKET,
                DIRECT_TEST_PACKET / plan.decimate as u64);
            assert_eq!(actual.next_seq, 92);
        }
        let raw = direct_test_tone(DIRECT_TEST_PACKET, 260_000.0, 16_384.0);
        assert_eq!(actual.process(92, 101, next, &raw).unwrap(),
            expected.process(92, 101, next, &raw).unwrap());
        assert_eq!(actual.framer.finish_native().unwrap(), expected.framer.finish_native().unwrap());
        assert_direct_test_clocks(&actual, 2 * DIRECT_TEST_PACKET,
            2 * DIRECT_TEST_PACKET / plan.decimate as u64);
    }

    #[test]
    fn mixed_direct_retune_and_disable_preserve_tails_and_narrow_bytes() {
        let center = 1_575_420_000.0;
        let mut configs: Vec<_> = (1..=3).map(receiver_workers::ChannelConfig::for_slot).collect();
        configs[0].freq = center;
        configs[1].enabled = true;
        configs[1].freq = center;
        configs[1].mode = "gps".into();
        configs[1].if_bw = 2_046_000.0;
        let mut baseline_configs = configs.clone();
        baseline_configs[1].enabled = false;
        let mut baseline = Ddc::configured_service(center, 6_250_000.0, &mut None,
            16, DIRECT_TEST_FS, &baseline_configs).unwrap();
        let mut mixed = Ddc::configured_service(center, 6_250_000.0, &mut None,
            16, DIRECT_TEST_FS, &configs).unwrap();
        assert_eq!(mixed.direct.len(), 1);
        assert!(mixed.direct.contains_key(&2));
        assert_eq!(mixed.bank.clocks().unwrap().len(), 1);
        let old_id = mixed.direct[&2].id;
        let decimate = mixed.direct[&2].decimate;
        let primary_id = mixed.channel_registry()[0].channel_id;
        let mut expected_frames = Vec::new();
        let mut mixed_frames = Vec::new();
        baseline.resume_fine().unwrap();
        mixed.resume_fine().unwrap();
        for _ in 0..8 {
            expected_frames.extend(baseline.packet().unwrap());
            mixed_frames.extend(mixed.packet().unwrap());
        }
        expected_frames.extend(baseline.suspend_fine().unwrap());
        mixed_frames.extend(mixed.suspend_fine().unwrap());
        let retune_origin = mixed.input_pos;
        let retune_seq = mixed.next_seq;
        configs[1].freq += 100_000.0;
        let retired = mixed.configure_service_channel(configs[1].clone()).unwrap();
        assert!(retired.iter().any(|frame| frame.starts_with(b"IQAX") && frame.len() > 56
            && u32::from_le_bytes(frame[32..36].try_into().unwrap()) as u64 == old_id));
        mixed_frames.extend(retired);
        let new_id = mixed.direct[&2].id;
        assert_ne!(new_id, old_id);
        assert_eq!(mixed.direct[&2].origin, retune_origin);
        assert_eq!(mixed.direct[&2].next_seq, retune_seq);
        assert_eq!(mixed.channel_registry()[0].channel_id, primary_id);
        assert_eq!(mixed.channel_registry()[1].frequency_hz, configs[1].freq);
        baseline.resume_fine().unwrap();
        mixed.resume_fine().unwrap();
        for _ in 0..8 {
            expected_frames.extend(baseline.packet().unwrap());
            mixed_frames.extend(mixed.packet().unwrap());
        }
        expected_frames.extend(baseline.suspend_fine().unwrap());
        mixed_frames.extend(mixed.suspend_fine().unwrap());
        configs[1].enabled = false;
        let disabled = mixed.configure_service_channel(configs[1].clone()).unwrap();
        assert!(disabled.iter().any(|frame| frame.starts_with(b"IQAX") && frame.len() > 56
            && u32::from_le_bytes(frame[32..36].try_into().unwrap()) as u64 == new_id));
        mixed_frames.extend(disabled);
        assert!(mixed.direct.is_empty());
        assert_eq!(mixed.channel_registry().iter().map(|row| row.channel).collect::<Vec<_>>(), vec![1]);
        let after_disable = mixed_frames.len();
        for _ in 0..8 {
            expected_frames.extend(baseline.packet().unwrap());
            mixed_frames.extend(mixed.packet().unwrap());
        }
        finish(&mut baseline, &mut expected_frames);
        finish(&mut mixed, &mut mixed_frames);
        assert!(mixed_frames[after_disable..].iter().all(|frame| !frame.starts_with(b"IQAX")));
        for (id, origin) in [(old_id, 0), (new_id, retune_origin)] {
            let frames: Vec<_> = mixed_frames.iter().filter(|frame| frame.starts_with(b"IQAX")
                && u32::from_le_bytes(frame[32..36].try_into().unwrap()) as u64 == id).collect();
            assert!(frames.len() >= 2, "each identity needs a marker and its flushed tail");
            let mut samples = 0;
            for (seq, frame) in frames.into_iter().enumerate() {
                assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize, seq);
                let count = u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64;
                assert_eq!(count == 0, seq == 0);
                assert_eq!(u64::from_le_bytes(frame[40..48].try_into().unwrap()),
                    origin + (samples + 1) * decimate - 1);
                samples += count;
            }
            assert_eq!(samples, 8 * DIRECT_TEST_PACKET / decimate);
        }
        let narrow: Vec<_> = mixed_frames.iter().filter(|frame| frame.starts_with(b"IQBF")).collect();
        assert!(!narrow.is_empty());
        assert_eq!(narrow.len(), expected_frames.len());
        for (actual, expected) in narrow.into_iter().zip(&expected_frames) {
            // Each independent plan allocates an epoch; all signal bytes and clocks must match.
            assert_eq!(&actual[..36], &expected[..36]);
            assert_eq!(&actual[40..], &expected[40..]);
            assert_eq!(u32::from_le_bytes(actual[36..40].try_into().unwrap()) as u64, mixed.stream.epoch());
        }
        assert_eq!(mixed.bank.clocks().unwrap(), baseline.bank.clocks().unwrap());
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

    fn finish(ddc: &mut Ddc, frames: &mut IqFrames) {
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
    fn product_geometry_is_independent_of_cpu_and_output_predictions() {
        let profile=websa_channelizer::FineCostBudget::new([1e12;23],2.0,1.0).unwrap();
        assert_eq!(check_plan_admission(&profile,3,2,1,COARSE_RATE).unwrap_err(),"fine channel budget exceeded");
        assert!(check_plan_admission(&profile,1,2,2,1.0).is_err());
        check_plan_admission(&profile,3,3,3,1.0).unwrap();
        let ddc=Ddc::configured_with_cost_budget(100e6,100e6,3,50e6,&mut None,
            &[100e6+BIN_SPACING,100e6-BIN_SPACING],16,IQ_FS,3,1.0,profile).unwrap();
        assert_eq!(ddc.channel_registry().len(),1);
    }

    #[test]
    fn predictions_do_not_gate_legacy_control_and_geometry_errors_keep_contiguous_output() {
        let build=||Ddc::configured_with_cost_budget(100e6,100e6,3,50e6,&mut None,
            &[100e6+BIN_SPACING],16,IQ_FS,4,1.0,
            websa_channelizer::FineCostBudget::new([1e12;23],2.0,1.0).unwrap()).unwrap();
        let mut expected=build();let mut reference=Vec::new();
        for _ in 0..48 {reference.extend(expected.packet().unwrap());}finish(&mut expected,&mut reference);
        let mut actual=build();actual.resume_fine().unwrap();let mut frames=Vec::new();
        for _ in 0..24 {frames.extend(actual.packet().unwrap());}frames.extend(actual.suspend_fine().unwrap());
        let before=actual.channel_registry();
        assert!(actual.tune(100e6,1).is_err());
        assert!(Arc::ptr_eq(&before,&actual.channel_registry()));
        actual.resume_fine().unwrap();for _ in 0..24 {frames.extend(actual.packet().unwrap());}finish(&mut actual,&mut frames);
        assert_eq!(frames.len(),reference.len());
        for (a,b) in frames.iter().zip(reference) {assert_eq!(&a[..36],&b[..36]);assert_eq!(&a[40..],&b[40..]);}
        let mut allowed=build();allowed.configure_aux(2,true,100e6+BIN_SPACING).unwrap();
        allowed.configure_aux(3,true,100e6-BIN_SPACING).unwrap();
        allowed.tune(100e6+1000.0,2).unwrap();
        allowed.replan_with_aux(100e6,2,&[(2,100e6+BIN_SPACING)]).unwrap();
        let front_end=DevState::new(json!({"preset_defaults":{}}),json!({})).front_end(0.0);
        allowed.recapture(100e6,100e6,2,32,front_end,16).unwrap();
        assert!(!allowed.capture_failed);
    }

    #[test]
    fn producer_disconnect_delivers_queued_tail_before_terminal_error() {
        let mut queue = DeliveryQueue::new(1);
        let first = queued_iq(1);
        let second = queued_iq(2);
        assert!(queue.push(Ok(PipelineOutput { frames:vec![second.clone()], pan:Vec::new() })).is_empty());
        queue.closed = true;
        let mut failure = None;
        let pending = PipelineOutput { frames:vec![first.clone()], pan:Vec::new() };
        let output = queue.collect(pending, &mut failure).unwrap();
        assert_eq!(output.frames, vec![first, second]);
        assert_eq!(queue.collect(PipelineOutput::default(), &mut failure).err().as_deref(), Some("pipeline output disconnected"));
    }

    #[test]
    fn explicit_producer_error_preserves_tail_and_original_failure() {
        let mut queue = DeliveryQueue::new(1);
        let first = queued_iq(1);
        assert!(queue.push(Ok(PipelineOutput { frames:vec![first.clone()], pan:Vec::new() })).is_empty());
        assert!(queue.push(Err("SDK reconfigure failed".into())).is_empty());
        let mut failure = None;
        assert_eq!(queue.collect(PipelineOutput::default(), &mut failure).unwrap().frames, vec![first]);
        assert_eq!(queue.collect(PipelineOutput::default(), &mut failure).err().as_deref(), Some("SDK reconfigure failed"));
    }

    #[test]
    fn empty_live_producer_is_not_a_terminal_failure() {
        let mut queue = DeliveryQueue::new(1);
        let mut failure = None;
        let output = queue.collect(PipelineOutput::default(), &mut failure).unwrap();
        assert!(output.frames.is_empty());
        assert!(failure.is_none());
    }

    fn queued_iq(seq: u32) -> IqFrame {
        IqFrame::native(websa_channelizer::BasebandMagic::Primary, seq, 48_000.0,
            100e6, 1, 9, u64::from(seq) * 64, 0.0, IQ_FS, vec![0.25, 0.5]).unwrap()
    }

    #[test]
    fn capture_grid_changes_expire_only_old_pan() {
        let ddc=plan();
        let mut snapshot = ddc.pipeline_snapshot(Arc::from([]));
        assert_eq!(snapshot.input_rate,ddc.stream.input_sample_rate());
        assert_eq!(snapshot.epoch,ddc.stream.epoch());
        assert_ne!(snapshot.input_rate,snapshot.rate);
        let previous = (snapshot.capture_center, snapshot.device_decimate, snapshot.generation);
        let mut output = PipelineOutput { frames:vec![vec![1].into(), vec![2].into()], pan:vec![(vec![100e6], vec![-40.0], 0, 0, 1, 0)] };
        // A DSP-only change retains spectra on the same device frequency grid.
        snapshot.rate /= 2.0;
        output.capture_boundary(previous, &snapshot);
        assert_eq!(output.pan, vec![(vec![100e6], vec![-40.0], 0, 0, 1, 0)]);
        snapshot.capture_center = 200e6;
        output.capture_boundary(previous, &snapshot);
        assert!(output.pan.is_empty());
        assert_eq!(output.frames, vec![vec![1], vec![2]]);
        snapshot.capture_center = previous.0;
        snapshot.device_decimate = previous.1 + 1;
        output.pan.push((vec![100e6], vec![-40.0], 0, 0, 1, 0));
        output.capture_boundary(previous, &snapshot);
        assert!(output.pan.is_empty());
        assert_eq!(output.frames, vec![vec![1], vec![2]]);
        snapshot.device_decimate = previous.1;
        snapshot.generation += 1;
        output.pan.push((vec![100e6], vec![-40.0], -12, previous.2, 1, 0));
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
