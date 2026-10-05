//! Power-delay-spectrum based tracking (port of Dream's `CTimeSyncTrack` / DecDRM's
//! `track.rs`): timing tracking from the estimated impulse response (the "energy" method),
//! sample-rate-offset estimation from the drift of the strongest path, and the delay-spread
//! estimate the frequency-direction Wiener filter is adapted with.
//!
//! The averaged power delay profile is the IFFT of the Hamming-windowed channel estimate on
//! the `num_pil` gain-reference grid carriers; one impulse-response sample lasts
//! `fft_len / (x · num_pil)` input samples. The constants are cross-checked against Dream's
//! `CTrack`/`CTimeSyncTrack` and DecDRM's `track.rs`, which agree with each other.

use std::collections::VecDeque;

use crate::digital::drm::cellmap::CellMap;
use crate::digital::drm::dsp::util::{hamming, iir1_lambda};
use crate::digital::drm::dsp::{Cplx, FullFft};

const TICONST_PDS: f64 = 0.25;
const CONT_PROP_ENERGY: f64 = 0.02;
const NUM_SAM_IR_FOR_MIN_STAT: usize = 10;
const OVER_EST_FACT_MIN_STAT: f64 = 4.0;
/// Drift history for SRO tracking, s.
const HIST_LEN_SAM_OFF_S: f64 = 30.0;
/// SRO acquisition: the first estimate after this long.
const SAM_OFF_ACQ_LEN_S: f64 = 4.0;
/// The first part of the acquisition window is left out (the averaged PDS still builds up).
const SAM_OFF_ACQ_SETTLE_S: f64 = 1.0;
/// Time between PDS snapshots for the drift measurement, s.
const SRO_STEP_S: f64 = 0.1;
/// Largest drift searched for between snapshots, IR bins.
const SRO_MAX_STEP_BINS: usize = 3;
/// Tracking: fraction of the residual offset corrected per second.
const SRO_TRACK_RATE: f64 = 0.1;
/// Tracking starts with this much drift history, s.
const SRO_TRACK_MIN_S: f64 = 5.0;

/// Outputs of one tracking step.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackOutput {
    /// Timing correction to apply to the FFT window, in samples (positive = later).
    pub timing_adjust: i64,
    /// Correction to add to the sample-rate offset, in Hz.
    pub sro_delta_hz: f64,
    /// Delay-spread length and start (in impulse-response samples).
    pub pds_len: f64,
    pub pds_offset: f64,
}

/// Impulse-response tracker: delay spread, timing drift and sample-rate offset.
pub struct PdsTracker {
    /// Impulse-response samples per input sample.
    ir_per_sample: f64,
    num_pil: usize,
    ti_corr_hist: VecDeque<i64>,
    new_meas_hist: VecDeque<i64>,
    fft: FullFft,
    window: Vec<f64>,
    work: Vec<Cplx>,
    avg_pds: Vec<f64>,
    rotated: Vec<f64>,
    scratch: Vec<f64>,
    fft_a_re: Vec<f64>,
    fft_a_im: Vec<f64>,
    fft_b_re: Vec<f64>,
    fft_b_im: Vec<f64>,
    lambda: f64,
    guard_ir: f64,
    st_po_rot: usize,
    frac_contr: f64,
    pub tracking: bool,
    pub sro_acquisition: bool,
    // Sample-rate-offset estimation.
    fs: f64,
    sym_rate: f64,
    acq_cnt_max: usize,
    acq_settle: usize,
    acq_cnt: usize,
    /// Symbols between PDS snapshots, and symbols since the last one.
    xc_period: usize,
    xc_count: usize,
    /// Last snapshot: the rotated averaged PDS and the position of its frame.
    xc_ref: Option<(Vec<f64>, f64)>,
    /// Drift between successive snapshots (IR bins) and the cumulative SRO correction when
    /// it was measured (Hz), newest last.
    drift: VecDeque<(f64, f64)>,
    drift_max: usize,
    /// SRO corrections emitted since the drift history started, Hz.
    applied_hz: f64,
    /// Timing corrections followed by the averaged PDS so far (IR bins).
    frame_pos: f64,
    sym_len_ir: f64,
    /// Duration of one impulse-response sample, ms.
    ir_step_ms: f64,
    // Delay-spread estimate.
    pub pds_begin: f64,
    pub pds_end: f64,
}

impl PdsTracker {
    /// `sym_delay` = channel-estimation delay + 1 (Dream's `iLenHistBuff`).
    pub fn new(map: &CellMap, num_pil: usize, sym_delay: usize) -> Self {
        let mode = map.mode();
        let fft_len = mode.fft_size();
        let fs = f64::from(mode.sample_rate());
        let ir_per_sample = (num_pil * map.scattered.freq_int) as f64 / fft_len as f64;
        let guard_ir = mode.guard_len() as f64 * ir_per_sample;
        let st_po_rot = if guard_ir as usize > num_pil {
            num_pil
        } else {
            (guard_ir + ((num_pil as f64 - guard_ir) / 2.0).ceil() + 1.0) as usize
        };
        let sym_rate = fs / mode.symbol_len() as f64;
        let acq_cnt_max = (SAM_OFF_ACQ_LEN_S * sym_rate) as usize;
        let xc_period = ((SRO_STEP_S * sym_rate).round() as usize).max(1);
        let fft = FullFft::new(num_pil);
        Self {
            ir_per_sample,
            num_pil,
            ti_corr_hist: VecDeque::from(vec![0i64; sym_delay]),
            new_meas_hist: VecDeque::from(vec![0i64; sym_delay.saturating_sub(1)]),
            fft,
            window: hamming(num_pil),
            work: vec![Cplx::zero(); num_pil],
            avg_pds: vec![0.0; num_pil],
            rotated: vec![0.0; num_pil],
            scratch: vec![0.0; num_pil],
            fft_a_re: vec![0.0; num_pil],
            fft_a_im: vec![0.0; num_pil],
            fft_b_re: vec![0.0; num_pil],
            fft_b_im: vec![0.0; num_pil],
            lambda: iir1_lambda(TICONST_PDS, sym_rate),
            guard_ir,
            st_po_rot,
            frac_contr: 0.0,
            tracking: false,
            sro_acquisition: true,
            fs,
            sym_rate,
            acq_cnt_max,
            acq_settle: (SAM_OFF_ACQ_SETTLE_S * sym_rate) as usize,
            acq_cnt: acq_cnt_max,
            xc_period,
            xc_count: 0,
            xc_ref: None,
            drift: VecDeque::new(),
            drift_max: ((HIST_LEN_SAM_OFF_S * sym_rate) as usize / xc_period).max(1),
            applied_hz: 0.0,
            frame_pos: 0.0,
            sym_len_ir: mode.symbol_len() as f64 * ir_per_sample,
            ir_step_ms: 1e3 / (ir_per_sample * fs),
            pds_begin: 0.0,
            pds_end: guard_ir,
        }
    }

    /// `chan` holds the time-interpolated channel at the `num_pil` grid carriers;
    /// `input_shift` is the timing shift of the current input symbol (Dream sign: `−shift`),
    /// which reaches the delayed estimate `sym_delay` symbols later.
    pub fn process(&mut self, chan: &[Cplx], input_shift: i64) -> TrackOutput {
        let p = self.num_pil;
        let mut out = TrackOutput::default();

        // Follow timing shifts: rotate the averaged PDS.
        self.ti_corr_hist.pop_front();
        self.ti_corr_hist.push_back(-input_shift);
        let oldest = *self.ti_corr_hist.front().unwrap_or(&0);
        let shift = -(oldest as f64) * self.ir_per_sample;
        if shift != 0.0 && shift.abs() < p as f64 {
            rotate_left_frac(&mut self.avg_pds, shift, &mut self.scratch);
            self.frame_pos += shift;
        }

        // New PDS estimate: IFFT of the Hamming-windowed channel.
        for i in 0..p {
            self.work[i] = chan[i] * self.window[i];
        }
        cplx_to_reim(&self.work, &mut self.fft_a_re, &mut self.fft_a_im);
        self.fft.inverse_re_im(&mut self.fft_a_re, &mut self.fft_a_im);
        reim_to_cplx(&self.fft_a_re, &self.fft_a_im, &mut self.work);
        let norm = 1.0 / (p as f64 * p as f64);
        for i in 0..p {
            let v = self.work[i].norm_sqr() * norm;
            self.avg_pds[i] = self.lambda * (self.avg_pds[i] - v) + v;
        }
        let rot_start = self.st_po_rot - 1;
        for i in 0..p {
            self.rotated[i] = self.avg_pds[(rot_start + i) % p];
        }

        // Energy method: window of one guard interval with maximum energy.
        let g = self.guard_ir;
        let gi = g as usize;
        let mut first_path = 0usize;
        let mut best = 0.0;
        let limit = (p as f64 - 1.0 - g).max(0.0) as usize;
        for i in 0..limit {
            let e: f64 = self.rotated[i..(i + gi).min(p)].iter().sum();
            if e > best {
                best = e;
                first_path = i;
            }
        }

        if self.tracking {
            let delay = first_path as i64 + self.st_po_rot as i64 - p as i64 - 1;
            let ti_offset = -(delay as f64) / self.ir_per_sample
                - *self.new_meas_hist.front().unwrap_or(&0) as f64;
            let mut gain = CONT_PROP_ENERGY;
            if self.sro_acquisition {
                gain *= 2.0;
            }
            let cur = ti_offset * gain + self.frac_contr;
            let contr = cur.trunc() as i64;
            self.frac_contr = cur - contr as f64;
            // Corrections applied in the last `sym_delay − 1` symbols are not yet visible in
            // the delayed estimate; the front holds their sum.
            if !self.new_meas_hist.is_empty() {
                self.new_meas_hist.pop_front();
                self.new_meas_hist.push_back(0);
                for v in &mut self.new_meas_hist {
                    *v += contr;
                }
            }
            out.timing_adjust = -contr;
        }

        // Sample-rate offset from the drift of the impulse response.
        let frame_pos = self.frame_pos;
        if self.acq_cnt > 0 {
            self.acq_cnt -= 1;
        }
        self.xc_count += 1;
        if self.xc_count >= self.xc_period {
            self.xc_count = 0;
            match &mut self.xc_ref {
                Some((prev, prev_pos)) => {
                    let s = profile_shift(
                        prev,
                        &self.rotated,
                        SRO_MAX_STEP_BINS,
                        &mut self.fft,
                        &mut self.fft_a_re,
                        &mut self.fft_a_im,
                        &mut self.fft_b_re,
                        &mut self.fft_b_im,
                    );
                    self.drift.push_back((s + frame_pos - *prev_pos, self.applied_hz));
                    if self.drift.len() > self.drift_max {
                        self.drift.pop_front();
                    }
                    prev.copy_from_slice(&self.rotated);
                    *prev_pos = frame_pos;
                }
                None => self.xc_ref = Some((self.rotated.clone(), frame_pos)),
            }
            out.sro_delta_hz = self.sro_step();
        }

        // Delay spread from noise-corrected cumulative energy.
        let tot: f64 = self.rotated.iter().sum();
        let mut sorted = self.rotated.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let k = NUM_SAM_IR_FOR_MIN_STAT.min(p);
        let sigma_noise = sorted[..k.saturating_sub(1)].iter().sum::<f64>()
            / NUM_SAM_IR_FOR_MIN_STAT as f64
            * OVER_EST_FACT_MIN_STAT;
        let sig_bound = (tot - sigma_noise * p as f64).max(0.0);
        let mut end = (p - 1) as f64;
        let mut acc = 0.0;
        for (i, v) in self.rotated.iter().enumerate() {
            if acc > sig_bound {
                end = i as f64;
                break;
            }
            acc += v - sigma_noise;
        }
        let mut begin = 0.0;
        acc = 0.0;
        for i in (0..p).rev() {
            if acc > sig_bound {
                begin = i as f64;
                break;
            }
            acc += self.rotated[i] - sigma_noise;
        }
        if begin > end {
            begin = 0.0;
            end = (p - 1) as f64;
        }
        let corr = p as f64 - self.st_po_rot as f64 + 1.0;
        self.pds_begin = begin - corr;
        self.pds_end = end - corr;
        out.pds_len = self.pds_end - self.pds_begin;
        out.pds_offset = self.pds_begin;
        out
    }

    /// The averaged power delay profile ordered by delay (linear power) and its delay axis,
    /// in (value, delay ms) pairs.
    pub fn pds_view(&self) -> Vec<(f64, f64)> {
        let p = self.num_pil;
        let rot_start = self.st_po_rot - 1;
        (0..p)
            .map(|i| {
                let v = self.avg_pds[(rot_start + i) % p];
                let delay_ms = (rot_start as f64 - p as f64 + i as f64) * self.ir_step_ms;
                (v, delay_ms)
            })
            .collect()
    }

    /// The cumulative sample-rate-offset correction emitted so far (Hz), for diagnostics.
    pub fn applied_sro_hz(&self) -> f64 {
        self.applied_hz
    }

    /// Restart the sample-rate-offset measurement (after an external correction, whose
    /// effect would otherwise pollute the drift history).
    pub fn reset_sro(&mut self) {
        self.acq_cnt = self.acq_cnt_max;
        self.sro_acquisition = true;
        self.xc_ref = None;
        self.xc_count = 0;
        self.drift.clear();
        self.applied_hz = 0.0;
    }

    /// SRO correction (Hz) after a new drift measurement; usually 0 during acquisition,
    /// small steps during tracking.
    fn sro_step(&mut self) -> f64 {
        let step_syms = self.xc_period as f64;
        if self.sro_acquisition {
            if self.acq_cnt > 0 {
                return 0.0;
            }
            // End of acquisition: mean drift after the settling time. The history then
            // restarts, since the correction changes the drift.
            self.sro_acquisition = false;
            let settle = self.acq_settle.div_ceil(self.xc_period);
            let used: Vec<f64> = self.drift.iter().skip(settle).map(|d| d.0).collect();
            self.drift.clear();
            self.applied_hz = 0.0;
            if used.is_empty() {
                return 0.0;
            }
            let rate = median(&used) / step_syms;
            return -self.sam_off_hz(rate);
        }
        // Tracking: the drift over the history gives the correction that was needed on
        // average over it; corrections applied since then are subtracted.
        let n = self.drift.len();
        if (n as f64) * step_syms < SRO_TRACK_MIN_S * self.sym_rate {
            return 0.0;
        }
        let steps: Vec<f64> = self.drift.iter().map(|d| d.0).collect();
        let m = median(&steps);
        let dev: Vec<f64> = steps.iter().map(|v| (v - m).abs()).collect();
        let lim = 4.0 * median(&dev).max(1e-3);
        let rate =
            steps.iter().map(|v| v.clamp(m - lim, m + lim)).sum::<f64>() / (n as f64 * step_syms);
        let mean_applied = self.drift.iter().map(|d| d.1).sum::<f64>() / n as f64;
        let residual = -self.sam_off_hz(rate) - (self.applied_hz - mean_applied);
        let delta = residual * SRO_TRACK_RATE * step_syms / self.sym_rate;
        self.applied_hz += delta;
        delta
    }

    /// Sample-rate offset (Hz) for a drift of the impulse response of `slope` IR bins per
    /// symbol.
    fn sam_off_hz(&self, slope: f64) -> f64 {
        let norm = slope / self.sym_len_ir;
        self.fs * (1.0 - 1.0 / (1.0 + norm))
    }
}

fn cplx_to_reim(v: &[Cplx], re: &mut [f64], im: &mut [f64]) {
    for (i, c) in v.iter().enumerate() {
        re[i] = c.re;
        im[i] = c.im;
    }
}

fn reim_to_cplx(re: &[f64], im: &[f64], v: &mut [Cplx]) {
    for i in 0..v.len() {
        v[i] = Cplx::new(re[i], im[i]);
    }
}

/// Circular left rotation by a fractional number of samples (linear interpolation):
/// `v[i] ← v[i + shift]`.
fn rotate_left_frac(v: &mut [f64], shift: f64, scratch: &mut [f64]) {
    let n = v.len();
    let k = shift.floor();
    let f = shift - k;
    let k = (k as isize).rem_euclid(n as isize) as usize;
    for i in 0..n {
        scratch[i] = (1.0 - f) * v[(i + k) % n] + f * v[(i + k + 1) % n];
    }
    v.copy_from_slice(scratch);
}

/// Median (the mean of the two middle values for an even count).
fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        0.5 * (s[n / 2 - 1] + s[n / 2])
    }
}

/// Shift `s` (IR bins, with sub-bin precision) that best aligns `cur` with `prev`, i.e.
/// `cur(i + s) ≈ prev(i)`, searched within ±`max`. The integer part is the maximum of the
/// circular cross-correlation of the mean-removed profiles; the fraction is either the slope
/// of the cross-spectrum phase (a translation's Fourier shift theorem) or a parabola through
/// the correlation peak when resolved paths change power.
fn profile_shift(
    prev: &[f64],
    cur: &[f64],
    max: usize,
    fft: &mut FullFft,
    a_re: &mut [f64],
    a_im: &mut [f64],
    b_re: &mut [f64],
    b_im: &mut [f64],
) -> f64 {
    const MAX_PHASE_RESIDUAL: f64 = 0.04;
    let n = prev.len();
    if n < 8 || cur.len() != n || fft.len() != n {
        return 0.0;
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / n as f64;
    let (mp, mc) = (mean(prev), mean(cur));
    let m = max.min(n / 2 - 2) as isize;
    let at = |i: usize, s: isize| cur[(i as isize + s).rem_euclid(n as isize) as usize];
    let corr = |s: isize| (0..n).map(|i| (prev[i] - mp) * (at(i, s) - mc)).sum::<f64>();
    let Some(k0) = (-m..=m).max_by(|&x, &y| corr(x).total_cmp(&corr(y))) else {
        return 0.0;
    };

    // Parabola through the correlation peak.
    let (y0, y1, y2) = (corr(k0 - 1), corr(k0), corr(k0 + 1));
    let den = y0 - 2.0 * y1 + y2;
    let parabola = if den < 0.0 {
        (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    };

    // Phase slope of the cross-spectrum, with `cur` moved back by the integer part.
    for i in 0..n {
        a_re[i] = prev[i];
        a_im[i] = 0.0;
        b_re[i] = at(i, k0);
        b_im[i] = 0.0;
    }
    fft.forward_re_im(a_re, a_im);
    fft.forward_re_im(b_re, b_im);
    let bins = 1..=(n / 8).max(2);
    let (mut num, mut den_sum, mut wsum) = (0.0, 0.0, 0.0);
    for k in bins.clone() {
        // c = conj(A_k) * B_k
        let c = Cplx::new(a_re[k], -a_im[k]) * Cplx::new(b_re[k], b_im[k]);
        let (w, kf) = (c.norm(), k as f64);
        num += w * kf * c.arg();
        den_sum += w * kf * kf;
        wsum += w;
    }
    if den_sum <= 0.0 {
        return k0 as f64 + parabola;
    }
    let slope = num / den_sum;
    let resid2: f64 = bins
        .map(|k| {
            let c = Cplx::new(a_re[k], -a_im[k]) * Cplx::new(b_re[k], b_im[k]);
            c.norm() * (c.arg() - slope * k as f64).powi(2)
        })
        .sum::<f64>()
        / wsum;
    let phase = (-slope * n as f64 / (2.0 * core::f64::consts::PI)).clamp(-1.0, 1.0);
    k0 as f64 + if resid2.sqrt() < MAX_PHASE_RESIDUAL { phase } else { parabola }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm::params::{RobustnessMode, SpectrumOccupancy};

    /// Power delay profile of paths (delay in bins, amplitude) as the tracker computes it:
    /// |IFFT(Hamming · H)|² over `n` pilot-grid carriers.
    fn ir_profile(n: usize, paths: &[(f64, f64)]) -> Vec<f64> {
        let mut fft = FullFft::new(n);
        let w = hamming(n);
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        for k in 0..n {
            let h = paths
                .iter()
                .fold(Cplx::zero(), |acc, &(d, a)| {
                    acc + Cplx::from_polar(a, -2.0 * core::f64::consts::PI * k as f64 * d / n as f64)
                })
                * w[k];
            re[k] = h.re;
            im[k] = h.im;
        }
        fft.inverse_re_im(&mut re, &mut im);
        (0..n).map(|i| re[i] * re[i] + im[i] * im[i]).collect()
    }

    fn shift(prev: &[f64], cur: &[f64]) -> f64 {
        let mut fft = FullFft::new(prev.len());
        let mut ar = vec![0.0; prev.len()];
        let mut ai = vec![0.0; prev.len()];
        let mut br = vec![0.0; prev.len()];
        let mut bi = vec![0.0; prev.len()];
        profile_shift(prev, cur, 3, &mut fft, &mut ar, &mut ai, &mut br, &mut bi)
    }

    #[test]
    fn shift_follows_translation() {
        for base in [10.0, 10.3, 10.5] {
            for true_shift in [-2.3, -0.4, -0.02, 0.0, 0.01, 0.05, 0.15, 1.7] {
                let prev = ir_profile(104, &[(base, 1.0), (base + 6.0, 0.5)]);
                let cur =
                    ir_profile(104, &[(base + true_shift, 1.0), (base + 6.0 + true_shift, 0.5)]);
                let s = shift(&prev, &cur);
                let tol = 0.003 + 0.03 * true_shift.abs();
                assert!((s - true_shift).abs() < tol, "base {base} shift {true_shift}: measured {s}");
            }
        }
    }

    #[test]
    fn resolved_paths_swapping_power_are_not_a_shift() {
        let prev = ir_profile(104, &[(20.0, 1.0), (25.0, 0.6)]);
        let cur = ir_profile(104, &[(20.0, 0.6), (25.0, 1.0)]);
        let s = shift(&prev, &cur);
        assert!(s.abs() < 0.3, "measured {s}");
    }

    #[test]
    fn fractional_rotation() {
        let mut v = vec![0.0, 1.0, 2.0, 3.0];
        let mut scratch = vec![0.0; 4];
        rotate_left_frac(&mut v, 1.25, &mut scratch);
        assert_eq!(v, [1.25, 2.25, 2.25, 0.25]);
        let mut w = vec![0.0, 1.0, 2.0, 3.0];
        rotate_left_frac(&mut w, -0.5, &mut scratch);
        assert_eq!(w, [1.5, 0.5, 1.5, 2.5]);
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    /// The tracker's geometry must follow the spec's grid for the bench layout (mode B,
    /// 10 kHz): 104 pilot-grid carriers spaced 2 apart, one IR sample ≈ 4.92 input samples.
    #[test]
    fn bench_geometry_matches_the_spec_grid() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        let num_pil = (map.num_carriers - 1) / map.scattered.freq_int + 1;
        assert_eq!(num_pil, 104);
        let t = PdsTracker::new(&map, num_pil, 4);
        // 104 * 2 / 1024 = 0.203125 IR samples per input sample → 4.923 ms per IR sample.
        assert!((t.ir_per_sample - 0.203_125).abs() < 1e-12);
        assert!((t.guard_ir - 52.0).abs() < 1e-9);
    }
}
