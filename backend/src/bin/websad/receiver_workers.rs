pub(super) use metrics::cpu_now;
use super::{sdr_pipeline::ChannelRegistryEntry, MAX_CHANNELS};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use websa_channelizer::{encode_audio, encode_decode_event, AudioFrameMetadata, MAX_BASEBAND_SAMPLES, IqInput, IqSamples};
use websa_channelizer::receiver::{AudioBlock, Ft8Job, Ft8Result, InputClock, PreparedDecoder, Receiver, ReceiverConfig, ReceiverEvent, SamplePosition};

#[path = "receiver_metrics.rs"]
mod metrics;

pub(super) const WORKERS: usize = MAX_CHANNELS;
pub(super) fn worker_for(channel:u32)->usize {channel.saturating_sub(1) as usize}
// Item protection plus real retained bytes/media duration, including execution.
const INPUT_CAPACITY: usize = 64;
const INPUT_BYTES_PER_CHANNEL:u64=4*1024*1024;
const INPUT_MEDIA_NS_PER_CHANNEL:u64=2_000_000_000;
const RESULT_CAPACITY: usize = 32;
// Two pending protocol windows per UID, independent of the number of receivers.
const SEARCH_CAPACITY: usize = 2;
const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
const OUTPUT_BYTES_PER_CHANNEL:u64=8*1024*1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ChannelConfig {
    pub channel: u32,
    pub enabled: bool,
    #[serde(skip)]
    pub recovery_settle:bool,
    pub continuity: super::reception_recovery::Policy,
    /// Independent baseband; None retains the legacy mode-derived plan.
    pub baseband: Option<BasebandConfig>,
    pub processing_enabled: bool,
    pub freq: f64,
    pub mode: String,
    pub am_sync: bool,
    pub am_sync_generation: u32,
    pub if_bw: f64,
    pub squelch: f64,
    pub volume: f64,
    pub agc: bool,
    pub pitch: f64,
    pub deemph_us: f64,
    pub nr: bool,
    pub nr_strength: f64,
    pub nr_algorithm: String,
    pub nr_atten_db: f64,
    pub stereo: bool,
    pub audio_rate: u32,
    pub sstv_mode: Option<usize>,
    pub radiofax_acquisition: String,
    pub radiofax_ioc: String,
    pub radiofax_lpm: String,
    pub radiofax_modulation: String,
    pub radiofax_polarity: String,
    pub radiofax_clock_recovery: String,
    pub radiofax_phase_pixels: f32,
    pub radiofax_clock_ppm: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BasebandConfig {
    pub passband_hz: f64,
    pub transition_hz: f64,
    pub minimum_rate_hz: f64,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            channel: 1, enabled: true, recovery_settle:false, continuity: Default::default(), baseband: None, processing_enabled: true,
            freq: 0.0, mode: "am".into(), if_bw: 6000.0,
            am_sync: false,
            am_sync_generation: 0,
            squelch: -110.0, volume: 0.8, agc: true, pitch: 700.0, deemph_us: -1.0,
            nr: false, nr_strength: 0.6, nr_algorithm: "wiener".into(), nr_atten_db: 10.0,
            stereo: true, audio_rate: 48_000, sstv_mode: None,
            radiofax_acquisition: "auto".into(), radiofax_ioc: "auto".into(),
            radiofax_lpm: "auto".into(), radiofax_modulation: "fm".into(),
            radiofax_polarity: "normal".into(), radiofax_clock_recovery: "auto".into(),
            radiofax_phase_pixels: 0.0, radiofax_clock_ppm: 0.0,
        }
    }
}

impl ChannelConfig {
    pub fn for_slot(slot: u32) -> Self {
        Self { channel: slot, enabled: slot == 1, ..Self::default() }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if let Some(baseband) = &self.baseband {
            websa_channelizer::rate_plan::RatePlan::baseband(62_500_000.0,
                baseband.passband_hz, baseband.transition_hz, baseband.minimum_rate_hz)
                .map_err(str::to_owned)?;
        }
        if !(1..=MAX_CHANNELS as u32).contains(&self.channel)
            || !self.freq.is_finite() || !self.if_bw.is_finite() || self.if_bw <= 0.0
            || !self.squelch.is_finite() || !self.volume.is_finite() || !(0.0..=2.0).contains(&self.volume)
            || !self.pitch.is_finite() || self.pitch <= 0.0 || !self.deemph_us.is_finite()
            || (self.mode == "cw" && !(100.0..=1600.0).contains(&self.pitch))
            || !self.nr_strength.is_finite() || !(0.0..=1.0).contains(&self.nr_strength)
            || !self.nr_atten_db.is_finite() || !(0.0..=100.0).contains(&self.nr_atten_db)
            || websa_channelizer::nr::NrAlgorithm::parse(&self.nr_algorithm).is_none()
            || !(8_000..=192_000).contains(&self.audio_rate)
            || self.sstv_mode.is_some_and(|index| index >= websa_dsp::digital::sstv::SSTV_MODES.len())
            || !matches!(self.radiofax_acquisition.as_str(), "auto" | "manual")
            || !matches!(self.radiofax_ioc.as_str(), "auto" | "288" | "576")
            || !matches!(self.radiofax_lpm.as_str(), "auto" | "60" | "90" | "120" | "180" | "240")
            || !matches!(self.radiofax_modulation.as_str(), "fm" | "am")
            || !matches!(self.radiofax_polarity.as_str(), "normal" | "inverted")
            || !matches!(self.radiofax_clock_recovery.as_str(), "auto" | "off")
            || !self.radiofax_phase_pixels.is_finite() || !(-4_096.0..=4_096.0).contains(&self.radiofax_phase_pixels)
            || !self.radiofax_clock_ppm.is_finite() || !(-5_000.0..=5_000.0).contains(&self.radiofax_clock_ppm)
            || (self.radiofax_acquisition == "manual" && (self.radiofax_ioc == "auto" || self.radiofax_lpm == "auto"))
            || (self.radiofax_acquisition == "auto" && self.radiofax_lpm == "180")
        {
            return Err(format!("invalid receiver configuration for channel {}", self.channel));
        }
        if self.processing_enabled {websa_channelizer::catalog::validate(&self.mode, self.if_bw)?;}
        if self.processing_enabled && matches!(self.mode.as_str(), "ais" | "aprs" | "ft4" | "wspr" | "meteor" | "adsb") {
            websa_channelizer::external::onair::validate_frequency(&self.mode, self.freq.round() as u64)?;
        }
        Ok(())
    }

    pub(super) fn resets_receiver(&self, next: &Self) -> bool {
        self.enabled != next.enabled || self.freq != next.freq || self.mode != next.mode
            || self.baseband != next.baseband || self.processing_enabled != next.processing_enabled
            || self.if_bw != next.if_bw || self.pitch != next.pitch || self.sstv_mode != next.sstv_mode
            || self.radiofax_acquisition != next.radiofax_acquisition || self.radiofax_ioc != next.radiofax_ioc
            || self.radiofax_lpm != next.radiofax_lpm || self.radiofax_modulation != next.radiofax_modulation
            || self.radiofax_polarity != next.radiofax_polarity || self.radiofax_clock_recovery != next.radiofax_clock_recovery
            || self.audio_rate != next.audio_rate || self.stereo != next.stereo
    }

    fn receiver_config(&self, input_rate: f64) -> ReceiverConfig {
        ReceiverConfig {
            mode: self.mode.clone(), input_rate, audio_rate: self.audio_rate, if_bw: self.if_bw,
            center_hz: self.freq.max(0.0).round() as u64,
            am_sync: self.am_sync,
            am_sync_generation: self.am_sync_generation,
            pitch: self.pitch, agc: self.agc, gain: self.volume as f32, squelch_dbm: self.squelch,
            nr: self.nr, nr_strength: self.nr_strength, nr_algorithm: self.nr_algorithm.clone(),
            nr_atten_db: self.nr_atten_db as f32, deemph_us: self.deemph_us, stereo: self.stereo,
            sstv_mode: self.sstv_mode,
            radiofax_acquisition: self.radiofax_acquisition.clone(),
            radiofax_ioc: self.radiofax_ioc.clone(),
            radiofax_lpm: self.radiofax_lpm.clone(),
            radiofax_modulation: self.radiofax_modulation.clone(),
            radiofax_polarity: self.radiofax_polarity.clone(),
            radiofax_clock_recovery: self.radiofax_clock_recovery.clone(),
            radiofax_phase_pixels: self.radiofax_phase_pixels,
            radiofax_clock_ppm: self.radiofax_clock_ppm,
        }
    }
}

struct PreparedChannel {
    config: ChannelConfig,
    decoder: Option<PreparedDecoder>,
}

/// Admission owns these processes until sequence zero transfers them to a worker.
pub(super) struct PreparedDecoders {
    selected: Vec<bool>,
    channels: Vec<Option<PreparedChannel>>,
    previous: Vec<Option<PreparedChannel>>,
    rollback: bool,
}

impl Default for PreparedDecoders {
    fn default() -> Self {
        Self {selected:vec![false;MAX_CHANNELS],channels:(0..MAX_CHANNELS).map(|_|None).collect(),
            previous:(0..MAX_CHANNELS).map(|_|None).collect(),rollback:false}
    }
}

impl PreparedDecoders {
    pub fn new(configs: &[ChannelConfig]) -> Result<Self, String> {
        Self::prepare_with(configs, |config| {
            if websa_channelizer::catalog::is_external(&config.mode) {
                PreparedDecoder::new(&config.mode, config.freq.max(0.0).round() as u64).map(Some)
            } else { Ok(None) }
        })
    }

    fn prepare_with(configs: &[ChannelConfig], mut prepare: impl FnMut(&ChannelConfig) -> Result<Option<PreparedDecoder>, String>) -> Result<Self, String> {
        let mut result = Self::default();
        let mut seen = vec![false; MAX_CHANNELS];
        for config in configs {
            config.validate()?;
            let slot = config.channel as usize - 1;
            if seen[slot] { return Err(format!("duplicate receiver channel {}", config.channel)); }
            seen[slot] = true;
            if config.enabled && config.processing_enabled {
                let decoder = prepare(config)?;
                result.selected[slot] = true;
                result.channels[slot] = Some(PreparedChannel { config: config.clone(), decoder });
            }
        }
        Ok(result)
    }

    fn matches(&self, slot: usize, config: &ChannelConfig) -> bool {
        self.previous[slot].as_ref().into_iter().chain(self.channels[slot].as_ref())
            .any(|ready| !ready.config.resets_receiver(config))
    }

    fn take(&mut self, slot: usize, config: &ChannelConfig) -> Option<PreparedChannel> {
        if self.previous[slot].as_ref().is_some_and(|ready| !ready.config.resets_receiver(config)) {
            self.previous[slot].take()
        } else if self.channels[slot].as_ref().is_some_and(|ready| !ready.config.resets_receiver(config)) {
            self.channels[slot].take()
        } else { None }
    }
}

#[derive(Default)]
pub(super) struct ReceiverFrames {
    pub audio_configs: Vec<Arc<ChannelConfig>>,
    pub audio: Vec<Vec<u8>>,
    pub decode: Vec<Vec<u8>>,
    pub faults:Vec<BranchFault>,
    pub retired:Vec<RetiredFrames>,
}

pub(super) struct RetiredFrames {pub identity:u64,pub channel:u32,pub decode_offset:usize,pub frames:ReceiverFrames}

impl ReceiverFrames {
    pub(super) fn append(&mut self, mut other: Self) {
        for tail in &mut other.retired {tail.decode_offset+=self.decode.len();}
        self.audio.append(&mut other.audio);
        self.audio_configs.append(&mut other.audio_configs);
        self.decode.append(&mut other.decode);
        self.faults.append(&mut other.faults);
        self.retired.append(&mut other.retired);
    }
}

#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    completed: AtomicU64,
    submitted_samples: AtomicU64,
    completed_samples: AtomicU64,
    queue_full: AtomicU64,
    audio_frames: AtomicU64,
    audio_samples: AtomicU64,
    decode_frames: AtomicU64,
    search_submitted: AtomicU64,
    search_completed: AtomicU64,
    chain_count: AtomicU64,
    chain_total_ns: AtomicU64,
    chain_max_ns: AtomicU64,
}

#[derive(Default)]
struct WorkerCounters {
    busy_ns:AtomicU64, jobs:AtomicU64, max_ns:AtomicU64, wait_ns:AtomicU64, wait_max_ns:AtomicU64,
    peak_samples:AtomicU64, peak_jobs:AtomicU64,
}
struct QueueEntry {worker:usize, at:Instant, samples:u64}
struct QueueDiagnostics {
    next:AtomicU64,
    queued:Mutex<std::collections::BTreeMap<u64,QueueEntry>>,
    workers:Vec<WorkerCounters>,
}
impl Default for QueueDiagnostics {
    fn default()->Self {Self {next:AtomicU64::new(0),queued:Mutex::new(Default::default()),
        workers:(0..WORKERS*2).map(|_|WorkerCounters::default()).collect()}}
}

struct Shared {
    stopping: AtomicBool,
    generations: Vec<AtomicU64>,
    pending: AtomicUsize,
    failure: Mutex<Option<String>>,
    counters: Counters,
    diagnostics:QueueDiagnostics,
    metrics: metrics::Directory,
    branches:Vec<Branch>,
}
#[derive(Clone,Debug,Serialize)]
pub(super) struct BranchFault {pub channel:u32,pub generation:u64,pub error:String,pub kind:super::reception_recovery::FaultKind,pub receiver_uid:Option<String>,pub service_id:Option<String>,
    pub last_success:serde_json::Value,pub last_delivered:serde_json::Value,pub first_unavailable:serde_json::Value,pub gap_end:Option<u64>}
struct Branch {
    lifecycle:AtomicUsize, // 0 absent, 1 active, 2 retiring; only its owner can finish.
    failure:Mutex<Option<BranchFault>>,
    notice:AtomicBool,
    binding:Mutex<Option<(String,String)>>,
    output_pending:AtomicUsize,
    output_bytes:AtomicU64,
    close:Mutex<Option<(u64,bool)>>,
    mailbox:Mutex<Option<mpsc::Receiver<PendingWork>>>,
    state_generation:AtomicU64,
    preparing:AtomicBool,
    recovery_floor:AtomicU64,
    input_bytes:AtomicU64,
    input_media_ns:AtomicU64,
    controls:super::receiver_controls::Log,
}
impl Default for Branch {
    fn default()->Self {Self {lifecycle:AtomicUsize::new(0),failure:Mutex::new(None),notice:AtomicBool::new(false),binding:Mutex::new(None),
        output_pending:AtomicUsize::new(0),output_bytes:AtomicU64::new(0),close:Mutex::new(None),mailbox:Mutex::new(None),state_generation:AtomicU64::new(0),preparing:AtomicBool::new(false),recovery_floor:AtomicU64::new(0),input_bytes:AtomicU64::new(0),input_media_ns:AtomicU64::new(0),controls:Default::default()}}
}
struct InputPermit {shared:Arc<Shared>,slot:usize,bytes:u64,media:u64}
impl InputPermit {
    fn reserve(shared:&Arc<Shared>,slot:usize,samples:usize,rate:f64)->Result<Self,String> {
        let branch=&shared.branches[slot];let bytes=(samples*8) as u64;
        let media=(samples as f64/rate*1e9).ceil() as u64;
        branch.input_bytes.try_update(Ordering::AcqRel,Ordering::Acquire,
            |held|held.checked_add(bytes).filter(|total|*total<=INPUT_BYTES_PER_CHANNEL))
            .map_err(|_|"receiver input queue failure: retained input including execution exceeds 4 MiB per UID".to_owned())?;
        if branch.input_media_ns.try_update(Ordering::AcqRel,Ordering::Acquire,
            |held|held.checked_add(media).filter(|total|*total<=INPUT_MEDIA_NS_PER_CHANNEL)).is_err() {
            branch.input_bytes.fetch_sub(bytes,Ordering::AcqRel);
            return Err("receiver input queue failure: retained input including execution exceeds two seconds of actual media per UID".into());
        }
        Ok(Self {shared:shared.clone(),slot,bytes,media})
    }
}
impl Drop for InputPermit {fn drop(&mut self) {
    self.shared.branches[self.slot].input_bytes.fetch_sub(self.bytes,Ordering::AcqRel);
    self.shared.branches[self.slot].input_media_ns.fetch_sub(self.media,Ordering::AcqRel);
}}
struct Preparing<'a>(&'a AtomicBool);
impl Drop for Preparing<'_> {fn drop(&mut self) {self.0.store(false,Ordering::Release);}}

impl Shared {
    fn fail_branch(&self,slot:usize,error:String) {
        self.fail_generation(slot,self.generations[slot].load(Ordering::Acquire),error);
    }
    fn fail_generation(&self,slot:usize,generation:u64,error:String) {
        if self.generations[slot].load(Ordering::Acquire)!=generation {return;}
        let branch=&self.branches[slot];
        let mut failure=branch.failure.lock().unwrap_or_else(|e|e.into_inner());
        if failure.as_ref().is_none_or(|fault|fault.generation!=generation) {
            let binding=branch.binding.lock().unwrap_or_else(|e|e.into_inner());
            let rows=self.metrics.snapshot();
            let row=rows.as_array().and_then(|rows|rows.iter().rev().find(|row|row["channel"]==slot+1
                && row["processing_generation"]==generation.to_string()));
            *failure=Some(BranchFault {channel:slot as u32+1,generation,kind:super::reception_recovery::FaultKind::processing(&error),error,
                receiver_uid:binding.as_ref().map(|b|b.0.clone()),service_id:binding.as_ref().map(|b|b.1.clone()),
                last_delivered:row.map_or(serde_json::Value::Null,|row|row["watermarks"]["consumed"].clone()),
                last_success:row.map_or(serde_json::Value::Null,|row|row["watermarks"]["succeeded"].clone()),
                first_unavailable:row.map_or(serde_json::Value::Null,|row| {
                    if !row["first_rejected"].is_null() {row["first_rejected"].clone()} else {row["gap"]["first_failed"].clone()}
                }),gap_end:None});
            branch.notice.store(true,Ordering::Release);
        }
        branch.lifecycle.store(2,Ordering::Release);
    }
    fn branch_failure(&self,slot:usize)->Option<BranchFault> {
        self.branches[slot].failure.lock().unwrap_or_else(|e|e.into_inner()).clone()
            .filter(|failure|failure.generation==self.generations[slot].load(Ordering::Acquire))
    }
    fn publish(self:&Arc<Self>,sender:&mpsc::SyncSender<Output>,mut output:Output)->Result<(),String> {
        let slot=output.slot;
        let bytes=output.frames.audio.iter().chain(&output.frames.decode).map(|frame|frame.len() as u64).sum::<u64>();
        self.branches[slot].output_bytes.try_update(Ordering::AcqRel,Ordering::Acquire,
            |held|held.checked_add(bytes).filter(|sum|*sum<=OUTPUT_BYTES_PER_CHANNEL))
            .map_err(|_|"per-channel receiver output exceeds 8 MiB pending payload; no samples were skipped to resume".to_owned())?;
        if self.branches[slot].output_pending.try_update(Ordering::AcqRel,Ordering::Acquire,
            |count|(count<RESULT_CAPACITY).then_some(count+1)).is_err() {
            self.branches[slot].output_bytes.fetch_sub(bytes,Ordering::AcqRel);
            return Err("per-channel receiver output queue full".into());
        }
        output.permit=Some(OutputPermit {shared:self.clone(),slot,bytes});
        sender.try_send(output).map_err(|error|format!("receiver output queue failure: {error}"))
    }
    fn check(&self) -> Result<(), String> {
        match self.failure.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn current(&self, slot: usize, generation: u64) -> bool {
        !self.stopping.load(Ordering::Acquire) && self.generations[slot].load(Ordering::Acquire) == generation
            && self.branch_failure(slot).is_none()
    }
}

struct Pending {shared:Arc<Shared>, queue_id:Option<u64>, tracking:Option<metrics::Job>}

impl Pending {
    fn new(shared: &Arc<Shared>) -> Self {
        shared.pending.fetch_add(1, Ordering::AcqRel);
        Self {shared:Arc::clone(shared),queue_id:None,tracking:None}
    }
    fn queued(shared:&Arc<Shared>,worker:usize,samples:u64)->Self {
        let mut pending=Self::new(shared);
        pending.enqueue(worker,samples);pending
    }
    fn enqueue(&mut self,worker:usize,samples:u64) {
        let shared=&self.shared;
        let id=shared.diagnostics.next.fetch_add(1,Ordering::Relaxed);
        let mut queue=shared.diagnostics.queued.lock().unwrap_or_else(|e|e.into_inner());
        queue.insert(id,QueueEntry {worker,at:Instant::now(),samples});
        let count=queue.values().filter(|entry|entry.worker==worker).count() as u64;
        let samples=queue.values().filter(|entry|entry.worker==worker).map(|e|e.samples).sum();
        let counters=&shared.diagnostics.workers[worker];
        counters.peak_jobs.fetch_max(count,Ordering::Relaxed);
        counters.peak_samples.fetch_max(samples,Ordering::Relaxed);
        self.queue_id=Some(id);
    }
    fn started(&mut self) {
        if let Some(id)=self.queue_id.take() {
            if let Some(entry)=self.shared.diagnostics.queued.lock().unwrap_or_else(|e|e.into_inner()).remove(&id) {
                let wait=entry.at.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                let counters=&self.shared.diagnostics.workers[entry.worker];
                counters.wait_ns.fetch_add(wait,Ordering::Relaxed);
                counters.wait_max_ns.fetch_max(wait,Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(id)=self.queue_id {self.shared.diagnostics.queued.lock().unwrap_or_else(|e|e.into_inner()).remove(&id);}
        self.shared.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PendingWork {
    work: Work,
    pending: Pending,
}

struct Packet {
    slot: usize,
    generation: u64,
    config: Arc<ChannelConfig>,
    channel_id: u32,
    epoch: u32,
    seq: u32,
    rate: f64,
    frequency: f64,
    clock: InputClock,
    power_dbm: Option<f64>,
    iq: Arc<IqSamples>,
    radiofax_new_page: bool,
    prepared: Option<PreparedDecoder>,
    metrics: Option<Arc<metrics::Lane>>,
    input_permit:Option<InputPermit>,
    commands:Vec<super::receiver_controls::Command>,
}

struct SearchJob {
    slot: usize,
    generation: u64,
    channel_id: u32,
    epoch: u32,
    job: Ft8Job,
    pending: Pending,
}

struct SearchResult {
    slot: usize,
    generation: u64,
    channel_id: u32,
    epoch: u32,
    result: Ft8Result,
}

enum Work {
    Packet(Packet),
    Search(SearchResult),
    FinishRadiofax { slot:usize, generation:u64, live:bool },
    Deactivate { slot:usize, generation:u64, reply:mpsc::SyncSender<()> },
}

struct Output {
    slot: usize,
    generation: u64,
    frames: ReceiverFrames,
    squelch: Option<SquelchState>,
    accounting: Option<metrics::OutputHold>,
    permit:Option<OutputPermit>,
    retired:bool,
    stream:Option<(u32,u32)>,
}
struct OutputPermit {shared:Arc<Shared>,slot:usize,bytes:u64}
impl Drop for OutputPermit {
    fn drop(&mut self) {
        self.shared.branches[self.slot].output_pending.fetch_sub(1,Ordering::AcqRel);
        self.shared.branches[self.slot].output_bytes.fetch_sub(self.bytes,Ordering::AcqRel);
    }
}

#[derive(Clone, Copy)]
struct InputLevel {
    channel_id: u32,
    epoch: u32,
    generation: u64,
    measured: Instant,
    dbfs: f64,
}

#[derive(Clone, Copy)]
struct SquelchState {
    channel_id: u32,
    epoch: u32,
    generation: u64,
    measured: Instant,
    open: bool,
    gain: f64,
}

pub(super) struct ReceiverWorkers {
    configs: Vec<Arc<ChannelConfig>>,
    prepared: PreparedDecoders,
    active: Vec<Option<(u64, u32, u32)>>,
    power: Vec<Option<f64>>,
    levels: Vec<Option<InputLevel>>,
    radiofax_new_page: Vec<bool>,
    squelch: Vec<Option<SquelchState>>,
    inputs: Vec<mpsc::SyncSender<PendingWork>>,
    outputs: mpsc::Receiver<Output>,
    deferred: Vec<Output>,
    threads: Vec<Option<JoinHandle<()>>>,
    input_rxs:Vec<Option<mpsc::Receiver<PendingWork>>>,
    search_inputs:Vec<mpsc::SyncSender<SearchJob>>,
    search_rxs:Vec<Option<mpsc::Receiver<SearchJob>>>,
    result_inputs:Vec<mpsc::SyncSender<PendingWork>>,
    result_rxs:Vec<Option<mpsc::Receiver<PendingWork>>>,
    output_tx:mpsc::SyncSender<Output>,
    shared: Arc<Shared>,
    bindings: Vec<Option<(String,String)>>,
    controls:Vec<std::collections::VecDeque<super::receiver_controls::Command>>,
}

impl ReceiverWorkers {
    pub fn stop_failed(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        for generation in &self.shared.generations {generation.fetch_add(1, Ordering::AcqRel);}
        self.inputs.clear();
        self.prepared = PreparedDecoders::default();
        self.active = vec![None; MAX_CHANNELS];
        self.levels=vec![None;MAX_CHANNELS];
        self.squelch=vec![None;MAX_CHANNELS];
    }

    pub fn stopped(&self)->bool {self.threads.iter().flatten().all(JoinHandle::is_finished)}

    pub fn new() -> Result<Self, String> {
        let shared = Arc::new(Shared {
            stopping: AtomicBool::new(false), generations: (0..MAX_CHANNELS).map(|_| AtomicU64::new(1)).collect(),
            pending: AtomicUsize::new(0), failure: Mutex::new(None), counters: Counters::default(), diagnostics:QueueDiagnostics::default(), metrics:metrics::Directory::default(),branches:(0..MAX_CHANNELS).map(|_|Branch::default()).collect(),
        });
        let (output_tx, outputs) = mpsc::sync_channel(RESULT_CAPACITY*MAX_CHANNELS);
        let mut inputs=Vec::new();let mut input_rxs=Vec::new();
        let mut search_inputs=Vec::new();let mut search_rxs=Vec::new();
        let mut result_inputs=Vec::new();let mut result_rxs=Vec::new();
        for _ in 0..MAX_CHANNELS {
            let (tx,rx)=mpsc::sync_channel(INPUT_CAPACITY);inputs.push(tx);input_rxs.push(Some(rx));
            let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);search_inputs.push(tx);search_rxs.push(Some(rx));
            let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);result_inputs.push(tx);result_rxs.push(Some(rx));
        }
        Ok(Self {
            configs: (1..=MAX_CHANNELS as u32).map(|slot| Arc::new(ChannelConfig::for_slot(slot))).collect(),
            prepared: PreparedDecoders::default(), active: vec![None; MAX_CHANNELS],
            power:vec![None;MAX_CHANNELS],levels:vec![None;MAX_CHANNELS],squelch:vec![None;MAX_CHANNELS],
            radiofax_new_page:vec![false;MAX_CHANNELS],inputs,input_rxs,search_inputs,search_rxs,result_inputs,result_rxs,
            output_tx,outputs,deferred:Vec::new(),threads:(0..MAX_CHANNELS*2).map(|_|None).collect(),
            shared,bindings:vec![None;MAX_CHANNELS],controls:(0..MAX_CHANNELS).map(|_|Default::default()).collect(),
        })
    }

    fn ensure_owner(&mut self,slot:usize,searching:bool)->Result<(),String> {
        if (self.shared.branches[slot].lifecycle.load(Ordering::Acquire)==2 || self.threads[slot].as_ref().is_some_and(JoinHandle::is_finished))
            && [slot,WORKERS+slot].iter().all(|&index|self.threads[index].as_ref().is_none_or(JoinHandle::is_finished)) {
            if let Some(handle)=self.threads[slot].take() {let _=handle.join();}
            if self.threads[WORKERS+slot].as_ref().is_some_and(|h|!h.is_finished()) {
                return Err(format!("channel {} search still retiring; retry after completion",slot+1));
            }
            if let Some(handle)=self.threads[WORKERS+slot].take() {let _=handle.join();}
            let (tx,rx)=mpsc::sync_channel(INPUT_CAPACITY);self.inputs[slot]=tx;self.input_rxs[slot]=Some(rx);
            let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);self.search_inputs[slot]=tx;self.search_rxs[slot]=Some(rx);
            let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);self.result_inputs[slot]=tx;self.result_rxs[slot]=Some(rx);
            self.shared.branches[slot].lifecycle.store(0,Ordering::Release);
        }
        if self.shared.branches[slot].lifecycle.load(Ordering::Acquire)==2 {
            return Err(format!("channel {} owner still retiring; retry after completion",slot+1));
        }
        if self.threads[slot].is_none() {
            let rx=self.input_rxs[slot].take().ok_or("receiver input owner absent")?;
            let results=self.result_rxs[slot].take().ok_or("receiver search return owner absent")?;
            let shared=self.shared.clone();let output=self.output_tx.clone();let search=self.search_inputs[slot].clone();
            shared.branches[slot].lifecycle.store(1,Ordering::Release);
            self.threads[slot]=Some(thread::Builder::new().name(format!("receiver-C{}",slot+1)).spawn(move|| {
                let failure=shared.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
                    run_realtime(slot,rx,output,search,shared,Some(results)))).is_err() {
                    failure.fail_branch(slot,"receiver owner panicked; model state discarded".into());
                }
            }).map_err(|error|format!("spawn receiver C{}: {error}",slot+1))?);
        }
        if searching && self.threads[WORKERS+slot].is_none() {
            let rx=self.search_rxs[slot].take().ok_or("search owner absent")?;
            let shared=self.shared.clone();let destinations=self.result_inputs.clone();
            self.threads[WORKERS+slot]=Some(thread::Builder::new().name(format!("search-C{}",slot+1)).spawn(move|| {
                let failure=shared.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
                    run_search(rx,destinations,shared,Some(slot)))).is_err() {
                    failure.fail_branch(slot,"search owner panicked".into());
                }
            }).map_err(|error|format!("spawn search C{}: {error}",slot+1))?);
        }
        Ok(())
    }

    pub(super) fn fail_source(&mut self,channel:u32,error:&str) {
        if let Some(slot)=channel.checked_sub(1).filter(|slot|(*slot as usize)<MAX_CHANNELS) {
            if self.shared.branch_failure(slot as usize).is_none() {self.shared.fail_branch(slot as usize,format!("fine source failed: {error}"));}
        }
    }
    pub(super) fn channel_failure(&self,channel:u32)->Option<BranchFault> {
        channel.checked_sub(1).filter(|&slot|(slot as usize)<MAX_CHANNELS)
            .and_then(|slot|self.shared.branch_failure(slot as usize))
    }
    pub(super) fn channel_preparing(&self,channel:u32)->bool {
        channel.checked_sub(1).and_then(|slot|self.shared.branches.get(slot as usize))
            .is_some_and(|branch|branch.preparing.load(Ordering::Acquire))
    }

    pub(super) fn restart_ready(&self,channel:u32)->bool {
        let slot=channel as usize-1;
        [slot,WORKERS+slot].iter().all(|&i|self.threads[i].as_ref().is_none_or(|h|h.is_finished()))
    }
    pub(super) fn recovery_progress(&self,channel:u32)->Option<(u64,Option<super::reception_recovery::Position>)> {
        self.shared.metrics.progress(channel,self.shared.generations[channel as usize-1].load(Ordering::Acquire))
    }
    pub(super) fn restart_channel(&mut self,channel:u32)->Result<(),String> {
        let slot=channel.checked_sub(1).filter(|&slot|(slot as usize)<MAX_CHANNELS).ok_or("invalid receiver channel")? as usize;
        if [slot,WORKERS+slot].iter().any(|&index|self.threads[index].as_ref().is_some_and(|h|!h.is_finished())) {
            return Err(format!("channel {channel} is still retiring; its native call has not completed"));
        }
        if self.configs[slot].enabled && self.configs[slot].processing_enabled {self.ensure_owner(slot,self.configs[slot].mode=="ft8")?;}
        self.bump_generation(slot)?;
        self.shared.branches[slot].recovery_floor.store(self.shared.generations[slot].load(Ordering::Acquire),Ordering::Release);
        *self.shared.branches[slot].failure.lock().unwrap_or_else(|e|e.into_inner())=None;
        self.shared.branches[slot].notice.store(false,Ordering::Release);
        self.active[slot]=None;self.levels[slot]=None;self.squelch[slot]=None;
        Ok(())
    }
    pub(super) fn retiring_tail(&self,channel:u32)->bool {
        let Some(slot)=channel.checked_sub(1).filter(|&slot|(slot as usize)<MAX_CHANNELS).map(|slot|slot as usize) else {return false;};
        self.channel_failure(channel).is_none() && (
            self.shared.branches[slot].close.lock().unwrap_or_else(|e|e.into_inner()).is_some()
            || (self.shared.branches[slot].lifecycle.load(Ordering::Acquire)==2
                && self.threads[slot].as_ref().is_some_and(|handle|!handle.is_finished()))
            || self.shared.branches[slot].output_pending.load(Ordering::Acquire)>0)
    }
    pub(super) fn retiring_uid(&self,channel:u32,uid:&str)->bool {
        let Some(slot)=channel.checked_sub(1).filter(|&slot|(slot as usize)<MAX_CHANNELS).map(|slot|slot as usize) else {return false;};
        let branch=&self.shared.branches[slot];
        let retiring=branch.close.lock().unwrap_or_else(|e|e.into_inner()).is_some()
            || branch.lifecycle.load(Ordering::Acquire)==2
                && self.threads[slot].as_ref().is_some_and(|handle|!handle.is_finished());
        self.shared.metrics.retains_output(channel,uid,
            retiring.then(||branch.state_generation.load(Ordering::Acquire)))
    }

    pub(super) fn bind_identity(&mut self, channel:u32, uid:&str, service:&str) {
        if let Some(binding)=channel.checked_sub(1).and_then(|slot|self.bindings.get_mut(slot as usize)) {
            if binding.as_ref().is_some_and(|old|old.0!=uid||old.1!=service) {self.controls[channel as usize-1].clear();}
            *binding=Some((uid.into(),service.into()));
            *self.shared.branches[channel as usize-1].binding.lock().unwrap_or_else(|e|e.into_inner())=Some((uid.into(),service.into()));
        }
    }
    pub(super) fn runtime_status(&self,channel:u32,uid:&str)->serde_json::Value {
        let slot=channel as usize-1;let generation=self.shared.generations[slot].load(Ordering::Acquire);
        let statistics=self.uid_stats(channel,uid);
        let current=statistics.as_array().unwrap().iter().rev().find(|row|row["processing_generation"]==generation.to_string());
        let waiting=current.map_or(0,|row|row["occupancy"]["input_queued"]["oldest_age_ns"].as_u64().unwrap_or(0));
        let failure=self.channel_failure(channel);
        let state=if !self.configs[slot].enabled||!self.configs[slot].processing_enabled {"disabled"}
            else if failure.is_some() {"failed"}
            else if self.shared.branches[slot].lifecycle.load(Ordering::Acquire)==2 {"retiring"}
            else if self.channel_preparing(channel) {"preparing"}
            else if waiting>250_000_000 {"backlogged"}
            else if current.is_none_or(|row|row["packets"]["successful_samples"].as_u64().unwrap_or(0)==0) {"waiting"}
            else {"active"};
        serde_json::json!({"state":state,"processing_generation":generation.to_string(),"fault":failure,
            "oldest_input_wait_ns":waiting,"backlog_notice_after_ns":250_000_000_u64,
            "observed_receiver":current.map(|row|row["receiver"].clone()),
            "input_held_bytes":self.shared.branches[slot].input_bytes.load(Ordering::Acquire),
            "input_held_media_ns":self.shared.branches[slot].input_media_ns.load(Ordering::Acquire)})
    }
    pub(super) fn controls_available(&self,channel:u32)->bool {self.shared.branches[channel as usize-1].controls.available()}
    pub(super) fn control_receipts(&self,channel:u32,uid:&str)->serde_json::Value {self.shared.branches[channel as usize-1].controls.snapshot(uid)}
    pub(super) fn queue_control(&mut self,channel:u32,revision:u64,id:&str,page:bool,reset:bool,action_id:Option<&str>,remaining:Option<Duration>)->Result<(),String> {
        let slot=channel as usize-1;
        let (uid,service)=self.bindings[slot].as_ref().ok_or("receiver identity unavailable")?;
        let generation=self.shared.generations[slot].load(Ordering::Acquire);
        let mut command=self.shared.branches[slot].controls.accept(uid,service,revision,generation,id,page,reset,action_id,remaining)?;
        if !self.configs[slot].enabled || !self.configs[slot].processing_enabled {
            command.finish("inactive",serde_json::json!({"precision":"control","source_boundary":null}),None);
        } else {
            if command.configuration() {self.controls[slot].retain_mut(|old| {
                if old.configuration() {old.finish("cancelled",serde_json::Value::Null,Some("superseded before a data boundary"));false} else {true}
            });}
            self.controls[slot].push_back(command);
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn channel_stats(&self,channel:u32)->serde_json::Value {self.shared.metrics.snapshot_for(channel,None)}
    pub(super) fn uid_stats(&self,channel:u32,uid:&str)->serde_json::Value {self.shared.metrics.snapshot_for(channel,Some(uid))}

    pub fn set_power(&mut self, power: impl IntoIterator<Item=Option<f64>>) {
        self.power.fill(None);
        for (target, value) in self.power.iter_mut().zip(power) {*target=value.filter(|value|value.is_finite());}
    }

    /// Latest normalized complex-IQ power, before receiver AGC or gain.
    pub fn level_dbfs(&self, entry: &ChannelRegistryEntry, now: Instant) -> Option<f64> {
        let slot = entry.channel.checked_sub(1)? as usize;
        let config = self.configs.get(slot)?;
        let level = self.levels[slot]?;
        (config.enabled && entry.state == "active"
            && config.freq == entry.frequency_hz
            && level.channel_id == entry.channel_id && level.epoch == entry.epoch
            && self.shared.current(slot, level.generation)
            && now.saturating_duration_since(level.measured) <= Duration::from_millis(1500))
            .then_some(level.dbfs)
    }

    pub fn squelch_status(&self, entry: &ChannelRegistryEntry, now: Instant) -> Option<(bool, f64)> {
        let slot = entry.channel.checked_sub(1)? as usize;
        let config = self.configs.get(slot)?;
        let gate = self.squelch[slot]?;
        (config.enabled && entry.state == "active" && config.freq == entry.frequency_hz
            && gate.channel_id == entry.channel_id && gate.epoch == entry.epoch
            && self.shared.current(slot, gate.generation)
            && now.saturating_duration_since(gate.measured) <= Duration::from_millis(1500))
            .then_some((gate.open, gate.gain))
    }
    /// End one IQ session while retaining only the thread-local prepared models.
    pub fn reset_session(&mut self) -> Result<(), String> {
        self.shared.check()?;
        for slot in 0..self.configs.len() {
            self.bump_generation(slot)?;
        }
        // Generation changes discard old output and queued searches. Wait for any
        // active decode before dropping its receiver and all stream-local buffers.
        self.finish()?;
        for slot in 0..self.configs.len() {
            if self.threads[slot].is_none() || self.threads[slot].as_ref().is_some_and(JoinHandle::is_finished) {continue;}
            let generation = self.shared.generations[slot].load(Ordering::Acquire);
            let (reply, done) = mpsc::sync_channel(1);
            let pending = Pending::queued(&self.shared,slot%WORKERS,0);
            if let Err(error) = self.inputs[slot % WORKERS].try_send(PendingWork {
                work: Work::Deactivate { slot, generation, reply }, pending,
            }) {
                return Err(format!("receiver session reset queue failure: {error}"));
            }
            done.recv_timeout(Duration::from_secs(5))
                .map_err(|error| format!("receiver session reset acknowledgement: {error}"))?;
        }
        self.finish()?;
        self.power = vec![None; MAX_CHANNELS];
        self.active = vec![None; MAX_CHANNELS];
        Ok(())
    }

    /// Select resources to construct outside the service lock, before changing a producer.
    pub fn preparation_needed(&self, configs: &[ChannelConfig], restart: bool) -> Vec<ChannelConfig> {
        configs.iter().filter(|config| {
            let Some(slot) = config.channel.checked_sub(1).map(|slot| slot as usize).filter(|&slot| slot < MAX_CHANNELS) else { return false; };
            config.enabled && config.processing_enabled && websa_channelizer::catalog::is_external(&config.mode)
                && !self.prepared.matches(slot, config)
                && (restart || self.configs[slot].resets_receiver(config) || self.active[slot].is_none())
        }).cloned().collect()
    }

    pub fn missing_preparations(&self, configs: &[ChannelConfig]) -> Vec<ChannelConfig> {
        self.preparation_needed(configs, true)
    }

    /// Retain the old admission until its pending sequence zero has drained.
    /// The returned token rolls back only candidates that have not been consumed.
    pub fn exchange_prepared(&mut self, mut batch: PreparedDecoders) -> PreparedDecoders {
        let mut displaced = PreparedDecoders { selected: batch.selected.clone(), rollback: !batch.rollback,
            ..PreparedDecoders::default() };
        for slot in 0..MAX_CHANNELS {
            if !batch.selected[slot] { continue; }
            if batch.rollback {
                displaced.channels[slot] = self.prepared.channels[slot].take();
                self.prepared.channels[slot] = self.prepared.previous[slot].take();
            } else {
                displaced.previous[slot] = self.prepared.previous[slot].take();
                self.prepared.previous[slot] = self.prepared.channels[slot].take();
                self.prepared.channels[slot] = batch.channels[slot].take();
            }
        }
        displaced
    }

    pub fn install_prepared(&mut self, batch: PreparedDecoders) {
        drop(self.exchange_prepared(batch));
    }

    /// Remove unused candidates after committing controls; drop them outside the service lock.
    pub fn release_prepared(&mut self, configs: &[ChannelConfig]) -> PreparedDecoders {
        let mut released = PreparedDecoders::default();
        for slot in 0..MAX_CHANNELS {
            released.previous[slot] = self.prepared.previous[slot].take();
            if self.prepared.channels[slot].is_some() && !configs.iter().any(|config|
                config.channel as usize == slot + 1 && config.enabled && self.prepared.matches(slot, config)) {
                released.selected[slot] = true;
                released.channels[slot] = self.prepared.channels[slot].take();
            }
        }
        released
    }

    /// Release a cancelled transition's candidates only after the current identity is active.
    /// A new registry identity whose sequence zero has not arrived still needs its candidate.
    pub fn discard_staged(&mut self, registry: &[ChannelRegistryEntry]) -> PreparedDecoders {
        let mut discarded = PreparedDecoders::default();
        for row in registry {
            let Some(slot) = row.channel.checked_sub(1).map(|slot| slot as usize).filter(|&slot| slot < MAX_CHANNELS) else { continue; };
            let generation = self.shared.generations[slot].load(Ordering::Acquire);
            if row.state == "active" && self.active[slot] == Some((generation, row.channel_id, row.epoch)) {
                discarded.selected[slot] = true;
                discarded.channels[slot] = self.prepared.channels[slot].take();
                discarded.previous[slot] = self.prepared.previous[slot].take();
            }
        }
        discarded
    }

    pub fn configure(&mut self, configs: &[ChannelConfig]) -> Result<(), String> {
        self.shared.check()?;
        let mut desired = (1..=MAX_CHANNELS as u32).map(ChannelConfig::for_slot).collect::<Vec<_>>();
        let mut seen = vec![false; MAX_CHANNELS];
        for config in configs {
            config.validate()?;
            let slot = config.channel as usize - 1;
            if seen[slot] { return Err(format!("duplicate receiver channel {}", config.channel)); }
            seen[slot] = true;
            desired[slot] = config.clone();
        }
        // Preparation failures must leave every active configuration and generation intact.
        for (slot, config) in desired.iter().enumerate() {
            if config.enabled && config.processing_enabled && websa_channelizer::catalog::is_external(&config.mode)
                && (self.configs[slot].resets_receiver(config) || self.active[slot].is_none())
                && !self.prepared.matches(slot, config) {
                return Err(format!("external decoder for channel {} was not prepared before admission", config.channel));
            }
        }
        for (slot, config) in desired.into_iter().enumerate() {
            if *self.configs[slot] != config {
                if self.configs[slot].resets_receiver(&config) {
                    self.bump_generation(slot)?;
                    if config.enabled && config.processing_enabled && self.threads[slot].as_ref().is_some_and(|h|!h.is_finished())
                        && self.shared.branches[slot].lifecycle.load(Ordering::Acquire)==1 {
                        let (tx,rx)=mpsc::sync_channel(INPUT_CAPACITY);
                        *self.shared.branches[slot].mailbox.lock().unwrap_or_else(|e|e.into_inner())=Some(rx);
                        self.inputs[slot]=tx;
                    }
                    self.active[slot] = None;
                }
                if !config.enabled || !config.processing_enabled {
                    if self.threads[slot].is_some() {self.shared.branches[slot].lifecycle.store(2,Ordering::Release);}
                }
                self.configs[slot] = Arc::new(config);
            }
        }
        Ok(())
    }

    fn bump_generation(&self, slot: usize) -> Result<(), String> {
        self.shared.generations[slot].try_update(Ordering::AcqRel, Ordering::Acquire,
            |generation| generation.checked_add(1))
            .map(|_| ()).map_err(|_| "receiver configuration generation overflow".into())
    }

    pub fn submit<T:IqInput+?Sized>(&mut self, frame: &T, registry: &[ChannelRegistryEntry]) -> Result<(), String> {
        self.submit_as(frame,registry,None)
    }
    pub(super) fn submit_processing<T:IqInput+?Sized>(&mut self,frame:&T,registry:&[ChannelRegistryEntry],id:u32,epoch:u32,sequence:u32)->Result<(),String> {
        self.submit_as(frame,registry,Some((id,epoch,sequence)))
    }
    fn submit_as<T:IqInput+?Sized>(&mut self,input:&T,registry:&[ChannelRegistryEntry],processing:Option<(u32,u32,u32)>)->Result<(),String> {
        self.shared.check()?;
        let frame=input.iq_header();let native=input.native_samples();
        if frame.len() < 56 || !matches!(&frame[..4], b"IQBF" | b"IQAX") {
            return Err("invalid receiver IQ frame header".into());
        }
        let u32_at = |at| u32::from_le_bytes(frame[at..at + 4].try_into().unwrap());
        let f64_at = |at| f64::from_le_bytes(frame[at..at + 8].try_into().unwrap());
        let samples = u32_at(12) as usize;
        let seq = u32_at(8);
        let rate = f64_at(16);
        let frequency = f64_at(24);
        let channel_id = u32_at(32);
        let epoch = u32_at(36);
        let input_index = u64::from_le_bytes(frame[40..48].try_into().unwrap());
        let delay = f64_at(48);
        if u32_at(4) != 2 || samples > MAX_BASEBAND_SAMPLES || native.map_or(frame.len(),|native|56+native.samples.bytes()) != 56 + samples * 8
            || !(1.0..=1e9).contains(&rate) || !frequency.is_finite()
            || channel_id == 0 || epoch == 0 || !delay.is_finite() || delay < 0.0
            || (seq == 0) != (samples == 0) || input_index == u64::MAX
        { return Err("invalid receiver IQ frame metadata".into()); }
        // The service filters stale identities before submission; reaching this
        // boundary without the frame's registry is a caller error.
        let row = registry.iter().find(|row| row.channel_id == channel_id && row.epoch == epoch)
            .ok_or_else(|| format!("receiver IQ identity {channel_id}/{epoch} missing from registry"))?;
        if !(1..=MAX_CHANNELS as u32).contains(&row.channel) || row.state != "active"
            || row.output_rate != rate || row.frequency_hz != frequency
            || !(1.0..=1e9).contains(&row.input_rate)
            || !row.group_delay_samples.is_finite() || row.group_delay_samples < 0.0
            || !(delay * row.input_rate / rate).is_finite()
        { return Err("receiver IQ registry clock mismatch".into()); }
        let slot = row.channel as usize - 1;
        if !self.configs[slot].enabled || !self.configs[slot].processing_enabled { return Err(format!("IQ submitted for disabled receiver {}", row.channel)); }
        if self.channel_failure(row.channel).is_some() {return Ok(());}
        if let Err(error)=self.ensure_owner(slot,self.configs[slot].mode=="ft8") {
            self.shared.fail_branch(slot,error);return Ok(());
        }
        let generation = self.shared.generations[slot].load(Ordering::Acquire);
        let iq=if let Some(native)=native {
            if native.input_rate!=row.input_rate || native.source_generation!=epoch as u64 {
                return Err("native receiver source clock mismatch".into());
            }
            native.samples.clone()
        } else {
            let mut decoded=Vec::with_capacity(samples*2);
            for bytes in frame[56..].chunks_exact(4) {decoded.push(f32::from_le_bytes(bytes.try_into().unwrap()));}
            IqSamples::new(decoded).map_err(str::to_owned)?
        };
        let power=iq.iter().map(|&value|f64::from(value)*f64::from(value)).sum::<f64>();
        // Parse the immutable source payload once. Branch identity/sequence are
        // typed metadata, so attaching a processor never copies/re-encodes IQ.
        let (processed_id,processed_epoch,processed_seq)=processing.unwrap_or((channel_id,epoch,seq));
        let prepared = if processed_seq == 0 {
            let decoder = self.prepared.take(slot, &self.configs[slot]).and_then(|ready| ready.decoder);
            if decoder.is_none() && websa_channelizer::catalog::is_external(&self.configs[slot].mode) {
                return Err(format!("external decoder for channel {} was not prepared before IQ admission", row.channel));
            }
            decoder
        } else { None };
        let packet = Packet {
            slot, generation: self.shared.generations[slot].load(Ordering::Acquire),
            config: Arc::clone(&self.configs[slot]), channel_id:processed_id, epoch:processed_epoch, seq:processed_seq, rate, frequency,
            power_dbm: self.power[slot],
            clock: InputClock { first_sample_index: input_index, input_rate: row.input_rate,
                delay_input_samples: delay * row.input_rate / rate }, iq,
            radiofax_new_page: std::mem::replace(&mut self.radiofax_new_page[slot], false),
            prepared,
            metrics:None,
            input_permit:None,commands:Vec::new(),
        };
        let lane=self.shared.metrics.lane(metrics::Identity {channel:row.channel,generation,
            channel_id:processed_id,epoch:processed_epoch,
            receiver_uid:self.bindings[slot].as_ref().map(|b|b.0.clone()),
            service_id:self.bindings[slot].as_ref().map(|b|b.1.clone())});
        let mut packet=packet;packet.metrics=Some(lane.clone());
        for _ in 0..super::receiver_controls::CAPACITY {if let Some(command)=self.controls[slot].pop_front() {packet.commands.push(command);} else {break;}}
        let clock=serde_json::json!({"channel_id":processed_id.to_string(),"epoch":processed_epoch.to_string(),
            "input_index":input_index.to_string(),"input_rate":row.input_rate,"actual_rate":rate,
            "sample_count":samples,"delay_input_samples":delay*row.input_rate/rate,"precision":"block"});
        let mut pending = Pending::queued(&self.shared,slot%WORKERS,samples as u64);
        pending.tracking=Some(lane.job(false,samples as u64,rate,clock,(samples*8) as u64));
        match InputPermit::reserve(&self.shared,slot,samples,rate) {
            Ok(permit)=>packet.input_permit=Some(permit),
            Err(error)=>{
                if let Some(tracking)=pending.tracking.as_mut() {tracking.reject(&error);}
                self.shared.counters.queue_full.fetch_add(1,Ordering::Relaxed);
                self.shared.fail_generation(slot,generation,error);return Ok(());
            }
        }
        match self.inputs[slot % WORKERS].try_send(PendingWork { work:Work::Packet(packet), pending }) {
            Ok(()) => {
                if processed_seq == 0 { self.active[slot] = Some((generation, processed_id, processed_epoch)); }
                self.levels[slot] = if samples == 0 { None } else {
                    Some(InputLevel { channel_id, epoch,
                        generation: self.shared.generations[slot].load(Ordering::Acquire),
                        measured: Instant::now(),
                        // A zero-power block has no finite logarithmic level.
                        dbfs: 10.0 * (power / samples as f64).log10(),
                    }).filter(|level| level.dbfs.is_finite())
                };
                self.shared.counters.submitted.fetch_add(1, Ordering::Relaxed);
                self.shared.counters.submitted_samples.fetch_add(samples as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                let description=error.to_string();
                let mut rejected=match error {mpsc::TrySendError::Full(work)|mpsc::TrySendError::Disconnected(work)=>work};
                if let Some(tracking)=rejected.pending.tracking.as_mut() {tracking.reject(&description);}
                self.shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                let error = format!("receiver input queue failure: {description}; channel={} mode={} rate={rate} seq={seq} pending={} chain_max_ns={}",
                    row.channel, self.configs[slot].mode,
                    self.shared.pending.load(Ordering::Acquire),
                    self.shared.counters.chain_max_ns.load(Ordering::Relaxed));
                self.shared.fail_branch(slot,error);
                Ok(())
            }
        }
    }

    pub fn poll(&mut self) -> Result<ReceiverFrames, String> {
        self.shared.check()?;
        let mut frames = ReceiverFrames::default();
        for output in self.deferred.drain(..).chain(self.outputs.try_iter()) {
            if self.shared.current(output.slot,output.generation) || output.retired && output.generation>=self.shared.branches[output.slot].recovery_floor.load(Ordering::Acquire) {
                if let Some(accounting)=output.accounting.as_ref() {accounting.consumed();}
                if output.retired {
                    let identity=output.stream.map(|(id,epoch)|super::service_stream::identity_key(id,epoch));
                    if let Some(identity)=identity {frames.retired.push(RetiredFrames {identity,channel:output.slot as u32+1,decode_offset:frames.decode.len(),frames:output.frames});}
                } else {
                    self.squelch[output.slot]=output.squelch;frames.append(output.frames);
                }
            }
        }
        for (slot,branch) in self.shared.branches.iter().enumerate() {
            if branch.notice.swap(false,Ordering::AcqRel) {
                if let Some(fault)=self.shared.branch_failure(slot) {frames.faults.push(fault);}
            }
        }
        self.shared.check()?;
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<ReceiverFrames, String> {
        let mut frames = ReceiverFrames::default();
        let started = Instant::now();
        loop {
            frames.append(self.poll()?);
            if self.shared.pending.load(Ordering::Acquire) == 0 {
                frames.append(self.poll()?);
                return Ok(frames);
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err("receiver drain exceeded 120 seconds".into());
            }
            match self.outputs.recv_timeout(Duration::from_millis(10)) {
                Ok(output)=>self.deferred.push(output),
                Err(mpsc::RecvTimeoutError::Timeout)=>{},
                Err(mpsc::RecvTimeoutError::Disconnected)=>return Err("receiver output queue disconnected".into()),
            }
        }
    }
    /// Close selected radiofax pages while their current stream identities are valid.
    pub fn finish_radiofax(&mut self, channels: &[u32]) -> Result<ReceiverFrames, String> {
        self.close_channels(channels, false)
    }

    /// Preserve the old identity's available output without an EOF wait on capture.
    pub fn retire_channels(&mut self, channels: &[u32]) -> Result<ReceiverFrames, String> {
        self.close_channels(channels, true)
    }

    fn close_channels(&mut self, channels: &[u32], live: bool) -> Result<ReceiverFrames, String> {
        if live {
            for &channel in channels {
                let slot=channel.checked_sub(1).filter(|&slot|(slot as usize)<MAX_CHANNELS).ok_or("invalid receiver channel")? as usize;
                if self.threads[slot].as_ref().is_none_or(JoinHandle::is_finished) {continue;}
                let generation=self.shared.generations[slot].load(Ordering::Acquire);
                let mut close=self.shared.branches[slot].close.lock().unwrap_or_else(|e|e.into_inner());
                if close.is_none() {self.shared.pending.fetch_add(1,Ordering::AcqRel);}
                *close=Some((generation,true));
            }
            return self.poll();
        }
        let mut frames=self.finish()?;
        for &channel in channels {
            let slot=channel.checked_sub(1).filter(|&slot|(slot as usize)<MAX_CHANNELS).ok_or("invalid radiofax channel")? as usize;
            if self.threads[slot].is_none() {continue;}
            let generation=self.shared.generations[slot].load(Ordering::Acquire);
            let pending=Pending::queued(&self.shared,slot%WORKERS,0);
            self.inputs[slot%WORKERS].try_send(PendingWork {
                work:Work::FinishRadiofax {slot,generation,live},pending,
            }).map_err(|error|format!("receiver finish queue failure: {error}"))?;
        }
        frames.append(self.finish()?);
        Ok(frames)
    }

    #[cfg(test)]
    pub(super) fn radiofax_page_pending(&self, channel:u32)->bool {
        self.radiofax_new_page[(channel-1) as usize] || self.controls[(channel-1) as usize].iter().any(|command|command.page)
    }


    pub fn stats(&self) -> serde_json::Value {
        let c = &self.shared.counters;
        let load = |v: &AtomicU64| v.load(Ordering::Relaxed);
        let queued=self.shared.diagnostics.queued.lock().unwrap_or_else(|e|e.into_inner());
        let worker_stats:Vec<_>=self.shared.diagnostics.workers.iter().enumerate().map(|(worker,c)| {
            let entries:Vec<_>=queued.values().filter(|e|e.worker==worker).collect();
            serde_json::json!({"worker":worker,"role":if worker>=WORKERS {"search"} else {"realtime"},"busy_ns":load(&c.busy_ns),"jobs":load(&c.jobs),"max_ns":load(&c.max_ns),
                "queue_wait_ns":load(&c.wait_ns),"queue_wait_max_ns":load(&c.wait_max_ns),
                "queued_jobs":entries.len(),"queued_samples":entries.iter().map(|e|e.samples).sum::<u64>(),
                "oldest_age_ns":entries.iter().map(|e|e.at.elapsed().as_nanos().min(u64::MAX as u128) as u64).max().unwrap_or(0),
                "peak_queued_jobs":load(&c.peak_jobs),"peak_queued_samples":load(&c.peak_samples)})
        }).collect();
        serde_json::json!({
            "owner_threads":self.threads.iter().flatten().filter(|handle|!handle.is_finished()).count(),
            "branches":self.shared.branches.iter().enumerate().map(|(slot,branch)|serde_json::json!({"channel":slot+1,
                "lifecycle":match branch.lifecycle.load(Ordering::Acquire) {0=>"absent",1=>"active",_=>"retiring"},
                "preparing":branch.preparing.load(Ordering::Acquire),
                "fault":self.shared.branch_failure(slot),"output_pending":branch.output_pending.load(Ordering::Acquire),
                "output_pending_bytes":branch.output_bytes.load(Ordering::Acquire),
                "input_held_bytes":branch.input_bytes.load(Ordering::Acquire),"input_held_media_ns":branch.input_media_ns.load(Ordering::Acquire)})).collect::<Vec<_>>(),
            "workers":worker_stats,"channels":self.shared.metrics.snapshot(),
            "submitted":load(&c.submitted), "completed":load(&c.completed),
            "submitted_samples":load(&c.submitted_samples), "completed_samples":load(&c.completed_samples),
            "queue_full":load(&c.queue_full), "audio_frames":load(&c.audio_frames),
            "audio_samples":load(&c.audio_samples), "decode_frames":load(&c.decode_frames),
            "search_submitted":load(&c.search_submitted), "search_completed":load(&c.search_completed),
            "pending":self.shared.pending.load(Ordering::Acquire),
            "shared_iq":websa_channelizer::IqFrame::retention(),
            "filter_plans":websa_channelizer::filter_plans::statistics(),
            "host_resources":websa_channelizer::host_resources::snapshot(),
            "model_preparation":websa_channelizer::model_preparation::snapshot(),
            "native_chain":{"count":load(&c.chain_count),"total_ns":load(&c.chain_total_ns),"max_ns":load(&c.chain_max_ns)},
            "limits":{"workers":WORKERS,"input_per_worker":INPUT_CAPACITY,"results":RESULT_CAPACITY,
                "input_bytes_per_channel":INPUT_BYTES_PER_CHANNEL,"input_media_ns_per_channel":INPUT_MEDIA_NS_PER_CHANNEL,
                "results_per_channel":RESULT_CAPACITY,"output_bytes_per_channel":OUTPUT_BYTES_PER_CHANNEL,
                "ft8_jobs":SEARCH_CAPACITY,"max_iq_samples":MAX_BASEBAND_SAMPLES,"max_event_bytes":MAX_EVENT_BYTES}
        })
    }
}

impl Drop for ReceiverWorkers {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.inputs.clear();
        for branch in &self.shared.branches {branch.mailbox.lock().unwrap_or_else(|e|e.into_inner()).take();}
        for handle in self.threads.drain(..).flatten() {
            if handle.is_finished() {let _=handle.join();}
            // Dropping a JoinHandle detaches. Publication has already been revoked;
            // a native call cannot safely be killed from another Rust thread.
        }
    }
}

struct ChannelState {
    generation: u64,
    channel_id: u32,
    epoch: u32,
    frequency: f64,
    mode: String,
    rate: f64,
    source_rate: f64,
    next_seq: u32,
    event_seq: u32,
    audio_seq: u32,
    audio_index: u64,
    audio_format: Option<(u32, u16)>,
    protocol_timing: Option<(InputClock, SamplePosition, bool)>,
    last_clock: InputClock,
    receiver: Receiver,
    metrics:Option<Arc<metrics::Lane>>,
    config:Arc<ChannelConfig>,
}

impl ChannelState {
    fn new(packet: &mut Packet) -> Result<Self, String> {
        if packet.seq != 0 {
            return Err(format!("receiver identity must start with IQ sequence zero: channel={} identity={}/{} generation={} seq={} samples={} input_index={} rate={} input_rate={} frequency={} mode={}",
                packet.slot + 1, packet.channel_id, packet.epoch, packet.generation, packet.seq,
                packet.iq.len() / 2, packet.clock.first_sample_index, packet.rate, packet.clock.input_rate,
                packet.frequency, packet.config.mode));
        }
        Ok(Self {
            generation: packet.generation, channel_id: packet.channel_id, epoch: packet.epoch,
            frequency: packet.frequency, mode: packet.config.mode.clone(), rate: packet.rate,
            source_rate: packet.clock.input_rate, next_seq: 0, event_seq: 0, audio_seq: 0,
            audio_index: 0, audio_format: None,
            protocol_timing: None,
            last_clock: packet.clock,
            receiver: Receiver::new_prepared(packet.config.receiver_config(packet.rate), packet.prepared.take())?,
            metrics:packet.metrics.clone(),
            config:packet.config.clone(),
        })
    }

    fn close(&mut self, slot:usize, frames:&mut ReceiverFrames, live:bool)->Result<(),String> {
        let events = if live { self.receiver.retire_external()? } else { self.receiver.finish_external()? };
        for event in events {
            frames.decode.push(self.event(slot,self.last_clock,event)?);
        }
        if self.mode!="radiofax" {return Ok(());}
        self.receiver.finish_radiofax()?;
        for event in self.receiver.take_radiofax_events()? {
            frames.decode.push(self.event(slot,self.last_clock,event)?);
        }
        Ok(())
    }

    fn event(&mut self, slot: usize, clock: InputClock, event: ReceiverEvent) -> Result<Vec<u8>, String> {
        self.event_at(slot, clock, SamplePosition {index:clock.first_sample_index,fraction_num:0,fraction_den:1}, event)
    }

    fn event_at(&mut self, slot: usize, clock: InputClock, position: SamplePosition, mut event: ReceiverEvent) -> Result<Vec<u8>, String> {
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("ft8") => { event["centerHz"] = serde_json::json!(self.frequency); }
            Some("am-info") => {
                event["carrierHz"] = serde_json::json!(event.get("carrierOffsetHz")
                    .and_then(serde_json::Value::as_f64).map(|offset| self.frequency + offset));
            }
            Some("wfm-info") => {
                // The stream's channel centre; not an independent carrier estimate.
                event["carrierHz"] = serde_json::json!(self.frequency);
            }
            Some("sstv-image" | "radiofax-image") => {
                if let Some(meta) = event.get_mut("meta").and_then(serde_json::Value::as_object_mut) {
                    meta.insert("hz".into(), serde_json::json!(self.frequency));
                }
            }
            _ => {}
        }
        let protocol=matches!(self.mode.as_str(),"drm"|"drmplus");
        let (clock,position,delay_known,timing_kind)=if protocol {
            if let Some((clock,position,known))=self.protocol_timing {
                (clock,position,known,"protocol_provenance")
            } else {(clock,position,false,"causal_presentation")}
        } else if matches!(self.mode.as_str(), "sstv" | "radiofax")
            || event.get("type").and_then(serde_json::Value::as_str) == Some("decoder-result") {
            // Image updates are presented after decoding the current block; the
            // decoder does not report an exact contributing pixel sample.
            (clock,position,false,"causal_presentation")
        } else {(clock,position,true,"sample")};
        let envelope = serde_json::json!({
            "version":1,"channel":slot + 1,"channel_id":self.channel_id,"epoch":self.epoch,
            "seq":self.event_seq,"input_index":position.index.to_string(),
            "input_rate":clock.input_rate,"input_fraction_num":position.fraction_num.to_string(),
            "input_fraction_den":position.fraction_den.to_string(),
            "delay_seconds":clock.delay_input_samples / clock.input_rate,
            "delay_known":delay_known,"causal_presentation":timing_kind!="sample","timing_kind":timing_kind,
            "mode":self.mode,"event":event
        });
        let mut bounded = BoundedJson(Vec::new());
        serde_json::to_writer(&mut bounded, &envelope).map_err(|error| format!("encode receiver event: {error}"))?;
        let frame = encode_decode_event(&bounded.0).map_err(str::to_owned)?;
        self.event_seq = self.event_seq.checked_add(1).ok_or("receiver event sequence overflow")?;
        Ok(frame)
    }

    fn audio(&mut self, block: AudioBlock, frames: &mut ReceiverFrames, config: &Arc<ChannelConfig>) -> Result<(), String> {
        if block.samples.is_empty() { return Ok(()); }
        let format = (block.rate, block.channels);
        if self.audio_format.is_some_and(|old| old != format)
            || (block.first_frame_index == 0 && self.audio_index != 0) {
            self.audio_seq = 0;
            self.audio_index = 0;
        }
        if block.first_frame_index != self.audio_index {
            return Err("receiver PCM index discontinuity".into());
        }
        let position = block.source_position.ok_or("receiver PCM has no source position")?;
        let delay = block.clock.delay_input_samples / block.clock.input_rate;
        if matches!(self.mode.as_str(),"drm"|"drmplus") {
            self.protocol_timing=Some((block.clock,position,block.codec_delay_known));
        }
        self.audio_format = Some(format);
        let mut metadata = AudioFrameMetadata {
            seq: self.audio_seq, rate: block.rate as f64, frequency_hz: self.frequency,
            channel_id: self.channel_id, epoch: self.epoch, input_index: position.index,
            delay_seconds: delay, channels: block.channels as u32, audio_index: self.audio_index,
            input_rate: block.clock.input_rate, input_fraction_num: position.fraction_num,
            input_fraction_den: position.fraction_den,
            delay_known: block.codec_delay_known,
            causal_presentation: block.timing_kind != "sample",
        };
        if self.audio_seq == 0 {
            frames.audio.push(encode_audio(metadata, &[]).map_err(str::to_owned)?);
            frames.audio_configs.push(config.clone());
            self.audio_seq = 1;
        }
        metadata.seq = self.audio_seq;
        frames.audio.push(encode_audio(metadata, &block.samples).map_err(str::to_owned)?);
        frames.audio_configs.push(config.clone());
        self.audio_seq = self.audio_seq.checked_add(1).ok_or("receiver audio sequence overflow")?;
        self.audio_index = self.audio_index.checked_add((block.samples.len() / block.channels as usize) as u64)
            .ok_or("receiver audio sample clock overflow")?;
        Ok(())
    }
}

struct BoundedJson(Vec<u8>);
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_EVENT_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "receiver event exceeds 2 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn run_realtime(worker:usize,mut input: mpsc::Receiver<PendingWork>, output: mpsc::SyncSender<Output>,
    search: mpsc::SyncSender<SearchJob>, shared: Arc<Shared>, returns:Option<mpsc::Receiver<PendingWork>>) {
    let mut states: Vec<Option<ChannelState>> = (0..MAX_CHANNELS).map(|_|None).collect();
    let mut prefer_return=false;
    while !shared.stopping.load(Ordering::Acquire) {
        if let Some(replacement)=shared.branches[worker].mailbox.lock().unwrap_or_else(|e|e.into_inner()).take() {input=replacement;}
        let closing=shared.branches[worker].close.lock().unwrap_or_else(|e|e.into_inner()).take();
        if let Some((generation,live))=closing {
            let _pending=Pending {shared:shared.clone(),queue_id:None,tracking:None};
            if shared.branch_failure(worker).is_none() {
                if let Some(state)=states[worker].as_mut().filter(|state|state.generation==generation) {
                    let mut frames=ReceiverFrames::default();
                    let result=state.close(worker,&mut frames,live).and_then(|_| {
                        frames.decode.push(state.event(worker,state.last_clock,serde_json::json!({
                            "type":"processing-end","drained":false,"tail_complete":true,"processing_complete":false,
                            "reason":"reconfigured","processing_generation":generation.to_string()}))?);
                        let bytes=frames.decode.iter().map(|frame|frame.len() as u64).sum();
                        let accounting=state.metrics.as_ref().map(|lane|lane.hold_output(serde_json::json!({
                            "input_index":state.last_clock.first_sample_index.to_string(),"input_rate":state.last_clock.input_rate}),bytes,"tail"));
                        let closed=Output {slot:worker,generation,frames,squelch:None,accounting,permit:None,
                            retired:true,stream:Some((state.channel_id,state.epoch))};
                        shared.publish(&output,closed)
                    });
                    if let Err(error)=result {shared.fail_generation(worker,generation,error);}
                    states[worker]=None;
                    shared.branches[worker].state_generation.store(0,Ordering::Release);
                }
            }
        }
        if shared.branches[worker].lifecycle.load(Ordering::Acquire)==2 {break;}
        let returned=||returns.as_ref().and_then(|rx|rx.try_recv().ok());
        let next=if prefer_return {returned().or_else(||input.try_recv().ok())}
            else {input.try_recv().ok().or_else(returned)};
        prefer_return=!prefer_return;
        let PendingWork {work,mut pending}=match next {
            Some(work)=>work,
            None=>match input.recv_timeout(Duration::from_millis(2)) {
                Ok(work)=>work,Err(mpsc::RecvTimeoutError::Timeout)=>continue,
                Err(mpsc::RecvTimeoutError::Disconnected)=>{
                    if shared.branches[worker].mailbox.lock().unwrap_or_else(|e|e.into_inner()).is_some() {continue;}
                    break;
                },
            }
        };
        let (slot,generation)=match &work {Work::Packet(packet)=>(packet.slot,packet.generation),
            Work::Search(result)=>(result.slot,result.generation),
            Work::FinishRadiofax {slot,generation,..}|Work::Deactivate {slot,generation,..}=>(*slot,*generation)};
        pending.started();
        if let Some(tracking)=pending.tracking.as_mut() {tracking.start();}
        let packet_samples=match &work {Work::Packet(packet)=>Some(packet.iq.len() as u64/2),_=>None};
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||match work {
            Work::Packet(packet) => process_packet(packet, &mut states, &search, &shared),
            Work::Search(completed) => process_search(completed, &mut states, &shared),
            Work::FinishRadiofax {slot,generation,live} => {
                if shared.current(slot,generation) {
                    if let Some(state)=states[slot].as_mut() {
                        let mut frames=ReceiverFrames::default();
                        state.close(slot,&mut frames,live).map(|_|Some(Output {
                            slot,generation,frames,squelch:None,accounting:None,permit:None,retired:false,stream:None,
                        }))
                    } else {Ok(None)}
                } else {Ok(None)}
            }
            Work::Deactivate {slot,generation,reply} => {
                if shared.current(slot,generation) {states[slot]=None;}
                let _=reply.try_send(());
                Ok(None)
            }
        })).unwrap_or_else(|_|Err(format!("receiver C{} panicked; state requires explicit recovery",slot+1)));
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        shared.counters.chain_count.fetch_add(1, Ordering::Relaxed);
        shared.counters.chain_total_ns.fetch_add(elapsed, Ordering::Relaxed);
        shared.counters.chain_max_ns.fetch_max(elapsed, Ordering::Relaxed);
        let counters=&shared.diagnostics.workers[worker];
        counters.busy_ns.fetch_add(elapsed,Ordering::Relaxed);
        counters.jobs.fetch_add(1,Ordering::Relaxed);
        counters.max_ns.fetch_max(elapsed,Ordering::Relaxed);
        match result {
            Ok(Some(mut result)) => {
                if !shared.current(slot,generation) {
                    if let Some(tracking)=pending.tracking.as_mut() {tracking.finish(Ok(false));}
                    continue;
                }
                if let Some(tracking)=pending.tracking.as_ref() {
                    let bytes=result.frames.audio.iter().chain(&result.frames.decode).map(|frame|frame.len() as u64).sum();
                    result.accounting=Some(tracking.output(bytes));
                }
                shared.counters.audio_frames.fetch_add(result.frames.audio.len() as u64, Ordering::Relaxed);
                let samples: u64 = result.frames.audio.iter().map(|f|
                    u32::from_le_bytes(f[12..16].try_into().unwrap()) as u64).sum();
                shared.counters.audio_samples.fetch_add(samples, Ordering::Relaxed);
                shared.counters.decode_frames.fetch_add(result.frames.decode.len() as u64, Ordering::Relaxed);
                if let Err(error) = shared.publish(&output,result) {
                    shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    if let Some(tracking)=pending.tracking.as_mut() {tracking.finish(Err(&error));}
                    shared.fail_generation(slot,generation,error);
                } else {
                    if let Some(count)=packet_samples {
                        shared.counters.completed.fetch_add(1,Ordering::Relaxed);
                        shared.counters.completed_samples.fetch_add(count,Ordering::Relaxed);
                    }
                    if let Some(tracking)=pending.tracking.as_mut() {tracking.finish(Ok(true));tracking.published();}
                }
            }
            Ok(None) => {if let Some(tracking)=pending.tracking.as_mut() {tracking.finish(Ok(false));}}
            Err(error) => {
                if let Some(tracking)=pending.tracking.as_mut() {tracking.finish(Err(&error));}
                states[slot]=None;
                shared.fail_generation(slot,generation,error);
            },
        }
        drop(pending);
    }
}

fn process_packet(mut packet:Packet,states:&mut [Option<ChannelState>],search:&mpsc::SyncSender<SearchJob>,shared:&Arc<Shared>)->Result<Option<Output>,String> {
    let result=process_packet_inner(&mut packet,states,search,shared);
    if let Err(error)=&result {for command in &mut packet.commands {command.finish("failed",serde_json::Value::Null,Some(error));}}
    result
}

fn process_packet_inner(packet:&mut Packet,states:&mut [Option<ChannelState>],search:&mpsc::SyncSender<SearchJob>,shared:&Arc<Shared>)->Result<Option<Output>,String> {
    if !shared.current(packet.slot, packet.generation) { return Ok(None); }
    let mut frames = ReceiverFrames::default();
    let needs_reset = states[packet.slot].as_ref().is_none_or(|state|
        state.generation != packet.generation || state.channel_id != packet.channel_id || state.epoch != packet.epoch);
    let cold=needs_reset || states[packet.slot].as_ref().is_some_and(|state|
        state.config.nr!=packet.config.nr || state.config.nr_algorithm!=packet.config.nr_algorithm);
    let _preparing=if cold {
        shared.branches[packet.slot].preparing.store(true,Ordering::Release);Some(Preparing(&shared.branches[packet.slot].preparing))
    } else {None};
    if needs_reset {
        if states[packet.slot].as_ref().is_some_and(|state|
            state.channel_id == packet.channel_id && state.epoch == packet.epoch) {
            let state = states[packet.slot].as_ref().unwrap();
            return Err(format!("receiver config reset requires a new fine channel identity: channel={} identity={}/{} generation={} previous_generation={} seq={} expected_seq={} mode={} previous_mode={}",
                packet.slot + 1, packet.channel_id, packet.epoch, packet.generation, state.generation,
                packet.seq, state.next_seq, packet.config.mode, state.mode));
        }
        if let Some(previous) = states[packet.slot].as_mut() { previous.close(packet.slot,&mut frames,true)?; }
        states[packet.slot] = Some(ChannelState::new(packet)?);
        shared.branches[packet.slot].state_generation.store(packet.generation,Ordering::Release);
    }
    let state = states[packet.slot].as_mut().unwrap();
    if packet.radiofax_new_page { state.receiver.request_radiofax_page(); }
    state.receiver.configure(packet.config.receiver_config(packet.rate))?;
    state.config=packet.config.clone();
    for command in &mut packet.commands {
        if command.generation!=packet.generation {continue;}
        if command.expired() {command.finish("expired",serde_json::Value::Null,Some("action expired before data boundary"));continue;}
        if (command.page||command.reset) && state.mode!="radiofax" {
            command.finish("cancelled",serde_json::Value::Null,Some("Radiofax is no longer active"));continue;
        }
        if command.page {state.receiver.request_radiofax_page();state.receiver.configure(packet.config.receiver_config(packet.rate))?;}
        if command.reset {let mut config=packet.config.receiver_config(packet.rate);config.radiofax_phase_pixels=0.0;config.radiofax_clock_ppm=0.0;state.receiver.configure(config)?;}
        command.finish("applied",serde_json::json!({"precision":"block","channel_id":packet.channel_id.to_string(),
            "epoch":packet.epoch.to_string(),"input_index":packet.clock.first_sample_index.to_string(),
            "fraction_num":"0","fraction_den":"1","input_rate":packet.clock.input_rate,
            "delay_input_samples":packet.clock.delay_input_samples}),None);
    }

    if packet.seq != state.next_seq || packet.rate != state.rate || packet.clock.input_rate != state.source_rate
        || packet.frequency != state.frequency {
        return Err(format!("receiver IQ sequence or clock discontinuity: channel={} identity={}/{} generation={} previous_generation={} seq={} expected_seq={} samples={} input_index={} rate={} expected_rate={} input_rate={} expected_input_rate={} frequency={} expected_frequency={} delay_input_samples={} mode={}",
            packet.slot + 1, packet.channel_id, packet.epoch, packet.generation, state.generation,
            packet.seq, state.next_seq, packet.iq.len() / 2, packet.clock.first_sample_index,
            packet.rate, state.rate, packet.clock.input_rate, state.source_rate, packet.frequency,
            state.frequency, packet.clock.delay_input_samples, packet.config.mode));
    }
    state.next_seq = state.next_seq.checked_add(1).ok_or("receiver IQ sequence overflow")?;
    state.last_clock=packet.clock;
    if packet.seq == 0 {
        frames.decode.push(state.event(packet.slot, packet.clock, serde_json::json!({"type":"start"}))?);
    } else {
        let result = state.receiver.process(&packet.iq, packet.clock, packet.power_dbm)?;
        for audio in result.audio { state.audio(audio, &mut frames, &packet.config)?; }
        for event in result.events { frames.decode.push(state.event(packet.slot, packet.clock, event)?); }
        if let Some(job) = result.ft8_job {
            let mut pending = Pending::queued(shared,WORKERS+packet.slot,0);
            if let Some(lane)=packet.metrics.as_ref() {
                pending.tracking=Some(lane.job(true,(job.retained_bytes()/8) as u64,12_000.0,serde_json::json!({
                    "input_index":job.clock.first_sample_index.to_string(),"input_rate":job.clock.input_rate}),job.retained_bytes() as u64));
            }
            match search.try_send(SearchJob { slot:packet.slot, generation:packet.generation,
                channel_id:packet.channel_id, epoch:packet.epoch, job, pending }) {
                Ok(()) => { shared.counters.search_submitted.fetch_add(1, Ordering::Relaxed); }
                Err(error) => {
                    shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    let description=error.to_string();
                    let mut rejected=match error {mpsc::TrySendError::Full(job)|mpsc::TrySendError::Disconnected(job)=>job};
                    if let Some(tracking)=rejected.pending.tracking.as_mut() {tracking.reject(&description);}
                    return Err(format!("FT8 search queue failure: {description}"));
                }
            }
        }
    }
    if !shared.current(packet.slot,packet.generation) {return Ok(None);}
    if let Some(lane)=packet.metrics.as_ref() {lane.observe_receiver(state.receiver.processing_status());}
    let squelch = (packet.seq > 0).then(|| state.receiver.squelch_status()).flatten()
        .map(|(open, gain)| SquelchState { channel_id:packet.channel_id, epoch:packet.epoch,
            generation:packet.generation, measured:Instant::now(), open, gain });
    Ok(Some(Output {slot:packet.slot, generation:packet.generation, frames, squelch,accounting:None,permit:None,retired:false,stream:None}))
}

fn process_search(completed: SearchResult, states: &mut [Option<ChannelState>], shared: &Shared)
    -> Result<Option<Output>, String> {
    if !shared.current(completed.slot, completed.generation) { return Ok(None); }
    let Some(state) = states[completed.slot].as_mut() else { return Ok(None); };
    if state.generation != completed.generation || state.channel_id != completed.channel_id
        || state.epoch != completed.epoch { return Ok(None); }
    let clock = completed.result.clock;
    let position = completed.result.source_position;
    let events = state.receiver.accept_ft8(completed.result);
    let mut frames = ReceiverFrames::default();
    for event in events { frames.decode.push(state.event_at(completed.slot, clock, position, event)?); }
    let squelch = state.receiver.squelch_status().map(|(open, gain)| SquelchState {
        channel_id:completed.channel_id, epoch:completed.epoch, generation:completed.generation,
        measured:Instant::now(), open, gain });
    Ok(Some(Output { slot:completed.slot, generation:completed.generation, frames, squelch,accounting:None,permit:None,retired:false,stream:None }))
}

fn run_search(input: mpsc::Receiver<SearchJob>, destinations: Vec<mpsc::SyncSender<PendingWork>>, shared: Arc<Shared>, owner:Option<usize>) {
    websa_channelizer::host_resources::background_current_thread();
    while !shared.stopping.load(Ordering::Acquire)
        && owner.is_none_or(|slot|shared.branches[slot].lifecycle.load(Ordering::Acquire)!=2) {
        let mut job = match input.recv_timeout(Duration::from_millis(20)) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if !shared.current(job.slot, job.generation) {
            continue;
        }
        job.pending.started();
        if let Some(tracking)=job.pending.tracking.as_mut() {tracking.start();}
        let started = Instant::now();
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(||job.job.decode())) {
            Ok(result)=>result,Err(_)=>{
                if let Some(tracking)=job.pending.tracking.as_mut() {tracking.finish(Err("search execution panicked"));}
                shared.fail_generation(job.slot,job.generation,"search execution panicked".into());continue;
            }
        };
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        shared.counters.chain_count.fetch_add(1, Ordering::Relaxed);
        shared.counters.chain_total_ns.fetch_add(elapsed, Ordering::Relaxed);
        shared.counters.chain_max_ns.fetch_max(elapsed, Ordering::Relaxed);
        shared.counters.search_completed.fetch_add(1, Ordering::Relaxed);
        let counters=&shared.diagnostics.workers[WORKERS+job.slot];
        counters.busy_ns.fetch_add(elapsed,Ordering::Relaxed);
        counters.jobs.fetch_add(1,Ordering::Relaxed);
        counters.max_ns.fetch_max(elapsed,Ordering::Relaxed);
        if !shared.current(job.slot, job.generation) { continue; }
        if let Some(tracking)=job.pending.tracking.as_mut() {tracking.returning();}
        job.pending.enqueue(job.slot%WORKERS,0);
        if let Err(error) = destinations[job.slot % WORKERS].try_send(PendingWork {
            work: Work::Search(SearchResult {
                slot:job.slot, generation:job.generation, channel_id:job.channel_id, epoch:job.epoch, result,
            }), pending:job.pending,
        }) {
            shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
            let description=format!("FT8 result queue failure: {error}");
            let mut rejected=match error {mpsc::TrySendError::Full(work)|mpsc::TrySendError::Disconnected(work)=>work};
            if let Some(tracking)=rejected.pending.tracking.as_mut() {tracking.finish(Err(&description));}
            shared.fail_generation(job.slot,job.generation,description);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_statistics_follow_actual_dsp_success_squelch_and_failure() {
        let shared=Arc::new(Shared {stopping:AtomicBool::new(false),
            generations:(0..MAX_CHANNELS).map(|_|AtomicU64::new(1)).collect(),pending:AtomicUsize::new(0),
            failure:Mutex::new(None),counters:Counters::default(),diagnostics:QueueDiagnostics::default(),metrics:metrics::Directory::default(),branches:(0..MAX_CHANNELS).map(|_|Branch::default()).collect()});
        let lane=shared.metrics.lane(metrics::Identity {channel:1,generation:1,channel_id:1,epoch:1,
            receiver_uid:Some("receiver-test".into()),service_id:Some("service-test".into())});
        let (input,rx)=mpsc::sync_channel(INPUT_CAPACITY);
        let (output,received)=mpsc::sync_channel(RESULT_CAPACITY);
        let (search,_jobs)=mpsc::sync_channel(SEARCH_CAPACITY);
        for (seq,samples) in [(0,0),(1,4096),(9,4096)] {
            let mut pending=Pending::queued(&shared,0,samples as u64);
            pending.tracking=Some(lane.job(false,samples as u64,48000.0,serde_json::json!({"input_index":seq.to_string()}),(samples*8) as u64));
            let packet=Packet {slot:0,generation:1,config:Arc::new(ChannelConfig {squelch:0.0,..ChannelConfig::default()}),
                channel_id:1,epoch:1,seq,rate:48000.0,frequency:0.0,
                clock:InputClock {first_sample_index:0,input_rate:48000.0,delay_input_samples:0.0},
                power_dbm:Some(-100.0),iq:IqSamples::new(vec![0.1,0.0].repeat(samples)).unwrap(),radiofax_new_page:false,
                prepared:None,metrics:Some(lane.clone()),input_permit:None,commands:Vec::new()};
            input.try_send(PendingWork {work:Work::Packet(packet),pending}).unwrap_or_else(|_|panic!("test queue"));
        }
        drop(input);run_realtime(0,rx,output,search,shared.clone(),None);
        let snapshot=shared.metrics.snapshot();let row=&snapshot[0];
        assert_eq!(row["receiver_uid"],"receiver-test");assert_eq!(row["packets"]["accepted"],3);
        assert_eq!(row["packets"]["succeeded"],2);assert_eq!(row["packets"]["failed"],1);
        assert_eq!(row["packets"]["successful_samples"],4096);
        assert_eq!(shared.counters.completed_samples.load(Ordering::Relaxed),4096);
        assert_eq!(row["receiver"]["squelch"]["open"],false);
        assert!(row["packets"]["thread_cpu_ns"].as_u64().is_some());
        let frames:Vec<_>=received.try_iter().collect();assert_eq!(frames.len(),2);
        assert!(frames.iter().flat_map(|output|&output.frames.audio).any(|frame|frame.len()>96));
        drop(frames);assert_eq!(shared.metrics.snapshot()[0]["held_reference_bytes"],0);
    }

    #[test]
    fn radiofax_terminal_drain_publishes_partial_under_retiring_identity() {
        radiofax_retirement_order(false);
    }

    #[test]
    fn live_retirement_keeps_old_tail_before_new_start_and_all_iq() {
        radiofax_retirement_order(true);
    }

    fn radiofax_retirement_order(live: bool) {
        let mut config=ChannelConfig {mode:"radiofax".into(),freq:7.8e6,if_bw:3_000.0,
            ..ChannelConfig::default()};
        let mut workers=ReceiverWorkers::new().unwrap();
        workers.configure(&[config.clone()]).unwrap();
        let mut registry=[ChannelRegistryEntry {channel:1,channel_id:11,epoch:7,
            frequency_hz:config.freq,output_rate:12_000.0,input_rate:12_000.0,
            group_delay_samples:0.0,state:"active"}];
        let frame=|seq,index,epoch,freq,iq:&[f32]| websa_channelizer::encode_baseband(
            websa_channelizer::BasebandMagic::Primary,seq,12_000.0,freq,11,epoch,index,0.0,iq).unwrap();
        workers.submit(&frame(0,0,7,config.freq,&[]),&registry).unwrap();
        workers.finish().unwrap();
        let mut images=0;
        for block in 0..16_u32 {
            let first=u64::from(block)*1_500;
            let iq:Vec<f32>=(first..first+1_500).flat_map(|index| {
                let gray=if (index/1_810)%2==0 {0.15}else{0.85};
                let frequency=1_900.0+(gray-0.5)*800.0;
                let sample=(0.45*(std::f64::consts::TAU*frequency*index as f64/12_000.0).sin()) as f32;
                [sample,0.0]
            }).collect();
            workers.submit(&frame(block+1,first,7,config.freq,&iq),&registry).unwrap();
            images+=workers.finish().unwrap().decode.iter().filter(|frame| {
                let event:serde_json::Value=serde_json::from_slice(&frame[8..]).unwrap();
                event["event"]["type"]=="radiofax-image"
            }).count();
        }
        assert!(images>0);
        let mut closed=if live {workers.retire_channels(&[1])} else {workers.finish_radiofax(&[1])}.unwrap();
        if live {
            closed.append(workers.finish().unwrap());
            for tail in std::mem::take(&mut closed.retired) {closed.append(tail.frames);}
        }
        let events:Vec<serde_json::Value>=closed.decode.iter().map(|frame|
            serde_json::from_slice(&frame[8..]).unwrap()).collect();
        assert!(events.iter().all(|event|event["epoch"]==7));
        let image=events.iter().find(|event|event["event"]["type"]=="radiofax-image").unwrap();
        assert_eq!(image["event"]["row"],0);
        assert_eq!(image["event"]["meta"]["partial"],true);
        assert!(events.iter().any(|event|event["event"]["type"]=="radiofax-state"
            && event["event"]["reason"]=="eof"));
        config.freq+=1_000.0;
        workers.configure(&[config.clone()]).unwrap();
        registry[0].epoch=8;
        registry[0].frequency_hz=config.freq;
        workers.submit(&frame(0,0,8,config.freq,&[]),&registry).unwrap();
        let next=workers.finish().unwrap();
        let events:Vec<serde_json::Value>=next.decode.iter().map(|frame|
            serde_json::from_slice(&frame[8..]).unwrap()).collect();
        assert!(events.iter().all(|event|event["event"]["type"]!="radiofax-image"));
        assert_eq!(events.last().unwrap()["epoch"],8);
        assert_eq!(events.last().unwrap()["event"]["type"],"start");
        for seq in 1..=INPUT_CAPACITY as u32 * 4 {
            workers.submit(&frame(seq,u64::from(seq-1)*1_500,8,config.freq,&vec![0.1,0.0].repeat(1_500)),&registry).unwrap();
            workers.finish().unwrap();
        }
        let stats=workers.stats();
        assert_eq!(stats["submitted"],stats["completed"]);
        assert_eq!(stats["submitted_samples"],stats["completed_samples"]);
        assert_eq!(stats["completed_samples"],(16+INPUT_CAPACITY*4)*1_500);
        assert_eq!(stats["queue_full"],0);
    }

    #[test]
    fn ft8_pending_releases_on_stop_and_result_disconnect() {
        for stopped in [true, false] {
            let shared = Arc::new(Shared {
                stopping:AtomicBool::new(stopped),
                generations:(0..MAX_CHANNELS).map(|_|AtomicU64::new(1)).collect(),
                pending:AtomicUsize::new(0), failure:Mutex::new(None), counters:Counters::default(), diagnostics:QueueDiagnostics::default(), metrics:metrics::Directory::default(),branches:(0..MAX_CHANNELS).map(|_|Branch::default()).collect(),
            });
            let config = ChannelConfig {mode:"ft8".into(),if_bw:3200.0,..ChannelConfig::default()};
            let mut receiver = Receiver::new(config.receiver_config(12_000.0)).unwrap();
            let iq = vec![0.0; 60_000 * 2];
            let mut job = None;
            for block in 0..3 {
                job = receiver.process(&iq, InputClock {first_sample_index:block * 60_000,
                    input_rate:12_000.0,delay_input_samples:0.0}, None).unwrap().ft8_job;
            }
            let (search, queued) = mpsc::sync_channel(SEARCH_CAPACITY);
            search.try_send(SearchJob {slot:0,generation:1,channel_id:1,epoch:1,
                job:job.expect("15 seconds produces an FT8 search"),pending:Pending::new(&shared)})
                .unwrap_or_else(|_|panic!("test search queue full"));
            drop(search);
            let (destination, disconnected) = mpsc::sync_channel(INPUT_CAPACITY);
            drop(disconnected);
            run_search(queued, vec![destination], Arc::clone(&shared),None);
            assert_eq!(shared.pending.load(Ordering::Acquire), 0);
            if stopped {
                assert_eq!(shared.counters.search_completed.load(Ordering::Relaxed), 0);
            } else {
                assert!(shared.check().is_ok());
                assert!(shared.branch_failure(0).unwrap().error.contains("FT8 result queue failure"));
                assert_eq!(shared.counters.search_completed.load(Ordering::Relaxed), 1);
            }
        }
    }

    #[test]
    fn simultaneous_known_searches_keep_order_identity_and_report_duration() {
        let shared=Arc::new(Shared {stopping:AtomicBool::new(false),
            generations:(0..MAX_CHANNELS).map(|_|AtomicU64::new(1)).collect(),pending:AtomicUsize::new(0),
            failure:Mutex::new(None),counters:Counters::default(),diagnostics:QueueDiagnostics::default(),metrics:metrics::Directory::default(),branches:(0..MAX_CHANNELS).map(|_|Branch::default()).collect()});
        let source=super::super::service_fixture::fixture("ft8/ft8_cq_iq.bin").unwrap();
        let mut input=source.to_vec();input.resize(2*725_000,0.0);
        let mut search_inputs=Vec::new();
        let mut receivers=Vec::new();
        for slot in [6,7] {
            let config=ChannelConfig {channel:slot+1,mode:"ft8".into(),if_bw:3200.0,..ChannelConfig::default()};
            let mut receiver=Receiver::new(config.receiver_config(48_000.0)).unwrap();
            let mut job=None;
            for (block,iq) in input.chunks(16384).enumerate() {
                if let Some(ready)=receiver.process(iq,InputClock {first_sample_index:(block*8192) as u64,
                    input_rate:48_000.0,delay_input_samples:0.0},None).unwrap().ft8_job {job=Some(ready);break;}
            }
            let (search,queued)=mpsc::sync_channel(SEARCH_CAPACITY);
            search.try_send(SearchJob {slot:slot as usize,generation:1,channel_id:slot+11,epoch:9,
                job:job.expect("known full slot must produce a search"),pending:Pending::queued(&shared,WORKERS,0)})
                .unwrap_or_else(|_|panic!("bounded search queue rejected two jobs"));
            drop(search);search_inputs.push(queued);receivers.push(receiver);
        }
        let mut destinations=Vec::new();let mut results=Vec::new();
        for _ in 0..WORKERS {let (tx,rx)=mpsc::sync_channel(INPUT_CAPACITY);destinations.push(tx);results.push(rx);}
        let began=Instant::now();let mut handles=Vec::new();
        for (index,input) in search_inputs.into_iter().enumerate() {
            let shared=shared.clone();let destinations=destinations.clone();
            handles.push(thread::spawn(move||run_search(input,destinations,shared,Some(index+6))));
        }
        for handle in handles {handle.join().unwrap();}
        let elapsed=began.elapsed().as_secs_f64();
        // Heavy-search duration is reported for this host; correctness and
        // isolation do not inherit the former fixed twenty-percent margin.

        for (index,receiver) in receivers.iter_mut().enumerate() {
            let PendingWork {work,pending}=results[index+6].recv().unwrap();
            let Work::Search(result)=work else {panic!("expected search result")};
            assert_eq!(result.slot,index+6);assert_eq!(result.channel_id,index as u32+17);assert_eq!(result.epoch,9);
            let events=receiver.accept_ft8(result.result);
            assert!(events.iter().any(|event|event["type"]=="ft8" && event["text"].as_str().is_some_and(|text|text.contains("CQ"))),"{events:?}");
            drop(pending);
        }
        assert_eq!(shared.pending.load(Ordering::Acquire),0);
        assert!(shared.diagnostics.queued.lock().unwrap().is_empty());
        println!("simultaneous_searches=2 elapsed_seconds={elapsed:.6} peak_jobs={} margin_seconds={:.6}",
            shared.diagnostics.workers[WORKERS].peak_jobs.load(Ordering::Relaxed),15.0-elapsed);
    }

    #[test]
    fn failed_worker_releases_queued_work_before_replacement() {
        for _ in 0..4 {
            let shared = Arc::new(Shared {
                stopping: AtomicBool::new(false),
                generations: (0..MAX_CHANNELS).map(|_| AtomicU64::new(1)).collect(),
                pending: AtomicUsize::new(0), failure: Mutex::new(None), counters: Counters::default(), diagnostics:QueueDiagnostics::default(), metrics:metrics::Directory::default(),branches:(0..MAX_CHANNELS).map(|_|Branch::default()).collect(),
            });
            let (input, queued) = mpsc::sync_channel(INPUT_CAPACITY);
            let (output, _received) = mpsc::sync_channel(RESULT_CAPACITY);
            let (search, _jobs) = mpsc::sync_channel(SEARCH_CAPACITY);
            // Channels one and three share a worker; channel one fails with IQ still queued.
            for slot in [0, 2, 0, 2] {
                input.try_send(PendingWork {
                    work: Work::Packet(Packet {
                        slot, generation: 1,
                        config: Arc::new(ChannelConfig { enabled:true, ..ChannelConfig::for_slot(slot as u32 + 1) }),
                        channel_id:slot as u32 + 1, epoch:1, seq:1, rate:48_000.0, frequency:0.0,
                        clock:InputClock {first_sample_index:0,input_rate:48_000.0,delay_input_samples:0.0},
                        power_dbm:None, iq:IqSamples::new(vec![0.1,0.0]).unwrap(), radiofax_new_page:false,
                        prepared: None,metrics:None,input_permit:None,commands:Vec::new(),
                    }),
                    pending:Pending::new(&shared),
                }).unwrap_or_else(|_| panic!("test input queue full"));
            }
            assert_eq!(shared.pending.load(Ordering::Acquire), 4);
            run_realtime(0, queued, output, search, Arc::clone(&shared),None);
            assert!(shared.check().is_ok());
            assert!(shared.branch_failure(0).unwrap().error.contains("sequence zero"));
            assert_eq!(shared.pending.load(Ordering::Acquire), 0);
            assert_eq!(shared.counters.completed.load(Ordering::Relaxed), 0);
        }
    }
    use websa_channelizer::{encode_baseband, BasebandMagic};

    #[test]
    fn new_ssb_stream_identity_restarts_agc_without_a_loud_filter_transient() {
        let mut workers = ReceiverWorkers::new().unwrap();
        workers.configure(&[ChannelConfig {mode:"usb".into(),freq:14_270_000.0,
            if_bw:2700.0,agc:true,volume:1.0,squelch:-160.0,..ChannelConfig::default()}]).unwrap();
        let mut previous = None;
        for epoch in [7,9] {
            let registry = [ChannelRegistryEntry {channel:1,channel_id:1,epoch,
                frequency_hz:14_270_000.0,output_rate:6781.684027777777,input_rate:3_906_250.0,
                group_delay_samples:0.0,state:"active"}];
            let frame = |seq,index,samples:&[f32]| encode_baseband(BasebandMagic::Primary,
                seq,6781.684027777777,14_270_000.0,1,epoch as u64,index,0.0,samples).unwrap();
            workers.submit(&frame(0,0,&[]),&registry).unwrap();
            let mut pcm = Vec::new();
            for packet in 0..12_u32 {
                let iq:Vec<f32> = (packet * 240..(packet+1)*240).flat_map(|index| {
                    let phase = std::f64::consts::TAU * 1000.0 * index as f64 / 6781.684027777777;
                    [(0.0002 * phase.cos()) as f32,(0.0002 * phase.sin()) as f32]
                }).collect();
                workers.submit(&frame(packet+1,packet as u64*240*576,&iq),&registry).unwrap();
                for audio in workers.finish().unwrap().audio {
                    // AUD2 carries exact float PCM after its fixed 96-byte clock header.
                    pcm.extend(audio[96..].chunks_exact(4).map(|bytes|f32::from_le_bytes(bytes.try_into().unwrap())));
                }
            }
            assert!(pcm.len()>9600);
            let rms = (pcm[4800..9600].iter().map(|v|(*v as f64).powi(2)).sum::<f64>()/4800.0).sqrt();
            assert!((0.09..0.11).contains(&rms),"fresh identity {epoch} startup RMS {rms}");
            if let Some(expected) = previous {assert_eq!(pcm,expected,"stream replacement must start fresh");}
            previous=Some(pcm);
        }
    }

    #[test]
    fn wfm_default_emits_interleaved_stereo_aud2_and_explicit_false_emits_mono() {
        use std::f64::consts::TAU;
        for stereo in [true, false] {
            let mut config = ChannelConfig { mode: "wfm".into(), freq: 100e6,
                if_bw: 180_000.0, agc: false, squelch: -200.0, ..ChannelConfig::default() };
            if !stereo { config.stereo = false; }
            let mut workers = ReceiverWorkers::new().unwrap();
            workers.configure(&[config]).unwrap();
            let registry = [ChannelRegistryEntry { channel: 1, channel_id: 1, epoch: 7,
                frequency_hz: 100e6, output_rate: 384_000.0, input_rate: 384_000.0,
                group_delay_samples: 0.0, state: "active" }];
            let frame = |seq, index, iq: &[f32]| encode_baseband(BasebandMagic::Primary,
                seq, 384_000.0, 100e6, 1, 7, index, 0.0, iq).unwrap();
            workers.submit(&frame(0, 0, &[]), &registry).unwrap();
            let channels = if stereo { 2 } else { 1 };
            let mut difference_energy = 0.0_f64;
            let mut total_energy = 0.0_f64;
            for block in 0..120_u32 {
                let first = block as u64 * 4096;
                // Analytic FM multiplex: programme sum and difference at 1 kHz,
                // with a 19 kHz pilot and 38 kHz suppressed stereo subcarrier.
                let iq: Vec<f32> = (first..first + 4096).flat_map(|index| {
                    let t = index as f64 / 384_000.0;
                    let phase = 12.5 * (TAU * 1_000.0 * t).sin()
                        - (2_250.0 / 19_000.0) * (TAU * 19_000.0 * t).cos()
                        + (1_875.0 / 39_000.0) * (TAU * 39_000.0 * t).sin()
                        + (1_875.0 / 37_000.0) * (TAU * 37_000.0 * t).sin();
                    [(0.4 * phase.cos()) as f32, (0.4 * phase.sin()) as f32]
                }).collect();
                workers.submit(&frame(block + 1, first, &iq), &registry).unwrap();
                let output = workers.finish().unwrap();
                for audio in output.audio {
                    assert_eq!(&audio[..4], b"AUD2");
                    assert_eq!(u32::from_le_bytes(audio[56..60].try_into().unwrap()), channels);
                    let count = u32::from_le_bytes(audio[12..16].try_into().unwrap()) as usize;
                    assert_eq!(audio.len(), 96 + count * channels as usize * 4);
                    if block >= 100 {
                        let samples: Vec<f32> = audio[96..].chunks_exact(4)
                            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect();
                        for pair in samples.chunks_exact(channels as usize) {
                            total_energy += f64::from(pair[0]).powi(2);
                            if stereo { difference_energy += f64::from(pair[0] - pair[1]).powi(2); }
                        }
                    }
                }
            }
            assert!(total_energy > 1e-6, "WFM programme must reach the AUD2 payload");
            if stereo {
                assert!(difference_energy > total_energy * 0.01,
                    "stereo AUD2 must carry distinct left and right PCM, not duplicated mono");
            }
        }
    }

    #[test]
    fn input_meter_tracks_twenty_db_before_agc_and_expires() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let mut config = ChannelConfig { mode: "nfm".into(), freq: 100e6, ..ChannelConfig::default() };
        workers.configure(&[config.clone()]).unwrap();
        let mut registry = [ChannelRegistryEntry { channel: 1, channel_id: 1, epoch: 7,
            frequency_hz: 100e6, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let frame = |seq, amplitude| encode_baseband(BasebandMagic::Primary,
            seq, 48_000.0, 100e6, 1, 7, if seq == 0 { 0 } else { (seq as u64 - 1) * 4096 },
            0.0, &if seq == 0 { vec![] } else { vec![amplitude, 0.0].repeat(4096) }).unwrap();
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), None);
        workers.submit(&frame(0, 0.0), &registry).unwrap();
        workers.submit(&frame(1, 0.5), &registry).unwrap();
        workers.finish().unwrap();
        let high = workers.level_dbfs(&registry[0], Instant::now()).unwrap();
        assert!((high - 10.0 * 0.25_f64.log10()).abs() < 1e-6);
        config.agc = false;
        config.volume = 0.1;
        workers.configure(&[config.clone()]).unwrap();
        workers.submit(&frame(2, 0.5), &registry).unwrap();
        workers.finish().unwrap();
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), Some(high));
        workers.submit(&frame(3, 0.05), &registry).unwrap();
        workers.finish().unwrap();
        let low = workers.level_dbfs(&registry[0], Instant::now()).unwrap();
        assert!((high - low - 20.0).abs() < 1e-5);
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now() + Duration::from_millis(1501)), None);
        registry[0].epoch += 1;
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), None);
        registry[0].epoch -= 1;
        workers.reset_session().unwrap();
        workers.submit(&frame(0, 0.0), &registry).unwrap();
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), None);
        workers.submit(&frame(1, 0.0), &registry).unwrap();
        workers.finish().unwrap();
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), None);
        workers.submit(&frame(2, 0.5), &registry).unwrap();
        workers.finish().unwrap();
        config.freq += 1000.0;
        workers.configure(&[config]).unwrap();
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), None);
        workers.reset_session().unwrap();
        assert_eq!(workers.level_dbfs(&registry[0], Instant::now()), None);
    }

    #[test]
    fn squelch_reports_the_processed_gate_and_invalidates_stale_streams() {
        let mut workers = ReceiverWorkers::new().unwrap();
        workers.configure(&[ChannelConfig { mode:"am".into(), freq:100e6, squelch:-110.0,
            ..ChannelConfig::default() }]).unwrap();
        let mut registry = [ChannelRegistryEntry { channel:1, channel_id:1, epoch:7,
            frequency_hz:100e6, output_rate:48_000.0, input_rate:48_000.0,
            group_delay_samples:0.0, state:"active" }];
        workers.set_power([Some(-120.0), None, None]);
        for seq in 0..10_u32 {
            let iq = if seq == 0 { Vec::new() } else { vec![0.1, 0.0].repeat(4096) };
            let frame = encode_baseband(BasebandMagic::Primary, seq, 48_000.0, 100e6,
                1, 7, seq.saturating_sub(1) as u64 * 4096, 0.0, &iq).unwrap();
            workers.submit(&frame, &registry).unwrap();
            workers.finish().unwrap();
        }
        let gate = workers.squelch_status(&registry[0], Instant::now()).unwrap();
        assert!(!gate.0 && gate.1 < 0.01, "below-threshold RF must actually close the processed gate");
        workers.set_power([Some(-100.0), None, None]);
        let frame = encode_baseband(BasebandMagic::Primary, 10, 48_000.0, 100e6,
            1, 7, 9 * 4096, 0.0, &vec![0.1, 0.0].repeat(4096)).unwrap();
        workers.submit(&frame, &registry).unwrap(); workers.finish().unwrap();
        let gate = workers.squelch_status(&registry[0], Instant::now()).unwrap();
        assert!(gate.0 && gate.1 > 0.99, "above-threshold RF must open the processed gate");
        assert!(workers.squelch_status(&registry[0], Instant::now() + Duration::from_millis(1501)).is_none());
        registry[0].epoch += 1;
        assert!(workers.squelch_status(&registry[0], Instant::now()).is_none());
    }

    #[test]
    fn wfm_info_reports_the_stream_channel_frequency() {
        let mut workers = ReceiverWorkers::new().unwrap();
        workers.configure(&[ChannelConfig { mode: "wfm".into(), freq: 95.5e6,
            ..ChannelConfig::default() }]).unwrap();
        let registry = [ChannelRegistryEntry { channel: 1, channel_id: 1, epoch: 7,
            frequency_hz: 95.5e6, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let frame = |seq: u32, index: u64| encode_baseband(BasebandMagic::Primary,
            seq, 48_000.0, 95.5e6, 1, 7, index, 0.0,
            &if seq == 0 { Vec::new() } else { vec![0.0f32, 0.0].repeat(4096) }).unwrap();
        workers.submit(&frame(0, 0), &registry).unwrap();
        workers.submit(&frame(1, 0), &registry).unwrap();
        let frames = workers.finish().unwrap();
        let info = frames.decode.iter().find_map(|frame| {
            if frame.len() < 8 || &frame[..4] != b"SDE2" { return None; }
            let event: serde_json::Value = serde_json::from_slice(&frame[8..]).ok()?;
            (event["event"]["type"] == "wfm-info").then_some(event["event"].clone())
        }).expect("wfm-info event");
        assert_eq!(info["carrierHz"].as_f64(), Some(95.5e6));
    }

    #[test]
    fn session_reset_discards_old_output_and_restarts_receiver_dsp() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let config = ChannelConfig { mode: "nfm".into(), freq: 100e6, ..ChannelConfig::default() };
        workers.configure(&[config]).unwrap();
        let registry = [ChannelRegistryEntry { channel: 1, channel_id: 1, epoch: 7,
            frequency_hz: 100e6, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let iq: Vec<f32> = (0..4096).flat_map(|index| {
            let phase = index as f32 * 0.17;
            [phase.cos(), phase.sin()]
        }).collect();
        let frame = |seq, index, samples: &[f32]| encode_baseband(BasebandMagic::Primary,
            seq, 48_000.0, 100e6, 1, 7, index, 0.0, samples).unwrap();
        let marker = frame(0, 0, &[]);
        let first = frame(1, 0, &iq);
        workers.submit(&marker, &registry).unwrap();
        workers.submit(&first, &registry).unwrap();
        let before = workers.finish().unwrap();
        assert_eq!(before.audio_configs.len(),before.audio.len());
        assert!(before.audio_configs.iter().all(|config|config.mode=="nfm" && config.volume==0.8));
        assert!(before.audio.iter().any(|frame| u32::from_le_bytes(frame[8..12].try_into().unwrap()) > 0));
        workers.submit(&frame(2, 4096, &iq), &registry).unwrap();
        workers.reset_session().unwrap();
        let stale = workers.poll().unwrap();
        assert!(stale.audio.is_empty() && stale.decode.is_empty());
        assert_eq!(workers.shared.pending.load(Ordering::Acquire), 0);

        // Reusing the identity here also proves the old ChannelState was dropped;
        // generation filtering alone would reject this fresh sequence-zero marker.
        workers.submit(&marker, &registry).unwrap();
        workers.submit(&first, &registry).unwrap();
        let after = workers.finish().unwrap();
        assert_eq!(after.audio, before.audio);
        assert_eq!(after.audio_configs,before.audio_configs);
        assert_eq!(after.decode, before.decode);
    }
    #[test]
    fn radiofax_image_meta_receives_the_actual_channel_frequency() {
        let mut state = ChannelState {
            generation: 1, channel_id: 4, epoch: 9, frequency: 13_882_500.0, mode: "radiofax".into(),
            rate: 12_000.0, source_rate: 48_000.0, next_seq: 1, event_seq: 3, audio_seq: 0,
            audio_index: 0, audio_format: None, protocol_timing: None,
            last_clock: InputClock { first_sample_index: 80, input_rate: 48_000.0, delay_input_samples: 12.0 },
            receiver: Receiver::new(ChannelConfig { mode: "usb".into(), if_bw: 2_700.0,
                ..ChannelConfig::default() }.receiver_config(12_000.0)).unwrap(),
            metrics:None,config:Arc::new(ChannelConfig::default()),
        };
        let clock = InputClock { first_sample_index: 80, input_rate: 48_000.0, delay_input_samples: 12.0 };
        let frame = state.event(0, clock, serde_json::json!({
            "type":"radiofax-image","width":1,"row":0,"rows":1,"gray":"AA==","revision":1,
            "meta":{"serial":"page","epoch":9,"hz":null,"ioc":576,"lpm":120,"lines":1,
                "complete":false,"partial":true,"segmentIndex":0,"transmissionId":"1",
                "missingRows":[],"reason":null}
        })).unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&frame[8..]).unwrap();
        assert_eq!(envelope["seq"], 3);
        assert_eq!(envelope["timing_kind"], "causal_presentation");
        assert_eq!(envelope["delay_known"], false);
        assert_eq!(envelope["event"]["meta"]["hz"].as_f64(), Some(13_882_500.0));
        let state_frame = state.event(0, clock, serde_json::json!({"type":"radiofax-state","hz":1.0})).unwrap();
        let state_event: serde_json::Value = serde_json::from_slice(&state_frame[8..]).unwrap();
        assert_eq!(state_event["seq"], 4);
        assert_eq!(state_event["event"]["hz"], 1.0);
    }
    #[test]
    fn radiofax_controls_map_without_resetting_the_receiver_generation() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let mut config = ChannelConfig { mode: "usb".into(), freq: 100e6, ..ChannelConfig::default() };
        workers.configure(&[config.clone()]).unwrap();
        let generation = workers.shared.generations[0].load(Ordering::Acquire);
        config.radiofax_phase_pixels = 1.5;
        config.radiofax_clock_ppm = -2.0;
        workers.configure(&[config.clone()]).unwrap();
        assert_eq!(workers.shared.generations[0].load(Ordering::Acquire), generation);
        config.radiofax_ioc = "576".into();
        config.radiofax_lpm = "120".into();
        workers.configure(&[config.clone()]).unwrap();
        assert_eq!(workers.shared.generations[0].load(Ordering::Acquire), generation + 1);
        let mapped = workers.configs[0].receiver_config(12_000.0);
        assert_eq!(mapped.radiofax_ioc, "576");
        assert_eq!(mapped.radiofax_lpm, "120");
        assert_eq!(mapped.radiofax_phase_pixels, 1.5);
        assert_eq!(mapped.radiofax_clock_ppm, -2.0);
        config.radiofax_modulation = "ask".into();
        assert!(workers.configure(&[config]).unwrap_err().contains("invalid receiver configuration"));
    }

    #[test]
    fn mode_switch_discards_a_full_slow_decoder_queue_without_failing() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let drm = ChannelConfig { mode: "drm".into(), freq: 100e6, if_bw: 10_000.0, ..ChannelConfig::default() };
        workers.configure(&[drm]).unwrap();
        let old = [ChannelRegistryEntry { channel: 1, channel_id: 4, epoch: 7,
            frequency_hz: 100e6, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let frame = |seq, index| encode_baseband(BasebandMagic::Primary,
            seq, 48_000.0, 100e6, 4, 7, index, 0.0, &vec![0.1, 0.0].repeat(4096)).unwrap();
        let start = encode_baseband(BasebandMagic::Primary, 0, 48_000.0, 100e6, 4, 7, 0, 0.0, &[]).unwrap();
        workers.submit(&start, &old).unwrap();
        workers.finish().unwrap();
        for seq in 1..=INPUT_CAPACITY as u32 {
            workers.submit(&frame(seq, u64::from(seq - 1) * 4096), &old).unwrap();
        }
        // Whether the worker has already consumed part of the batch depends on
        // scheduling; the contract under test is that retiring whatever backlog
        // exists never latches a failure, not that a backlog must persist here.
        let am = ChannelConfig { mode: "am".into(), freq: 100e6, if_bw: 6_000.0, ..ChannelConfig::default() };
        workers.configure(&[am]).unwrap();
        assert!(workers.shared.check().is_ok(), "changing demodulation must not latch a queue failure");
        workers.finish().unwrap(); // Explicit test barrier; the control itself is asynchronous.
        assert_eq!(workers.shared.pending.load(Ordering::Acquire), 0);
        let fresh = [ChannelRegistryEntry { channel: 1, channel_id: 5, epoch: 8,
            frequency_hz: 100e6, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let start = encode_baseband(BasebandMagic::Primary, 0, 48_000.0, 100e6, 5, 8, 0, 0.0, &[]).unwrap();
        workers.submit(&start, &fresh).unwrap();
        let resumed = encode_baseband(BasebandMagic::Primary,
            1, 48_000.0, 100e6, 5, 8, 0, 0.0, &vec![0.1, 0.0].repeat(4096)).unwrap();
        workers.submit(&resumed, &fresh).unwrap();
        let output = workers.finish().unwrap();
        assert!(output.audio.iter().any(|audio| u32::from_le_bytes(audio[36..40].try_into().unwrap()) == 8),
            "the replacement demodulator must emit audio on its own stream");
    }

    #[test]
    fn reconfigure_preserves_unchanged_output_and_retires_deferred_generations() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let mut configs = [ChannelConfig::default(), ChannelConfig {
            channel: 2, enabled: true, ..ChannelConfig::default()
        }];
        workers.configure(&configs).unwrap();
        let (sender, outputs) = mpsc::sync_channel(RESULT_CAPACITY);
        workers.outputs = outputs;
        let output = |slot, byte| Output {
            slot, generation: workers.shared.generations[slot].load(Ordering::Acquire),
            frames: ReceiverFrames { audio: vec![vec![byte]], decode: vec![vec![byte]], ..ReceiverFrames::default() },
            squelch: None,accounting:None,permit:None,retired:false,stream:None,
        };
        sender.send(output(0, 1)).unwrap();
        sender.send(output(1, 2)).unwrap();
        configs[0].freq += 1.0;
        workers.configure(&configs).unwrap();
        let preserved = workers.poll().unwrap();
        assert_eq!(preserved.audio, vec![vec![2]]);
        assert_eq!(preserved.decode, vec![vec![2]]);

        sender.send(Output { slot: 1,
            generation: workers.shared.generations[1].load(Ordering::Acquire),
            frames: ReceiverFrames { audio: vec![vec![3]], decode: vec![vec![3]], ..ReceiverFrames::default() }, squelch: None,accounting:None,permit:None,retired:false,stream:None,
        }).unwrap();
        configs[0].freq += 1.0;
        workers.configure(&configs).unwrap();
        configs[1].freq += 1.0;
        workers.configure(&configs).unwrap();
        let retired = workers.poll().unwrap();
        assert!(retired.audio.is_empty() && retired.decode.is_empty());
    }

    #[test]
    fn failed_preparation_preserves_active_generation_and_contiguous_input() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let config = ChannelConfig { freq: 100e6, ..ChannelConfig::default() };
        workers.configure(&[config.clone()]).unwrap();
        let registry = [ChannelRegistryEntry { channel: 1, channel_id: 42, epoch: 7,
            frequency_hz: config.freq, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let frame = |seq| encode_baseband(BasebandMagic::Primary, seq, 48_000.0,
            config.freq, 42, 7, u64::from(seq.saturating_sub(1)) * 1024, 0.0,
            &if seq == 0 { Vec::new() } else { vec![0.1, 0.0].repeat(1024) }).unwrap();
        workers.submit(&frame(0), &registry).unwrap();
        workers.submit(&frame(1), &registry).unwrap();
        workers.finish().unwrap();
        let generation = workers.shared.generations[0].load(Ordering::Acquire);
        let active = workers.active.clone();
        let replacement = ChannelConfig { freq: 101e6, ..config.clone() };
        let prepared = PreparedDecoders::prepare_with(&[replacement], |_| Err("controlled constructor failure".into()));
        assert!(matches!(&prepared, Err(error) if error == "controlled constructor failure"));
        assert_eq!(*workers.configs[0], config);
        assert_eq!(workers.shared.generations[0].load(Ordering::Acquire), generation);
        assert_eq!(workers.active, active);
        workers.submit(&frame(2), &registry).unwrap();
        let output = workers.finish().unwrap();
        assert!(!output.audio.is_empty());
        assert_eq!(workers.stats()["completed"], 3);
        assert_eq!(workers.stats()["queue_full"], 0);
    }

    #[test]
    fn slow_admission_completes_before_producer_and_preserves_every_frame() {
        let config = ChannelConfig { freq: 100e6, ..ChannelConfig::default() };
        let candidate = config.clone();
        let (entered, entering) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(0);
        let preparation = thread::spawn(move || PreparedDecoders::prepare_with(&[candidate], |_| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Ok(None)
        }));
        entering.recv().unwrap();
        let mut workers = ReceiverWorkers::new().unwrap();
        // Admission can remain blocked indefinitely without admitting any IQ.
        assert_eq!(workers.stats()["submitted"], 0);
        assert_eq!(workers.stats()["pending"], 0);
        release.send(()).unwrap();
        workers.install_prepared(preparation.join().unwrap().unwrap());
        workers.configure(&[config.clone()]).unwrap();
        let registry = [ChannelRegistryEntry { channel: 1, channel_id: 43, epoch: 8,
            frequency_hz: config.freq, output_rate: 48_000.0, input_rate: 48_000.0,
            group_delay_samples: 0.0, state: "active" }];
        let blocks = INPUT_CAPACITY * 4;
        let mut audio_sequence = 0;
        let mut audio_index = 0;
        for seq in 0..=blocks as u32 {
            let frame = encode_baseband(BasebandMagic::Primary, seq, 48_000.0, config.freq,
                43, 8, u64::from(seq.saturating_sub(1)) * 1024, 0.0,
                &if seq == 0 { Vec::new() } else { vec![0.1, 0.0].repeat(1024) }).unwrap();
            workers.submit(&frame, &registry).unwrap();
            for audio in workers.finish().unwrap().audio {
                assert_eq!(u32::from_le_bytes(audio[32..36].try_into().unwrap()), 43);
                assert_eq!(u32::from_le_bytes(audio[36..40].try_into().unwrap()), 8);
                assert_eq!(u32::from_le_bytes(audio[8..12].try_into().unwrap()), audio_sequence);
                assert_eq!(u64::from_le_bytes(audio[64..72].try_into().unwrap()), audio_index);
                audio_sequence += 1;
                audio_index += u32::from_le_bytes(audio[12..16].try_into().unwrap()) as u64;
            }
        }
        assert!(audio_sequence > blocks as u32);
        assert_eq!(workers.stats()["submitted"], blocks + 1);
        assert_eq!(workers.stats()["completed"], blocks + 1);
        assert_eq!(workers.stats()["submitted_samples"], blocks * 1024);
        assert_eq!(workers.stats()["completed_samples"], blocks * 1024);
        assert_eq!(workers.stats()["queue_full"], 0);
    }

    #[test]
    fn replacement_admission_preserves_pending_old_start_and_consumed_rollback() {
        for retune in [false, true] {
            for consume_old in [false, true] {
                let mut workers = ReceiverWorkers::new().unwrap();
                let old = ChannelConfig { freq: 100e6, ..ChannelConfig::default() };
                let next = ChannelConfig { freq: if retune {101e6} else {100e6}, ..old.clone() };
                workers.install_prepared(PreparedDecoders::new(&[old.clone()]).unwrap());
                workers.configure(&[old.clone()]).unwrap();
                let rollback = workers.exchange_prepared(PreparedDecoders::new(&[next.clone()]).unwrap());
                assert!(workers.prepared.matches(0, &old));
                assert!(workers.prepared.matches(0, &next));
                workers.configure(&[old.clone()]).unwrap();
                if consume_old {
                    let registry = [ChannelRegistryEntry { channel: 1, channel_id: 42, epoch: 7,
                        frequency_hz: old.freq, output_rate: 48_000.0, input_rate: 48_000.0,
                        group_delay_samples: 0.0, state: "active" }];
                    let start = encode_baseband(BasebandMagic::Primary, 0, 48_000.0,
                        old.freq, 42, 7, 0, 0.0, &[]).unwrap();
                    workers.submit(&start, &registry).unwrap();
                    workers.finish().unwrap();
                    assert!(workers.prepared.previous[0].is_none());
                    assert!(workers.prepared.channels[0].is_some());
                }
                let rejected = workers.exchange_prepared(rollback);
                assert_eq!(workers.prepared.matches(0, &old), !consume_old);
                assert!(workers.prepared.previous[0].is_none());
                assert!(rejected.channels[0].is_some());
                assert_eq!(workers.stats()["queue_full"], 0);
            }
        }
    }

    #[test]
    fn committed_admission_releases_only_retired_candidate() {
        let mut workers = ReceiverWorkers::new().unwrap();
        let old = ChannelConfig { freq: 100e6, ..ChannelConfig::default() };
        let next = ChannelConfig { freq: 101e6, ..old.clone() };
        workers.install_prepared(PreparedDecoders::new(&[old.clone()]).unwrap());
        let _rollback = workers.exchange_prepared(PreparedDecoders::new(&[next.clone()]).unwrap());
        let retired = workers.release_prepared(&[next.clone()]);
        assert!(retired.previous[0].is_some());
        assert!(!workers.prepared.matches(0, &old));
        assert!(workers.prepared.matches(0, &next));
        assert!(workers.prepared.take(0, &next).is_some());
        assert!(!workers.prepared.matches(0, &next));
    }
}

#[cfg(all(test,feature="bench-instrument"))]
#[path="receiver_isolation_tests.rs"]
mod isolation_tests;
