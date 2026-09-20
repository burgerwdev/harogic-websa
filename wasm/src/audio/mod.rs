//! Audio enhancement for the analog PCM path.
//!
//! Everything in this module is speech/listening oriented (DC block, LPF, AGC, squelch, STFT
//! Wiener, adaptive notch, IF noise blanker) and is reachable **only** through
//! [`crate::pipeline::AudioChain`], which takes an [`crate::pipeline::AnalogPcm`]. The digital
//! path does not have one of those, so a denoiser cannot touch a decoder's samples.
//!
//! Stages are registered in `plugin::AUDIO_PLUGINS`; a stage that is not implemented there yet is
//! skipped rather than stubbed.

pub mod dc_block;

pub use dc_block::DcBlock;
