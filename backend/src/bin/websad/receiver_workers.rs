use super::sdr_pipeline::ChannelRegistryEntry;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use websa_channelizer::{encode_audio, encode_decode_event, AudioFrameMetadata, MAX_BASEBAND_SAMPLES};
use websa_channelizer::receiver::{AudioBlock, Ft8Job, Ft8Result, InputClock, Receiver, ReceiverConfig, ReceiverEvent, SamplePosition};

const WORKERS: usize = 2;
const INPUT_CAPACITY: usize = 8;
const RESULT_CAPACITY: usize = 32;
const SEARCH_CAPACITY: usize = 3;
const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ChannelConfig {
    pub channel: u32,
    pub enabled: bool,
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

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            channel: 1, enabled: true, freq: 0.0, mode: "am".into(), if_bw: 6000.0,
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

    fn validate(&self) -> Result<(), String> {
        if !(1..=3).contains(&self.channel)
            || !self.freq.is_finite() || !self.if_bw.is_finite() || self.if_bw <= 0.0
            || !self.squelch.is_finite() || !self.volume.is_finite() || !(0.0..=2.0).contains(&self.volume)
            || !self.pitch.is_finite() || self.pitch <= 0.0 || !self.deemph_us.is_finite()
            || (self.mode == "cw" && !(100.0..=1600.0).contains(&self.pitch))
            || !self.nr_strength.is_finite() || !(0.0..=1.0).contains(&self.nr_strength)
            || !self.nr_atten_db.is_finite() || !(0.0..=100.0).contains(&self.nr_atten_db)
            || !matches!(self.nr_algorithm.as_str(), "wiener" | "dfn")
            || !matches!(self.mode.as_str(), "am" | "dsb" | "nfm" | "wfm" | "usb" | "lsb" | "cw" | "pm" | "ft8" | "sstv" | "drm" | "drmplus" | "radiofax")
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
        Ok(())
    }

    fn receiver_config(&self, input_rate: f64) -> ReceiverConfig {
        ReceiverConfig {
            mode: self.mode.clone(), input_rate, audio_rate: self.audio_rate, if_bw: self.if_bw,
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

#[derive(Default)]
pub(super) struct ReceiverFrames {
    pub audio: Vec<Vec<u8>>,
    pub decode: Vec<Vec<u8>>,
}

impl ReceiverFrames {
    fn append(&mut self, mut other: Self) {
        self.audio.append(&mut other.audio);
        self.decode.append(&mut other.decode);
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

struct Shared {
    stopping: AtomicBool,
    generations: [AtomicU64; 3],
    pending: AtomicUsize,
    failure: Mutex<Option<String>>,
    counters: Counters,
}

impl Shared {
    fn fail(&self, error: String) {
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_none() { *failure = Some(error); }
        self.stopping.store(true, Ordering::Release);
    }

    fn check(&self) -> Result<(), String> {
        match self.failure.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn current(&self, slot: usize, generation: u64) -> bool {
        !self.stopping.load(Ordering::Acquire) && self.generations[slot].load(Ordering::Acquire) == generation
    }
}

struct Pending(Arc<Shared>);

impl Pending {
    fn new(shared: &Arc<Shared>) -> Self {
        shared.pending.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(shared))
    }
}

impl Drop for Pending {
    fn drop(&mut self) { self.0.pending.fetch_sub(1, Ordering::AcqRel); }
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
    iq: Vec<f32>,
    radiofax_new_page: bool,
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
    Deactivate { slot:usize, generation:u64, reply:mpsc::SyncSender<()> },
}

struct Output {
    slot: usize,
    generation: u64,
    frames: ReceiverFrames,
    squelch: Option<SquelchState>,
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
    configs: [Arc<ChannelConfig>; 3],
    power: [Option<f64>; 3],
    levels: [Option<InputLevel>; 3],
    radiofax_new_page: [bool; 3],
    squelch: [Option<SquelchState>; 3],
    inputs: Vec<mpsc::SyncSender<PendingWork>>,
    outputs: mpsc::Receiver<Output>,
    threads: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl ReceiverWorkers {
    pub fn stop_failed(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        for generation in &self.shared.generations {generation.fetch_add(1, Ordering::AcqRel);}
        self.inputs.clear();
        self.levels=[None;3];
        self.squelch=[None;3];
    }

    pub fn stopped(&self)->bool {self.threads.iter().all(JoinHandle::is_finished)}

    pub fn new() -> Result<Self, String> {
        let shared = Arc::new(Shared {
            stopping: AtomicBool::new(false), generations: std::array::from_fn(|_| AtomicU64::new(1)),
            pending: AtomicUsize::new(0), failure: Mutex::new(None), counters: Counters::default(),
        });
        let (output_tx, outputs) = mpsc::sync_channel(RESULT_CAPACITY);
        let (search_tx, search_rx) = mpsc::sync_channel::<SearchJob>(SEARCH_CAPACITY);
        let mut inputs = Vec::with_capacity(WORKERS);
        let mut input_rxs = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let (tx, rx) = mpsc::sync_channel(INPUT_CAPACITY);
            inputs.push(tx);
            input_rxs.push(rx);
        }
        let mut workers = Self {
            configs: std::array::from_fn(|slot| Arc::new(ChannelConfig::for_slot(slot as u32 + 1))),
            power: [None; 3],
            levels: [None; 3],
            squelch: [None; 3],
            radiofax_new_page: [false; 3],
            inputs, outputs, threads: Vec::with_capacity(WORKERS + 1), shared,
        };
        for (index, rx) in input_rxs.into_iter().enumerate() {
            let shared = Arc::clone(&workers.shared);
            let output = output_tx.clone();
            let search = search_tx.clone();
            let (ready_tx, ready_rx) = mpsc::sync_channel(0);
            let handle = thread::Builder::new().name(format!("receiver-{index}"))
                .spawn(move || {
                    websa_channelizer::nr::prepare_dfn();
                    if ready_tx.send(()).is_err() { return; }
                    let failure = Arc::clone(&shared);
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
                        run_realtime(rx, output, search, shared))).is_err() {
                        failure.fail("receiver realtime worker panicked".into());
                    }
                })
                .map_err(|error| format!("spawn receiver worker: {error}"))?;
            ready_rx.recv().map_err(|_| "receiver DFN preparation failed".to_string())?;
            workers.threads.push(handle);
        }
        let shared = Arc::clone(&workers.shared);
        let destinations = workers.inputs.clone();
        workers.threads.push(thread::Builder::new().name("receiver-ft8".into())
            .spawn(move || {
                let failure = Arc::clone(&shared);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
                    run_search(search_rx, destinations, shared))).is_err() {
                    failure.fail("receiver FT8 worker panicked".into());
                }
            })
            .map_err(|error| format!("spawn FT8 worker: {error}"))?);
        Ok(workers)
    }

    pub fn set_power(&mut self, power: [Option<f64>; 3]) {
        self.power = power.map(|value| value.filter(|value| value.is_finite()));
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
    pub fn request_radiofax_page(&mut self, channel: u32) -> Result<(), String> {
        if !(1..=3).contains(&channel) { return Err("radiofax channel must be 1..=3".into()); }
        self.radiofax_new_page[(channel - 1) as usize] = true;
        Ok(())
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
            let generation = self.shared.generations[slot].load(Ordering::Acquire);
            let (reply, done) = mpsc::sync_channel(1);
            let pending = Pending::new(&self.shared);
            if let Err(error) = self.inputs[slot % WORKERS].try_send(PendingWork {
                work: Work::Deactivate { slot, generation, reply }, pending,
            }) {
                return Err(format!("receiver session reset queue failure: {error}"));
            }
            done.recv_timeout(Duration::from_secs(5))
                .map_err(|error| format!("receiver session reset acknowledgement: {error}"))?;
        }
        self.finish()?;
        self.power = [None; 3];
        Ok(())
    }

    pub fn configure(&mut self, configs: &[ChannelConfig]) -> Result<(), String> {
        self.shared.check()?;
        let mut desired = std::array::from_fn::<_, 3, _>(|slot| ChannelConfig::for_slot(slot as u32 + 1));
        let mut seen = [false; 3];
        for config in configs {
            config.validate()?;
            let slot = config.channel as usize - 1;
            if seen[slot] { return Err(format!("duplicate receiver channel {}", config.channel)); }
            seen[slot] = true;
            desired[slot] = config.clone();
        }
        for (slot, config) in desired.into_iter().enumerate() {
            if *self.configs[slot] != config {
                if self.configs[slot].enabled != config.enabled || self.configs[slot].freq != config.freq
                    || self.configs[slot].mode != config.mode || self.configs[slot].if_bw != config.if_bw
                    || self.configs[slot].pitch != config.pitch || self.configs[slot].sstv_mode != config.sstv_mode
                    || self.configs[slot].radiofax_acquisition != config.radiofax_acquisition
                    || self.configs[slot].radiofax_ioc != config.radiofax_ioc || self.configs[slot].radiofax_lpm != config.radiofax_lpm
                    || self.configs[slot].radiofax_modulation != config.radiofax_modulation || self.configs[slot].radiofax_polarity != config.radiofax_polarity
                    || self.configs[slot].radiofax_clock_recovery != config.radiofax_clock_recovery
                    || self.configs[slot].audio_rate != config.audio_rate || self.configs[slot].stereo != config.stereo {
                    self.bump_generation(slot)?;
                }
                if self.configs[slot].enabled && !config.enabled {
                    let generation=self.shared.generations[slot].load(Ordering::Acquire);
                    let (reply,done)=mpsc::sync_channel(1);
                    let pending=Pending::new(&self.shared);
                    if let Err(error)=self.inputs[slot%WORKERS].try_send(PendingWork {
                        work:Work::Deactivate {slot,generation,reply},pending,
                    }) {
                        let error=format!("receiver deactivate queue failure: {error}");
                        self.shared.fail(error.clone());return Err(error);
                    }
                    done.recv_timeout(Duration::from_secs(5)).map_err(|error|format!("receiver deactivate acknowledgement: {error}"))?;
                }
                self.configs[slot] = Arc::new(config);
            }
        }
        Ok(())
    }

    fn bump_generation(&self, slot: usize) -> Result<(), String> {
        self.shared.generations[slot].fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |generation| generation.checked_add(1))
            .map(|_| ()).map_err(|_| "receiver configuration generation overflow".into())
    }

    pub fn submit(&mut self, frame: &[u8], registry: &[ChannelRegistryEntry]) -> Result<(), String> {
        self.shared.check()?;
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
        if u32_at(4) != 2 || samples > MAX_BASEBAND_SAMPLES || frame.len() != 56 + samples * 8
            || !(1.0..=1e9).contains(&rate) || !frequency.is_finite()
            || channel_id == 0 || epoch == 0 || !delay.is_finite() || delay < 0.0
            || (seq == 0) != (samples == 0) || input_index == u64::MAX
        { return Err("invalid receiver IQ frame metadata".into()); }
        let row = registry.iter().find(|row| row.channel_id == channel_id && row.epoch == epoch)
            .ok_or_else(|| format!("receiver IQ identity {channel_id}/{epoch} missing from registry"))?;
        if !(1..=3).contains(&row.channel) || row.state != "active"
            || row.output_rate != rate || row.frequency_hz != frequency
            || !(1.0..=1e9).contains(&row.input_rate)
            || !row.group_delay_samples.is_finite() || row.group_delay_samples < 0.0
            || !(delay * row.input_rate / rate).is_finite()
        { return Err("receiver IQ registry clock mismatch".into()); }
        let slot = row.channel as usize - 1;
        if !self.configs[slot].enabled { return Err(format!("IQ submitted for disabled receiver {}", row.channel)); }
        let mut iq = Vec::with_capacity(samples * 2);
        let mut power = 0.0_f64;
        for bytes in frame[56..].chunks_exact(4) {
            let value = f32::from_le_bytes(bytes.try_into().unwrap());
            if !value.is_finite() { return Err("nonfinite receiver IQ sample".into()); }
            power += f64::from(value) * f64::from(value);
            iq.push(value);
        }
        let packet = Packet {
            slot, generation: self.shared.generations[slot].load(Ordering::Acquire),
            config: Arc::clone(&self.configs[slot]), channel_id, epoch, seq, rate, frequency,
            power_dbm: self.power[slot],
            clock: InputClock { first_sample_index: input_index, input_rate: row.input_rate,
                delay_input_samples: delay * row.input_rate / rate }, iq,
            radiofax_new_page: std::mem::replace(&mut self.radiofax_new_page[slot], false),
        };
        let pending = Pending::new(&self.shared);
        match self.inputs[slot % WORKERS].try_send(PendingWork { work:Work::Packet(packet), pending }) {
            Ok(()) => {
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
                self.shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                let error = format!("receiver input queue failure: {error}; channel={} mode={} rate={rate} seq={seq} pending={} chain_max_ns={}",
                    row.channel, self.configs[slot].mode,
                    self.shared.pending.load(Ordering::Acquire),
                    self.shared.counters.chain_max_ns.load(Ordering::Relaxed));
                self.shared.fail(error.clone());
                Err(error)
            }
        }
    }

    pub fn poll(&mut self) -> Result<ReceiverFrames, String> {
        self.shared.check()?;
        let mut frames = ReceiverFrames::default();
        while let Ok(output) = self.outputs.try_recv() {
            if self.shared.current(output.slot, output.generation) {
                self.squelch[output.slot] = output.squelch;
                frames.append(output.frames);
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
                Ok(output) if self.shared.current(output.slot, output.generation) => {
                    self.squelch[output.slot] = output.squelch;
                    frames.append(output.frames);
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("receiver output queue disconnected".into()),
            }
        }
    }


    pub fn stats(&self) -> serde_json::Value {
        let c = &self.shared.counters;
        let load = |v: &AtomicU64| v.load(Ordering::Relaxed);
        serde_json::json!({
            "submitted":load(&c.submitted), "completed":load(&c.completed),
            "submitted_samples":load(&c.submitted_samples), "completed_samples":load(&c.completed_samples),
            "queue_full":load(&c.queue_full), "audio_frames":load(&c.audio_frames),
            "audio_samples":load(&c.audio_samples), "decode_frames":load(&c.decode_frames),
            "search_submitted":load(&c.search_submitted), "search_completed":load(&c.search_completed),
            "pending":self.shared.pending.load(Ordering::Acquire),
            "native_chain":{"count":load(&c.chain_count),"total_ns":load(&c.chain_total_ns),"max_ns":load(&c.chain_max_ns)},
            "limits":{"workers":WORKERS,"input_per_worker":INPUT_CAPACITY,"results":RESULT_CAPACITY,
                "ft8_jobs":SEARCH_CAPACITY,"max_iq_samples":MAX_BASEBAND_SAMPLES,"max_event_bytes":MAX_EVENT_BYTES}
        })
    }
}

impl Drop for ReceiverWorkers {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.inputs.clear();
        for handle in self.threads.drain(..) {
            if handle.join().is_err() { self.shared.fail("receiver worker panicked".into()); }
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
    receiver: Receiver,
}

impl ChannelState {
    fn new(packet: &Packet) -> Result<Self, String> {
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
            receiver: Receiver::new(packet.config.receiver_config(packet.rate))?,
        })
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
        } else if matches!(self.mode.as_str(), "sstv" | "radiofax") {
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

    fn audio(&mut self, block: AudioBlock, frames: &mut ReceiverFrames) -> Result<(), String> {
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
            self.audio_seq = 1;
        }
        metadata.seq = self.audio_seq;
        frames.audio.push(encode_audio(metadata, &block.samples).map_err(str::to_owned)?);
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

fn run_realtime(input: mpsc::Receiver<PendingWork>, output: mpsc::SyncSender<Output>,
    search: mpsc::SyncSender<SearchJob>, shared: Arc<Shared>) {
    let mut states: [Option<ChannelState>; 3] = std::array::from_fn(|_| None);
    while !shared.stopping.load(Ordering::Acquire) {
        let PendingWork { work, pending } = match input.recv_timeout(Duration::from_millis(20)) {
            Ok(work) => work,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let started = Instant::now();
        let result = match work {
            Work::Packet(packet) => {
                let count = packet.iq.len() as u64 / 2;
                let result = process_packet(packet, &mut states, &search, &shared);
                shared.counters.completed.fetch_add(1, Ordering::Relaxed);
                shared.counters.completed_samples.fetch_add(count, Ordering::Relaxed);
                result
            }
            Work::Search(completed) => process_search(completed, &mut states, &shared),
            Work::Deactivate {slot,generation,reply} => {
                if shared.current(slot,generation) {states[slot]=None;}
                let _=reply.try_send(());
                Ok(None)
            }
        };
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        shared.counters.chain_count.fetch_add(1, Ordering::Relaxed);
        shared.counters.chain_total_ns.fetch_add(elapsed, Ordering::Relaxed);
        shared.counters.chain_max_ns.fetch_max(elapsed, Ordering::Relaxed);
        match result {
            Ok(Some(result)) => {
                shared.counters.audio_frames.fetch_add(result.frames.audio.len() as u64, Ordering::Relaxed);
                let samples: u64 = result.frames.audio.iter().map(|f|
                    u32::from_le_bytes(f[12..16].try_into().unwrap()) as u64).sum();
                shared.counters.audio_samples.fetch_add(samples, Ordering::Relaxed);
                shared.counters.decode_frames.fetch_add(result.frames.decode.len() as u64, Ordering::Relaxed);
                if let Err(error) = output.try_send(result) {
                    shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    shared.fail(format!("receiver output queue failure: {error}"));
                }
            }
            Ok(None) => {}
            Err(error) => shared.fail(error),
        }
        drop(pending);
    }
}

fn process_packet(packet: Packet, states: &mut [Option<ChannelState>; 3],
    search: &mpsc::SyncSender<SearchJob>, shared: &Arc<Shared>) -> Result<Option<Output>, String> {
    if !shared.current(packet.slot, packet.generation) { return Ok(None); }
    let needs_reset = states[packet.slot].as_ref().is_none_or(|state|
        state.generation != packet.generation || state.channel_id != packet.channel_id || state.epoch != packet.epoch);
    if needs_reset {
        if states[packet.slot].as_ref().is_some_and(|state|
            state.channel_id == packet.channel_id && state.epoch == packet.epoch) {
            let state = states[packet.slot].as_ref().unwrap();
            return Err(format!("receiver config reset requires a new fine channel identity: channel={} identity={}/{} generation={} previous_generation={} seq={} expected_seq={} mode={} previous_mode={}",
                packet.slot + 1, packet.channel_id, packet.epoch, packet.generation, state.generation,
                packet.seq, state.next_seq, packet.config.mode, state.mode));
        }
        if let Some(previous) = states[packet.slot].as_mut() { previous.receiver.finish_radiofax()?; }
        states[packet.slot] = Some(ChannelState::new(&packet)?);
    }
    let state = states[packet.slot].as_mut().unwrap();
    if packet.radiofax_new_page { state.receiver.request_radiofax_page(); }
    state.receiver.configure(packet.config.receiver_config(packet.rate))?;
    if packet.seq != state.next_seq || packet.rate != state.rate || packet.clock.input_rate != state.source_rate
        || packet.frequency != state.frequency {
        return Err(format!("receiver IQ sequence or clock discontinuity: channel={} identity={}/{} generation={} previous_generation={} seq={} expected_seq={} samples={} input_index={} rate={} expected_rate={} input_rate={} expected_input_rate={} frequency={} expected_frequency={} delay_input_samples={} mode={}",
            packet.slot + 1, packet.channel_id, packet.epoch, packet.generation, state.generation,
            packet.seq, state.next_seq, packet.iq.len() / 2, packet.clock.first_sample_index,
            packet.rate, state.rate, packet.clock.input_rate, state.source_rate, packet.frequency,
            state.frequency, packet.clock.delay_input_samples, packet.config.mode));
    }
    state.next_seq = state.next_seq.checked_add(1).ok_or("receiver IQ sequence overflow")?;
    let mut frames = ReceiverFrames::default();
    if packet.seq == 0 {
        frames.decode.push(state.event(packet.slot, packet.clock, serde_json::json!({"type":"start"}))?);
    } else {
        let result = state.receiver.process(&packet.iq, packet.clock, packet.power_dbm)?;
        for audio in result.audio { state.audio(audio, &mut frames)?; }
        for event in result.events { frames.decode.push(state.event(packet.slot, packet.clock, event)?); }
        if let Some(job) = result.ft8_job {
            let pending = Pending::new(shared);
            match search.try_send(SearchJob { slot:packet.slot, generation:packet.generation,
                channel_id:packet.channel_id, epoch:packet.epoch, job, pending }) {
                Ok(()) => { shared.counters.search_submitted.fetch_add(1, Ordering::Relaxed); }
                Err(error) => {
                    shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    return Err(format!("FT8 search queue failure: {error}"));
                }
            }
        }
    }
    let squelch = (packet.seq > 0).then(|| state.receiver.squelch_status()).flatten()
        .map(|(open, gain)| SquelchState { channel_id:packet.channel_id, epoch:packet.epoch,
            generation:packet.generation, measured:Instant::now(), open, gain });
    Ok(Some(Output {slot:packet.slot, generation:packet.generation, frames, squelch}))
}

fn process_search(completed: SearchResult, states: &mut [Option<ChannelState>; 3], shared: &Shared)
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
    Ok(Some(Output { slot:completed.slot, generation:completed.generation, frames, squelch }))
}

fn run_search(input: mpsc::Receiver<SearchJob>, destinations: Vec<mpsc::SyncSender<PendingWork>>, shared: Arc<Shared>) {
    while !shared.stopping.load(Ordering::Acquire) {
        let job = match input.recv_timeout(Duration::from_millis(20)) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if !shared.current(job.slot, job.generation) {
            continue;
        }
        let started = Instant::now();
        let result = job.job.decode();
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        shared.counters.chain_count.fetch_add(1, Ordering::Relaxed);
        shared.counters.chain_total_ns.fetch_add(elapsed, Ordering::Relaxed);
        shared.counters.chain_max_ns.fetch_max(elapsed, Ordering::Relaxed);
        shared.counters.search_completed.fetch_add(1, Ordering::Relaxed);
        if !shared.current(job.slot, job.generation) { continue; }
        if let Err(error) = destinations[job.slot % WORKERS].try_send(PendingWork {
            work: Work::Search(SearchResult {
                slot:job.slot, generation:job.generation, channel_id:job.channel_id, epoch:job.epoch, result,
            }), pending:job.pending,
        }) {
            shared.counters.queue_full.fetch_add(1, Ordering::Relaxed);
            shared.fail(format!("FT8 result queue failure: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ft8_pending_releases_on_stop_and_result_disconnect() {
        for stopped in [true, false] {
            let shared = Arc::new(Shared {
                stopping:AtomicBool::new(stopped),
                generations:std::array::from_fn(|_|AtomicU64::new(1)),
                pending:AtomicUsize::new(0), failure:Mutex::new(None), counters:Counters::default(),
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
            run_search(queued, vec![destination], Arc::clone(&shared));
            assert_eq!(shared.pending.load(Ordering::Acquire), 0);
            if stopped {
                assert_eq!(shared.counters.search_completed.load(Ordering::Relaxed), 0);
            } else {
                assert!(shared.check().unwrap_err().contains("FT8 result queue failure"));
                assert_eq!(shared.counters.search_completed.load(Ordering::Relaxed), 1);
            }
        }
    }

    #[test]
    fn failed_worker_releases_queued_work_before_replacement() {
        for _ in 0..4 {
            let shared = Arc::new(Shared {
                stopping: AtomicBool::new(false),
                generations: std::array::from_fn(|_| AtomicU64::new(1)),
                pending: AtomicUsize::new(0), failure: Mutex::new(None), counters: Counters::default(),
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
                        power_dbm:None, iq:vec![0.1, 0.0], radiofax_new_page:false,
                    }),
                    pending:Pending::new(&shared),
                }).unwrap_or_else(|_| panic!("test input queue full"));
            }
            assert_eq!(shared.pending.load(Ordering::Acquire), 4);
            run_realtime(queued, output, search, Arc::clone(&shared));
            assert!(shared.check().unwrap_err().contains("sequence zero"));
            assert_eq!(shared.pending.load(Ordering::Acquire), 0);
            assert_eq!(shared.counters.completed.load(Ordering::Relaxed), 1);
        }
    }
    use websa_channelizer::{encode_baseband, BasebandMagic};

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
        assert_eq!(after.decode, before.decode);
    }
    #[test]
    fn radiofax_image_meta_receives_the_actual_channel_frequency() {
        let mut state = ChannelState {
            generation: 1, channel_id: 4, epoch: 9, frequency: 13_882_500.0, mode: "radiofax".into(),
            rate: 12_000.0, source_rate: 48_000.0, next_seq: 1, event_seq: 3, audio_seq: 0,
            audio_index: 0, audio_format: None, protocol_timing: None,
            receiver: Receiver::new(ChannelConfig { mode: "usb".into(), if_bw: 2_700.0,
                ..ChannelConfig::default() }.receiver_config(12_000.0)).unwrap(),
        };
        let clock = InputClock { first_sample_index: 80, input_rate: 48_000.0, delay_input_samples: 12.0 };
        let frame = state.event(0, clock, serde_json::json!({
            "type":"radiofax-image","width":1,"row":0,"rows":1,"gray":"AA==","revision":1,
            "meta":{"serial":"page","epoch":9,"hz":null,"ioc":576,"lpm":120,"lines":1,
                "complete":false,"partial":true,"segmentIndex":0,"transmissionId":"tx"}
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
}
