//! The pipeline: one channelized baseband, two consumers, and one rule that cannot be broken.
//!
//! ```text
//!   baseband ──┬──▶ RAW baseband  (the digital decoder and the measurements read this)
//!   (DDC out)  ├──▶ AnalogDemod ──▶ PCM ──▶ AudioChain ──▶ speaker
//!              └──▶ DigitalDemod ─────────────────────────▶ decoded messages
//! ```
//!
//! The baseband arrives already channelized: the coarse decimation and the tuning happen on the
//! backend (the analyzer's DSP_DDC + a software NCO), exactly as the Python reference does it. What
//! runs here is the demodulator and the audio chain — the CPU-cheap half — which is why the browser
//! can keep up with it while it could not keep up with a 129-tap FIR at the raw IQ rate.
//!
//! The rule: **the audio-enhancement chain only ever receives [`AnalogPcm`]** — a newtype that the
//! RAW/digital paths cannot produce. A voice denoiser on an FT8 tone destroys the information the
//! decoder needs, so the separation is a type, not a comment, and
//! `wasm/tests/path_separation.rs` proves it by toggling the chain and comparing the RAW output.
//!
//! The audio chain is not a DDC stage: it exists for the human ear, so it is not part of the
//! channelizer.

use crate::audio::AudioPolicy;
use crate::ddc::resampler::ComplexResampler;
use crate::plugin::{AnalogDemodulator, AudioStage, DigitalDemodulator, DigitalReport, PluginKind, AUDIO_PLUGINS};

/// Real PCM destined for the speaker. Only the analog path produces one of these, and only the
/// audio chain accepts one.
#[derive(Debug, Default)]
pub struct AnalogPcm {
    samples: Vec<f32>,
}

impl AnalogPcm {
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Mutable access for the stages that own the buffer (and for tests that build a block).
    /// The type is still the only way into `AudioChain`, so the RAW/digital paths cannot obtain
    /// one — which is the guarantee this newtype exists for.
    pub fn samples_mut(&mut self) -> &mut Vec<f32> {
        &mut self.samples
    }

    pub fn into_inner(self) -> Vec<f32> {
        self.samples
    }
}

/// Which consumer this pipeline drives. The RAW baseband is emitted either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Demodulate to audio and (optionally) run the enhancement chain.
    Analog,
    /// Feed a protocol decoder; audio enhancement is unreachable from here.
    Digital,
}

/// One stage of the chain plus the per-stage switch a mode can flip.
struct StageEntry {
    stage: Box<dyn AudioStage>,
    enabled: bool,
}

/// The audio-enhancement chain. Stages run in registration order.
pub struct AudioChain {
    stages: Vec<StageEntry>,
    scratch: Vec<f32>,
    enabled: bool,
}

impl AudioChain {
    pub fn new(stages: Vec<Box<dyn AudioStage>>) -> Self {
        Self {
            stages: stages.into_iter().map(|stage| StageEntry { stage, enabled: true }).collect(),
            scratch: Vec::new(),
            enabled: true,
        }
    }

    /// Build the chain for the plugins that are implemented, in `AUDIO_PLUGINS` order, at the
    /// consumer's rate, with every stage **disabled**: the policy decides what runs.
    ///
    /// Stages whose kernels are not written yet are skipped rather than faked, so the chain can
    /// never "pass audio through" while pretending to process it. Installing a stage off is what
    /// keeps the default honest — the notch used to run because nobody said otherwise, and it
    /// removed the signal itself whenever the signal was a tone (measured: -33 dB on an AM test
    /// tone, which also made every A/B against the Python reference meaningless).
    pub fn from_registry(
        factories: &[(&'static str, fn(f64) -> Box<dyn AudioStage>)],
        rate: f64,
    ) -> Self {
        let mut stages: Vec<Box<dyn AudioStage>> = Vec::new();
        for descriptor in AUDIO_PLUGINS.iter().filter(|p| p.implemented) {
            if let Some((_, build)) = factories.iter().find(|(id, _)| *id == descriptor.id) {
                stages.push(build(rate));
            }
        }
        let mut chain = Self::new(stages);
        for entry in chain.stages.iter_mut() {
            entry.enabled = false;
        }
        chain
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.stages.iter().map(|entry| entry.stage.id()).collect()
    }

    /// Stage ids that would actually run right now (the ones the tests and the status readout care
    /// about: a disabled stage must not be reported as processing).
    pub fn active_ids(&self) -> Vec<&'static str> {
        self.stages
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.stage.id())
            .collect()
    }

    /// Enable/disable one stage by id. A mode that must not be enhanced (CW: the notch would
    /// remove the very carrier the operator is listening to) turns the stage off here instead of
    /// keeping a second chain.
    pub fn set_stage_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.stages.iter_mut().find(|entry| entry.stage.id() == id) {
            Some(entry) => {
                entry.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub fn is_stage_enabled(&self, id: &str) -> bool {
        self.stages.iter().any(|entry| entry.stage.id() == id && entry.enabled)
    }

    /// Hand a strength to every stage that has one; `false` when none does.
    pub fn set_strength(&mut self, strength: f64) -> bool {
        let mut taken = false;
        for entry in self.stages.iter_mut() {
            taken |= entry.stage.set_strength(strength);
        }
        taken
    }

    /// Hand a dBFS threshold to every stage that has one; `false` when none does.
    pub fn set_threshold(&mut self, dbfs: f64) -> bool {
        let mut taken = false;
        for entry in self.stages.iter_mut() {
            taken |= entry.stage.set_threshold(dbfs);
        }
        taken
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn reset(&mut self) {
        for entry in self.stages.iter_mut() {
            entry.stage.reset();
        }
    }

    /// Run the chain over one PCM block. With the chain disabled (or empty) the block is
    /// untouched, which is exactly the state the RAW-path test compares against.
    pub fn process(&mut self, pcm: &mut AnalogPcm, hold: bool) {
        if !self.enabled || self.stages.is_empty() {
            return;
        }
        for index in 0..self.stages.len() {
            if !self.stages[index].enabled {
                continue;
            }
            let mut out = core::mem::take(&mut self.scratch);
            self.stages[index].stage.process_into(&pcm.samples, hold, &mut out);
            pcm.samples.clear();
            pcm.samples.extend_from_slice(&out);
            self.scratch = out;
        }
    }
}

/// One processed baseband block: everything the consumers need, produced in one pass.
#[derive(Debug, Default)]
pub struct PipelineOutput {
    /// The channelized baseband as it arrived — the RAW stream. Never modified by the audio chain.
    pub raw: Vec<f32>,
    /// Analog audio (after the enhancement chain when it is enabled). Empty in the digital path.
    pub audio: Vec<f32>,
    /// Messages a digital demodulator decoded from this block.
    pub decoded: Vec<String>,
}

/// The assembled receiver.
pub struct Pipeline {
    kind: PathKind,
    analog: Option<Box<dyn AnalogDemodulator>>,
    digital: Option<Box<dyn DigitalDemodulator>>,
    /// The decoder's rate adapter: the baseband arrives at the backend's DDC rate and FT8 needs
    /// 48 kHz. `None` when the rates already match (the common case).
    resample: Option<ComplexResampler>,
    chain: AudioChain,
    /// What the listener asked the audio chain to do (the panel's NR/squelch controls).
    policy: AudioPolicy,
    /// Rate-converted baseband for the digital path.
    converted: Vec<f32>,
    pcm: AnalogPcm,
    decoded_total: u64,
}

impl Pipeline {
    pub fn new(kind: PathKind, mut chain: AudioChain, policy: AudioPolicy) -> Self {
        crate::audio::apply_policy(&mut chain, policy);
        Self {
            kind,
            analog: None,
            digital: None,
            resample: None,
            chain,
            policy,
            converted: Vec::new(),
            pcm: AnalogPcm::default(),
            decoded_total: 0,
        }
    }

    /// The audio-chain policy in force (the panel's NR/squelch settings).
    pub fn audio_policy(&self) -> AudioPolicy {
        self.policy
    }

    /// Turn noise reduction on/off and set its strength. The stages that run are the policy's.
    pub fn set_noise_reduction(&mut self, on: bool, strength: f64) -> bool {
        self.policy.nr = on;
        self.policy.nr_strength = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 0.6 };
        let known = crate::audio::apply_policy(&mut self.chain, self.policy);
        // CW never runs the notch (a carrier is not interference); the policy does not ask for it
        // either, but the rule is recorded here because it is a property of the mode, not of the
        // listener's setting.
        self.chain.set_stage_enabled("notch", false);
        known
    }

    /// Set the squelch threshold in dBFS (`<= -100` leaves the gate wide open and inert).
    pub fn set_squelch_dbfs(&mut self, dbfs: f64) -> bool {
        if dbfs.is_finite() {
            self.policy.squelch_dbfs = dbfs.clamp(-160.0, 0.0);
        }
        crate::audio::apply_policy(&mut self.chain, self.policy)
    }

    pub fn kind(&self) -> PathKind {
        self.kind
    }

    /// How many messages this pipeline's digital demodulator has decoded.
    pub fn decoded_total(&self) -> u64 {
        self.decoded_total
    }

    /// Input samples the digital demodulator has buffered towards its next decode attempt.
    pub fn digital_buffered(&self) -> usize {
        self.digital.as_ref().map(|demod| demod.buffered_input()).unwrap_or(0)
    }

    /// What the digital demodulator reports about its most recent decode (frequency, slot offset,
    /// SNR). The UI shows the timing with the text: a decoded message without it is of little use
    /// to an operator watching a band.
    pub fn digital_report(&self) -> Option<DigitalReport> {
        self.digital.as_ref().and_then(|demod| demod.last_report())
    }

    pub fn chain(&self) -> &AudioChain {
        &self.chain
    }

    pub fn chain_mut(&mut self) -> &mut AudioChain {
        &mut self.chain
    }

    /// Install the analog demodulator (the analog path's only way to produce PCM).
    ///
    /// The demodulator owns the rate conversion to the consumer's rate (it already resampled its
    /// detector output, so the baseband rate and the audio rate are both just parameters).
    pub fn set_analog_demod(&mut self, demod: Box<dyn AnalogDemodulator>) {
        self.analog = Some(demod);
        self.kind = PathKind::Analog;
    }

    /// Install the digital demodulator, adapting `fs_in` to the decoder's `out_rate` when they
    /// differ. The audio chain stays installed but is unreachable: the digital path has no way to
    /// hand it an [`AnalogPcm`].
    pub fn set_digital_demod(
        &mut self,
        demod: Box<dyn DigitalDemodulator>,
        fs_in: f64,
        out_rate: f64,
    ) {
        self.resample = if fs_in > 0.0 && out_rate > 0.0 && (fs_in - out_rate).abs() > 1e-9 {
            Some(ComplexResampler::new(fs_in, out_rate))
        } else {
            None
        };
        self.digital = Some(demod);
        self.kind = PathKind::Digital;
    }

    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.chain.set_enabled(enabled);
    }

    /// The plugins this pipeline actually uses, for the plugin-manifest contract test. The DDC base
    /// layer is *not* one of them any more: the backend performs the channelization.
    pub fn active_plugins(&self) -> Vec<(&'static str, PluginKind)> {
        let mut active = Vec::new();
        if let Some(analog) = self.analog.as_ref() {
            active.push((analog.id(), PluginKind::Analog));
        }
        if let Some(digital) = self.digital.as_ref() {
            active.push((digital.id(), PluginKind::Digital));
        }
        for id in self.chain.ids() {
            active.push((id, PluginKind::Audio));
        }
        active
    }

    /// Process one block of channelized baseband (interleaved complex f32) into `out`.
    ///
    /// `audio_hold` freezes the audio chain's adaptation (used while a reconfiguration transient
    /// is being discarded).
    pub fn process_f32_into(&mut self, baseband: &[f32], audio_hold: bool, out: &mut PipelineOutput) {
        out.raw.clear();
        out.audio.clear();
        out.decoded.clear();
        let n = baseband.len() & !1;              // a torn block would desync I/Q
        if n == 0 {
            return;
        }
        let baseband = &baseband[..n];
        // The RAW stream is a copy taken before any per-mode processing: this is what the digital
        // decoder reads, and it is bit-identical whatever the analog side is configured to do.
        out.raw.extend_from_slice(baseband);

        match self.kind {
            PathKind::Analog => {
                if let Some(demod) = self.analog.as_mut() {
                    let mut pcm = core::mem::take(&mut self.pcm);
                    demod.process_into(baseband, &mut pcm.samples);
                    // Only here, and only with an AnalogPcm, can the enhancement chain run.
                    self.chain.process(&mut pcm, audio_hold);
                    out.audio.extend_from_slice(pcm.samples());
                    self.pcm = pcm;
                }
            }
            PathKind::Digital => {
                let fed: &[f32] = match self.resample.as_mut() {
                    Some(resampler) => {
                        let mut converted = core::mem::take(&mut self.converted);
                        resampler.process_f32_into(&out.raw, &mut converted);
                        self.converted = converted;
                        &self.converted
                    }
                    None => &out.raw,
                };
                if let Some(demod) = self.digital.as_mut() {
                    out.decoded = demod.process_iq(fed);
                    if !out.decoded.is_empty() {
                        self.decoded_total += out.decoded.len() as u64;
                    }
                }
            }
        }
    }

    /// A retune on the backend: the channel moved, so the demodulator's history describes another
    /// channel. The level controls are deliberately kept (resetting them is what made every tune
    /// start with a loud burst).
    pub fn retune(&mut self) {
        if let Some(demod) = self.analog.as_mut() {
            demod.retune();
        }
    }

    pub fn reset(&mut self) {
        if let Some(demod) = self.analog.as_mut() {
            demod.reset();
        }
        if let Some(demod) = self.digital.as_mut() {
            demod.reset();
        }
        if let Some(resampler) = self.resample.as_mut() {
            resampler.reset();
        }
        self.chain.reset();
        self.converted.clear();
        self.pcm.samples.clear();
    }
}
