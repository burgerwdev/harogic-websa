//! Time synchronisation: robustness-mode detection and OFDM symbol timing from the
//! guard-interval (cyclic-prefix) autocorrelation. Port of Dream's `CTimeSync`
//! (`src/sync/TimeSync.cpp` + `TimeSyncTrack.cpp`).
//!
//! Dream runs the correlation on a signal low-passed to about +/-5 kHz and decimated by four
//! (`GRDCRR_DEC_FACT`, its 10 kHz Hilbert filter path). Everything below is therefore in the
//! decimated domain, whose sample rate is `SAMPLE_RATE / DEC`.
//!
//! Constants cross-checked against Dream's `sync/TimeSync.h`: `LAMBDA_LOW_PASS_START` 0.99,
//! `TIMING_BOUND_ABS` 150, `NUM_SYM_BEFORE_RESET` 5, `NUM_BLOCKS_FOR_RM_CORR` 16,
//! `STEP_SIZE_GUARD_CORR` 4 and `GRDCRR_DEC_FACT` 4. Dream's mode-detection threshold
//! `THRESHOLD_RELI_MEASURE` is 8.0; the reference port documents that a clean mode A with a
//! narrow occupancy only reaches about 7, so this port uses Dream's 8.0 for the 16-symbol
//! window and the reference's lower 5.0 on a 48-symbol window as the weak/narrow-signal
//! fallback — the deviation is the reference port's, recorded here so it is not a surprise.
//!
//! The guard correlation per evaluation is Dream's maximum-likelihood metric
//! `|c| - p/2` (with `c = sum x[i]*conj(x[i+nu])` and `p = sum |x[i]|^2 + |x[i+nu]|^2`), and
//! the normalised coefficient `2|c|/p` (0..1) is used for mode detection, because the ML
//! metric carries the symbol-periodic power fluctuation of the pilots.

use std::collections::VecDeque;

use crate::digital::drm2::dsp::fir::{lowpass, FirDecimator};
use crate::digital::drm2::dsp::Cplx;
use crate::digital::drm2::params::{RobustnessMode, SAMPLE_RATE};

/// Decimation factor of the correlation path (`GRDCRR_DEC_FACT`).
const DEC: usize = 4;
/// Guard correlation is evaluated every `STEP` decimated samples (`STEP_SIZE_GUARD_CORR`).
const STEP: usize = 4;
/// Robustness-mode detection observes this many symbols (`NUM_BLOCKS_FOR_RM_CORR`).
const RM_BLOCKS: usize = 16;
/// Required ratio between the best and second-best mode score (`THRESHOLD_RELI_MEASURE`).
const RM_RELIABILITY: f64 = 8.0;
/// The weak/narrow-signal fallback: a longer observation with the reference port's lower
/// threshold.
const RM_BLOCKS_LONG: usize = 48;
const RM_RELIABILITY_LONG: f64 = 5.0;
/// Low-pass of the acquired timing (`LAMBDA_LOW_PASS_START`).
const LAMBDA_START: f64 = 0.99;
/// Candidates further than this from the current estimate count as outliers
/// (`TIMING_BOUND_ABS`).
const TIMING_BOUND: f64 = 150.0;
/// After this many consecutive outliers the estimate jumps to their average
/// (`NUM_SYM_BEFORE_RESET`).
const OUTLIERS_BEFORE_RESET: usize = 5;
/// Low-pass of the correlation path: +/-6 kHz passband, 60 dB stopband.
const LPF_CUTOFF: f64 = 6000.0 / SAMPLE_RATE as f64;
const LPF_TAPS: usize = 97;

/// Geometry of one mode in the decimated domain.
#[derive(Debug, Clone, Copy)]
struct Geom {
    nu: usize,
    g: usize,
    ts: usize,
}

fn geom(mode: RobustnessMode) -> Geom {
    Geom {
        nu: mode.fft_size() / DEC,
        g: mode.guard_len() / DEC,
        ts: mode.symbol_len() / DEC,
    }
}

/// One demodulation window: the useful part of an OFDM symbol, at the full rate.
#[derive(Debug, Clone)]
pub struct SymbolWindow {
    /// The `fft_size` samples of the useful part, starting at [`SymbolWindow::start`].
    pub samples: Vec<Cplx>,
    /// Absolute input sample index of the first sample (full rate).
    pub start: i64,
    /// Timing change against the previous window's grid, full-rate samples.
    pub shift: i64,
    /// Normalised cyclic-prefix correlation (0..=1); `None` when the guard is no longer
    /// buffered.
    pub guard_corr: Option<f64>,
    /// Mean power of the window's samples.
    pub power: f64,
}

/// What the stage learned from the most recent input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// A robustness mode was detected (it may equal the current one).
    ModeDetected { mode: RobustnessMode, reliability: f64 },
}

/// The mode detector's per-mode state: a ring of correlation values and two sliding DFTs,
/// because the guard correlation is symbol-periodic and the period identifies the mode.
struct ModeDetector {
    values: [VecDeque<f64>; 4],
    acc_short: [Cplx; 4],
    acc_long: [Cplx; 4],
    len_short: usize,
    len_long: usize,
    count: usize,
    /// The mode that has been winning and for how many evaluations.
    candidate: Option<RobustnessMode>,
    streak: usize,
    /// Scores of the most recent decision (diagnostics/tests).
    pub last_scores: [f64; 4],
}

impl ModeDetector {
    fn new(mode: RobustnessMode) -> Self {
        let g = geom(mode);
        let mut d = Self {
            values: Default::default(),
            acc_short: [Cplx::zero(); 4],
            acc_long: [Cplx::zero(); 4],
            len_short: RM_BLOCKS * g.ts / STEP,
            len_long: RM_BLOCKS_LONG * g.ts / STEP,
            count: 0,
            candidate: None,
            streak: 0,
            last_scores: [0.0; 4],
        };
        d.reset();
        d
    }

    fn reset(&mut self) {
        for v in &mut self.values {
            v.clear();
        }
        self.acc_short = [Cplx::zero(); 4];
        self.acc_long = [Cplx::zero(); 4];
        self.count = 0;
        self.candidate = None;
        self.streak = 0;
    }

    /// Reconfigure for another mode's symbol rate (the windows are lengths in evaluations).
    fn configure(&mut self, mode: RobustnessMode) {
        let g = geom(mode);
        let len_short = RM_BLOCKS * g.ts / STEP;
        if len_short != self.len_short {
            self.len_short = len_short;
            self.reset();
        }
        self.len_long = RM_BLOCKS_LONG * g.ts / STEP;
    }

    /// Add one evaluation's normalised correlations and decide whether a mode is reliable.
    fn push(&mut self, geoms: &[Geom; 4], rho: &[f64; 4]) -> Option<(RobustnessMode, f64)> {
        let n = self.count;
        for (m, g) in geoms.iter().enumerate() {
            let k = (n * STEP) % g.ts;
            let phasor = Cplx::from_polar(1.0, -2.0 * core::f64::consts::PI * k as f64 / g.ts as f64);
            let v = rho[m];
            let ring = &mut self.values[m];
            ring.push_back(v);
            self.acc_short[m] += phasor * v;
            self.acc_long[m] += phasor * v;
            if ring.len() > self.len_short {
                let old = ring[ring.len() - 1 - self.len_short];
                let k_old = ((n - self.len_short) * STEP) % g.ts;
                let ph_old = Cplx::from_polar(
                    1.0,
                    -2.0 * core::f64::consts::PI * k_old as f64 / g.ts as f64,
                );
                self.acc_short[m] -= ph_old * old;
            }
            if ring.len() > self.len_long {
                let old = ring.pop_front().unwrap_or(0.0);
                let k_old = ((n - self.len_long) * STEP) % g.ts;
                let ph_old = Cplx::from_polar(
                    1.0,
                    -2.0 * core::f64::consts::PI * k_old as f64 / g.ts as f64,
                );
                self.acc_long[m] -= ph_old * old;
            }
        }
        self.count += 1;

        let len = self.values[0].len();
        if len < self.len_short {
            return None;
        }
        let short: [f64; 4] =
            self.acc_short.map(|a| a.norm() / self.len_short.max(1) as f64);
        self.last_scores = short;
        if let Some(found) = decide(&short, RM_RELIABILITY) {
            return self.accept(found, geoms);
        }
        // The long window takes over once it holds 1.5x the short length, for weak or narrow
        // signals (the reference port's documented fallback).
        if 2 * len >= 3 * self.len_short {
            let long: [f64; 4] = self.acc_long.map(|a| a.norm() / len.max(1) as f64);
            if let Some(found) = decide(&long, RM_RELIABILITY_LONG) {
                return self.accept(found, geoms);
            }
        }
        None
    }

    /// Require the same mode to win for a whole symbol before declaring it, so a transient
    /// cannot latch a mode.
    fn accept(
        &mut self,
        (mode, reliability): (RobustnessMode, f64),
        geoms: &[Geom; 4],
    ) -> Option<(RobustnessMode, f64)> {
        if self.candidate == Some(mode) {
            self.streak += 1;
            if self.streak >= geoms[mode.index()].ts / STEP {
                return Some((mode, reliability));
            }
        } else {
            self.candidate = Some(mode);
            self.streak = 1;
        }
        None
    }
}

/// Best mode and its ratio to the second best, if the ratio exceeds `threshold`.
fn decide(scores: &[f64; 4], threshold: f64) -> Option<(RobustnessMode, f64)> {
    let (best, &max) = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let second = scores
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != best)
        .map(|(_, &v)| v)
        .fold(0.0f64, f64::max);
    let reliability = if second > 0.0 { max / second } else { f64::INFINITY };
    (reliability > threshold).then(|| (RobustnessMode::DRM30[best], reliability))
}

/// Timing acquisition and tracking, in the decimated domain.
struct Timing {
    /// Per-position average of the correlation metric within the symbol period.
    average: Vec<f64>,
    index: usize,
    lambda: f64,
    /// Moving average over one guard interval.
    moving: VecDeque<f64>,
    moving_len: usize,
    /// The last symbol period's moving averages with their positions.
    maxima: VecDeque<(f64, i64)>,
    maxima_len: usize,
    init: usize,
    outliers: usize,
    outlier_sum: f64,
}

impl Timing {
    fn new(mode: RobustnessMode) -> Self {
        let g = geom(mode);
        Self {
            average: vec![0.0; g.ts / STEP],
            index: 0,
            lambda: 1.0,
            moving: VecDeque::new(),
            moving_len: (g.g / STEP).max(1),
            maxima: VecDeque::new(),
            maxima_len: g.ts / STEP,
            init: g.ts / STEP,
            outliers: OUTLIERS_BEFORE_RESET,
            outlier_sum: 0.0,
        }
    }

    fn configure(&mut self, mode: RobustnessMode) {
        let g = geom(mode);
        self.average = vec![0.0; g.ts / STEP];
        self.index = 0;
        self.lambda = 1.0;
        self.moving.clear();
        self.moving_len = (g.g / STEP).max(1);
        self.maxima.clear();
        self.maxima_len = g.ts / STEP;
        self.init = g.ts / STEP;
        self.outliers = OUTLIERS_BEFORE_RESET;
        self.outlier_sum = 0.0;
    }

    /// Feed one evaluation's ML metric; returns the position of the correlation maximum when
    /// it is at the centre of the window (the classic guard-correlation timing estimate).
    fn push(&mut self, metric: f64, position: i64) -> Option<i64> {
        if self.init > 0 {
            self.init -= 1;
            return None;
        }
        let slot = &mut self.average[self.index];
        *slot = (1.0 - self.lambda) * (*slot - metric) + metric;
        let avg = *slot;
        self.index += 1;
        if self.index == self.average.len() {
            self.index = 0;
            // Dream halves the averaging factor after the first pass over the symbol.
            self.lambda = if self.lambda <= 0.1 { 0.1 } else { self.lambda / 2.0 };
        }
        self.moving.push_back(avg);
        while self.moving.len() > self.moving_len {
            self.moving.pop_front();
        }
        let mean = self.moving.iter().sum::<f64>() / self.moving.len() as f64;
        self.maxima.push_back((mean, position));
        while self.maxima.len() > self.maxima_len {
            self.maxima.pop_front();
        }
        if self.maxima.len() < self.maxima_len {
            return None;
        }
        let (imax, _) = self
            .maxima
            .iter()
            .enumerate()
            .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
            .expect("non-empty");
        let centre = (self.maxima_len - 1) / 2;
        (imax == centre).then(|| self.maxima[centre].1)
    }
}

/// The streaming time-synchronisation stage.
pub struct TimeSync {
    /// The decimated correlation path.
    dec: Vec<Cplx>,
    /// Decimated index of `dec[0]` (it advances as the head is dropped).
    dec_index_base: i64,
    /// Full-rate index of `dec[0]`: the first decimator output is emitted after `DEC` inputs
    /// and its FIR is centred `delay` samples earlier, so this is all it is — a constant.
    dec_input_base: i64,
    /// Decimator outputs produced so far (for the bookkeeping above).
    dec_outputs: u64,
    lpf: FirDecimator,
    /// Decimated index of the next correlation evaluation.
    next_eval: i64,
    /// Decimated samples of the free-running correlation path.
    detector: ModeDetector,
    timing: Timing,
    mode: RobustnessMode,
    /// The next window's start, full rate, once timing is known.
    next_start: Option<f64>,
    last_start: Option<i64>,
    /// Full-rate history, kept for the windows.
    buf: Vec<Cplx>,
    /// Absolute full-rate index of `buf[0]`.
    buf_base: i64,
}

impl TimeSync {
    pub fn new(mode: RobustnessMode) -> Self {
        let lpf = FirDecimator::new(lowpass(LPF_TAPS, LPF_CUTOFF, 60.0), DEC);
        let dec_input_base = DEC as i64 - 1 - (LPF_TAPS as i64 - 1) / 2;
        Self {
            dec: Vec::new(),
            dec_index_base: 0,
            dec_input_base,
            dec_outputs: 0,
            lpf,
            next_eval: 0,
            detector: ModeDetector::new(mode),
            timing: Timing::new(mode),
            mode,
            next_start: None,
            last_start: None,
            buf: Vec::new(),
            buf_base: 0,
        }
    }

    pub fn mode(&self) -> RobustnessMode {
        self.mode
    }

    /// Switch the assumed mode (the FAC can force one); timing restarts, the history stays.
    pub fn configure(&mut self, mode: RobustnessMode) {
        self.mode = mode;
        self.detector.configure(mode);
        self.timing.configure(mode);
        self.next_start = None;
        self.last_start = None;
    }

    pub fn restart(&mut self) {
        self.dec.clear();
        self.dec_index_base = 0;
        self.dec_outputs = 0;
        self.lpf.reset();
        self.next_eval = 0;
        self.detector.reset();
        self.timing.configure(self.mode);
        self.next_start = None;
        self.last_start = None;
        self.buf.clear();
        self.buf_base = 0;
    }

    pub fn has_timing(&self) -> bool {
        self.next_start.is_some()
    }

    /// Feed full-rate baseband samples; returns detection events.
    pub fn push(&mut self, iq: &[Cplx]) -> Vec<Event> {
        let mut events = Vec::new();
        self.buf.extend_from_slice(iq);
        let mut decimated = Vec::new();
        self.lpf.process(iq, &mut decimated);
        self.dec_outputs += decimated.len() as u64;
        self.dec.extend_from_slice(&decimated);
        self.evaluate(&mut events);
        events
    }

    fn evaluate(&mut self, events: &mut Vec<Event>) {
        let geoms: [Geom; 4] = RobustnessMode::DRM30.map(geom);
        let span = geoms.iter().map(|g| g.g + g.nu).max().unwrap_or(0);
        let sel = self.mode.index();
        let dec_end = self.dec_index_base + self.dec.len() as i64;
        if self.next_eval < self.dec_index_base {
            self.next_eval = self.dec_index_base;
        }
        while self.next_eval + span as i64 <= dec_end {
            let t = (self.next_eval - self.dec_index_base) as usize;
            let mut metric = [0.0f64; 4];
            let mut rho = [0.0f64; 4];
            for (m, g) in geoms.iter().enumerate() {
                let mut c = Cplx::zero();
                let mut p = 0.0f64;
                for i in 0..g.g {
                    let a = self.dec[t + i];
                    let b = self.dec[t + i + g.nu];
                    c += a * b.conj();
                    p += a.norm_sqr() + b.norm_sqr();
                }
                metric[m] = c.norm() - 0.5 * p;
                rho[m] = if p > 0.0 { 2.0 * c.norm() / p } else { 0.0 };
            }
            if let Some((mode, reliability)) = self.detector.push(&geoms, &rho) {
                events.push(Event::ModeDetected { mode, reliability });
            }
            // Timing follows the detected mode's metric.
            if let Some(position) = self.timing.push(metric[sel], self.next_eval) {
                // `position` is a decimated index of the correlation maximum, which sits at the
                // guard's centre (the moving average spans one guard). The useful part follows
                // the guard, so the FFT window starts half a guard after that maximum. The
                // decimated-to-full-rate conversion is exact: `dec[i]` is `dec_input_base +
                // i * DEC`.
                let window_dec = position + (geoms[sel].g / 2) as i64;
                let candidate = (self.dec_input_base + window_dec * DEC as i64) as f64;
                self.timing_candidate(candidate);
            }
            self.next_eval += STEP as i64;
        }
        // Bound the correlation history: one symbol plus the evaluator's span.
        let keep = span + 4 * geoms.iter().map(|g| g.ts).max().unwrap_or(0);
        if self.dec.len() > 2 * keep {
            let drop = self.dec.len() - keep;
            self.dec.drain(..drop);
            self.dec_index_base += drop as i64;
        }
    }

    /// Filter a new window-start candidate (full-rate absolute index).
    fn timing_candidate(&mut self, candidate: f64) {
        let ts = self.mode.symbol_len() as f64;
        let Some(current) = self.next_start else {
            self.next_start = Some(candidate);
            return;
        };
        // The error against the nearest point of the current symbol grid.
        let mut e = (candidate - current).rem_euclid(ts);
        if e >= ts / 2.0 {
            e -= ts;
        }
        if e.abs() < TIMING_BOUND {
            self.next_start = Some(current + (1.0 - LAMBDA_START) * e);
            self.timing.outliers = 0;
            self.timing.outlier_sum = 0.0;
        } else {
            self.timing.outliers += 1;
            self.timing.outlier_sum += e;
            if self.timing.outliers > OUTLIERS_BEFORE_RESET {
                let jump = self.timing.outlier_sum / self.timing.outliers as f64;
                self.next_start = Some(current + jump);
                self.timing.outliers = 0;
                self.timing.outlier_sum = 0.0;
            }
        }
    }

    /// Apply a timing correction (samples, positive = later) from the channel estimator's
    /// impulse-response tracker to the next window positions.
    pub fn adjust_timing(&mut self, samples: f64) {
        if let Some(s) = &mut self.next_start {
            *s += samples;
        }
    }

    /// The next symbol window, when timing is known and the samples are buffered.
    pub fn next_window(&mut self) -> Option<SymbolWindow> {
        let start = self.next_start?;
        let start_i = start.round() as i64;
        let rel = start_i - self.buf_base;
        if rel < 0 {
            self.next_start = Some(start + self.mode.symbol_len() as f64);
            return None;
        }
        let rel = rel as usize;
        let n = self.mode.fft_size();
        if rel + n > self.buf.len() {
            return None;
        }
        let samples = self.buf[rel..rel + n].to_vec();
        let power = samples.iter().map(|s| s.norm_sqr()).sum::<f64>() / n as f64;
        let guard_corr = self.guard_correlation(rel);
        let shift = match self.last_start {
            Some(l) => start_i - (l + self.mode.symbol_len() as i64),
            None => 0,
        };
        self.last_start = Some(start_i);
        self.next_start = Some(start + self.mode.symbol_len() as f64);
        // Keep a symbol of margin before the next window.
        if rel > 2 * self.mode.symbol_len() {
            let drop = rel - self.mode.symbol_len();
            self.buf.drain(..drop);
            self.buf_base += drop as i64;
        }
        Some(SymbolWindow { samples, start: start_i, shift, guard_corr, power })
    }

    /// Normalised cyclic-prefix correlation for a window at buffer index `rel`, over guard
    /// placements within +/-G/2 of the window start (the timing loop may park the window
    /// anywhere in the guard, so its exact position is not a reference).
    fn guard_correlation(&self, rel: usize) -> Option<f64> {
        let g = self.mode.guard_len();
        let n = self.mode.fft_size();
        let mut best: Option<f64> = None;
        for step in -4isize..=4 {
            let d = step * g as isize / 8;
            let first = rel as isize - g as isize + d;
            let last = rel as isize + n as isize + d;
            if first < 0 || last as usize > self.buf.len() {
                continue;
            }
            let first = first as usize;
            let (mut c, mut p) = (Cplx::zero(), 0.0f64);
            for i in 0..g {
                let (a, b) = (self.buf[first + i], self.buf[first + n + i]);
                c += a * b.conj();
                p += a.norm_sqr() + b.norm_sqr();
            }
            let rho = if p > 0.0 { 2.0 * c.norm() / p } else { 0.0 };
            best = Some(best.map_or(rho, |b: f64| b.max(rho)));
        }
        best
    }

    /// Drop samples when no timing exists yet (bounded memory).
    pub fn trim_unsynchronised(&mut self) {
        if self.next_start.is_none() && self.buf.len() > 8 * self.mode.symbol_len() {
            let drop = self.buf.len() - 4 * self.mode.symbol_len();
            self.buf.drain(..drop);
            self.buf_base += drop as i64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddc::resampler::ComplexResampler;

    fn load(path: &str) -> Vec<Cplx> {
        let raw = std::fs::read(path).expect("capture file");
        raw.chunks_exact(8)
            .map(|c| {
                Cplx::new(
                    f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                    f64::from(f32::from_le_bytes([c[4], c[5], c[6], c[7]])),
                )
            })
            .collect()
    }

    fn to_core_rate(path: &str, rate: f64) -> Vec<Cplx> {
        let raw = std::fs::read(path).expect("capture file");
        let iq: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut resampler = ComplexResampler::new(rate, f64::from(SAMPLE_RATE));
        let mut converted: Vec<f32> = Vec::new();
        resampler.process_f32_into(&iq, &mut converted);
        converted
            .chunks_exact(2)
            .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
            .collect()
    }

    /// Feed the whole capture in worker-sized blocks, collect the detection events and the
    /// symbol windows the stage emits.
    fn run(iq: &[Cplx]) -> (Vec<Event>, Vec<SymbolWindow>) {
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut events = Vec::new();
        let mut windows = Vec::new();
        for block in iq.chunks(3248) {
            events.extend(ts.push(block));
            while let Some(w) = ts.next_window() {
                windows.push(w);
            }
        }
        (events, windows)
    }

    #[test]
    fn detects_mode_b_and_times_the_committed_fixture() {
        let iq = load("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (events, windows) = run(&iq);
        assert!(
            events.iter().any(|e| matches!(e, Event::ModeDetected { mode: RobustnessMode::B, .. })),
            "mode B must be detected, got {events:?}"
        );
        assert!(windows.len() > 150, "expected a window per symbol, got {}", windows.len());
        // Clean fixture: the cyclic-prefix correlation of a correctly placed window is high.
        let good = windows.iter().filter(|w| w.guard_corr.unwrap_or(0.0) > 0.8).count();
        assert!(
            good * 10 >= windows.len() * 9,
            "only {good} of {} windows have a high guard correlation",
            windows.len()
        );
        // The windows advance on the symbol grid: 1280 samples in mode B.
        let mut step_bad = 0;
        for pair in windows.windows(2) {
            let d = pair[1].start - pair[0].start;
            if (d - 1280).abs() > 1 {
                step_bad += 1;
            }
        }
        assert_eq!(step_bad, 0, "windows must advance by one symbol (1280 samples)");
    }

    #[test]
    fn detects_mode_b_on_the_committed_live_fixture() {
        let iq = to_core_rate("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32", 48_828.125);
        let (events, windows) = run(&iq);
        assert!(
            events.iter().any(|e| matches!(e, Event::ModeDetected { mode: RobustnessMode::B, .. })),
            "mode B must be detected on the live fixture, got {events:?}"
        );
        assert!(!windows.is_empty(), "the live fixture must yield symbol windows");
    }
}
