//! Carrier-offset removal: the numerically controlled oscillator the chain applies between the
//! coarse acquisition and the OFDM demodulation.
//!
//! The acquisition measures the DRM DC carrier's frequency (Dream's `CFreqSyncAcq`), but the
//! OFDM demodulation needs the carriers on the FFT grid: half a carrier spacing already destroys
//! the constellation, and an uncorrected offset rotates each symbol's cells against the next,
//! which no channel estimate can follow. This stage removes the measured offset from the stream,
//! with the phase taken from the absolute sample index so consecutive blocks join without a step.

use crate::digital::drm::dsp::Cplx;
use crate::digital::drm::params::SAMPLE_RATE;

/// A streaming mixer that removes a fixed frequency offset.
pub struct Nco {
    /// Phase increment per sample, radians.
    w: f64,
    /// Phase for the next sample (from the absolute index, so there is no discontinuity).
    phase: f64,
    /// Samples mixed so far.
    position: u64,
}

impl Nco {
    /// `offset_hz` is the measured carrier offset (the DC carrier's position).
    pub fn new(offset_hz: f64) -> Self {
        Self { w: -2.0 * core::f64::consts::PI * offset_hz / f64::from(SAMPLE_RATE), phase: 0.0, position: 0 }
    }

    /// The offset being removed, Hz.
    pub fn offset_hz(&self) -> f64 {
        -self.w * f64::from(SAMPLE_RATE) / (2.0 * core::f64::consts::PI)
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.position = 0;
    }

    /// Change the offset being removed (the reference's `track_hz` update), keeping the phase
    /// continuous so the next sample joins without a step.
    pub fn set_offset(&mut self, offset_hz: f64) {
        self.w = -2.0 * core::f64::consts::PI * offset_hz / f64::from(SAMPLE_RATE);
    }

    /// Mix `input` in place, continuing the phase from the previous call.
    pub fn process(&mut self, input: &mut [Cplx]) {
        for v in input.iter_mut() {
            let (s, c) = self.phase.sin_cos();
            *v = Cplx::new(v.re * c - v.im * s, v.re * s + v.im * c);
            self.phase += self.w;
            self.position += 1;
        }
    }

    /// Mix a block into `out` (cleared first).
    pub fn process_into(&mut self, input: &[Cplx], out: &mut Vec<Cplx>) {
        out.clear();
        out.reserve(input.len());
        for &v in input {
            let (s, c) = self.phase.sin_cos();
            out.push(Cplx::new(v.re * c - v.im * s, v.re * s + v.im * c));
            self.phase += self.w;
            self.position += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mixing a tone by its own offset brings it to DC.
    #[test]
    fn removes_a_tone_at_the_measured_offset() {
        let fs = f64::from(SAMPLE_RATE);
        let offset = 121.0;
        let mut nco = Nco::new(offset);
        let input: Vec<Cplx> = (0..4096)
            .map(|n| {
                let ph = 2.0 * core::f64::consts::PI * offset * n as f64 / fs;
                Cplx::new(ph.cos(), ph.sin())
            })
            .collect();
        let mut out = Vec::new();
        nco.process_into(&input, &mut out);
        // Every sample must now be (1, 0), up to the phase step's rounding.
        let mean = out.iter().fold(Cplx::zero(), |a, b| a + *b) / out.len() as f64;
        assert!(mean.norm() > 0.999, "mean {:?}", mean);
    }

    /// Consecutive blocks keep the phase: no step at the boundary.
    #[test]
    fn blocks_join_without_a_step() {
        let fs = f64::from(SAMPLE_RATE);
        let offset = 100.0;
        let tone = |n: u64| {
            let ph = 2.0 * core::f64::consts::PI * offset * n as f64 / fs;
            Cplx::new(ph.cos(), ph.sin())
        };
        let mut nco = Nco::new(offset);
        let a: Vec<Cplx> = (0..64).map(tone).collect();
        let b: Vec<Cplx> = (64..128).map(tone).collect();
        let mut out_a = Vec::new();
        nco.process_into(&a, &mut out_a);
        let mut out_b = Vec::new();
        nco.process_into(&b, &mut out_b);
        for v in out_a.iter().chain(&out_b) {
            assert!((v.re - 1.0).abs() < 1e-6 && v.im.abs() < 1e-6, "{v:?}");
        }
    }
}
