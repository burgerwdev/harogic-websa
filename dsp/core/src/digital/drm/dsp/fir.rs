//! FIR filtering and decimation for the correlation path.
//!
//! Dream runs its guard-interval correlation on a signal low-passed to about +/-5 kHz and
//! decimated (`GRDCRR_DEC_FACT` 4 with its 10 kHz Hilbert filter, `sync/TimeSyncFilter.h`).
//! That both cuts the correlation's cost and removes the carriers outside the guard's
//! coherence band, which is what makes the correlation peak usable.

use crate::digital::drm::dsp::Cplx;

/// A windowed-sinc low-pass. `cutoff` is normalised to the sample rate (0.5 = Nyquist) and
/// `attenuation_db` sizes the Kaiser window: 60 dB gives a stop-band that is flat enough for
/// the correlation, and 97 taps is the reference's length.
pub fn lowpass(taps: usize, cutoff: f64, attenuation_db: f64) -> Vec<f64> {
    assert!(taps % 2 == 1, "an odd tap count keeps the filter linear-phase and symmetric");
    let n = taps as isize;
    let centre = (n - 1) / 2;
    // Kaiser beta from the stop-band attenuation (Kaiser's empirical fit).
    let beta = if attenuation_db > 50.0 {
        0.1102 * (attenuation_db - 8.7)
    } else if attenuation_db >= 21.0 {
        0.5842 * (attenuation_db - 21.0).powf(0.4) + 0.07886 * (attenuation_db - 21.0)
    } else {
        0.0
    };
    let i0 = |x: f64| -> f64 {
        // Zeroth-order modified Bessel function, series expansion (converges fast for |x| < 20).
        let mut sum = 1.0f64;
        let mut term = 1.0f64;
        for k in 1..40 {
            term *= (x / (2.0 * k as f64)) * (x / (2.0 * k as f64));
            sum += term;
            if term < 1e-16 * sum {
                break;
            }
        }
        sum
    };
    let denom = i0(beta);
    let mut h: Vec<f64> = (0..taps)
        .map(|i| {
            let k = i as isize - centre;
            let sinc = if k == 0 {
                2.0 * cutoff
            } else {
                (2.0 * core::f64::consts::PI * cutoff * k as f64).sin() / (core::f64::consts::PI * k as f64)
            };
            let r = k as f64 / centre as f64;
            let w = i0(beta * (1.0 - r * r).max(0.0).sqrt()) / denom;
            sinc * w
        })
        .collect();
    // Normalise to unity gain at DC.
    let sum: f64 = h.iter().sum();
    if sum.abs() > 0.0 {
        for v in h.iter_mut() {
            *v /= sum;
        }
    }
    h
}

/// A complex FIR decimator: streams input samples, emits one output per `decim` inputs.
pub struct FirDecimator {
    taps: Vec<f64>,
    decim: usize,
    /// Input samples seen so far (for the phase).
    phase: usize,
    /// History, newest last, length `taps.len()`.
    history: Vec<Cplx>,
}

impl FirDecimator {
    pub fn new(taps: Vec<f64>, decim: usize) -> Self {
        assert!(decim >= 1);
        let n = taps.len();
        Self { taps, decim, phase: 0, history: vec![Cplx::zero(); n] }
    }

    /// The filter's group delay in input samples: a filtered output at input index `i` is
    /// centred on input `i - delay`.
    pub fn delay(&self) -> f64 {
        (self.taps.len() as f64 - 1.0) / 2.0
    }

    pub fn reset(&mut self) {
        self.phase = 0;
        self.history.iter_mut().for_each(|v| *v = Cplx::zero());
    }

    /// Filter and decimate: one output every `decim` inputs, starting with the `decim`-th.
    pub fn process(&mut self, input: &[Cplx], out: &mut Vec<Cplx>) {
        for &x in input {
            self.history.rotate_right(1);
            self.history[0] = x;
            self.phase += 1;
            if self.phase == self.decim {
                self.phase = 0;
                let mut acc = Cplx::zero();
                for (h, v) in self.taps.iter().zip(&self.history) {
                    acc += *v * *h;
                }
                out.push(acc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A low-pass must keep a slow signal and reject one above its cutoff.
    #[test]
    fn the_low_pass_separates_its_bands() {
        let taps = lowpass(97, 6000.0 / 48000.0, 60.0);
        let fs = 48_000.0;
        let response = |hz: f64| -> f64 {
            let mut re = 0.0;
            let mut im = 0.0;
            for (k, h) in taps.iter().enumerate() {
                let ph = -2.0 * core::f64::consts::PI * hz * k as f64 / fs;
                re += h * ph.cos();
                im += h * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        assert!((response(0.0) - 1.0).abs() < 1e-9, "unity at DC");
        assert!(response(1000.0) > 0.99, "passband stays flat: {}", response(1000.0));
        assert!(response(15_000.0) < 1e-3, "stopband is rejected: {}", response(15_000.0));
    }

    #[test]
    fn the_decimator_emits_one_sample_per_factor() {
        let mut dec = FirDecimator::new(vec![1.0], 4);
        let input: Vec<Cplx> = (0..10).map(|i| Cplx::new(i as f64, 0.0)).collect();
        let mut out = Vec::new();
        dec.process(&input, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].re, 3.0);
        assert_eq!(out[1].re, 7.0);
    }
}
