//! Wiener interpolation of the channel in the time direction at the gain-reference grid,
//! with a Doppler-spread estimate (port of Dream's `CTimeWiener` / DecDRM's `time_wiener.rs`).
//!
//! The channel is assumed to have a Gaussian Doppler spectrum:
//! `r(τ) = exp(−2π²·σ²·Ts²·τ²)`, τ in OFDM symbols. The Doppler spread σ is estimated from
//! the averaged time correlation of the pilot estimates (Dream's "modified linear
//! regression"), and the Wiener taps are rebuilt from it once per frame once tracking is on.

use std::collections::VecDeque;

use crate::digital::drm2::cellmap::CellMap;
use crate::digital::drm2::dsp::util::{iir1_c, iir1_lambda, linear_regression_slope};
use crate::digital::drm2::dsp::{levinson::levinson, Cplx};
use crate::digital::drm2::params::RobustnessMode;

const SIGMA_TAPS: usize = 3;
const TICONST_TI_CORREL_EST: f64 = 60.0;
const SIGMA_OVERESTIMATION: f64 = 3.0;
const LOW_BOUND_SIGMA: f64 = 0.1 / 2.0;
const INIT_SNR_DB: f64 = 25.0;

/// (filter length in pilots, maximum one-sided σ in Hz) per robustness mode.
fn params(mode: RobustnessMode) -> (usize, f64) {
    match mode {
        RobustnessMode::A => (5, 1.6 / 2.0),
        RobustnessMode::B => (7, 2.7 / 2.0),
        RobustnessMode::C => (9, 5.7 / 2.0),
        RobustnessMode::D => (9, 4.5 / 2.0),
        RobustnessMode::E => (5, 1.6 / 2.0),
    }
}

/// One pilot estimate and the cumulative timing shift at the time it was taken.
#[derive(Clone, Copy)]
struct PilotSample {
    h: Cplx,
    cum_shift: i64,
}

/// Time-direction Wiener interpolator over the gain-reference grid.
pub struct TimeWiener {
    len: usize,
    t: usize,
    /// Output delay in symbols.
    pub delay: usize,
    ts: f64,
    sigma_max: f64,
    sigma: f64,
    /// Per grid carrier, the last `len` pilot estimates (front = newest).
    hist: Vec<VecDeque<PilotSample>>,
    /// Whether a grid carrier has received its first pilot yet.
    seeded: Vec<bool>,
    /// Filter taps per phase.
    filters: Vec<Vec<f64>>,
    mmse: f64,
    ticorr: [Cplx; SIGMA_TAPS],
    lambda_ticorr: f64,
    pub tracking: bool,
    update_cnt: usize,
    snr_acc: f64,
    snr_cnt: usize,
    ns: usize,
    s0: usize,
}

impl TimeWiener {
    pub fn new(map: &CellMap) -> Self {
        let mode = map.mode();
        let (len, sigma_max) = params(mode);
        let t = map.scattered.time_int;
        let delay = ((len * t - t + 1) as f64 / 2.0).ceil() as usize + t / 2 - 1;
        let ts = mode.symbol_len() as f64 / f64::from(mode.sample_rate());
        let grid_len = (map.num_carriers - 1) / map.scattered.freq_int + 1;
        let s0 = (0..mode.symbols_per_frame())
            .find(|&s| map.cell(s, 0).is_scattered())
            .unwrap_or(0);
        let num_pil_one_sym = grid_len.div_ceil(t);
        let lambda_ticorr = iir1_lambda(TICONST_TI_CORREL_EST * num_pil_one_sym as f64, 1.0 / ts);
        let init = PilotSample { h: Cplx::new(1.0, 0.0), cum_shift: 0 };
        let mut w = Self {
            len,
            t,
            delay,
            ts,
            sigma_max,
            sigma: sigma_max,
            hist: vec![VecDeque::from(vec![init; len]); grid_len],
            seeded: vec![false; grid_len],
            filters: vec![vec![0.0; len]; t],
            mmse: 1.0,
            ticorr: [Cplx::new(0.0, 0.0); SIGMA_TAPS],
            lambda_ticorr,
            tracking: false,
            update_cnt: mode.symbols_per_frame(),
            snr_acc: 0.0,
            snr_cnt: 0,
            ns: mode.symbols_per_frame(),
            s0,
        };
        w.mmse = w.update_filters(10f64.powf(INIT_SNR_DB / 10.0), sigma_max);
        w
    }

    /// One-sided Doppler spread σ in Hz.
    pub fn sigma(&self) -> f64 {
        self.sigma
    }

    /// Rebuild the per-phase Wiener taps for a Gaussian Doppler spectrum of spread σ.
    fn update_filters(&mut self, snr: f64, sigma: f64) -> f64 {
        let fac = -2.0 * core::f64::consts::PI * core::f64::consts::PI * self.ts * self.ts * sigma * sigma;
        let r = |tau: i64| (fac * (tau * tau) as f64).exp();
        let mut mmse = 0.0;
        for phase in 0..self.t {
            let rhp: Vec<f64> = (0..self.len)
                .map(|j| r(self.delay as i64 - phase as i64 - (j * self.t) as i64))
                .collect();
            let mut rpp: Vec<f64> = (0..self.len).map(|j| r((j * self.t) as i64)).collect();
            rpp[0] += 1.0 / snr;
            let taps = levinson(&rpp, &rhp);
            mmse += 1.0 - rhp.iter().zip(&taps).map(|(a, b)| a * b).sum::<f64>();
            self.filters[phase] = taps;
        }
        mmse / self.t as f64
    }

    /// Interpolate one OFDM symbol's pilots into the gain-reference grid. Writes the channel
    /// estimate at every grid carrier for the output symbol (delayed `self.delay` symbols) and
    /// returns the SNR improvement factor of the interpolation.
    #[allow(clippy::too_many_arguments)]
    pub fn estimate(
        &mut self,
        map: &CellMap,
        sym: &[Cplx],
        s: usize,
        cum_shift: i64,
        out_cum_shift: i64,
        snr_pilots: f64,
        out: &mut [Cplx],
    ) -> f64 {
        let x = map.scattered.freq_int;
        let n = map.mode().fft_size() as f64;
        let rot = |h: Cplx, c: usize, dshift: i64| -> Cplx {
            if dshift == 0 {
                h
            } else {
                let k = (map.kmin + c as i32) as f64;
                h * Cplx::from_polar(1.0, 2.0 * core::f64::consts::PI * k * dshift as f64 / n)
            }
        };
        for (p, hist) in self.hist.iter_mut().enumerate() {
            let c = p * x;
            if c >= map.num_carriers {
                break;
            }
            if map.cell(s, c).is_scattered() {
                let sample = PilotSample { h: sym[c] / map.pilot(s, c), cum_shift };
                if self.seeded[p] {
                    hist.pop_back();
                    hist.push_front(sample);
                } else {
                    // Start from the first measurement (Dream's h = 1 dominates the
                    // normalised input otherwise).
                    hist.iter_mut().for_each(|h| *h = sample);
                    self.seeded[p] = true;
                }
                let newest = hist[0].h;
                for j in 0..SIGMA_TAPS.min(self.len) {
                    let old = rot(hist[j].h, c, cum_shift - hist[j].cum_shift);
                    iir1_c(&mut self.ticorr[j], newest.conj() * old, self.lambda_ticorr);
                }
            }
            if map.cell(s, c).is_dc() {
                out[p] = Cplx::new(0.0, 0.0);
                continue;
            }
            let phase = (s + self.t * self.ns - self.s0 - p % self.t) % self.t;
            let taps = &self.filters[phase];
            let mut acc = Cplx::new(0.0, 0.0);
            for (j, tap) in taps.iter().enumerate() {
                let ps = hist[j];
                acc += rot(ps.h, c, out_cum_shift - ps.cum_shift) * *tap;
            }
            out[p] = acc;
        }

        if self.tracking {
            if self.update_cnt > 0 {
                self.update_cnt -= 1;
                self.snr_acc += snr_pilots;
                self.snr_cnt += 1;
            } else {
                self.sigma = self.estimate_sigma();
                let s_over = (self.sigma * SIGMA_OVERESTIMATION).min(self.sigma_max);
                let snr = if self.snr_cnt > 0 { self.snr_acc / self.snr_cnt as f64 } else { snr_pilots };
                self.mmse = self.update_filters(snr.max(1.0), s_over);
                if 1.0 / self.mmse < snr_pilots {
                    self.mmse = 1.0 / snr_pilots.max(1e-3);
                }
                self.update_cnt = self.ns;
                self.snr_acc = 0.0;
                self.snr_cnt = 0;
            }
        }
        1.0 / self.mmse
    }

    /// Fit |R(τ)| = a·exp(−b·τ²) to the averaged time correlation and convert to σ.
    fn estimate_sigma(&self) -> f64 {
        let m = SIGMA_TAPS.min(self.len);
        let w: Vec<f64> = (0..m).map(|i| ((i * self.t) as f64).powi(2)).collect();
        let z: Vec<f64> = self.ticorr[..m].iter().map(|c| c.norm().max(1e-30).ln()).collect();
        let a1 = linear_regression_slope(&w, &z);
        let sigma = 0.5 / core::f64::consts::PI * (-2.0 * a1).max(0.0).sqrt() / self.ts;
        sigma.clamp(LOW_BOUND_SIGMA, self.sigma_max)
    }
}
