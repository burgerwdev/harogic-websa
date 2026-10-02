//! Channel coding for DRM30 (ES 201 980 §7): energy dispersal, the punctured
//! convolutional code, bit interleaving, QAM mapping/metrics, multilevel coding and
//! the CRCs used by FAC/SDC/audio.
//!
//! Independent reimplementation of the standard; DecDRM's `fec/` modules were used as
//! a cross-check reference.

pub mod conv;
pub mod crc;
pub mod dispersal;
pub mod interleaver;
pub mod mlc;
pub mod puncture;
pub mod qam;
pub mod viterbi;

/// Soft input for one coded bit: the distance metric towards a transmitted 0 and
/// towards a transmitted 1 (smaller is more likely).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BitMetric {
    pub to0: f64,
    pub to1: f64,
}

impl BitMetric {
    /// An erasure: no information about the bit.
    pub const ERASURE: Self = Self { to0: 0.0, to1: 0.0 };
}
