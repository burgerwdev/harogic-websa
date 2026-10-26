//! One independently owned receiver. Protocol decoders consume unenhanced IQ; listener
//! processing is downstream of demodulation and never feeds back into a decoder.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use websa_dsp::analog::{self, AnalogDemod};
use websa_dsp::audio::{self, AudioPolicy};
use crate::rational::Rational;
use websa_dsp::digital::{self, ft8::Ft8Decoder};
use websa_dsp::pipeline::{AnalogPcm, AudioChain};
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig, DigitalDemodulator};
use crate::cw::CwDecoder;
use crate::nr::{NoiseReduction, NrAlgorithm};

const FT8_RATE: u32 = 12_000;
const MAX_BLOCK_COMPLEX: usize = 65_536;
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
pub type ReceiverEvent = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReceiverConfig {
    pub mode: String,
    /// IQ cadence after channelization, distinct from InputClock's source clock.
    pub input_rate: f64,
    pub audio_rate: u32,
    pub if_bw: f64,
    pub pitch: f64,
    pub agc: bool,
    pub am_sync: bool,
    /// Server-owned AM control generation, echoed from the configuration actually applied.
    pub am_sync_generation: u32,
    pub gain: f32,
    pub squelch_dbm: f64,
    pub nr: bool,
    pub nr_strength: f64,
    pub nr_algorithm: String,
    pub nr_atten_db: f32,
    pub deemph_us: f64,
    pub stereo: bool,
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
impl Default for ReceiverConfig {
    fn default() -> Self {
        Self { mode: "am".into(), input_rate: 48_000.0, audio_rate: 48_000,
            if_bw: 6_000.0, pitch: 700.0, agc: true, am_sync: false, am_sync_generation: 0, gain: 1.0,
            squelch_dbm: -110.0, nr: false, nr_strength: 0.6,
            nr_algorithm: "wiener".into(), nr_atten_db: 10.0, deemph_us: -1.0,
            stereo: true, sstv_mode: None,
            radiofax_acquisition: "auto".into(), radiofax_ioc: "auto".into(),
            radiofax_lpm: "auto".into(), radiofax_modulation: "fm".into(),
            radiofax_polarity: "normal".into(), radiofax_clock_recovery: "auto".into(),
            radiofax_phase_pixels: 0.0, radiofax_clock_ppm: 0.0 }
    }
}
impl ReceiverConfig {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.mode.as_str(), "am"|"dsb"|"usb"|"lsb"|"cw"|"nfm"|"wfm"|"pm"|"ft8"|"sstv"|"drm"|"drmplus"|"radiofax") {
            return Err(format!("unsupported receiver mode {}", self.mode));
        }
        if clock_ratio(self.input_rate).is_none() || !(8_000..=192_000).contains(&self.audio_rate)
            || !self.if_bw.is_finite() || self.if_bw <= 0.0 || !self.pitch.is_finite()
            || self.pitch <= 0.0 || !self.gain.is_finite() || self.gain < 0.0
            || !self.squelch_dbm.is_finite() || !self.nr_strength.is_finite()
            || !(0.0..=1.0).contains(&self.nr_strength) || !self.nr_atten_db.is_finite()
            || !(0.0..=100.0).contains(&self.nr_atten_db) || !self.deemph_us.is_finite() {
            return Err("invalid receiver geometry or listening controls".into());
        }
        if self.mode == "cw" && !(100.0..=1600.0).contains(&self.pitch) {
            return Err("CW requires a 100–1600 Hz pitch".into());
        }
        let listen_mode = if matches!(self.mode.as_str(), "ft8"|"sstv"|"radiofax") { "usb" } else { self.mode.as_str() };
        if let Some(spec) = analog::spec_for(listen_mode) {
            let (low, high) = spec.band_hz(self.if_bw.max(50.0), self.pitch);
            if low.abs().max(high.abs()) >= self.input_rate / 2.0 {
                return Err("receiver passband must fit below the IQ Nyquist limit".into());
            }
        }
        if !matches!(self.nr_algorithm.as_str(), "wiener"|"dfn") {
            return Err("NR algorithm must be wiener or dfn".into());
        }
        if self.sstv_mode.is_some_and(|index| index >= digital::sstv::SSTV_MODES.len()) {
            return Err("invalid SSTV mode index".into());
        }
        if self.mode == "radiofax" {
            websa_dsp::digital::radiofax::validate_radiofax(&self.radiofax_view())?;
        }
        Ok(())
    }
    fn nr_algorithm(&self) -> NrAlgorithm {
        if !self.nr || matches!(self.mode.as_str(), "drm"|"drmplus") { NrAlgorithm::Off }
        else if self.nr_algorithm == "dfn" { NrAlgorithm::Dfn }
        else { NrAlgorithm::Wiener }
    }
    fn radiofax_view(&self) -> websa_dsp::plugin::RadiofaxConfigView {
        websa_dsp::plugin::RadiofaxConfigView {
            acquisition: self.radiofax_acquisition.clone(), ioc: self.radiofax_ioc.clone(),
            lpm: self.radiofax_lpm.clone(), modulation: self.radiofax_modulation.clone(),
            polarity: self.radiofax_polarity.clone(), clock_recovery: self.radiofax_clock_recovery.clone(),
            phase_pixels: self.radiofax_phase_pixels, clock_ppm: self.radiofax_clock_ppm,
        }
    }
    fn radiofax_protocol_changed(&self, config: &Self) -> bool {
        self.radiofax_acquisition != config.radiofax_acquisition || self.radiofax_ioc != config.radiofax_ioc
            || self.radiofax_lpm != config.radiofax_lpm || self.radiofax_modulation != config.radiofax_modulation
            || self.radiofax_polarity != config.radiofax_polarity
            || self.radiofax_clock_recovery != config.radiofax_clock_recovery
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InputClock {
    /// Exact causal capture anchor supplied by the channelizer, never a local counter.
    pub first_sample_index: u64,
    /// Raw capture clock rate (not the IQ cadence after decimation).
    pub input_rate: f64,
    pub delay_input_samples: f64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SamplePosition {
    pub index: u64,
    pub fraction_num: u64,
    pub fraction_den: u64,
}
impl SamplePosition {
    pub fn checked_advance(self, frames: u64, source_rate: f64, pcm_rate: u32) -> Result<Self, String> {
        if self.fraction_den == 0 || self.fraction_num >= self.fraction_den || pcm_rate == 0 {
            return Err("invalid sample position or PCM rate".into());
        }
        let (source_num, source_den) = clock_ratio(source_rate).ok_or("unsupported source clock rate")?;
        let step_num = frames as u128 * source_num as u128;
        let step_den = pcm_rate as u128 * source_den as u128;
        let mut a = self.fraction_den as u128;
        let mut b = step_den;
        while b != 0 { (a,b) = (b,a%b); }
        let denominator = (self.fraction_den as u128 / a).checked_mul(step_den)
            .ok_or("source fraction denominator overflow")?;
        let numerator = (self.fraction_num as u128).checked_mul(step_den / a)
            .and_then(|n| step_num.checked_mul(self.fraction_den as u128 / a).and_then(|step| n.checked_add(step)))
            .ok_or("source clock offset overflow")?;
        let whole = u64::try_from(numerator / denominator).map_err(|_|"source clock offset overflow")?;
        let mut remainder = numerator % denominator;
        let (mut a,mut b) = (remainder,denominator);
        while b != 0 { (a,b) = (b,a%b); }
        remainder /= a;
        Ok(Self { index:self.index.checked_add(whole).ok_or("source sample index overflow")?,
            fraction_num:u64::try_from(remainder).map_err(|_|"source fraction overflow")?,
            fraction_den:u64::try_from(denominator/a).map_err(|_|"source fraction denominator overflow")? })
    }
}
#[derive(Debug)]
pub struct AudioBlock {
    pub samples: Vec<f32>,
    pub rate: u32,
    pub channels: u16,
    pub clock: InputClock,
    pub first_frame_index: u64,
    pub source_position: Option<SamplePosition>,
    /// DRM carries earliest contributing IQ provenance, not a fabricated PCM source clock.
    pub timing_kind: String,
    pub codec_delay_known: bool,
}
#[derive(Default)]
pub struct ReceiverOutput {
    pub audio: Vec<AudioBlock>,
    pub events: Vec<ReceiverEvent>,
    pub ft8_job: Option<Ft8Job>,
}

/// Owned search input: dispatch only to the fixed decoder worker. No search runs in process().
pub struct Ft8Job {
    epoch: u64,
    sequence: u64,
    iq: Vec<f32>,
    pub clock: InputClock,
    pub source_position: SamplePosition,
    window_start: u64,
}
pub struct Ft8Result {
    epoch: u64,
    sequence: u64,
    pub clock: InputClock,
    pub source_position: SamplePosition,
    window_start: u64,
    messages: Vec<websa_dsp::digital::ft8::Ft8Message>,
}
impl Ft8Job {
    pub fn decode(self) -> Ft8Result {
        let mut decoder = Ft8Decoder::new(FT8_RATE as f64);
        decoder.push_iq(&self.iq);
        Ft8Result { epoch: self.epoch, sequence: self.sequence, clock: self.clock,
            source_position: self.source_position, window_start: self.window_start, messages: decoder.decode() }
    }
}

struct Ft8Window {
    samples: VecDeque<f32>,
    origin: Option<InputClock>,
    start: u64,
    early: bool,
    attempts: u64,
    accepted: u64,
    decoded: u64,
    seen: VecDeque<(String, f64)>,
}
impl Ft8Window {
    fn new() -> Self {
        Self { samples: VecDeque::with_capacity(FT8_RATE as usize * 58), origin: None,
            start: 0, early: false, attempts: 0, accepted: 0, decoded: 0, seen: VecDeque::new() }
    }
    fn extract(&mut self, iq: &[f32], clock: InputClock, epoch: u64) -> Result<Option<Ft8Job>, String> {
        self.origin.get_or_insert(clock);
        self.samples.extend(iq.iter().copied());
        let window = (FT8_RATE as f64 * 28.04).round() as usize;
        let hop = FT8_RATE as usize * 15;
        let n = self.samples.len() / 2;
        let count = if !self.early && n >= hop { hop }
            else if n >= window { window } else { return Ok(None) };
        self.attempts += 1;
        let origin = self.origin.unwrap();
        let position = try_position_at(origin, self.start, FT8_RATE)?;
        let job = Ft8Job { epoch, sequence: self.attempts,
            iq: self.samples.iter().take(count * 2).copied().collect(),
            clock: InputClock { first_sample_index: position.index, ..origin },
            source_position: position, window_start: self.start };
        if !self.early { self.early = true; }
        else {
            self.samples.drain(..hop * 2);
            self.start += hop as u64;
        }
        Ok(Some(job))
    }
    fn flush(&mut self, epoch: u64) -> Result<Option<Ft8Job>, String> {
        if self.samples.len()/2 < (FT8_RATE as f64 * 12.64) as usize { return Ok(None); }
        let Some(origin) = self.origin else { return Ok(None) };
        self.attempts += 1;
        let source_position = try_position_at(origin, self.start, FT8_RATE)?;
        let job = Ft8Job { epoch, sequence: self.attempts,
            iq: self.samples.iter().copied().collect(),
            clock: InputClock { first_sample_index:source_position.index,..origin },
            source_position,window_start:self.start };
        self.start += (self.samples.len()/2) as u64;
        self.samples.clear();
        Ok(Some(job))
    }
}

pub struct Receiver {
    config: ReceiverConfig,
    epoch: u64,
    analog: Option<AnalogDemod>,
    digital: Option<Box<dyn DigitalDemodulator>>,
    adapter: Option<Rational>,
    converted: Vec<f32>,
    listener: AudioChain,
    side_listener: AudioChain,
    side_pcm: AnalogPcm,
    pcm: AnalogPcm,
    side: Vec<f32>,
    nr: NoiseReduction,
    nr_right: NoiseReduction,
    nr_scratch: Vec<f32>,
    nr_left_output: Vec<f32>,
    nr_right_output: Vec<f32>,
    cw: Option<CwDecoder>,
    ft8: Option<Ft8Window>,
    pushes: u64,
    elapsed_samples: u64,
    analog_frames: u64,
    audio_source_offset: u64,
    next_status: u64,
    next_wfm_info: u64,
    image_serial: Option<u64>,
    next_am_info: u64,
    image_lines: u32,
    radiofax_new_page: bool,
    audio_origin: Option<InputClock>,
    input_origin: Option<InputClock>,
    audio_frames: u64,
    audio_format: Option<(u32, u16)>,
    peak_hold: f32,
}
impl Receiver {
    pub fn new(config: ReceiverConfig) -> Result<Self, String> {
        config.validate()?;
        let mode = config.mode.as_str();
        let listen_mode = if matches!(mode, "ft8"|"sstv"|"radiofax") { "usb" } else { mode };
        let mut demod_config = DemodConfig::new(config.input_rate, config.if_bw);
        demod_config.pitch = config.pitch;
        let mut analog = analog::spec_for(listen_mode).map(|spec|
            AnalogDemod::new_prechannelized(spec, demod_config, config.audio_rate as f64));
        if let Some(demod) = analog.as_mut() {
            demod.set_agc_enabled(config.agc);
            demod.set_am_sync(config.am_sync);
            if config.deemph_us >= 0.0 { demod.set_deemph_us(config.deemph_us); }
        }
        let decoder_rate = match mode { "ft8" => FT8_RATE, "sstv" => 8_000, "radiofax" => 12_000,
            "drm" => 48_000, "drmplus" => 96_000, _ => 0 };
        let mut digital = if matches!(mode, "sstv"|"drm"|"drmplus"|"radiofax") {
            digital::build(mode, decoder_rate as f64)
        } else { None };
        if mode == "radiofax" {
            let fax = digital.as_mut().and_then(|demod| demod.as_radiofax()).ok_or("radiofax decoder unavailable")?;
            fax.apply_radiofax(&config.radiofax_view(), false)?;
        } else if let Some(demod) = digital.as_mut() {
            demod.set_mode_pin(config.sstv_mode);
        }
        let adapter = if decoder_rate != 0 && config.input_rate != decoder_rate as f64 {
            let passband = match mode {
                // Mode E's outer carrier is 47,111 Hz; keep it inside the flat passband.
                "drmplus" => 47_200.0,
                "drm" => config.if_bw / 2.0,
                "ft8" => 3_200.0,
                "sstv" | "radiofax" => 3_000.0,
                _ => unreachable!(),
            };
            Some(Rational::from_rates(config.input_rate, decoder_rate, passband)
                .map_err(|error| format!("decoder rate conversion: {error}"))?)
        } else { None };
        let policy = AudioPolicy { nr: false, nr_strength: config.nr_strength,
            squelch_dbm: config.squelch_dbm };
        let listener = audio::chain_for_mode(listen_mode, config.audio_rate as f64, policy);
        let side_listener = audio::chain_for_mode(listen_mode, config.audio_rate as f64, policy);
        let algorithm = config.nr_algorithm();
        let mut nr = NoiseReduction::new(config.audio_rate);
        let mut nr_right = NoiseReduction::new(config.audio_rate);
        nr.configure(config.audio_rate, algorithm, config.nr_strength as f32, config.nr_atten_db);
        let right_algorithm = if mode == "wfm" && config.stereo { algorithm } else { NrAlgorithm::Off };
        nr_right.configure(config.audio_rate, right_algorithm, config.nr_strength as f32, config.nr_atten_db);
        let cw = if mode == "cw" { Some(CwDecoder::new(config.audio_rate, config.pitch as f32)?) } else { None };
        let ft8 = if mode == "ft8" { Some(Ft8Window::new()) } else { None };
        let audio_format = analog.as_ref().map(|_|(config.audio_rate,
            if config.mode == "wfm" && config.stereo {2} else {1}));
        Ok(Self { config, epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed), analog, digital,
            adapter, converted: Vec::new(), listener, side_listener, side_pcm:AnalogPcm::default(), pcm: AnalogPcm::default(),
            side: Vec::new(), nr, nr_right, nr_scratch: Vec::new(), cw, ft8, pushes: 0,
            nr_left_output: Vec::new(), nr_right_output: Vec::new(),
            elapsed_samples: 0, next_status: 0, next_wfm_info: 0, next_am_info: 0, image_serial: None, image_lines: 0,
            radiofax_new_page: false,
            analog_frames: 0, audio_source_offset: 0,
            audio_origin: None, input_origin:None, audio_frames: 0, audio_format, peak_hold: 0.0 })
    }
    pub fn config(&self) -> &ReceiverConfig { &self.config }
    pub fn epoch(&self) -> u64 { self.epoch }
    /// Geometry changes replace only this channel's state. Listening changes retain decoder history.
    pub fn configure(&mut self, config: ReceiverConfig) -> Result<(), String> {
        config.validate()?;
        if config == self.config && !self.radiofax_new_page { return Ok(()); }
        let geometry_changed = config.mode != self.config.mode || config.input_rate != self.config.input_rate
            || config.audio_rate != self.config.audio_rate || config.if_bw != self.config.if_bw
            || config.pitch != self.config.pitch || config.sstv_mode != self.config.sstv_mode
            || config.stereo != self.config.stereo;
        let fax_changed = self.config.mode == "radiofax" && (self.config.radiofax_protocol_changed(&config)
            || self.config.radiofax_phase_pixels != config.radiofax_phase_pixels
            || self.config.radiofax_clock_ppm != config.radiofax_clock_ppm || self.radiofax_new_page);
        if geometry_changed { *self = Self::new(config)?; return Ok(()); }
        if fax_changed {
            let new_page = self.radiofax_new_page;
            let digital = self.digital.as_mut().and_then(|demod| demod.as_radiofax()).ok_or("radiofax decoder unavailable")?;
            digital.apply_radiofax(&config.radiofax_view(), new_page)?;
            self.radiofax_new_page = false;
        }
        if let Some(demod) = self.analog.as_mut() {
            demod.set_agc_enabled(config.agc);
            demod.set_am_sync(config.am_sync);
            let tau = if config.deemph_us < 0.0 { analog::spec_for(demod.id()).map(|s|s.deemph_us).unwrap_or(0.0) }
                else { config.deemph_us };
            demod.set_deemph_us(tau);
        }
        audio::apply_policy(&mut self.listener, AudioPolicy { nr: false,
            nr_strength: config.nr_strength, squelch_dbm: config.squelch_dbm });
        audio::apply_policy(&mut self.side_listener, AudioPolicy { nr: false,
            nr_strength: config.nr_strength, squelch_dbm: config.squelch_dbm });
        let algorithm = config.nr_algorithm();
        if self.analog.is_some() && algorithm != self.config.nr_algorithm() {
            // A reducer change drops its un-emitted tail, but retains decoder history.
            self.audio_source_offset = self.analog_frames.checked_sub(self.audio_frames)
                .ok_or("noise reducer emitted beyond its input clock")?;
        }
        self.nr.configure(config.audio_rate, algorithm, config.nr_strength as f32, config.nr_atten_db);
        let right_algorithm = if config.mode == "wfm" && config.stereo { algorithm } else { NrAlgorithm::Off };
        self.nr_right.configure(config.audio_rate, right_algorithm, config.nr_strength as f32, config.nr_atten_db);
        self.next_status = 0;
        self.config = config;
        Ok(())
    }
    pub fn request_radiofax_page(&mut self) {
        self.radiofax_new_page = true;
    }
    pub fn reset(&mut self) -> Result<(), String> {
        *self = Self::new(self.config.clone())?;
        Ok(())
    }
    pub fn finish_radiofax(&mut self) -> Result<(), String> {
        if self.config.mode != "radiofax" { return Ok(()); }
        let digital = self.digital.as_mut().and_then(|demod| demod.as_radiofax()).ok_or("radiofax decoder unavailable")?;
        digital.finish_input()
    }
    pub fn take_radiofax_events(&mut self) -> Result<Vec<ReceiverEvent>, String> {
        if self.config.mode != "radiofax" { return Ok(Vec::new()); }
        let fax = self.digital.as_mut().and_then(|demod| demod.as_radiofax()).ok_or("radiofax decoder unavailable")?;
        let mut events = Vec::new();
        radiofax_events(fax, self.epoch, true, &mut events)?;
        Ok(events)
    }
    pub fn retune(&mut self) -> Result<(), String> {
        if let Some(analog) = self.analog.as_mut() { analog.retune(); }
        if self.config.mode == "radiofax" {
            if let Some(fax) = self.digital.as_mut().and_then(|demod| demod.as_radiofax()) {
                fax.mark_discontinuity()?;
            }
        } else if let Some(digital) = self.digital.as_mut() { digital.retune(); }
        if let Some(ft8) = self.ft8.as_mut() { *ft8 = Ft8Window::new(); }
        Ok(())
    }
    /// Protocol codec audio bypasses the analog listener chain.
    pub fn squelch_status(&self) -> Option<(bool, f64)> {
        if matches!(self.config.mode.as_str(), "drm" | "drmplus") { None }
        else { self.listener.squelch_status() }
    }
    pub fn process(&mut self, iq: &[f32], clock: InputClock, power_dbm: Option<f64>) -> Result<ReceiverOutput, String> {
        if iq.len() % 2 != 0 || iq.len() / 2 > MAX_BLOCK_COMPLEX {
            return Err("receiver expects complete IQ pairs in blocks of at most 65536 complex samples".into());
        }
        if clock_ratio(clock.input_rate).is_none()
            || !clock.delay_input_samples.is_finite() || clock.delay_input_samples < 0.0 {
            return Err("invalid receiver source clock".into());
        }
        if self.input_origin.is_some_and(|origin| origin.input_rate != clock.input_rate) {
            return Err("receiver source clock rate changed without a reset".into());
        }
        if iq.iter().any(|sample| !sample.is_finite()) {
            return Err("receiver IQ samples must be finite".into());
        }
        let mut out = ReceiverOutput::default();
        if iq.is_empty() { return Ok(out); }
        // Validate the actual emitted frame count in audio_block, rather than estimating
        // output from input length (codec bursts and buffered NR have different cadence).
        self.input_origin.get_or_insert(clock);
        self.pushes += 1;
        self.elapsed_samples = self.elapsed_samples.saturating_add((iq.len() / 2) as u64);
        let status_due = self.elapsed_samples >= self.next_status;
        if status_due { self.next_status = self.elapsed_samples.saturating_add((self.config.input_rate / 10.0).max(1.0) as u64); }
        // Every digital consumer sees IQ before listener AGC, squelch, NR, gain or stereo matrix.
        let mut decoder_clock = clock;
        // Source positions use the adapter's actual causal input emission index;
        // FIR group delay remains separate latency metadata.
        let digital_iq = if let Some(adapter) = self.adapter.as_mut() {
            self.converted.resize(2 * adapter.output_frames_for_input(iq.len() / 2), 0.0);
            let frames = adapter.process(iq, &mut self.converted);
            self.converted.truncate(2 * frames);
            decoder_clock.delay_input_samples += adapter.group_delay_input_samples()
                * clock.input_rate / self.config.input_rate;
            &self.converted
        } else { iq };
        if digital_iq.iter().any(|sample| !sample.is_finite())
            || !decoder_clock.delay_input_samples.is_finite() {
            return Err("decoder rate conversion exceeded finite sample or clock bounds".into());
        }
        if let Some(ft8) = self.ft8.as_mut() {
            out.ft8_job = ft8.extract(digital_iq, decoder_clock, self.epoch)?;
            if let Some(job) = out.ft8_job.as_mut() {
                let origin = self.input_origin.unwrap();
                job.source_position = decoder_position(origin, job.window_start, FT8_RATE,
                    self.config.input_rate, self.adapter.as_ref())?;
                job.clock.first_sample_index = job.source_position.index;
            }
            if status_due {
                let level = rms(digital_iq);
                out.events.push(json!({"type":"ft8-state","bufferedSamples":ft8.samples.len()/2,
                    "bufferedSeconds":ft8.samples.len() as f64 / (2.0*FT8_RATE as f64),
                    "attempts":ft8.attempts,"count":ft8.decoded,"level":level,"epoch":self.epoch}));
            }
        }
        if let Some(digital) = self.digital.as_mut() {
            let lines = digital.process_iq(digital_iq);
            if self.config.mode == "radiofax" {
                let fax = digital.as_radiofax().ok_or("radiofax decoder unavailable")?;
                radiofax_events(fax, self.epoch, status_due, &mut out.events)?;
            } else if matches!(self.config.mode.as_str(), "drm"|"drmplus") {
                if !lines.is_empty() || status_due {
                    let status = digital.status().map(|s| json!({"locked":s.locked,"mode":s.mode,
                        "stationLabel":s.station_label,"carrierOffsetHz":s.carrier_offset_hz,
                        "facOk":s.fac_ok,"facErrors":s.fac_errors,"sdcOk":s.sdc_ok,
                        "services":s.services.into_iter().map(|s| json!({"serviceId":s.service_id,
                            "label":s.label,"audioCoding":s.audio_coding,"sampleRate":s.sample_rate,
                            "channels":s.channels})).collect::<Vec<_>>() }));
                    out.events.push(json!({"type":"drm","lines":lines,"snrDb":digital.last_report().map(|r|r.snr_db),"status":status}));
                    let points: Vec<_> = digital.constellation().into_iter().take(2048)
                        .map(|(re,im)|json!({"re":re,"im":im})).collect();
                    out.events.push(json!({"type":"drm-constellation","points":points}));
                }
                let chunks = digital.take_audio_chunks();
                for chunk in chunks {
                    let rate = chunk.sample_rate;
                    let channels = chunk.channels;
                    let source = chunk.source_position;
                    let total_delay = chunk.total_delay_samples;
                    let codec_delay = chunk.codec_delay_samples;
                    let pcm = chunk.pcm;
                    if pcm.is_empty() { continue; }
                    if rate == 0 || !(1..=2).contains(&channels) || pcm.len() % channels as usize != 0 {
                        return Err("codec produced PCM without a valid format".into());
                    }
                    let samples = pcm.into_iter().map(|s|s as f32 / 32768.0 * self.config.gain).collect();
                    let origin = self.input_origin.unwrap();
                    let decoder_rate = if self.config.mode == "drmplus" {96_000} else {48_000};
                    let (index,num,den) = source.ok_or("codec PCM lacks contributing IQ provenance")?;
                    if num != 0 || den != 1 { return Err("unsupported fractional protocol provenance".into()); }
                    let source_position = decoder_position(origin,index,decoder_rate,
                        self.config.input_rate,self.adapter.as_ref())?;
                    let mut audio_clock = decoder_clock;
                    let delay = total_delay.filter(|delay| delay.is_finite() && *delay >= 0.0)
                        .ok_or("codec PCM lacks finite delay provenance")?;
                    // Decoder delays count decoder IQ samples; the wire clock counts capture samples.
                    audio_clock.delay_input_samples += delay * clock.input_rate / decoder_rate as f64;
                    let mut block = self.audio_block(samples,rate,channels as u16,audio_clock,true)?;
                    block.source_position = Some(source_position);
                    block.timing_kind = "protocol_provenance".into();
                    block.codec_delay_known = codec_delay.is_some();
                    out.audio.push(block);
                }
            } else {
                if let Some(status) = digital.image_status() {
                    let new_picture = self.image_serial != Some(status.serial);
                    if new_picture { self.image_serial = Some(status.serial); self.image_lines = 0; }
                    if new_picture || status.lines > self.image_lines {
                        if let Some(image) = digital.image() {
                            let row = self.image_lines.min(status.lines);
                            let end = status.lines.min(image.height);
                            let index = status.mode_index as usize;
                            let name = digital::sstv::SSTV_MODES.get(index).map(|m|m.0).unwrap_or("unknown");
                            // Bound numeric RGB JSON even when a large input block completes many rows.
                            for chunk_row in (row..end.max(row+1)).step_by(64) {
                                let chunk_end = (chunk_row+64).min(end);
                                let begin_byte = chunk_row as usize * image.width as usize * 3;
                                let end_byte = chunk_end as usize * image.width as usize * 3;
                                if end_byte > image.rgb.len() { break; }
                                out.events.push(json!({"type":"sstv-image","width":image.width,"height":image.height,
                                    "row":chunk_row,"rows":chunk_end-chunk_row,"rgb":&image.rgb[begin_byte..end_byte],
                                    "meta":{"name":name,"modeIndex":index,"vis":status.vis,
                                        "lines":chunk_end,"progress":chunk_end*100/image.height.max(1),
                                        "complete":status.complete && chunk_end==image.height,
                                        "serial":status.serial.to_string(),"epoch":self.epoch,"hz":null}}));
                                self.image_lines = chunk_end;
                            }
                        }
                        out.events.push(json!({"type":"sstv-mode","index":status.mode_index}));
                    }
                }
                if status_due {
                    out.events.push(json!({"type":"sstv-state","running":true,"pushes":self.pushes,
                        "attempts":digital.attempts(),"bufferedSamples":digital.buffered_input(),
                        "bufferedSeconds":digital.buffered_input() as f64/48000.0,
                        "hasPicture":digital.image().is_some(),"level":digital.audio_level(),"srcLevel":rms(iq)}));
                }
            }
        }
        if let Some(analog) = self.analog.as_mut() {
            self.audio_origin.get_or_insert(clock);
            analog.process_into(iq, self.pcm.samples_mut());
            self.analog_frames = self.analog_frames.checked_add(self.pcm.samples().len() as u64)
                .ok_or("analog frame index overflow")?;
            self.side = analog.take_side();
            if self.pcm.samples().iter().chain(self.side.iter()).any(|sample| !sample.is_finite()) {
                return Err("analog demodulator produced nonfinite PCM".into());
            }
            if let Some(cw) = self.cw.as_mut() {
                let chunk = cw.push(self.pcm.samples());
                if let Some(error) = chunk.error.as_ref() {
                    out.events.push(json!({"type":"error","mode":"cw","message":error}));
                }
                if !chunk.text.is_empty() || chunk.ends_line || status_due {
                    out.events.push(json!({"type":"cw","text":chunk.text,"endsLine":chunk.ends_line,
                        "share":chunk.share,"rms":chunk.rms,"keyed":chunk.keyed,
                        "centerHz":chunk.center_hz,"pitchHz":chunk.pitch_hz,"wpm":chunk.wpm,
                        "afcHz":chunk.center_hz-self.config.pitch as f32}));
                }
            }
            self.listener.set_squelch_level_dbm(power_dbm.unwrap_or(f64::NAN));
            self.listener.process(&mut self.pcm, false);
            let stereo = self.config.mode == "wfm" && self.config.stereo;
            let mut samples = Vec::with_capacity(self.pcm.samples().len() * if stereo {2} else {1});
            if stereo {
                // Both paths run identical gate state and PCM cadence before the stereo matrix.
                self.side_pcm.samples_mut().clear();
                self.side_pcm.samples_mut().extend_from_slice(&self.side);
                self.side_listener.set_squelch_level_dbm(power_dbm.unwrap_or(f64::NAN));
                self.side_listener.process(&mut self.side_pcm, false);
                let blend = if analog.stereo_detected() {1.0} else {0.0};
                self.nr_scratch.clear();
                self.nr_scratch.extend(self.pcm.samples().iter().enumerate()
                    .map(|(i,m)|*m + self.side_pcm.samples().get(i).copied().unwrap_or(0.0)*blend));
                self.nr.process(&self.nr_scratch, &mut self.nr_left_output)?;
                self.nr_scratch.clear();
                self.nr_scratch.extend(self.pcm.samples().iter().enumerate()
                    .map(|(i,m)|*m - self.side_pcm.samples().get(i).copied().unwrap_or(0.0)*blend));
                self.nr_right.process(&self.nr_scratch, &mut self.nr_right_output)?;
                if self.nr_left_output.len() != self.nr_right_output.len()
                    || self.nr.latency_samples() != self.nr_right.latency_samples() {
                    return Err("stereo noise reducers lost sample alignment".into());
                }
                samples.reserve(self.nr_left_output.len()*2);
                for (&l,&r) in self.nr_left_output.iter().zip(&self.nr_right_output) {
                    samples.push(l*self.config.gain);
                    samples.push(r*self.config.gain);
                }
            } else {
                self.nr.process(self.pcm.samples(), &mut samples)?;
                for sample in &mut samples { *sample *= self.config.gain; }
            }
            if self.config.mode == "wfm" && !samples.is_empty() && self.elapsed_samples >= self.next_wfm_info {
                // Scope updates have their own 20 Hz source clock; status remains at 10 Hz.
                // Keep the deadline on that clock's grid so IQ block rounding cannot slow it.
                let period = (self.config.input_rate / 20.0).max(1.0) as u64;
                self.next_wfm_info = self.elapsed_samples.saturating_add(period - self.elapsed_samples % period);
                let peak = samples.iter().fold(0.0_f32, |peak,s|peak.max(s.abs()));
                self.peak_hold = self.peak_hold.max(peak);
                let rds = analog.rds_state();
                let (scope_l,scope_r) = scope(&samples, if stereo {2} else {1});
                out.events.push(json!({"type":"wfm-info","stereo":analog.stereo_detected(),
                    "peakDevHz":analog.peak_deviation_hz(),"pilotPct":analog.pilot_ratio()*100.0,
                    "peakDbFs":format!("{:.1}",dbfs(peak)),"peakHoldDb":format!("{:.1}",dbfs(self.peak_hold)),
                    "rdsLocked":rds.as_ref().is_some_and(|r|r.locked),"rdsPi":rds.as_ref().map(|r|r.pi),
                    "rdsGroups":rds.as_ref().map(|r|r.groups).unwrap_or(0),
                    "ps":rds.as_ref().map(|r|r.ps_str()).unwrap_or_default(),
                    "rt":rds.as_ref().map(|r|r.rt_str()).unwrap_or_default(),
                    "carrierHz":null,"pilotDevHz":analog.pilot_deviation_hz(),
                    "pilotSnrDb":analog.pilot_snr_db(),"scopeL":scope_l,"scopeR":scope_r}));
            }
            if self.config.mode == "am" && !samples.is_empty() && self.elapsed_samples >= self.next_am_info {
                let period = (self.config.input_rate / 20.0).max(1.0) as u64;
                self.next_am_info = self.elapsed_samples.saturating_add(period - self.elapsed_samples % period);
                if let Some(status) = analog.am_status() {
                    let peak = samples.iter().fold(0.0_f32, |peak,s|peak.max(s.abs()));
                    let (waveform, _) = scope(&samples, 1);
                    out.events.push(json!({"type":"am-info","syncEnabled":status.sync_enabled,
                        "amSyncGeneration":self.config.am_sync_generation,
                        "syncLocked":status.sync_locked,"carrierOffsetHz":status.carrier_offset_hz,
                        "modulationPct":status.modulation_pct,"inputDbFs":status.input_dbfs,
                        "rmsDbFs":dbfs(rms(&samples)),"peakDbFs":dbfs(peak),"scope":waveform}));
                }
            }
            if !samples.is_empty() {
                let mut audio_clock = clock;
                // Only the asymmetric modes retain the IF selection FIR.
                let if_delay = if matches!(self.config.mode.as_str(), "usb"|"lsb"|"cw"|"ft8"|"sstv"|"radiofax") {128.0} else {0.0};
                audio_clock.delay_input_samples += (if_delay + 64.0) * clock.input_rate / self.config.input_rate;
                if let Some(delay) = self.nr.latency_samples() {
                    audio_clock.delay_input_samples += delay as f64 * clock.input_rate / self.config.audio_rate as f64;
                }
                let block = self.audio_block(samples,self.config.audio_rate,if stereo {2}else{1},audio_clock,false)?;
                out.audio.push(block);
            }
        }
        if status_due {
            let status = self.nr.status();
            out.events.push(json!({"type":"nr-status","requested":algorithm_name(status.requested),
                "effective":algorithm_name(status.effective),"state":status.state,"reason":status.reason}));
        }
        Ok(out)
    }
    fn audio_block(&mut self, samples: Vec<f32>, rate: u32, channels: u16, clock: InputClock, codec: bool) -> Result<AudioBlock,String> {
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err("receiver produced nonfinite PCM".into());
        }
        if !clock.delay_input_samples.is_finite() {
            return Err("receiver delay exceeds source clock bounds".into());
        }
        if self.audio_format != Some((rate, channels)) {
            self.audio_format = Some((rate,channels)); self.audio_origin = Some(clock); self.audio_frames = 0;
        }
        let origin = self.audio_origin.unwrap_or(clock);
        let first_frame_index = self.audio_frames;
        let next_frame_index = self.audio_frames.checked_add((samples.len()/channels as usize) as u64)
            .ok_or("audio frame index overflow")?;
        let source_position = if codec { None } else {
            let first_source_frame = first_frame_index.checked_add(self.audio_source_offset)
                .ok_or("audio source frame index overflow")?;
            let next_source_frame = next_frame_index.checked_add(self.audio_source_offset)
                .ok_or("audio source frame index overflow")?;
            try_position_at(origin,next_source_frame,rate)?;
            Some(try_position_at(origin,first_source_frame,rate)?)
        };
        self.audio_frames = next_frame_index;
        Ok(AudioBlock { samples, rate, channels, clock, first_frame_index, source_position,
            timing_kind: if codec {"causal_presentation"}else{"sample"}.into(),
            codec_delay_known: !codec && self.nr.latency_samples().is_some()
                && self.analog.as_ref().is_none_or(|a|a.deemph_us() <= 0.0) })
    }
    pub fn accept_ft8(&mut self, result: Ft8Result) -> Vec<ReceiverEvent> {
        if result.epoch != self.epoch { return Vec::new(); }
        let Some(ft8) = self.ft8.as_mut() else { return Vec::new(); };
        if result.sequence <= ft8.accepted { return Vec::new(); }
        ft8.accepted = result.sequence;
        let mut events = Vec::new();
        for message in result.messages {
            let absolute = result.window_start as f64 / FT8_RATE as f64 + message.time_offset_s;
            if ft8.seen.iter().any(|(text,time)|*text == message.text && (*time-absolute).abs()<2.0) { continue; }
            ft8.seen.push_back((message.text.clone(),absolute));
            while ft8.seen.len()>128 { ft8.seen.pop_front(); }
            ft8.decoded += 1;
            events.push(json!({"type":"ft8","text":message.text,"frequencyHz":message.frequency_hz,
                "timeOffsetS":message.time_offset_s,"snrDb":message.snr_db,"count":ft8.decoded,
                "centerHz":null,"windowStart":result.clock.first_sample_index.to_string(),
                "windowFractionNum":result.source_position.fraction_num.to_string(),
                "windowFractionDen":result.source_position.fraction_den.to_string(),
                "windowRate":result.clock.input_rate,"epoch":self.epoch}));
        }
        events
    }
    /// Search the complete remaining transmission at EOF without adding synthetic samples.
    pub fn flush_ft8(&mut self) -> Result<Option<Ft8Job>, String> {
        let Some(ft8) = self.ft8.as_mut() else { return Ok(None) };
        let Some(mut job) = ft8.flush(self.epoch)? else { return Ok(None) };
        let origin = self.input_origin.ok_or("FT8 window lacks source clock")?;
        job.source_position = decoder_position(origin,job.window_start,FT8_RATE,
            self.config.input_rate,self.adapter.as_ref())?;
        job.clock.first_sample_index = job.source_position.index;
        Ok(Some(job))
    }
}

fn radiofax_events(fax: &mut dyn websa_dsp::plugin::RadiofaxControl, epoch: u64, status_due: bool,
    events: &mut Vec<ReceiverEvent>) -> Result<(), String> {
    let state = fax.radiofax_state();
    if !state.running {
        return Err(state.reason.unwrap_or_else(|| "radiofax decoder stopped".into()));
    }
    for span in fax.take_radiofax_spans() {
        if span.rows == 0 || span.rows > 64 || span.gray.len() != span.width as usize * span.rows as usize {
            return Err("radiofax image geometry is invalid".into());
        }
        events.push(json!({"type":"radiofax-image","width":span.width,"row":span.row,
            "rows":span.rows,"gray":base64_encode(&span.gray),"revision":span.revision,
            "meta":{"serial":span.meta.serial.to_string(),"epoch":epoch,"hz":null,
                "ioc":span.meta.ioc,"lpm":span.meta.lpm,"lines":span.meta.lines,
                "complete":span.meta.complete,"partial":span.meta.partial,
                "segmentIndex":span.meta.segment_index,"transmissionId":span.meta.transmission_id.to_string(),
                "missingRows":span.meta.missing_rows,"reason":span.meta.reason}}));
    }
    if status_due {
        events.push(json!({"type":"radiofax-state","running":state.running,"stage":state.stage,
            "hasPicture":state.has_picture,"signalLevel":state.signal_level,
            "carrierCoherence":state.carrier_coherence,"ioc":state.ioc,"lpm":state.lpm,
            "modulation":state.modulation,"polarity":state.polarity,"width":state.width,
            "lines":state.lines,"elapsedSeconds":state.elapsed_seconds,
            "clockStatus":state.clock_status,"clockSource":state.clock_source,
            "phasePixels":state.phase_pixels,"clockPpm":state.clock_ppm,
            "clockConfidence":state.clock_confidence,"complete":state.complete,
            "partial":state.partial,"reason":state.reason}));
    }
    Ok(())
}

fn algorithm_name(algorithm: NrAlgorithm) -> &'static str {
    match algorithm { NrAlgorithm::Off=>"off",NrAlgorithm::Wiener=>"wiener",NrAlgorithm::Dfn=>"dfn" }
}
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut value = [0_u8; 3];
        value[..chunk.len()].copy_from_slice(chunk);
        let packed = (u32::from(value[0]) << 16) | (u32::from(value[1]) << 8) | u32::from(value[2]);
        out.push(TABLE[((packed >> 18) & 63) as usize] as char);
        out.push(TABLE[((packed >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((packed >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(packed & 63) as usize] as char } else { '=' });
    }
    out
}
fn try_position_at(origin: InputClock, frames: u64, rate: u32) -> Result<SamplePosition,String> {
    let (source_num, source_den) = clock_ratio(origin.input_rate).ok_or("unsupported source clock rate")?;
    let numerator = frames as u128 * source_num as u128;
    let den = (rate as u64).checked_mul(source_den).filter(|den|*den != 0).ok_or("source clock denominator overflow")?;
    let whole = u64::try_from(numerator / den as u128).map_err(|_|"source clock offset overflow")?;
    let index = origin.first_sample_index.checked_add(whole).ok_or("source sample index overflow")?;
    Ok(SamplePosition { index, fraction_num:(numerator % den as u128) as u64, fraction_den:den })
}
fn decoder_position(origin: InputClock, frames: u64, decoder_rate: u32,
    input_rate: f64, adapter: Option<&Rational>) -> Result<SamplePosition,String> {
    let Some(adapter) = adapter else { return try_position_at(origin,frames,decoder_rate) };
    let input_index = adapter.causal_input_index(frames).map_err(str::to_owned)?;
    let (capture_num,capture_den) = clock_ratio(origin.input_rate).ok_or("unsupported capture rate")?;
    let (input_num,input_den) = clock_ratio(input_rate).ok_or("unsupported IQ rate")?;
    let numerator = (input_index as u128).checked_mul(capture_num as u128)
        .and_then(|n|n.checked_mul(input_den as u128)).ok_or("source clock offset overflow")?;
    let denominator = (capture_den as u128).checked_mul(input_num as u128)
        .ok_or("source clock denominator overflow")?;
    let whole = u64::try_from(numerator / denominator).map_err(|_|"source clock offset overflow")?;
    let mut remainder = numerator % denominator;
    let mut denominator = denominator;
    let (mut a,mut b) = (remainder,denominator);
    while b != 0 { let next = a % b; a = b; b = next; }
    remainder /= a;
    denominator /= a;
    Ok(SamplePosition {
        index: origin.first_sample_index.checked_add(whole).ok_or("source sample index overflow")?,
        fraction_num: u64::try_from(remainder).map_err(|_|"source fraction overflow")?,
        fraction_den: u64::try_from(denominator).map_err(|_|"source fraction denominator overflow")?,
    })
}
fn clock_ratio(rate: f64) -> Option<(u64,u64)> {
    if !rate.is_finite() || !(1.0..=1e9).contains(&rate) { return None; }
    let bits = rate.to_bits();
    let mut mantissa = (bits & ((1_u64<<52)-1)) | (1_u64<<52);
    let mut shift = ((bits>>52)&0x7ff) as i32 - 1023 - 52;
    while mantissa & 1 == 0 { mantissa >>= 1; shift += 1; }
    if shift >= 0 { Some((mantissa.checked_shl(shift as u32)?,1)) }
    else if shift >= -52 { Some((mantissa,1_u64<<(-shift as u32))) }
    else { None }
}
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {0.0} else {(samples.iter().map(|s|(*s as f64).powi(2)).sum::<f64>()/samples.len() as f64).sqrt() as f32}
}
fn dbfs(value: f32) -> f32 { 20.0*value.max(1e-8).log10() }
fn scope(samples: &[f32], channels: usize) -> (Vec<f32>,Vec<f32>) {
    let frames = samples.len()/channels;
    let stride = (frames/256).max(1);
    let mut left = Vec::with_capacity(256);
    let mut right = Vec::with_capacity(256);
    for frame in (0..frames).step_by(stride).take(256) {
        left.push(samples[frame*channels]); right.push(samples[frame*channels+channels-1]);
    }
    (left,right)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [&str; 13] = ["am","dsb","usb","lsb","cw","nfm","wfm","pm","ft8","sstv","drm","drmplus","radiofax"];

    fn mode_config(mode: &str) -> ReceiverConfig {
        ReceiverConfig { mode:mode.into(),
            input_rate:if mode == "wfm" {240_000.0} else if mode == "drmplus" {96_000.0} else if mode == "radiofax" {12_000.0} else {48_000.0},
            if_bw:if mode == "wfm" {180_000.0} else if mode == "drmplus" {90_000.0} else if mode == "radiofax" {3_000.0} else {6_000.0},
            ..ReceiverConfig::default() }
    }

    #[test]
    fn invalid_geometry_is_rejected_before_constructing_any_mode() {
        for mode in MODES {
            for rate in [0,1,7_999,192_001,u32::MAX] {
                assert!(Receiver::new(ReceiverConfig {audio_rate:rate,..mode_config(mode)}).is_err(),"{mode}: {rate}");
            }
            for rate in [0.0,f64::MIN_POSITIVE,f64::NAN,f64::INFINITY,f64::MAX] {
                assert!(Receiver::new(ReceiverConfig {input_rate:rate,..mode_config(mode)}).is_err(),"{mode}: {rate}");
            }
        }
        for mode in ["am","dsb","usb","lsb","cw","nfm","wfm","pm","ft8","sstv","radiofax"] {
            let config = mode_config(mode);
            assert!(Receiver::new(ReceiverConfig {if_bw:f64::MAX,..config.clone()}).is_err(),"{mode}");
            if matches!(mode,"usb"|"lsb"|"ft8"|"sstv"|"radiofax") {
                assert!(Receiver::new(ReceiverConfig {if_bw:config.input_rate/2.0,..config}).is_err(),"{mode}");
            }
        }
    }

    #[test]
    fn invalid_iq_does_not_poison_any_receiver_mode() {
        for mode in MODES {
            let config = mode_config(mode);
            let clock = InputClock {first_sample_index:0,input_rate:config.input_rate,delay_input_samples:0.0};
            let mut receiver = Receiver::new(config).unwrap();
            let epoch = receiver.epoch();
            for iq in [&[0.0][..], &[f32::NAN,0.0], &[0.0,f32::INFINITY]] {
                assert!(receiver.process(iq,clock,None).is_err(),"{mode}");
            }
            assert!(receiver.process(&vec![0.0;2*(MAX_BLOCK_COMPLEX+1)],clock,None).is_err());
            assert!(receiver.process(&[],clock,None).unwrap().audio.is_empty());
            assert_eq!(receiver.epoch(),epoch);
            let output = receiver.process(&vec![0.0;2048],clock,None).unwrap();
            for block in output.audio {
                assert!(block.samples.iter().all(|sample| sample.is_finite() && *sample == 0.0),"{mode}");
                assert_eq!(block.first_frame_index,0);
                assert_eq!(block.source_position.unwrap().index,0);
            }
            assert!(receiver.process(&[0.0,0.0],InputClock {input_rate:clock.input_rate+1.0,..clock},None).is_err());
            assert_eq!(receiver.input_origin.unwrap().first_sample_index,0);
            assert_eq!(receiver.elapsed_samples,1024);
        }
    }

    #[test]
    fn codec_modes_keep_nr_preferences_without_running_a_reducer() {
        for mode in ["drm","drmplus"] {
            let mut config = mode_config(mode);
            config.nr = true;
            config.nr_algorithm = "dfn".into();
            let mut receiver = Receiver::new(config.clone()).unwrap();
            assert!(receiver.config().nr);
            assert_eq!(receiver.nr.status().effective,NrAlgorithm::Off);
            config.nr_atten_db = 20.0;
            receiver.configure(config).unwrap();
            assert_eq!(receiver.nr.status().effective,NrAlgorithm::Off);
            let mut listen = mode_config("usb");
            listen.nr = true;
            receiver.configure(listen).unwrap();
            assert_eq!(receiver.nr.status().effective,NrAlgorithm::Wiener);
        }
    }

    #[test]
    fn stereo_format_changes_reset_audio_clock_and_decoder_epoch() {
        let mut config = mode_config("wfm");
        let mut receiver = Receiver::new(config.clone()).unwrap();
        let clock = InputClock {first_sample_index:123,input_rate:config.input_rate,delay_input_samples:0.0};
        let iq = vec![0.0;4096];
        let stereo = receiver.process(&iq,clock,None).unwrap().audio.remove(0);
        assert_eq!(stereo.channels,2);
        let epoch = receiver.epoch();
        config.stereo = false;
        receiver.configure(config).unwrap();
        assert_ne!(receiver.epoch(),epoch);
        let clock = InputClock {first_sample_index:10_000,..clock};
        let mono = receiver.process(&iq,clock,None).unwrap().audio.remove(0);
        assert_eq!(mono.channels,1);
        assert_eq!(mono.samples.len()*2,stereo.samples.len());
        assert_eq!(mono.first_frame_index,0);
        assert_eq!(mono.source_position.unwrap().index,10_000);
    }

    #[test]
    fn ft8_window_clock_overflow_returns_errors() {
        let clock = InputClock {first_sample_index:u64::MAX,input_rate:12_000.0,delay_input_samples:0.0};
        let mut window = Ft8Window::new();
        window.start = 1;
        assert!(window.extract(&vec![0.0;12_000*15*2],clock,1).is_err());
        assert!(window.flush(1).is_err());
        let mut receiver = Receiver::new(mode_config("ft8")).unwrap();
        receiver.input_origin = Some(clock);
        receiver.ft8 = Some(window);
        assert!(receiver.flush_ft8().is_err());
    }

    #[test]
    fn invalid_listening_update_preserves_the_current_stream() {
        let config = mode_config("am");
        let mut receiver = Receiver::new(config.clone()).unwrap();
        let epoch = receiver.epoch();
        let invalid = ReceiverConfig {audio_rate:1,..config.clone()};
        assert!(receiver.configure(invalid).is_err());
        assert_eq!(receiver.config(),&config);
        assert_eq!(receiver.epoch(),epoch);
    }

    #[test]
    fn reducer_changes_skip_discarded_tail_without_resetting_ft8_history() {
        let mut config = mode_config("ft8");
        config.nr = true;
        let mut receiver = Receiver::new(config.clone()).unwrap();
        let epoch = receiver.epoch();
        let clock = InputClock {first_sample_index:100,input_rate:48_000.0,delay_input_samples:0.0};
        let first = receiver.process(&vec![0.0;600*2],clock,None).unwrap().audio.remove(0);
        assert_eq!(first.samples.len(),128);
        let buffered = receiver.ft8.as_ref().unwrap().samples.len();
        config.nr = false;
        receiver.configure(config.clone()).unwrap();
        assert_eq!(receiver.epoch(),epoch);
        assert_eq!(receiver.ft8.as_ref().unwrap().samples.len(),buffered);
        let second = receiver.process(&vec![0.0;600*2],InputClock {first_sample_index:700,..clock},None)
            .unwrap().audio.remove(0);
        assert_eq!(second.first_frame_index,128);
        assert_eq!(second.source_position.unwrap().index,700);
        config.nr = true;
        receiver.configure(config.clone()).unwrap();
        let third = receiver.process(&vec![0.0;600*2],InputClock {first_sample_index:1300,..clock},None)
            .unwrap().audio.remove(0);
        assert_eq!(third.first_frame_index,728);
        assert_eq!(third.source_position.unwrap().index,1300);
        config.nr = false;
        receiver.configure(config).unwrap();
        let fourth = receiver.process(&vec![0.0;600*2],InputClock {first_sample_index:1900,..clock},None)
            .unwrap().audio.remove(0);
        assert_eq!(fourth.first_frame_index,856);
        assert_eq!(fourth.source_position.unwrap().index,1900);
        assert_eq!(receiver.epoch(),epoch);
    }

    #[test]
    fn wfm_scope_keeps_twenty_hz_phase_without_raising_status_rate() {
        let config = ReceiverConfig {mode:"wfm".into(),input_rate:240_000.0,if_bw:180_000.0,
            ..ReceiverConfig::default()};
        let mut receiver = Receiver::new(config).unwrap();
        // A ~34 ms IQ packet does not divide either display or status periods.
        let iq = vec![0.0;8192*2];
        let mut scopes = 0;
        let mut statuses = 0;
        for packet in 0..60 {
            let clock = InputClock {first_sample_index:packet*8192,input_rate:240_000.0,
                delay_input_samples:0.0};
            let output = receiver.process(&iq,clock,None).unwrap();
            let mut packet_scopes = 0;
            for event in output.events {
                match event["type"].as_str() {
                    Some("wfm-info") => {
                        packet_scopes += 1;
                        for lane in ["scopeL","scopeR"] {
                            let samples = event[lane].as_array().unwrap();
                            assert!((2..=256).contains(&samples.len()));
                        }
                    }
                    Some("nr-status") => statuses += 1,
                    _ => {}
                }
            }
            assert!(packet_scopes <= 1);
            scopes += packet_scopes;
        }
        // 2.048 source seconds: an immediate snapshot, then each crossed 50 ms boundary.
        assert_eq!(scopes,41);
        assert_eq!(statuses,20);
    }

    #[test]
    fn fractional_capture_clock_preserves_large_anchor_and_pcm_phase() {
        let anchor = (1_u64<<53)+137;
        let clock = InputClock {first_sample_index:anchor,input_rate:976_562.5,delay_input_samples:32.0};
        let frames = 48_000_u64*601+17;
        let position = try_position_at(clock,frames,48_000).unwrap();
        let numerator = frames as u128*1_953_125;
        assert_eq!(position.index,anchor+(numerator/96_000) as u64);
        assert_eq!(position.fraction_num,(numerator%96_000) as u64);
        assert_eq!(position.fraction_den,96_000);
        assert_ne!(position.fraction_num,0);
    }

    #[test]
    fn source_index_overflow_is_an_error() {
        let clock = InputClock {first_sample_index:u64::MAX-2,input_rate:976_562.5,delay_input_samples:0.0};
        assert!(try_position_at(clock,48_000,48_000).is_err());
    }

    #[test]
    fn sample_position_advance_matches_fractional_capture_clock() {
        let clock = InputClock {first_sample_index:(1_u64<<53)+137,input_rate:976_562.5,delay_input_samples:0.0};
        let start = try_position_at(clock,17,48_000).unwrap();
        let advanced = start.checked_advance(48_000*601+23,clock.input_rate,48_000).unwrap();
        let expected = try_position_at(clock,48_000*601+40,48_000).unwrap();
        assert_eq!(advanced.index,expected.index);
        assert_eq!(advanced.fraction_num as u128*expected.fraction_den as u128,
            expected.fraction_num as u128*advanced.fraction_den as u128);
        let end = SamplePosition {index:u64::MAX,fraction_num:1,fraction_den:2};
        assert!(end.checked_advance(1,48_000.0,48_000).is_err());
        assert!(start.checked_advance(1,clock.input_rate,0).is_err());
    }

    #[test]
    fn planned_fractional_iq_rates_preserve_decoder_source_positions() {
        let capture_rate = 3_906_250.0;
        let origin = InputClock {first_sample_index:(1_u64<<53)+137,
            input_rate:capture_rate,delay_input_samples:32.0};
        for (mode, bandwidth, decoder_rate, passband) in [
            ("ft8",6_400.0,12_000,3_200.0),
            ("drm",12_000.0,48_000,6_000.0),
            ("drmplus",100_000.0,96_000,47_200.0),
        ] {
            let plan = crate::rate_plan::RatePlan::new(976_562.5,mode,bandwidth).unwrap();
            let rate = plan.output_rate;
            let adapter = Rational::from_rates(rate,decoder_rate,passband).unwrap();
            let (num,den) = clock_ratio(rate).unwrap();
            assert_eq!(num as f64 / den as f64,rate);
            let frame = decoder_rate as u64 * 601 + 17;
            let index = adapter.causal_input_index(frame).unwrap();
            let position = decoder_position(origin,frame,decoder_rate,rate,Some(&adapter)).unwrap();
            let ticks = index as u128 * capture_rate as u128 * den as u128;
            assert_eq!(position.index,origin.first_sample_index+(ticks/num as u128) as u64);
            assert_eq!(position.fraction_num as u128*num as u128,
                ticks%num as u128*position.fraction_den as u128);
        }
    }

    #[test]
    fn nr_buffering_keeps_first_input_origin_and_reset_discards_it() {
        let config = ReceiverConfig {mode:"usb".into(),input_rate:48_000.0,nr:true,
            nr_algorithm:"wiener".into(),..ReceiverConfig::default()};
        let mut receiver = Receiver::new(config).unwrap();
        let anchor = (1_u64<<53)+19;
        let clock = InputClock {first_sample_index:anchor,input_rate:976_562.5,delay_input_samples:8.0};
        assert!(receiver.process(&vec![0.0;128*2],clock,None).unwrap().audio.is_empty());
        let later = InputClock {first_sample_index:anchor+10_000,..clock};
        let block = receiver.process(&vec![0.0;512*2],later,None).unwrap().audio.remove(0);
        assert_eq!(block.first_frame_index,0);
        assert_eq!(block.source_position.unwrap().index,anchor);
        receiver.reset().unwrap();
        let block = receiver.process(&vec![0.0;640*2],later,None).unwrap().audio.remove(0);
        assert_eq!(block.first_frame_index,0);
        assert_eq!(block.source_position.unwrap().index,later.first_sample_index);
    }

    #[test]
    fn am_info_uses_source_cadence_rf_units_and_final_pcm() {
        let mut receiver = Receiver::new(ReceiverConfig { agc:false, gain:2.0,
            input_rate:48_000.0, if_bw:12_000.0, ..ReceiverConfig::default() }).unwrap();
        let epoch = receiver.epoch();
        let mut scopes = 0;
        let mut statuses = 0;
        let mut next_frame = 0;
        let anchor = (1_u64<<53)+17;
        for packet in 0..60_u64 {
            if packet == 12 || packet == 45 {
                let mut config = receiver.config().clone();
                config.am_sync = packet == 12;
                receiver.configure(config).unwrap();
                assert_eq!(receiver.epoch(),epoch);
            }
            let iq: Vec<f32> = (0..1536).flat_map(|sample| {
                let t = (packet*1536+sample) as f64/48_000.0;
                let envelope = 0.2*(1.0+0.5*(std::f64::consts::TAU*1000.0*t).cos());
                let phase = 0.7+std::f64::consts::TAU*173.0*t;
                [(envelope*phase.cos()) as f32,(envelope*phase.sin()) as f32]
            }).collect();
            let clock = InputClock { first_sample_index:anchor+packet*1536,
                input_rate:48_000.0, delay_input_samples:0.0 };
            let output = receiver.process(&iq,clock,Some(-40.0)).unwrap();
            for block in &output.audio {
                assert_eq!(block.first_frame_index,next_frame);
                assert_eq!(block.source_position.unwrap().index,anchor+next_frame);
                next_frame += block.samples.len() as u64;
            }
            for event in output.events {
                if event["type"] == "nr-status" { statuses += 1; }
                if event["type"] != "am-info" { continue; }
                scopes += 1;
                assert_eq!(event["syncEnabled"],(12..45).contains(&packet));
                let block = &output.audio[0];
                let peak = block.samples.iter().fold(0.0_f32,|p,s|p.max(s.abs()));
                assert!((event["rmsDbFs"].as_f64().unwrap()-dbfs(rms(&block.samples)) as f64).abs()<1e-5);
                assert!((event["peakDbFs"].as_f64().unwrap()-dbfs(peak) as f64).abs()<1e-5);
                let expected_scope = scope(&block.samples,1).0;
                assert_eq!(event["scope"],json!(expected_scope));
                if packet >= 25 {
                    assert!((event["inputDbFs"].as_f64().unwrap()-10.0*(0.04_f64*1.125).log10()).abs()<0.1);
                    assert!((event["modulationPct"].as_f64().unwrap()-50.0).abs()<1.0);
                    let offset = event["carrierOffsetHz"].as_f64().unwrap();
                    assert!(offset.is_finite() && offset.abs() < 2_400.0);
                    assert_eq!(event["syncLocked"],(12..45).contains(&packet));
                }
                // Carrier lock precedes the integral loop's frequency settling.
                // Check steady-state RF accuracy after 1.6 s of source samples.
                if packet >= 50 {
                    assert!((event["carrierOffsetHz"].as_f64().unwrap()-173.0).abs()<1.0);
                }
            }
        }
        assert_eq!(scopes,39); // initial event and each crossed 50 ms grid boundary in 1.92 s
        // Live listening updates deliberately request an immediate status, but never a new epoch.
        assert_eq!(statuses,16);
    }
    #[test]
    fn radiofax_phase_keeps_epoch_and_protocol_change_marks_partial() {
        let mut config = mode_config("radiofax");
        let mut receiver = Receiver::new(config.clone()).unwrap();
        let epoch = receiver.epoch();
        let clock = InputClock { first_sample_index: 10, input_rate: 12_000.0, delay_input_samples: 0.0 };
        assert!(receiver.process(&[f32::NAN, 0.0], clock, None).is_err());
        assert_eq!(receiver.epoch(), epoch);
        let quiet = receiver.process(&vec![0.0; 2_400], clock, None).unwrap();
        assert!(quiet.audio.iter().any(|block| block.channels == 1 && !block.samples.is_empty()));
        assert!(quiet.events.iter().all(|event| event["type"] != "sstv-image" && event["type"] != "sstv-state"));
        config.radiofax_phase_pixels = 4.0;
        receiver.configure(config.clone()).unwrap();
        assert_eq!(receiver.epoch(), epoch);
        config.radiofax_acquisition = "manual".into();
        config.radiofax_ioc = "576".into();
        config.radiofax_lpm = "180".into();
        receiver.configure(config).unwrap();
        assert_eq!(receiver.epoch(), epoch, "radiofax protocol controls must not rebuild RF");
        let state = receiver.process(&vec![0.0; 1_200], InputClock { first_sample_index: 1_210, ..clock }, None).unwrap();
        let fax = state.events.iter().find(|event| event["type"] == "radiofax-state").unwrap();
        assert!(matches!(fax["stage"].as_str(), Some("searching") | Some("partial") | Some("fallback")));
        assert_ne!(fax["clockSource"], "phasing");
    }
    #[test]
    fn synthesized_radiofax_iq_emits_base64_gray_without_sstv_events() {
        let mut pcm = Vec::with_capacity(24_000);
        for index in 0..24_000 {
            let t = index as f64 / 12_000.0;
            let gray = if (index / 1_810) % 2 == 0 { 0.15 } else { 0.85 };
            let frequency = 1_900.0 + (gray - 0.5) * 800.0;
            pcm.push((0.45 * (std::f64::consts::TAU * frequency * t).sin()) as f32);
        }
        let iq: Vec<f32> = pcm.iter().flat_map(|sample| [*sample, 0.0]).collect();
        let mut receiver = Receiver::new(mode_config("radiofax")).unwrap();
        let clock = InputClock { first_sample_index: 0, input_rate: 12_000.0, delay_input_samples: 0.0 };
        let mut images = Vec::new();
        for (index, block) in iq.chunks(3_000).enumerate() {
            let output = receiver.process(block, InputClock {
                first_sample_index: index as u64 * 1_500, ..clock }, None).unwrap();
            assert!(output.events.iter().all(|event| !event["type"].as_str().unwrap_or("").starts_with("sstv")));
            images.extend(output.events.into_iter().filter(|event| event["type"] == "radiofax-image"));
        }
        let image = images.first().expect("synthetic fax IQ should emit gray");
        assert!(image.get("height").is_none());
        assert!(image["gray"].as_str().unwrap().chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=')));
        assert_eq!(image["meta"]["hz"], serde_json::Value::Null);
        assert_eq!(image["meta"]["ioc"], 576);
        assert!(image["rows"].as_u64().unwrap() <= 64);
        let serial = images.last().unwrap()["meta"]["serial"].clone();
        let epoch = receiver.epoch();
        receiver.request_radiofax_page();
        receiver.configure(receiver.config().clone()).unwrap();
        let mut next_images = Vec::new();
        for (index, block) in iq.chunks(3_000).enumerate() {
            let output = receiver.process(block, InputClock {
                first_sample_index: 24_000 + index as u64 * 1_500, ..clock }, None).unwrap();
            next_images.extend(output.events.into_iter().filter(|event| event["type"] == "radiofax-image"));
        }
        assert_eq!(receiver.epoch(), epoch);
        assert!(next_images.iter().any(|event| event["meta"]["serial"] != serial && event["row"] == 0),
            "manual new page must start a distinct image without a parameter change");
        let revision=next_images.last().unwrap()["revision"].as_u64().unwrap();
        receiver.finish_radiofax().unwrap();
        let closed=receiver.take_radiofax_events().unwrap();
        let image=closed.iter().find(|event|event["type"]=="radiofax-image").expect("EOF must republish page closure");
        assert_eq!(image["row"],0);
        assert!(image["revision"].as_u64().unwrap()>revision);
        assert_eq!(image["meta"]["partial"],true);
        let state=closed.iter().find(|event|event["type"]=="radiofax-state").unwrap();
        assert_eq!(state["partial"],true);
        assert_eq!(state["reason"],"eof");
        receiver.finish_radiofax().unwrap();
        assert!(receiver.take_radiofax_events().unwrap().iter().all(|event|event["type"]!="radiofax-image"));
    }
}
