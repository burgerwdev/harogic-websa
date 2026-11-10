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
        if ntaps == 0 || x.is_empty() {
            out.extend_from_slice(x);
            return;
        }
        if ntaps == 1 {
            out.extend(x.iter().map(|sample| sample * self.taps[0]));
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
        #[cfg(target_arch = "x86_64")]
        let body = if n - head >= 4 && std::is_x86_feature_detected!("avx") {
            // Parallel outputs retain each output's original tap order and f64
            // multiply/add operations. There is no horizontal sum or fused FMA.
            unsafe { self.body_avx(x, out, head) }
        } else { head };
        #[cfg(not(target_arch = "x86_64"))]
        let body = head;
        for k in body..n {
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

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx")]
    unsafe fn body_avx(&self, x: &[f64], out: &mut Vec<f64>, mut k: usize) -> usize {
        use std::arch::x86_64::*;
        while k + 4 <= x.len() {
            let mut acc = _mm256_setzero_pd();
            for (j, &tap) in self.taps.iter().enumerate() {
                // k >= ntaps - 1 and k + 3 < x.len(), so all four
                // consecutive input samples are inside the current block.
                let samples = _mm256_loadu_pd(x.as_ptr().add(k - j));
                let product = _mm256_mul_pd(_mm256_set1_pd(tap), samples);
                acc = _mm256_add_pd(acc, product);
            }
            let mut values = [0.0; 4];
            _mm256_storeu_pd(values.as_mut_ptr(), acc);
            out.extend_from_slice(&values);
            k += 4;
        }
        k
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
        [self.i.taps(), self.q.taps()].iter()
            .all(|taps| taps.is_empty() || *taps == [1.0])
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
    #[cfg(feature = "native-fft")]
    fft: Option<super::fir_fft::ComplexFir>,
    re_i: FirState,
    im_q: FirState,
    im_i: FirState,
    re_q: FirState,
    im_zero: bool,
    scratch_a: Vec<f64>,
    scratch_b: Vec<f64>,
    out_a: Vec<f64>,
    out_b: Vec<f64>,
}

impl ComplexBandFilter {
    /// `(re, im)` are the tap components from [`design_complex_bandpass`].
    pub fn new(re: Vec<f64>, im: Vec<f64>) -> Self {
        let im_zero = im.iter().all(|&tap| tap == 0.0);
        Self {
            #[cfg(feature = "native-fft")]
            fft: (!im_zero && re.len() >= 192 && re.len() == im.len())
                .then(|| super::fir_fft::ComplexFir::new(&re,&im)),
            re_i: FirState::new(re.clone()),
            im_q: FirState::new(im.clone()),
            im_i: FirState::new(im),
            re_q: FirState::new(re),
            im_zero: im_zero,
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
            out_a: Vec::new(),
            out_b: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        #[cfg(feature = "native-fft")]
        if let Some(fft) = &mut self.fft { fft.reset(); }
        self.re_i.reset();
        self.im_q.reset();
        self.im_i.reset();
        self.re_q.reset();
    }

    /// Clear the filter history only (a retune drops the old channel's tail).
    pub fn clear_tail(&mut self) {
        #[cfg(feature = "native-fft")]
        if let Some(fft) = &mut self.fft { fft.reset(); }
        self.re_i.clear_tail();
        self.im_q.clear_tail();
        self.im_i.clear_tail();
        self.re_q.clear_tail();
    }

    /// Filter interleaved complex `iq` into interleaved complex `out`.
    pub fn process_complex_into(&mut self, iq: &[f64], out: &mut Vec<f64>) {
        #[cfg(feature = "native-fft")]
        if let Some(fft) = &mut self.fft { fft.process(iq,out); return; }
        let n = iq.len() / 2;
        self.scratch_a.clear();
        self.scratch_a.extend((0..n).map(|k| iq[2 * k]));
        self.scratch_b.clear();
        self.scratch_b.extend((0..n).map(|k| iq[2 * k + 1]));
        // Reuse the scratch slots for the four convolutions through two passes.
        let mut conv_i_re = core::mem::take(&mut self.out_a);
        let mut conv_q_im = core::mem::take(&mut self.out_b);
        self.re_i.process_into(&self.scratch_a, &mut conv_i_re);
        if self.im_zero {
            self.re_q.process_into(&self.scratch_b, &mut conv_q_im);
            out.clear();
            out.reserve(conv_i_re.len() * 2);
            for k in 0..conv_i_re.len() {
                out.push(conv_i_re[k]);
                out.push(conv_q_im[k]);
            }
            self.out_a = conv_i_re;
            self.out_b = conv_q_im;
            return;
        }
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
    fn real_band_uses_two_firs_without_changing_streaming_results() {
        let re = design_lowpass(325_520.0, 90_000.0, 257);
        let mut band = ComplexBandFilter::new(re.clone(), vec![0.0; re.len()]);
        assert!(band.im_zero);
        let mut expected_i = FirState::new(re.clone());
        let mut expected_q = FirState::new(re);
        for (start, len) in [(0, 17), (17, 509), (526, 9)] {
            let i: Vec<f64> = (start..start + len).map(|k| (k as f64 * 0.13).sin()).collect();
            let q: Vec<f64> = (start..start + len).map(|k| (k as f64 * 0.07).cos()).collect();
            let iq: Vec<f64> = i.iter().zip(&q).flat_map(|(&a, &b)| [a, b]).collect();
            let mut actual = Vec::new();
            band.process_complex_into(&iq, &mut actual);
            let mut ei = Vec::new();
            let mut eq = Vec::new();
            expected_i.process_into(&i, &mut ei);
            expected_q.process_into(&q, &mut eq);
            for (k, (&a, &b)) in ei.iter().zip(&eq).enumerate() {
                assert_eq!(actual[2*k], a);
                assert_eq!(actual[2*k+1], b);
            }
        }
        let mut complex = ComplexBandFilter::new(vec![1.0], vec![1.0]);
        assert!(!complex.im_zero);
        let mut out = Vec::new();
        complex.process_complex_into(&[1.0, 2.0], &mut out);
        assert_eq!(out, [-1.0, 3.0]);
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

    #[test]
    fn parallel_outputs_match_scalar_bits_across_short_and_long_blocks() {
        let input: Vec<f64> = (0..4097).map(|k| {
            (k as f64 * 0.371).sin() * 2f64.powi((k % 47) - 23)
        }).collect();
        for ntaps in [2, 3, 9, 33, 129, 257] {
            let taps: Vec<f64> = (0..ntaps).map(|j| (j as f64 * 0.73).cos() / ntaps as f64).collect();
            let expected: Vec<f64> = (0..input.len()).map(|k| {
                let mut acc = 0.0;
                for (j, &tap) in taps.iter().enumerate() {
                    acc += tap * if j <= k { input[k - j] } else { 0.0 };
                }
                acc
            }).collect();
            let mut filter = FirState::new(taps);
            let mut position = 0;
            let mut output = Vec::new();
            for count in [1, 0, 2, 7, 17, 61, 129, 3, 1024].into_iter().cycle() {
                let end = (position + count).min(input.len());
                filter.process_into(&input[position..end], &mut output);
                for (offset, (&actual, &wanted)) in output.iter().zip(&expected[position..end]).enumerate() {
                    assert_eq!(actual.to_bits(), wanted.to_bits(), "taps {ntaps}, sample {}", position + offset);
                }
                position = end;
                if position == input.len() { break; }
            }
        }
    }
}
    #[test]
    fn a_single_tap_multiplies_samples_by_its_coefficient() {
        let input = [2.0, -4.0, 8.0];
        let mut output = Vec::new();
        for coefficient in [0.0, 0.5, -2.0] {
            let mut filter = FirState::new(vec![coefficient]);
            filter.process_into(&input, &mut output);
            let expected: Vec<f64> = input.iter().map(|sample| sample * coefficient).collect();
            assert_eq!(output, expected);
            let mut iq = IqFilter::new(vec![coefficient]);
            assert!(!iq.is_pass_through());
            iq.process_complex_into(&[2.0, -4.0], &mut output);
            assert_eq!(output, [2.0 * coefficient, -4.0 * coefficient]);
        }
        assert!(IqFilter::new(vec![1.0]).is_pass_through());
    }

    #[test]
    fn a_single_complex_tap_obeys_complex_multiplication() {
        let mut filter = ComplexBandFilter::new(vec![0.5], vec![0.25]);
        let mut output = Vec::new();
        filter.process_complex_into(&[2.0, -4.0, -8.0, 4.0], &mut output);
        // (I+jQ)(1/2+j/4) = (I/2-Q/4) + j(I/4+Q/2).
        assert_eq!(output, [2.0, -1.5, -5.0, 0.0]);
    }

#[cfg(all(test,feature="native-fft"))]
mod native_fft_tests {
    use super::*;
    #[test]
    fn complex_fft_preserves_coefficients_boundaries_tail_and_reset() {
        let re:Vec<_>=(0..257).map(|n|(n as f64*0.031).cos()/257.0).collect();
        let im:Vec<_>=(0..257).map(|n|(n as f64*0.079).sin()/257.0).collect();
        let input:Vec<_>=(0..24000).map(|n|(n as f64*0.071).sin()+(n as f64*0.411).cos()).collect();
        let mut reference=ComplexBandFilter::new(re.clone(),im.clone());reference.fft=None;
        let mut expected=Vec::new();reference.process_complex_into(&input,&mut expected);
        for sizes in [&[12000][..],&[0,1,7,169,257,509,3,2048][..]] {
            let mut filter=ComplexBandFilter::new(re.clone(),im.clone());
            assert!(filter.fft.is_some());
            for _ in 0..2 {
                let (mut at,mut turn)=(0,0);let mut actual=Vec::new();let mut block=Vec::new();
                while at<input.len() {
                    let end=(at+2*sizes[turn%sizes.len()]).min(input.len());
                    filter.process_complex_into(&input[at..end],&mut block);
                    assert_eq!(block.len(),end-at);actual.extend_from_slice(&block);at=end;turn+=1;
                }
                let error=actual.iter().zip(&expected).map(|(a,b)|(a-b).abs()).fold(0.0f64,f64::max);
                assert!(error<2e-12,"complex FIR max error {error}");
                filter.process_complex_into(&vec![0.0;1028],&mut block);
                assert!(block[514..].iter().all(|s|s.abs()<2e-12));
                filter.clear_tail();
            }
        }
    }
    #[test]
    #[ignore = "quiet native DSP microbenchmark"]
    fn complex_fft_benchmark() {
        let (re,im)=design_complex_bandpass(325_520.8333333333,0.0,6000.0,257);
        let mut fft=ComplexBandFilter::new(re,im);let mut direct=fft.clone();direct.fft=None;
        let input:Vec<_>=(0..22000).map(|n|(n as f64*0.071).sin()).collect();let mut output=Vec::new();
        for (name,filter) in [("direct",&mut direct),("fft",&mut fft)] {
            let start=std::time::Instant::now();
            for _ in 0..100 {filter.process_complex_into(std::hint::black_box(&input),&mut output);std::hint::black_box(&output);}
            eprintln!("complex 257 tap / 11000 samples {name}: {:.3} ms/block",start.elapsed().as_secs_f64()*10.0);
        }
    }
}
