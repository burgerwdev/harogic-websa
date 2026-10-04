//! The DFT the chain uses: a radix-2 transform where the size allows it, a Bluestein DFT
//! otherwise (mode A's 1152, C's 704 and D's 448 are not powers of two).
//!
//! Two entry points, because the two users want different things:
//! [`FullFft::forward_re_im`] is the raw unscaled transform of a complex array (the coarse
//! acquisition's periodograms; Parseval then holds as `Σ|X|² = N·Σ|x·w|²`), and
//! [`FullFft::forward`] transforms DRM cells with the `1/N` scaling the OFDM demodulation
//! expects.

use crate::digital::drm2::dsp::Cplx;
use crate::fft::{Bluestein, Fft};

enum Backend {
    Pow2(Fft),
    Blue(Bluestein),
}

impl Backend {
    fn new(n: usize) -> Self {
        if n.is_power_of_two() {
            Backend::Pow2(Fft::new(n))
        } else {
            Backend::Blue(Bluestein::new(n))
        }
    }

    fn forward(&mut self, re: &mut [f64], im: &mut [f64]) {
        match self {
            Backend::Pow2(f) => f.transform(re, im, false),
            Backend::Blue(b) => b.forward(re, im),
        }
    }
}

/// Forward DFT of any of the DRM transform sizes, without allocating per call.
pub struct FullFft {
    backend: Backend,
    n: usize,
}

impl FullFft {
    pub fn new(n: usize) -> Self {
        Self { backend: Backend::new(n), n }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Unscaled forward DFT in place.
    pub fn forward_re_im(&mut self, re: &mut [f64], im: &mut [f64]) {
        assert_eq!(re.len(), self.n);
        assert_eq!(im.len(), self.n);
        self.backend.forward(re, im);
    }

    /// Unscaled inverse DFT in place: `X[k] = Σ x[n]·e^{+j2πkn/N}` (the conjugate of the
    /// forward transform of the conjugate). Used by the impulse-response tracker's IFFT.
    pub fn inverse_re_im(&mut self, re: &mut [f64], im: &mut [f64]) {
        assert_eq!(re.len(), self.n);
        assert_eq!(im.len(), self.n);
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.backend.forward(re, im);
        for v in im.iter_mut() {
            *v = -*v;
        }
    }

    /// Forward DFT of one window, scaled by `1/N`, in bin order 0..N.
    pub fn forward(&mut self, window: &[Cplx], out: &mut [Cplx]) {
        assert_eq!(window.len(), self.n);
        assert_eq!(out.len(), self.n);
        let mut re: Vec<f64> = window.iter().map(|v| v.re).collect();
        let mut im: Vec<f64> = window.iter().map(|v| v.im).collect();
        self.backend.forward(&mut re, &mut im);
        let scale = 1.0 / self.n as f64;
        for (i, v) in out.iter_mut().enumerate() {
            *v = Cplx::new(re[i] * scale, im[i] * scale);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DFT of a single bin's complex exponential is a delta at that bin.
    #[test]
    fn a_pure_tone_lands_in_one_bin() {
        for n in [1024usize, 1152, 704, 448, 6144, 216] {
            let k = 7usize;
            let mut re = vec![0.0; n];
            let mut im = vec![0.0; n];
            for i in 0..n {
                let ph = 2.0 * core::f64::consts::PI * k as f64 * i as f64 / n as f64;
                re[i] = ph.cos();
                im[i] = ph.sin();
            }
            let mut fft = FullFft::new(n);
            fft.forward_re_im(&mut re, &mut im);
            for (i, (r, m)) in re.iter().zip(&im).enumerate() {
                let mag = (r * r + m * m).sqrt();
                if i == k {
                    assert!((mag - n as f64).abs() < 1e-6 * n as f64, "n={n} bin {i} mag {mag}");
                } else {
                    assert!(mag < 1e-6 * n as f64, "n={n} bin {i} mag {mag}");
                }
            }
        }
    }

    /// The unscaled inverse is the forward transform of the conjugate, conjugated: it
    /// must round-trip with the forward transform scaled by N.
    #[test]
    fn inverse_re_im_round_trips() {
        for n in [1024usize, 216, 1152, 104] {
            let mut fft = FullFft::new(n);
            let mut re: Vec<f64> = (0..n).map(|i| ((i * 37 + 11) % 101) as f64 - 50.0).collect();
            let mut im: Vec<f64> = (0..n).map(|i| ((i * 53 + 7) % 89) as f64 - 44.0).collect();
            let (orig_re, orig_im) = (re.clone(), im.clone());
            fft.forward_re_im(&mut re, &mut im);
            fft.inverse_re_im(&mut re, &mut im);
            for i in 0..n {
                assert!((re[i] - orig_re[i] * n as f64).abs() < 1e-8 * n as f64, "n={n} re {i}");
                assert!((im[i] - orig_im[i] * n as f64).abs() < 1e-8 * n as f64, "n={n} im {i}");
            }
        }
    }
}
