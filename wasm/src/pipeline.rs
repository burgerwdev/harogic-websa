//! The pipeline: one DDC base layer, two consumers, and one rule that cannot be broken.
//!
//! ```text
//!   IQ ──▶ DDC ──┬──▶ RAW baseband  (always emitted; the digital decoder and the display read this)
//!                ├──▶ AnalogDemod ──▶ PCM ──▶ AudioChain ──▶ speaker
//!                └──▶ DigitalDemod ─────────────────────────▶ decoded messages
//! ```
//!
//! The rule: **the audio-enhancement chain only ever receives [`AnalogPcm`]** — a newtype that the
//! RAW/digital paths cannot produce. A voice denoiser on an FT8 tone destroys the information the
//! decoder needs, so the separation is a type, not a comment, and
//! `wasm/tests/path_separation.rs` proves it bit-for-bit by toggling the chain and comparing the
//! RAW output.
//!
//! The DDC stage set is shared on purpose (it is level control and channel selection, not speech
//! enhancement) and is therefore not part of `AudioChain`.

use crate::ddc::Ddc;
use crate::plugin::{AnalogDemodulator, AudioStage, DigitalDemodulator, PluginKind, AUDIO_PLUGINS};

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

    /// Build the chain for the plugins that are implemented, in `AUDIO_PLUGINS` order.
    ///
    /// Stages whose kernels are not written yet are skipped rather than faked, so the chain can
    /// never "pass audio through" while pretending to process it.
    pub fn from_registry(factories: &[(&'static str, fn() -> Box<dyn AudioStage>)]) -> Self {
        let mut stages: Vec<Box<dyn AudioStage>> = Vec::new();
        for descriptor in AUDIO_PLUGINS.iter().filter(|p| p.implemented) {
            if let Some((_, build)) = factories.iter().find(|(id, _)| *id == descriptor.id) {
                stages.push(build());
            }
        }
        Self::new(stages)
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

/// One processed IQ block: everything the consumers need, produced in one pass.
#[derive(Debug, Default)]
pub struct PipelineOutput {
    /// Interleaved complex baseband at the channel rate — the RAW stream. Never modified by the
    /// audio chain.
    pub raw: Vec<f32>,
    /// Analog audio (after the enhancement chain when it is enabled). Empty in the digital path.
    pub audio: Vec<f32>,
    /// Messages a digital demodulator decoded from this block.
    pub decoded: Vec<String>,
}

/// The assembled receiver.
pub struct Pipeline {
    ddc: Ddc,
    kind: PathKind,
    analog: Option<Box<dyn AnalogDemodulator>>,
    digital: Option<Box<dyn DigitalDemodulator>>,
    chain: AudioChain,
    baseband: Vec<f32>,
    pcm: AnalogPcm,
}

impl Pipeline {
    pub fn new(ddc: Ddc, kind: PathKind, chain: AudioChain) -> Self {
        Self {
            ddc,
            kind,
            analog: None,
            digital: None,
            chain,
            baseband: Vec::new(),
            pcm: AnalogPcm::default(),
        }
    }

    pub fn kind(&self) -> PathKind {
        self.kind
    }

    pub fn chain(&self) -> &AudioChain {
        &self.chain
    }

    pub fn chain_mut(&mut self) -> &mut AudioChain {
        &mut self.chain
    }

    /// Install the analog demodulator (the analog path's only way to produce PCM).
    pub fn set_analog_demod(&mut self, demod: Box<dyn AnalogDemodulator>) {
        self.analog = Some(demod);
        self.kind = PathKind::Analog;
    }

    /// Install the digital demodulator. The audio chain stays installed but is unreachable: the
    /// digital path has no way to hand it an [`AnalogPcm`].
    pub fn set_digital_demod(&mut self, demod: Box<dyn DigitalDemodulator>) {
        self.digital = Some(demod);
        self.kind = PathKind::Digital;
    }

    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.chain.set_enabled(enabled);
    }

    /// The plugins this pipeline actually uses, for the plugin-manifest contract test.
    pub fn active_plugins(&self) -> Vec<(&'static str, PluginKind)> {
        let mut active = vec![("nco", PluginKind::Ddc), ("fir", PluginKind::Ddc),
                              ("decimate", PluginKind::Ddc), ("resample", PluginKind::Ddc)];
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

    /// Process one int16 IQ block into `out`.
    ///
    /// `audio_hold` freezes the audio chain's adaptation (used while a reconfiguration transient
    /// is being discarded).
    pub fn process_i16_into(&mut self, iq: &[i16], audio_hold: bool, out: &mut PipelineOutput) {
        out.raw.clear();
        out.audio.clear();
        out.decoded.clear();

        let mut baseband = core::mem::take(&mut self.baseband);
        self.ddc.process_i16_into(iq, &mut baseband);

        // The RAW stream is a copy taken before any per-mode processing: this is what the digital
        // decoder and the visualization consume, and it is bit-identical whatever the analog side
        // is configured to do.
        out.raw.extend_from_slice(&baseband);

        match self.kind {
            PathKind::Analog => {
                if let Some(demod) = self.analog.as_mut() {
                    let mut pcm = core::mem::take(&mut self.pcm);
                    demod.process_into(&baseband, &mut pcm.samples);
                    // Only here, and only with an AnalogPcm, can the enhancement chain run.
                    self.chain.process(&mut pcm, audio_hold);
                    out.audio.extend_from_slice(pcm.samples());
                    self.pcm = pcm;
                }
            }
            PathKind::Digital => {
                if let Some(demod) = self.digital.as_mut() {
                    out.decoded = demod.process_iq(&baseband);
                }
            }
        }

        self.baseband = baseband;
    }

    pub fn reset(&mut self) {
        self.ddc.reset();
        if let Some(demod) = self.analog.as_mut() {
            demod.reset();
        }
        if let Some(demod) = self.digital.as_mut() {
            demod.reset();
        }
        self.chain.reset();
        self.baseband.clear();
        self.pcm.samples.clear();
    }
}
