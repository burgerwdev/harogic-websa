//! Complex NCO: the phase-continuous oscillator that brings an offset channel down to DC.
//!
//! The reference is `web_sa/measurements/sdr.py::_mix`: the phase is advanced by
//! `-2*pi*f/fs` per sample and wrapped per block, and the sample phase is computed as
//! `phase0 + inc*i` rather than by repeated addition, so a long block cannot drift.

use core::f64::consts::PI;

pub const TAU: f64 = 2.0 * PI;

#[derive(Debug, Default, Clone)]
pub struct Nco {
    phase: f64,
    inc: f64,
    freq_hz: f64,
    rate_hz: f64,
}

impl Nco {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the mixing frequency: a component at `freq_hz` in the input lands at DC.
    pub fn set_frequency(&mut self, freq_hz: f64, rate_hz: f64) {
        self.freq_hz = freq_hz;
        self.rate_hz = rate_hz;
        self.inc = if rate_hz > 0.0 { -TAU * freq_hz / rate_hz } else { 0.0 };
    }

    pub fn frequency(&self) -> f64 {
        self.freq_hz
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// True when the mixer is a no-op (the caller can skip the multiply entirely).
    pub fn is_bypassed(&self) -> bool {
        self.inc.abs() < 1e-6
    }

    /// Mix `iq` (interleaved I,Q as f64) in place.
    ///
    /// The phase advances across calls, so a continuous block stream stays phase-continuous
    /// (a per-block reset would put a click at every block boundary).
    pub fn mix(&mut self, iq: &mut [f64]) {
        let n = iq.len() / 2;
        if n == 0 || self.is_bypassed() {
            return;
        }
        for k in 0..n {
            let ph = self.phase + self.inc * (k as f64);
            let (c, s) = (ph.cos(), ph.sin());
            let (i, q) = (iq[2 * k], iq[2 * k + 1]);
            iq[2 * k] = i * c - q * s;
            iq[2 * k + 1] = i * s + q * c;
        }
        self.phase = (self.phase + self.inc * (n as f64)) % TAU;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_by_the_tone_frequency_lands_it_on_dc() {
        let rate = 1.0e6;
        let f = 25_000.0;
        let n = 4096;
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            let ph = TAU * f / rate * (k as f64);
            iq.push(ph.cos());
            iq.push(ph.sin());
        }
        let mut nco = Nco::new();
        nco.set_frequency(f, rate);
        nco.mix(&mut iq);
        // Every sample should now be ~1 + 0j (the channel sits at DC).
        for k in 0..n {
            assert!((iq[2 * k] - 1.0).abs() < 1e-9, "I at {k}: {}", iq[2 * k]);
            assert!(iq[2 * k + 1].abs() < 1e-9, "Q at {k}: {}", iq[2 * k + 1]);
        }
    }

    #[test]
    fn phase_carries_across_blocks() {
        let mut nco = Nco::new();
        nco.set_frequency(31_250.0, 250_000.0);
        let mut first = vec![1.0, 0.0];
        let mut second = vec![1.0, 0.0];
        nco.mix(&mut first);
        nco.mix(&mut second);
        let expected = TAU * 31_250.0 / 250_000.0;   // one sample of phase
        assert!((second[0] - expected.cos()).abs() < 1e-12);
        assert!((second[1] + expected.sin()).abs() < 1e-12);
    }

    #[test]
    fn zero_frequency_is_a_no_op() {
        let mut nco = Nco::new();
        nco.set_frequency(0.0, 1.0e6);
        assert!(nco.is_bypassed());
        let mut iq = vec![0.25, -0.5];
        nco.mix(&mut iq);
        assert_eq!(iq, vec![0.25, -0.5]);
    }
}
