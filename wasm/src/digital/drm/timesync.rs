//! Time synchronisation: robustness-mode detection and OFDM symbol timing from the
//! guard-interval (cyclic-prefix) autocorrelation.
//!
//! This is a deliberately simple first implementation: it evaluates the normalised
//! guard correlation for every robustness mode at a fixed stride and averages each
//! mode's correlation over the symbol period, then picks the mode whose correlation
//! peaks highest. A fine search at stride 1 refines the guard-start position to the
//! sample. It is exact for the clean synthesised fixture and is the hook where a
//! full (decimated, sliding-DFT) synchroniser like Dream's would slot in.

use crate::digital::drm::params::RobustnessMode;
use crate::digital::drm::Cplx;

/// Stride of the coarse correlation sweep, samples.
const STEP: usize = 4;
/// Minimum average peak correlation to accept a mode (clean signals reach ~1.0).
const MIN_PEAK: f64 = 0.4;

/// Result of acquisition: the detected mode and the absolute sample index (relative
/// to the buffer start) of the guard-interval start of a symbol.
#[derive(Debug, Clone, Copy)]
pub struct Acquired {
    pub mode: RobustnessMode,
    /// Index of the first sample of the guard interval (cyclic prefix) of a symbol.
    pub guard_start: usize,
}

/// Detect the robustness mode and symbol timing in `buf`. Needs at least a few symbol
/// periods of signal.
pub fn acquire(buf: &[Cplx]) -> Option<Acquired> {
    let mut best_mode: Option<RobustnessMode> = None;
    let mut best_score = MIN_PEAK;
    let mut best_phase = 0usize;

    for mode in RobustnessMode::ALL {
        let g = mode.guard_len();
        let nu = mode.fft_size();
        let ts = mode.symbol_len();
        if buf.len() < nu + g + ts {
            continue;
        }
        let slots = ts / STEP;
        let mut prof = vec![0.0f64; slots];
        let mut cnt = vec![0usize; slots];
        let span = nu + g;
        let mut t = 0usize;
        while t + span <= buf.len() {
            let (c, p) = guard_corr(buf, t, g, nu);
            let rho = if p > 0.0 { 2.0 * c.norm() / p } else { 0.0 };
            let slot = (t % ts) / STEP;
            prof[slot] += rho;
            cnt[slot] += 1;
            t += STEP;
        }
        let (peak, phase) = prof
            .iter()
            .zip(cnt.iter())
            .enumerate()
            .filter(|(_, (_, &c))| c > 0)
            .map(|(i, (&v, &c))| (v / c as f64, i * STEP))
            .fold((0.0f64, 0usize), |a, b| if b.0 > a.0 { b } else { a });
        if peak > best_score {
            best_score = peak;
            best_mode = Some(mode);
            best_phase = phase;
        }
    }

    let mode = best_mode?;
    // Refine to the sample: search ±STEP around the coarse phase.
    let g = mode.guard_len();
    let nu = mode.fft_size();
    let ts = mode.symbol_len();
    let lo = best_phase.saturating_sub(STEP);
    let hi = (best_phase + STEP).min(ts - 1);
    let mut guard_start = best_phase;
    let mut best = 0.0f64;
    for phase in lo..=hi {
        // Average the correlation over every available symbol at this phase.
        let (mut acc_c, mut acc_p) = (Cplx::new(0.0, 0.0), 0.0f64);
        let mut n = 0usize;
        let mut t = phase;
        while t + nu + g <= buf.len() {
            let (c, p) = guard_corr(buf, t, g, nu);
            acc_c = acc_c + c;
            acc_p += p;
            n += 1;
            t += ts;
        }
        let score = if acc_p > 0.0 { 2.0 * acc_c.norm() / acc_p } else { 0.0 };
        let _ = n;
        if score > best {
            best = score;
            guard_start = phase;
        }
    }
    Some(Acquired { mode, guard_start })
}

/// Guard correlation at `t`: Σ x[t+i]·conj(x[t+i+nu]) over the guard length, plus the
/// energy denominator Σ(|x[t+i]|² + |x[t+i+nu]|²).
fn guard_corr(buf: &[Cplx], t: usize, g: usize, nu: usize) -> (Cplx, f64) {
    let mut c = Cplx::new(0.0, 0.0);
    let mut p = 0.0f64;
    for i in 0..g {
        let a = buf[t + i];
        let b = buf[t + i + nu];
        c = c + a * b.conj();
        p += a.norm_sqr() + b.norm_sqr();
    }
    (c, p)
}
