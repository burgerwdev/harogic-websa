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
}
