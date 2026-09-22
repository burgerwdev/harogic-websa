//! Radix-2 FFT with precomputed twiddles and bit-reversal — the transform the STFT stages share.
//!
//! No dependency, so this is written out: an iterative Cooley-Tukey with a cached twiddle table and
//! a cached bit-reversal permutation per size. Real input is transformed by zeroing the imaginary
//! part, which costs 2x a split-radix real transform; the stages that use it run at audio rate
//! (48 kHz) with 512-point frames, so the budget leaves room for that, and the benchmark in
//! `wasm/tests/audio_bench.rs` is what would catch it if that stopped being true.
//!
//! Everything is f64: the stages feed each other and a denoiser's gain is applied per bin, so the
//! precision is worth more than the (unmeasured) speed of f32 here.

use core::f64::consts::PI;
use std::collections::HashMap;

/// A forward/inverse transform for one size. Reusable: `plan()` keeps the tables, `forward()`
/// applies them.
pub struct Fft {
    n: usize,
    /// Twiddles for the forward transform: `tw[k] = exp(-2*pi*i*k/n)`.
    tw_re: Vec<f64>,
    tw_im: Vec<f64>,
    /// Bit-reversal permutation (bit-reversed index for each position).
    rev: Vec<usize>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two(), "FFT size must be a power of two, got {n}");
        let mut rev = vec![0_usize; n];
        let bits = n.trailing_zeros();
        for (index, slot) in rev.iter_mut().enumerate() {
            *slot = index.reverse_bits() >> (usize::BITS - bits);
        }
        let mut tw_re = vec![0.0; n / 2];
        let mut tw_im = vec![0.0; n / 2];
        for (k, (re, im)) in tw_re.iter_mut().zip(tw_im.iter_mut()).enumerate() {
            let angle = -2.0 * PI * k as f64 / n as f64;
            *re = angle.cos();
            *im = angle.sin();
        }
        Self { n, tw_re, tw_im, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// In-place complex transform. `inverse` conjugates the twiddles and scales by 1/n.
    pub fn transform(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.n;
        assert!(re.len() == n && im.len() == n, "buffers must match the plan");
        for index in 0..n {
            let j = self.rev[index];
            if index < j {
                re.swap(index, j);
                im.swap(index, j);
            }
        }
        let sign = if inverse { -1.0 } else { 1.0 };
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                let mut k = 0;
                for pair in 0..half {
                    let t = pair * step;
                    let (wr, wi) = (self.tw_re[t], sign * self.tw_im[t]);
                    let a = start + pair;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                    k += 1;
                }
                let _ = k;
                start += len;
            }
            len *= 2;
        }
        if inverse {
            let scale = 1.0 / n as f64;
            for index in 0..n {
                re[index] *= scale;
                im[index] *= scale;
            }
        }
    }

    /// Forward transform of a real block (imaginary input is ignored).
    pub fn forward_real(&self, input: &[f64], re: &mut [f64], im: &mut [f64]) {
        re[..self.n].copy_from_slice(&input[..self.n]);
        for value in im[..self.n].iter_mut() {
            *value = 0.0;
        }
        self.transform(re, im, false);
    }

    /// Inverse transform keeping the real part (the imaginary part is discarded).
    pub fn inverse_real_into(&self, re: &mut [f64], im: &mut [f64], out: &mut [f64]) {
        self.transform(re, im, true);
        out[..self.n].copy_from_slice(&re[..self.n]);
    }
}

/// Bluestein (chirp-z) DFT for an arbitrary size `n`, computed with a power-of-two `Fft` of size
/// `m = next_pow2(2n-1)`.
///
/// The FT8 waterfall needs an FFT whose size is `block * freq_osr` (e.g. 7680 * 2 = 15360 at
/// 48 kHz), which is not a power of two but whose bin spacing must match the 6.25 Hz tone grid
/// *exactly* — zero-padding to the next power of two and rounding to a bin was tried and lost the
/// weak-signal margin. The kernel's FFT is precomputed once, so one transform is two FFTs plus two
/// chirp multiplies.
pub struct Bluestein {
    n: usize,
    fft: Fft,
    /// `chirp[k] = exp(-pi i k^2 / n)` for `k` in `0..n`.
    chirp_re: Vec<f64>,
    chirp_im: Vec<f64>,
    /// Precomputed FFT of the kernel `b[j] = conj(chirp[j])`, padded for the linear convolution.
    b_re: Vec<f64>,
    b_im: Vec<f64>,
    /// Reusable scratch (length `m`).
    scratch_re: Vec<f64>,
    scratch_im: Vec<f64>,
}

impl Bluestein {
    pub fn new(n: usize) -> Self {
        assert!(n > 0, "DFT size must be positive");
        let m = (2 * n - 1).next_power_of_two();
        let fft = Fft::new(m);
        let mut chirp_re = vec![0.0; m];
        let mut chirp_im = vec![0.0; m];
        let mut b_re = vec![0.0; m];
        let mut b_im = vec![0.0; m];
        for k in 0..n {
            // angle = -pi * (k^2 mod 2n) / n  ->  exp(-pi i k^2 / n)
            let k2 = ((k * k) % (2 * n)) as f64;
            let angle = -PI * k2 / n as f64;
            let (c_re, c_im) = (angle.cos(), angle.sin());
            chirp_re[k] = c_re;
            chirp_im[k] = c_im;
            // kernel b = conj(chirp), and b is symmetric so b[M-k] = b[k] for k > 0.
            b_re[k] = c_re;
            b_im[k] = -c_im;
            if k > 0 {
                b_re[m - k] = c_re;
                b_im[m - k] = -c_im;
            }
        }
        fft.transform(&mut b_re, &mut b_im, false);
        Self {
            n,
            fft,
            chirp_re,
            chirp_im,
            b_re,
            b_im,
            scratch_re: vec![0.0; m],
            scratch_im: vec![0.0; m],
        }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// In-place forward complex DFT: `re`/`im` (length `n`) are overwritten with the spectrum.
    pub fn forward(&mut self, re: &mut [f64], im: &mut [f64]) {
        let n = self.n;
        let m = self.fft.len();
        assert!(re.len() == n && im.len() == n, "buffers must match the plan");
        self.scratch_re.fill(0.0);
        self.scratch_im.fill(0.0);
        // a[k] = x[k] * chirp[k], zero-padded to m.
        for k in 0..n {
            let (xr, xi) = (re[k], im[k]);
            let (cr, ci) = (self.chirp_re[k], self.chirp_im[k]);
            self.scratch_re[k] = xr * cr - xi * ci;
            self.scratch_im[k] = xr * ci + xi * cr;
        }
        self.fft.transform(&mut self.scratch_re, &mut self.scratch_im, false);
        // Pointwise multiply by the kernel's FFT.
        for k in 0..m {
            let (ar, ai) = (self.scratch_re[k], self.scratch_im[k]);
            let (br, bi) = (self.b_re[k], self.b_im[k]);
            self.scratch_re[k] = ar * br - ai * bi;
            self.scratch_im[k] = ar * bi + ai * br;
        }
        self.fft.transform(&mut self.scratch_re, &mut self.scratch_im, true);
        // X[k] = chirp[k] * c[k] for k in 0..n.
        for k in 0..n {
            let (cr, ci) = (self.scratch_re[k], self.scratch_im[k]);
            let (wr, wi) = (self.chirp_re[k], self.chirp_im[k]);
            re[k] = cr * wr - ci * wi;
            im[k] = cr * wi + ci * wr;
        }
    }
}

/// Cache of plans by size: building a plan is O(n) and the stages keep their size for a session.
pub struct FftCache {
    plans: HashMap<usize, Fft>,
}

impl Default for FftCache {
    fn default() -> Self {
        Self { plans: HashMap::new() }
    }
}

impl FftCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&mut self, n: usize) -> &Fft {
        self.plans.entry(n).or_insert_with(|| Fft::new(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft(input: &[f64], k: usize) -> (f64, f64) {
        let n = input.len();
        let mut re = 0.0;
        let mut im = 0.0;
        for (index, value) in input.iter().enumerate() {
            let angle = -2.0 * PI * k as f64 * index as f64 / n as f64;
            re += value * angle.cos();
            im += value * angle.sin();
        }
        (re, im)
    }

    #[test]
    fn a_tone_lands_in_the_expected_bin() {
        let n = 64;
        let bin = 7;
        let fft = Fft::new(n);
        let input: Vec<f64> = (0..n)
            .map(|k| (2.0 * PI * bin as f64 * k as f64 / n as f64).cos())
            .collect();
        let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
        fft.forward_real(&input, &mut re, &mut im);
        let magnitude: Vec<f64> = (0..n).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let peak = magnitude
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(index, _)| index)
            .unwrap();
        assert!(peak == bin || peak == n - bin, "peak at {peak}, expected {bin}");
        assert!((magnitude[bin] - n as f64 / 2.0).abs() < 1e-9, "amplitude {}", magnitude[bin]);
    }

    #[test]
    fn the_transform_matches_a_naive_dft() {
        let n = 32;
        let fft = Fft::new(n);
        let input: Vec<f64> = (0..n).map(|k| ((k * 37 % 11) as f64) - 5.0).collect();
        let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
        fft.forward_real(&input, &mut re, &mut im);
        for k in 0..n {
            let (want_re, want_im) = naive_dft(&input, k);
            assert!((re[k] - want_re).abs() < 1e-9, "bin {k} re {} vs {want_re}", re[k]);
            assert!((im[k] - want_im).abs() < 1e-9, "bin {k} im {} vs {want_im}", im[k]);
        }
    }

    #[test]
    fn the_inverse_round_trips() {
        let n = 128;
        let fft = Fft::new(n);
        let input: Vec<f64> = (0..n).map(|k| (k as f64 * 0.13).sin() + 0.5).collect();
        let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
        let mut out = vec![0.0; n];
        fft.forward_real(&input, &mut re, &mut im);
        fft.inverse_real_into(&mut re, &mut im, &mut out);
        for index in 0..n {
            assert!((out[index] - input[index]).abs() < 1e-12, "sample {index}");
        }
    }

    #[test]
    fn the_cache_reuses_plans() {
        let mut cache = FftCache::new();
        assert_eq!(cache.get(64).len(), 64);
        assert_eq!(cache.get(64).len(), 64);
        assert_eq!(cache.get(512).len(), 512);
    }

    #[test]
    fn bluestein_matches_a_naive_dft_at_a_non_power_of_two_size() {
        let n = 12; // 3 * 4, deliberately not a power of two
        let mut plan = Bluestein::new(n);
        let input: Vec<f64> = (0..n).map(|k| ((k * 37 % 11) as f64) - 5.0).collect();
        let (mut re, mut im) = (input.clone(), vec![0.0; n]);
        plan.forward(&mut re, &mut im);
        for k in 0..n {
            let (want_re, want_im) = naive_dft(&input, k);
            assert!((re[k] - want_re).abs() < 1e-9, "bin {k} re {} vs {want_re}", re[k]);
            assert!((im[k] - want_im).abs() < 1e-9, "bin {k} im {} vs {want_im}", im[k]);
        }
    }

    #[test]
    fn bluestein_matches_a_complex_naive_dft() {
        let n = 20; // 4 * 5, non-pow2, with a complex input
        let mut plan = Bluestein::new(n);
        let mut re: Vec<f64> = (0..n).map(|k| (k as f64 * 0.31).sin()).collect();
        let mut im: Vec<f64> = (0..n).map(|k| (k as f64 * 0.17).cos()).collect();
        let (want_re, want_im) = (re.clone(), im.clone());
        plan.forward(&mut re, &mut im);
        for k in 0..n {
            let (mut wr, mut wi) = (0.0, 0.0);
            for (idx, (&xr, &xi)) in want_re.iter().zip(want_im.iter()).enumerate() {
                let angle = -2.0 * PI * k as f64 * idx as f64 / n as f64;
                let (c, s) = (angle.cos(), angle.sin());
                wr += xr * c - xi * s;
                wi += xr * s + xi * c;
            }
            assert!((re[k] - wr).abs() < 1e-9, "bin {k} re {} vs {wr}", re[k]);
            assert!((im[k] - wi).abs() < 1e-9, "bin {k} im {} vs {wi}", im[k]);
        }
    }
}
