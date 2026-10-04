//! Small DSP helpers shared by the channel-estimation and tracking stages, ported from
//! Dream/DecDRM's `dsp` helpers (MIT-clean: these are textbook formulas).

use crate::digital::drm2::dsp::Cplx;

/// Pole of a one-pole IIR smoother with time constant `tau` seconds updated at
/// `rate` Hz (Dream's `IIR1Lam`).
pub fn iir1_lambda(tau: f64, rate: f64) -> f64 {
    (-1.0 / (tau * rate)).exp()
}

/// One-pole smoother `y ← λ·y + (1−λ)·x` (Dream's `IIR1`).
#[inline]
pub fn iir1(y: &mut f64, x: f64, lambda: f64) {
    *y = lambda * (*y - x) + x;
}

/// Complex version of [`iir1`].
#[inline]
pub fn iir1_c(y: &mut Cplx, x: Cplx, lambda: f64) {
    *y = (*y - x) * lambda + x;
}

/// Normalised sinc: sin(πx)/(πx).
pub fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = core::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Symmetric Hamming window of length `n` (Matlab's `hamming`).
pub fn hamming(n: usize) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    (0..n)
        .map(|k| 0.54 - 0.46 * (2.0 * core::f64::consts::PI * k as f64 / (n - 1) as f64).cos())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinc_is_one_at_zero_and_decays() {
        assert_eq!(sinc(0.0), 1.0);
        assert!((sinc(0.5) - (core::f64::consts::FRAC_2_PI * 2.0 / 2.0)).abs() < 1e-12
            || (sinc(0.5) - 0.636_619_772_367_581_4).abs() < 1e-12);
        assert!(sinc(1.0).abs() < 1e-12, "sinc(1) is a zero");
    }

    #[test]
    fn iir_smooths_towards_the_input() {
        let mut y = 0.0;
        iir1(&mut y, 1.0, 0.5);
        assert_eq!(y, 0.5);
        iir1(&mut y, 1.0, 0.5);
        assert_eq!(y, 0.75);
    }

    #[test]
    fn hamming_has_the_expected_shape() {
        let w = hamming(5);
        // First and last taps are the minimum (0.08), the centre the maximum (1.0).
        assert!((w[0] - 0.08).abs() < 1e-12);
        assert!((w[2] - 1.0).abs() < 1e-12);
        assert!((w[4] - 0.08).abs() < 1e-12);
    }
}
