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
pub fn default_chain(rate: f64) -> crate::pipeline::AudioChain {
    crate::pipeline::AudioChain::from_registry(&stage_factories(), rate)
}

/// The chain as a specific mode should run it.
///
/// CW turns the adaptive notch **off**: the notch removes the strongest single tone in the channel,
/// which for CW is the carrier the operator is listening to (and for a lone test tone is the signal
/// itself). Voice modes keep it, because there a single steady tone is interference. This is the one
/// place that knows the difference; the stages themselves stay mode-agnostic.
pub fn chain_for_mode(mode: &str, rate: f64) -> crate::pipeline::AudioChain {
    let mut chain = default_chain(rate);
    if mode == "cw" {
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
    fn the_chain_turns_a_quiet_voice_like_signal_into_audible_pcm() {
        let mut chain = default_chain(DEFAULT_AUDIO_RATE);
        // A voice-like signal (a tone plus noise), not a lone tone: the adaptive notch is part of
        // the default chain and would remove a single steady tone by design. The level is above
        // the squelch threshold so the test measures the chain rather than the gate.
        let mut seed = 21_u32;
        let input: Vec<f32> = (0..9_600)
            .map(|k| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.2;
                (noise + 0.05 * (2.0 * PI * 1_000.0 * k as f64 / 48_000.0).sin()) as f32
            })
            .collect();
        let mut pcm = crate::pipeline::AnalogPcm::default();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
        assert_eq!(pcm.samples().len(), input.len());
        let rms = (pcm.samples().iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
            / pcm.samples().len() as f64)
            .sqrt();
        assert!(rms > 0.05, "the AGC must bring the signal up: rms {rms}");
        assert!(AudioChain::new(Vec::new()).is_empty());
    }

    #[test]
    fn the_default_chain_notches_a_lone_tone_away_and_the_cw_chain_does_not() {
        // Documents the policy that `chain_for_mode` encodes: the notch treats a single steady tone
        // as interference, so a CW carrier must run with it disabled.
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
        let with_notch = tone_after(&mut default_chain(DEFAULT_AUDIO_RATE));
        let mut cw_chain = chain_for_mode("cw", DEFAULT_AUDIO_RATE);
        assert!(!cw_chain.is_stage_enabled("notch"), "CW must run without the notch");
        let without_notch = tone_after(&mut cw_chain);
        println!("chain: lone 1 kHz tone -> {with_notch:.4} (default) vs {without_notch:.4} (cw)");
        assert!(
            without_notch > 10.0 * with_notch,
            "the CW chain must keep its carrier: {without_notch} vs {with_notch}"
        );
    }
}
