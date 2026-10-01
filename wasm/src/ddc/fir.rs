//! Windowed-sinc FIR design and a streaming block filter.
//!
//! Reference: `web_sa/demod/filters.py` (`design_lowpass`, `design_complex_bandpass`,
//! `StreamFilter`). The formulas, the tap rounding to f32 and the overlap tail are all kept
//! identical, because the whole point of this port is that the audio does not change when the
//! DSP moves into the browser.
//!
//! Taps are f64 internally but rounded through f32 exactly like the reference (numpy stores
//! `h.astype(np.float32)`), so a comparison against the Python golden files is a comparison of
//! the same numbers, not of two nearly-equal filters.

use core::f64::consts::PI;

/// `np.sinc`: sin(pi*x)/(pi*x), with 1.0 at 0 (the normalized, not the unnormalized, sinc).
#[inline]
pub fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let px = PI * x;
    px.sin() / px
}

/// `np.hamming` (symmetric): 0.54 - 0.46*cos(2*pi*n/(m-1)).
#[inline]
pub fn hamming(n: usize, m: usize) -> f64 {
    if m <= 1 {
        return 1.0;
    }
    0.54 - 0.46 * ((2.0 * PI * n as f64) / (m as f64 - 1.0)).cos()
}

/// Windowed-sinc real low-pass with unit DC gain (`design_lowpass`).
pub fn design_lowpass(fs: f64, cutoff_hz: f64, ntaps: usize) -> Vec<f64> {
    let cutoff = cutoff_hz.abs().min(0.499 * fs);
    if cutoff <= 0.0 {
        return vec![1.0];
    }
    let m = ntaps | 1;
    let mid = (m as f64 - 1.0) / 2.0;
    let mut h: Vec<f64> = (0..m)
        .map(|k| {
            let n = k as f64 - mid;
            (2.0 * cutoff / fs) * sinc(2.0 * cutoff / fs * n) * hamming(k, m)
        })
        .collect();
    let sum: f64 = h.iter().sum();
    if sum != 0.0 {
        for tap in h.iter_mut() {
            *tap /= sum;
        }
    }
    round_to_f32(h)
}

/// Complex band-pass built from a real low-pass shifted to the band centre
/// (`design_complex_bandpass`). Returned as `(re, im)` tap pairs.
pub fn design_complex_bandpass(fs: f64, f_lo: f64, f_hi: f64, ntaps: usize) -> (Vec<f64>, Vec<f64>) {
    let (lo, hi) = if f_hi < f_lo { (f_hi, f_lo) } else { (f_lo, f_hi) };
    let bw = (hi - lo).max(1.0);
    let f0 = (hi + lo) / 2.0;
    let m = ntaps | 1;
    let mid = (m as f64 - 1.0) / 2.0;
    let mut re = Vec::with_capacity(m);
    let mut im = Vec::with_capacity(m);
    for k in 0..m {
        let n = k as f64 - mid;
        let lp = (bw / fs) * sinc((bw / fs) * n) * hamming(k, m);
        let ph = 2.0 * PI * f0 / fs * n;
        re.push(lp * ph.cos());
        im.push(lp * ph.sin());
    }
    let mag: f64 = re
        .iter()
        .zip(im.iter())
        .map(|(r, i)| (r * r + i * i).sqrt())
        .sum();
    if mag > 0.0 {
        for tap in re.iter_mut() {
            *tap /= mag;
        }
        for tap in im.iter_mut() {
            *tap /= mag;
        }
    }
    (round_to_f32(re), round_to_f32(im))
}

fn round_to_f32(values: Vec<f64>) -> Vec<f64> {
    values.into_iter().map(|v| v as f32 as f64).collect()
}

/// Stateful FIR with the overlap tail between blocks (`StreamFilter`).
#[derive(Debug, Clone)]
pub struct FirState {
    taps: Vec<f64>,
    tail: Vec<f64>,
}

impl FirState {
    pub fn new(taps: Vec<f64>) -> Self {
        let tail = vec![0.0; taps.len().saturating_sub(1)];
        Self { taps, tail }
    }

    pub fn reset(&mut self) {
        for value in self.tail.iter_mut() {
            *value = 0.0;
        }
    }

    pub fn taps(&self) -> &[f64] {
        &self.taps
    }

    /// Clear the filter history only (a retune keeps the taps, drops the old channel's tail).
    pub fn clear_tail(&mut self) {
        self.reset();
    }

    /// `np.convolve([tail, x], taps, mode='valid')`, writing into `out` (cleared first).
    ///
    /// Split into a head (where the previous block's tail is still in the window) and a body
    /// (a plain sliding window over `x`). The split is what keeps the hot loop free of a
    /// per-tap branch and of bounds checks — this is the DDC's most expensive stage.
    pub fn process_into(&mut self, x: &[f64], out: &mut Vec<f64>) {
        out.clear();
        let ntaps = self.taps.len();
        if ntaps <= 1 || x.is_empty() {
            out.extend_from_slice(x);
            return;
        }
        let tlen = ntaps - 1;
        out.reserve(x.len());
        let n = x.len();
        let head = tlen.min(n);
        for k in 0..head {
            let mut acc = 0.0;
            for j in 0..=k {
                acc += self.taps[j] * x[k - j];
            }
            for j in (k + 1)..ntaps {
                acc += self.taps[j] * self.tail[tlen + k - j];
            }
            out.push(acc);
        }
        for k in head..n {
            let window = &x[k + 1 - ntaps..=k];
            let mut acc = 0.0;
            for (tap, sample) in self.taps.iter().zip(window.iter().rev()) {
                acc += tap * sample;
            }
            out.push(acc);
        }
        // New tail = the last (ntaps-1) samples of [tail, x].
        if n >= tlen {
            self.tail.copy_from_slice(&x[n - tlen..]);
        } else {
            self.tail.copy_within(n.., 0);
            self.tail[tlen - n..].copy_from_slice(x);
        }
    }
}

/// An I/Q filter with one real tap set applied to both components.
///
/// A real low-pass on a complex signal is exactly `conv(I, h)` and `conv(Q, h)` with the same
/// taps: the histories are separate, the taps are shared. (`design_complex_bandpass` returns
/// complex taps for the demodulators' sideband selection; those are convolved in the
/// demodulator, not here — this stage is the DDC's anti-alias filter.)
#[derive(Debug, Clone)]
pub struct IqFilter {
    pub i: FirState,
    pub q: FirState,
    scratch_i: Vec<f64>,
    scratch_q: Vec<f64>,
    out_i: Vec<f64>,
    out_q: Vec<f64>,
}

impl IqFilter {
    pub fn new(taps: Vec<f64>) -> Self {
        Self {
            i: FirState::new(taps.clone()),
            q: FirState::new(taps),
            scratch_i: Vec::new(),
            scratch_q: Vec::new(),
            out_i: Vec::new(),
            out_q: Vec::new(),
        }
    }

    pub fn is_pass_through(&self) -> bool {
        self.i.taps().len() <= 1
    }

    pub fn reset(&mut self) {
        self.i.reset();
        self.q.reset();
    }

    /// Clear the history only (a retune drops the old channel's tail but keeps the taps).
    pub fn clear_tail(&mut self) {
        self.i.clear_tail();
        self.q.clear_tail();
    }

    /// Filter interleaved complex `iq` into interleaved complex `out`.
    pub fn process_complex_into(&mut self, iq: &[f64], out: &mut Vec<f64>) {
        let n = iq.len() / 2;
        self.scratch_i.clear();
        self.scratch_i.extend((0..n).map(|k| iq[2 * k]));
        self.scratch_q.clear();
        self.scratch_q.extend((0..n).map(|k| iq[2 * k + 1]));
        self.i.process_into(&self.scratch_i, &mut self.out_i);
        self.q.process_into(&self.scratch_q, &mut self.out_q);
        out.clear();
        out.reserve(self.out_i.len() * 2);
        for k in 0..self.out_i.len() {
            out.push(self.out_i[k]);
            out.push(self.out_q.get(k).copied().unwrap_or(0.0));
        }
    }
}

/// A complex-tap FIR (sideband selection): `out = z * h` with `h = re + j*im`.
///
/// Unlike [`IqFilter`], the taps are complex, so the two output components mix the two input
/// components:
///
/// ```text
///   out_i = conv(i, re) - conv(q, im)
///   out_q = conv(i, im) + conv(q, re)
/// ```
///
/// Four histories, one tap set: this is what makes USB/LSB selection and any asymmetric band
/// possible.
#[derive(Debug, Clone)]
pub struct ComplexBandFilter {
    re_i: FirState,
    im_q: FirState,
    im_i: FirState,
    re_q: FirState,
    scratch_a: Vec<f64>,
    scratch_b: Vec<f64>,
    out_a: Vec<f64>,
    out_b: Vec<f64>,
}

impl ComplexBandFilter {
    /// `(re, im)` are the tap components from [`design_complex_bandpass`].
    pub fn new(re: Vec<f64>, im: Vec<f64>) -> Self {
        Self {
            re_i: FirState::new(re.clone()),
            im_q: FirState::new(im.clone()),
            im_i: FirState::new(im),
            re_q: FirState::new(re),
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
            out_a: Vec::new(),
            out_b: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.re_i.reset();
        self.im_q.reset();
        self.im_i.reset();
        self.re_q.reset();
    }

    /// Clear the filter history only (a retune drops the old channel's tail).
    pub fn clear_tail(&mut self) {
        self.re_i.clear_tail();
        self.im_q.clear_tail();
        self.im_i.clear_tail();
        self.re_q.clear_tail();
    }

    /// Filter interleaved complex `iq` into interleaved complex `out`.
    pub fn process_complex_into(&mut self, iq: &[f64], out: &mut Vec<f64>) {
        let n = iq.len() / 2;
        self.scratch_a.clear();
        self.scratch_a.extend((0..n).map(|k| iq[2 * k]));
        self.scratch_b.clear();
        self.scratch_b.extend((0..n).map(|k| iq[2 * k + 1]));
        // Reuse the scratch slots for the four convolutions through two passes.
        let mut conv_i_re = core::mem::take(&mut self.out_a);
        let mut conv_q_im = core::mem::take(&mut self.out_b);
        self.re_i.process_into(&self.scratch_a, &mut conv_i_re);
        self.im_q.process_into(&self.scratch_b, &mut conv_q_im);
        let mut conv_i_im = Vec::with_capacity(n);
        let mut conv_q_re = Vec::with_capacity(n);
        self.im_i.process_into(&self.scratch_a, &mut conv_i_im);
        self.re_q.process_into(&self.scratch_b, &mut conv_q_re);

        out.clear();
        out.reserve(conv_i_re.len() * 2);
        for k in 0..conv_i_re.len() {
            out.push(conv_i_re[k] - conv_q_im.get(k).copied().unwrap_or(0.0));
            out.push(conv_i_im.get(k).copied().unwrap_or(0.0) + conv_q_re.get(k).copied().unwrap_or(0.0));
        }
        self.out_a = conv_i_re;
        self.out_b = conv_q_im;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinc_matches_its_definition() {
        assert_eq!(sinc(0.0), 1.0);
        assert!((sinc(0.5) - 2.0 / PI).abs() < 1e-12);
        assert!(sinc(1.0).abs() < 1e-12);
    }

    #[test]
    fn lowpass_has_unit_dc_gain_and_an_odd_length() {
        let h = design_lowpass(1.0e6, 40_000.0, 128);
        assert_eq!(h.len(), 129);                       // ntaps | 1
        let sum: f64 = h.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6, "DC gain {sum}");
    }

    #[test]
    fn a_zero_cutoff_is_a_passthrough() {
        assert_eq!(design_lowpass(1.0e6, 0.0, 129), vec![1.0]);
    }

    #[test]
    fn the_filter_tail_keeps_blocks_continuous() {
        // Filtering a block stream in two pieces must equal filtering it in one piece.
        let h = design_lowpass(48_000.0, 8_000.0, 33);
        let x: Vec<f64> = (0..64).map(|k| (k as f64 * 0.37).sin()).collect();
        let mut whole = FirState::new(h.clone());
        let mut one = Vec::new();
        whole.process_into(&x, &mut one);

        let mut split = FirState::new(h);
        let mut a = Vec::new();
        let mut b = Vec::new();
        split.process_into(&x[..32], &mut a);
        split.process_into(&x[32..], &mut b);
        a.extend_from_slice(&b);
        assert_eq!(a.len(), one.len());
        for (i, (u, v)) in one.iter().zip(a.iter()).enumerate() {
            assert!((u - v).abs() < 1e-12, "sample {i}: {u} vs {v}");
        }
    }

    #[test]
    fn reset_drops_the_history() {
        let h = design_lowpass(48_000.0, 8_000.0, 9);
        let mut filter = FirState::new(h);
        let mut out = Vec::new();
        filter.process_into(&[1.0, 0.0, 0.0, 0.0, 0.0], &mut out);
        filter.reset();
        let mut again = Vec::new();
        filter.process_into(&[1.0, 0.0, 0.0, 0.0, 0.0], &mut again);
        assert_eq!(out, again);
    }
}
