//! Audio enhancement for the analog PCM path.
//!
//! Everything in this module is speech/listening oriented (DC block, LPF, AGC, squelch, STFT
//! Wiener, adaptive notch, IF noise blanker) and is reachable **only** through
//! [`crate::pipeline::AudioChain`], which takes an [`crate::pipeline::AnalogPcm`]. The digital
//! path does not have one of those, so a denoiser cannot touch a decoder's samples.
//!
//! Stages are registered in `plugin::AUDIO_PLUGINS`; a stage that is not implemented there yet is
//! skipped rather than stubbed, and `stage_factories()` must list exactly the implemented ones in
//! registry order (a test asserts it).

pub mod blanker;
pub mod dc_block;
pub mod notch;
pub mod stages;
pub mod wiener;

pub use dc_block::DcBlock;
pub use stages::{stage_factories, AgcStage, Lpf, Squelch, DEFAULT_AUDIO_RATE};

/// The default analog-PCM enhancement chain for a consumer running at `rate`, in registry order.
///
/// Every stage is installed; which ones *run* is [`AudioPolicy`]'s decision, so a listener who has
/// not asked for anything gets the Python reference's behaviour (no enhancement at all) and a
/// setting can be changed without rebuilding the chain.
pub fn default_chain(rate: f64) -> crate::pipeline::AudioChain {
    crate::pipeline::AudioChain::from_registry(&stage_factories(), rate)
}

/// What the listener asked the audio chain to do.
///
/// The Python reference has no enhancement chain at all, so the default is "nothing on" — the
/// browser path then produces the same audio the reference does, and the two can be compared. The
/// noise reducer is the switch the panel owns; the squelch is the level gate, always meaningful,
/// and only engaged once it is set above "wide open".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioPolicy {
    /// Noise reduction on/off.
    pub nr: bool,
    /// How hard the noise reducer pushes (0..1).
    pub nr_strength: f64,
    /// Squelch threshold in dBFS (`<= -100` means wide open).
    pub squelch_dbfs: f64,
}

impl Default for AudioPolicy {
    fn default() -> Self {
        Self { nr: false, nr_strength: 0.6, squelch_dbfs: -110.0 }
    }
}

/// The squelch threshold at or below which the gate is a pass-through (the reference's default).
pub const SQUELCH_OPEN_DBFS: f64 = -100.0;

impl AudioPolicy {
    /// The stages that must run for this policy, in registry order.
    pub fn active_stages(&self) -> Vec<&'static str> {
        let mut stages = Vec::new();
        if self.squelch_dbfs > SQUELCH_OPEN_DBFS {
            stages.push("squelch");
        }
        if self.nr {
            // The reducer is the noise reduction; the blanker removes impulse noise (clicks), which
            // is the other half of a quiet band and cannot hurt a tonal signal.
            stages.push("wiener");
            stages.push("blanker");
        }
        stages
    }
}

/// Apply `policy` to `chain`: exactly the stages it asks for run.
pub fn apply_policy(chain: &mut crate::pipeline::AudioChain, policy: AudioPolicy) -> bool {
    let wanted = policy.active_stages();
    let mut known = true;
    for id in chain.ids() {
        known &= chain.set_stage_enabled(id, wanted.contains(&id));
    }
    chain.set_strength(policy.nr_strength);
    chain.set_threshold(policy.squelch_dbfs);
    known
}

/// The chain as a specific mode should run it.
///
/// CW turns the adaptive notch **off**: the notch removes the strongest single tone in the channel,
/// which for CW is the carrier the operator is listening to (and for a lone test tone is the signal
/// itself). Voice modes keep it, because there a single steady tone is interference. This is the one
/// place that knows the difference; the stages themselves stay mode-agnostic.
pub fn chain_for_mode(mode: &str, rate: f64, policy: AudioPolicy) -> crate::pipeline::AudioChain {
    let mut chain = default_chain(rate);
    apply_policy(&mut chain, policy);
    if mode == "cw" {
        // Belt and braces: whatever the policy says, a CW carrier is never notched.
        chain.set_stage_enabled("notch", false);
    }
    chain
}

/// Modes whose audio must not be spectrally reshaped (no notch): a single carrier *is* the signal.
pub fn notch_is_inappropriate(mode: &str) -> bool {
    matches!(mode, "cw")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::AudioChain;
    use core::f64::consts::PI;

    #[test]
    fn every_implemented_audio_plugin_is_in_the_default_chain() {
        let chain = default_chain(DEFAULT_AUDIO_RATE);
        let ids = chain.ids();
        for descriptor in crate::plugin::AUDIO_PLUGINS.iter().filter(|p| p.implemented) {
            assert!(ids.contains(&descriptor.id), "{} is implemented but not in the chain", descriptor.id);
        }
        assert_eq!(ids.len(), crate::plugin::AUDIO_PLUGINS.iter().filter(|p| p.implemented).count());
    }

    #[test]
    fn the_chain_preserves_the_block_length_and_leaves_silence_quiet() {
        let mut chain = default_chain(DEFAULT_AUDIO_RATE);
        let mut pcm = crate::pipeline::AnalogPcm::default();
        pcm.samples_mut().extend(std::iter::repeat(0.0).take(960));
        chain.process(&mut pcm, false);
        assert_eq!(pcm.samples().len(), 960, "the chain must not change the block length");
        let peak = pcm.samples().iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(peak < 1e-3, "silence in, silence out (squelch closed): peak {peak}");
    }

    #[test]
    fn noise_reduction_turns_the_chain_on_and_the_default_leaves_the_audio_alone() {
        // The Python reference has no enhancement chain, so the default policy must not change a
        // single sample (that is what makes the two paths comparable); the NR switch is what
        // enables the reducer, and its strength reaches the stage.
        let mut seed = 21_u32;
        let input: Vec<f32> = (0..9_600)
            .map(|k| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.2;
                (noise + 0.05 * (2.0 * PI * 1_000.0 * k as f64 / 48_000.0).sin()) as f32
            })
            .collect();
        let mut chain = default_chain(DEFAULT_AUDIO_RATE);
        let mut pcm = crate::pipeline::AnalogPcm::default();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
        assert_eq!(pcm.samples(), &input[..], "the default policy is a bit-exact pass-through");
        assert!(chain.active_ids().is_empty(), "no stage runs before the listener asks");

        apply_policy(&mut chain, AudioPolicy { nr: true, ..AudioPolicy::default() });
        assert_eq!(chain.active_ids(), vec!["wiener", "blanker"]);
        assert!(!chain.is_stage_enabled("notch"), "the notch is not part of any policy");
        assert!(!chain.is_stage_enabled("agc"), "the demodulator already runs the reference AGC");
        let mut pcm = crate::pipeline::AnalogPcm::default();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
        assert_eq!(pcm.samples().len(), input.len());
        assert_ne!(pcm.samples(), &input[..], "the reducer must actually process the block");
        assert!(AudioChain::new(Vec::new()).is_empty());
    }

    #[test]
    fn a_lone_carrier_survives_every_policy_the_panel_can_ask_for() {
        // The old default enabled an adaptive notch, which removed the signal itself when the signal
        // was a tone (measured: -33 dB on an AM test tone, and it made every A/B against the Python
        // reference meaningless). No policy enables it now, so a CW carrier - the case that policy
        // existed for - survives, and so does a tone in any other mode.
        let input: Vec<f32> = (0..9_600)
            .map(|k| (0.2 * (2.0 * PI * 1_000.0 * k as f64 / 48_000.0).sin()) as f32)
            .collect();
        let tone_after = |chain: &mut AudioChain| {
            let mut pcm = crate::pipeline::AnalogPcm::default();
            pcm.samples_mut().extend_from_slice(&input);
            chain.process(&mut pcm, false);
            let samples = pcm.samples();
            let n = samples.len() as f64;
            let (mut re, mut im) = (0.0, 0.0);
            for (k, value) in samples.iter().enumerate() {
                let ph = 2.0 * PI * 1_000.0 * k as f64 / 48_000.0;
                re += *value as f64 * ph.cos();
                im -= *value as f64 * ph.sin();
            }
            (re * re + im * im).sqrt() * 2.0 / n
        };
        let level = tone_after(&mut default_chain(DEFAULT_AUDIO_RATE));
        assert!(level > 0.15, "the default chain must keep the tone: {level}");
        for mode in ["am", "cw", "nfm", "wfm"] {
            let policy = AudioPolicy { nr: true, nr_strength: 1.0, squelch_dbfs: -60.0 };
            let mut chain = chain_for_mode(mode, DEFAULT_AUDIO_RATE, policy);
            assert!(!chain.is_stage_enabled("notch"), "{mode}: a carrier is never notched");
            let level = tone_after(&mut chain);
            assert!(level > 0.15, "{mode}: the tone must survive the chain, got {level}");
        }
    }
}
