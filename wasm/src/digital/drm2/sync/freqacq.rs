//! Coarse carrier acquisition from the three continuous frequency-reference pilots.
//!
//! Port of Dream's `CFreqSyncAcq` (`src/sync/FreqSyncAcq.cpp`): the DRM signal carries three
//! continuous pilots at 750, 2250 and 3000 Hz above the DC carrier in every robustness mode.
//! This stage averages periodograms of a long FFT, normalises them by a smoothed noise floor,
//! and looks for the three pilots at those spacings; their position gives the DC carrier's
//! frequency, which the chain removes before the OFDM demodulation (half a carrier spacing
//! already destroys the constellation).
//!
//! Constants cross-checked against Dream's `FreqSyncAcq.h`/`.cpp`:
//! `iFrAcFFTSize = RMB_FFT_SIZE_N * NUM_BLOCKS_4_FREQ_ACQU` = 1024 * 6 = 6144,
//! `NUM_FFT_RES_AV_BLOCKS` = 6 * (3 - 1) + 1 = 13, `LAMBDA_FREQ_IIR_FILT` = 0.87,
//! `PEAK_BOUND_FILT2SIGNAL_1` = 9, `MAX_RAT_PEAKS_AT_PIL_POS_HIGH` = 0.99,
//! `MAX_RAT_PEAKS_AT_PIL_POS_LOW` = 0.8. The hop between periodograms is one mode-B symbol
//! (1280 samples), which is how Dream keeps the analysis span bounded.
//!
//! One deliberate addition, documented in the reference port: the mirrored pilot pattern is
//! searched too, so a spectrally inverted signal (an LSB reception, or a conjugated DDC
//! output) is detected rather than failing.

use std::collections::VecDeque;

use crate::digital::drm2::params::SAMPLE_RATE;
use crate::digital::drm2::dsp::FullFft;

/// FFT length: 6 mode-B FFTs, so the resolution is 48000/6144 = 7.8125 Hz.
const FFT_LEN: usize = 6 * 1024;
/// The three continuous pilots' offsets from DC, in Hz.
const PILOT_HZ: [f64; 3] = [750.0, 2250.0, 3000.0];
/// Number of periodograms averaged (`NUM_FFT_RES_AV_BLOCKS`).
const NUM_AVERAGE: usize = 13;
/// Hop between periodograms, samples (one mode-B symbol).
const HOP: usize = 1280;
/// Samples one acquisition needs before it can decide.
pub const ANALYSIS_SPAN: usize = FFT_LEN + (NUM_AVERAGE - 1) * HOP;
/// One-pole smoothing factor of the noise-floor estimate (`LAMBDA_FREQ_IIR_FILT`).
const LAMBDA_FLOOR: f64 = 0.87;
/// Detection threshold on the sum of the three normalised pilot powers.
const PEAK_BOUND: f64 = 9.0;
/// Sinusoid rejection ratios (`MAX_RAT_PEAKS_AT_PIL_POS_HIGH` / `_LOW`).
const MAX_RATIO_HIGH: f64 = 0.99;
const MAX_RATIO_LOW: f64 = 0.8;

/// A successful acquisition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Acquisition {
    /// Frequency of the DRM DC carrier in the input spectrum, Hz.
    pub dc_hz: f64,
    /// The pilots were found mirrored: the spectrum is inverted.
    pub inverted: bool,
    /// Detection metric: the sum of the three normalised pilot powers (>= [`PEAK_BOUND`]).
    pub score: f64,
    /// Mean input power over the analysed span, |sample|^2.
    pub power: f64,
}

/// Streaming coarse frequency acquisition.
pub struct FreqAcquisition {
    fft: FullFft,
    window: Vec<f64>,
    window_energy: f64,
    /// The newest [`FFT_LEN`] input samples, real then imaginary.
    history_re: VecDeque<f64>,
    history_im: VecDeque<f64>,
    /// Samples taken since the last periodogram.
    since_last: usize,
    /// The averaged periodograms (each `FFT_LEN` long, index 0 = -fs/2), oldest first.
    psds: VecDeque<Vec<f64>>,
    psd_sum: Vec<f64>,
    /// Also search the mirrored pilot pattern.
    allow_inverted: bool,
    work_re: Vec<f64>,
    work_im: Vec<f64>,
    /// Samples pushed so far (diagnostics).
    pub pushed: u64,
}

impl FreqAcquisition {
    pub fn new(allow_inverted: bool) -> Self {
        let window: Vec<f64> = (0..FFT_LEN)
            .map(|i| {
                let x = 2.0 * core::f64::consts::PI * i as f64 / (FFT_LEN - 1) as f64;
                0.54 - 0.46 * x.cos() // Hamming, as Dream's windowing
            })
            .collect();
        Self {
            fft: FullFft::new(FFT_LEN),
            window_energy: window.iter().map(|w| w * w).sum(),
            window,
            history_re: VecDeque::with_capacity(FFT_LEN),
            history_im: VecDeque::with_capacity(FFT_LEN),
            since_last: 0,
            psds: VecDeque::with_capacity(NUM_AVERAGE),
            psd_sum: vec![0.0; FFT_LEN],
            allow_inverted,
            work_re: vec![0.0; FFT_LEN],
            work_im: vec![0.0; FFT_LEN],
            pushed: 0,
        }
    }

    pub fn reset(&mut self) {
        self.history_re.clear();
        self.history_im.clear();
        self.since_last = 0;
        self.psds.clear();
        self.psd_sum.iter_mut().for_each(|v| *v = 0.0);
        self.pushed = 0;
    }

    /// Feed interleaved complex baseband samples (I, Q, I, Q, ...). Returns an acquisition as
    /// soon as an averaged spectrum holds the three pilots.
    pub fn push_iq(&mut self, iq: &[f64]) -> Option<Acquisition> {
        for pair in iq.chunks_exact(2) {
            if self.history_re.len() == FFT_LEN {
                self.history_re.pop_front();
                self.history_im.pop_front();
            }
            self.history_re.push_back(pair[0]);
            self.history_im.push_back(pair[1]);
            self.pushed += 1;
            self.since_last += 1;
            if self.history_re.len() == FFT_LEN && self.since_last >= HOP {
                self.since_last = 0;
                self.add_periodogram();
                if self.psds.len() == NUM_AVERAGE {
                    if let Some(found) = self.detect() {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// One windowed FFT of the newest samples, stored centred (index 0 = -fs/2).
    fn add_periodogram(&mut self) {
        for (i, w) in self.window.iter().enumerate() {
            self.work_re[i] = self.history_re[i] * w;
            self.work_im[i] = self.history_im[i] * w;
        }
        self.fft.forward_re_im(&mut self.work_re, &mut self.work_im);
        let half = FFT_LEN / 2;
        // Rotate so index j is the frequency (j - N/2) * fs / N.
        let mut psd = vec![0.0f64; FFT_LEN];
        for (j, p) in psd.iter_mut().enumerate() {
            let k = (j + half) % FFT_LEN;
            *p = self.work_re[k] * self.work_re[k] + self.work_im[k] * self.work_im[k];
        }
        if self.psds.len() == NUM_AVERAGE {
            if let Some(old) = self.psds.pop_front() {
                for (acc, v) in self.psd_sum.iter_mut().zip(&old) {
                    *acc -= v;
                }
            }
        }
        for (acc, v) in self.psd_sum.iter_mut().zip(&psd) {
            *acc += v;
        }
        self.psds.push_back(psd);
    }

    /// Average, normalise by the noise floor and look for the pilot pattern.
    fn detect(&mut self) -> Option<Acquisition> {
        let n = FFT_LEN;
        let psd: Vec<f64> = self.psd_sum.iter().map(|v| v / NUM_AVERAGE as f64).collect();

        // Noise floor: one-pole smoothing forward and backward, averaged (Dream).
        let mut forward = vec![0.0f64; n];
        let mut backward = vec![0.0f64; n];
        forward[0] = psd[0];
        for i in 1..n {
            forward[i] = (forward[i - 1] - psd[i]) * LAMBDA_FLOOR + psd[i];
        }
        backward[n - 1] = psd[n - 1];
        for i in (0..n - 1).rev() {
            backward[i] = (backward[i + 1] - psd[i]) * LAMBDA_FLOOR + psd[i];
        }
        let normalised: Vec<f64> = (0..n)
            .map(|i| {
                let floor = 0.5 * (forward[i] + backward[i]);
                if floor > 0.0 {
                    psd[i] / floor
                } else {
                    0.0
                }
            })
            .collect();

        let fs = f64::from(SAMPLE_RATE);
        let bin_hz = fs / n as f64;
        // The scan runs over array indices (index `half` is 0 Hz), and the pilots must fit
        // inside the spectrum: 3000 Hz above DC plus a bin of slack.
        let half = (n / 2) as isize;
        let margin = (PILOT_HZ[2] / bin_hz).ceil() as isize + 1;
        let lo = margin;
        let hi = n as isize - 1 - margin;
        let pilot_bins: Vec<isize> =
            PILOT_HZ.iter().map(|hz| (hz / bin_hz).round() as isize).collect();

        let mut best: Option<(isize, bool, f64)> = None;
        let orientations: &[bool] = if self.allow_inverted { &[false, true] } else { &[false] };
        for &inverted in orientations {
            for d in lo..=hi {
                let at = |off: isize| -> Option<usize> {
                    let idx = if inverted { d - off } else { d + off };
                    if idx < 0 || idx >= n as isize {
                        None
                    } else {
                        Some(idx as usize)
                    }
                };
                let (Some(a), Some(b), Some(c)) = (
                    at(pilot_bins[0]),
                    at(pilot_bins[1]),
                    at(pilot_bins[2]),
                ) else {
                    continue;
                };
                let score = normalised[a] + normalised[b] + normalised[c];
                if score <= PEAK_BOUND {
                    continue;
                }
                // Reject a single strong sinusoid: the three pilots must carry similar power.
                let mut v = [normalised[a], normalised[b], normalised[c]];
                v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(core::cmp::Ordering::Equal));
                if v[2] > 0.0 && (v[1] / v[2] < MAX_RATIO_HIGH || v[0] / v[2] < MAX_RATIO_LOW) {
                    continue;
                }
                if best.is_none_or(|(_, _, s)| score > s) {
                    best = Some((d, inverted, score));
                }
            }
        }

        // Parseval, with the window's energy: mean power over the analysed span.
        let power = psd.iter().sum::<f64>() / (n as f64 * self.window_energy);
        best.map(|(d, inverted, score)| Acquisition {
            dc_hz: (d - half) as f64 * bin_hz,
            inverted,
            score,
            power,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddc::resampler::ComplexResampler;

    /// Read an interleaved f32 I/Q file into f64 samples.
    fn load(path: &str) -> Vec<f64> {
        let raw = std::fs::read(path).expect("capture file");
        raw.chunks_exact(4)
            .map(|c| f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect()
    }

    /// Resample a capture at `rate` to the core rate, as the pipeline does before the chain.
    fn to_core_rate(path: &str, rate: f64) -> Vec<f64> {
        let iq = load(path);
        let mut resampler = ComplexResampler::new(rate, f64::from(SAMPLE_RATE));
        let mut converted: Vec<f32> = Vec::new();
        resampler.process_into(&iq, &mut converted);
        converted.into_iter().map(f64::from).collect()
    }

    #[test]
    fn acquires_the_committed_fixture() {
        let iq = load("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let mut acq = FreqAcquisition::new(true);
        let found = acq.push_iq(&iq).expect("the fixture's pilots are found");
        assert!(!found.inverted, "the fixture is not inverted");
        assert!(found.score > PEAK_BOUND, "score {:.1} must beat the bound", found.score);
        // The fixture's carrier sits within one carrier spacing of DC (46.875 Hz in mode B);
        // the exact figure is pinned by the receiver's own acquisition tests.
        assert!(found.dc_hz.abs() < 47.0, "fixture DC offset {:.1} Hz", found.dc_hz);
        assert!(found.power > 0.0);
    }

    #[test]
    fn acquires_the_committed_live_fixture_at_the_documented_offset() {
        // The committed bench capture is the regression base: `docs/en/DRM_BENCH.md` and the
        // receiver's own comments record its carrier at +121 Hz (2.58 carrier spacings of
        // 46.875 Hz). Acquisition measures the DC carrier, so allow one carrier.
        let iq = to_core_rate("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32", 48_828.125);
        let mut acq = FreqAcquisition::new(true);
        let found = acq.push_iq(&iq).expect("the live fixture's pilots are found");
        assert!(!found.inverted, "the bench loop is not spectrally inverted");
        assert!(found.score > PEAK_BOUND);
        assert!(
            (found.dc_hz - 121.0).abs() < 47.0,
            "fixture DC offset {:.1} Hz, expected about +121 Hz",
            found.dc_hz
        );
    }

    #[test]
    fn acquires_a_fresh_capture_when_one_is_present() {
        // A live capture in /tmp is a bonus, not a fixture: its carrier offset depends on the
        // session's tuning, so this only checks that acquisition succeeds and lands inside the
        // signal (no exact value to assert). Absent (CI, another machine) the test is a no-op.
        let path = "/tmp/lvl.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let iq = to_core_rate(path, 48_828.125);
        let mut acq = FreqAcquisition::new(true);
        let found = acq.push_iq(&iq).expect("the live capture's pilots are found");
        assert!(!found.inverted);
        assert!(
            found.dc_hz.abs() < 5.0 * 46.875,
            "live DC offset {:.1} Hz is outside the signal",
            found.dc_hz
        );
    }

    #[test]
    fn noise_does_not_acquire() {
        // A deterministic pseudo-random signal: no pilots, no acquisition.
        let mut state = 0x1234_5678u32;
        let mut iq = Vec::with_capacity(ANALYSIS_SPAN * 2);
        for _ in 0..ANALYSIS_SPAN {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let re = (state >> 8) as f64 / 8_388_608.0 - 1.0;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let im = (state >> 8) as f64 / 8_388_608.0 - 1.0;
            iq.push(re * 0.05);
            iq.push(im * 0.05);
        }
        let mut acq = FreqAcquisition::new(true);
        assert!(acq.push_iq(&iq).is_none(), "noise must not be acquired");
    }

    #[test]
    fn an_inverted_spectrum_is_detected() {
        // Conjugate the fixture's spectrum by conjugating its samples (I, -Q).
        let iq = load("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let flipped: Vec<f64> = iq
            .chunks_exact(2)
            .flat_map(|p| [p[0], -p[1]])
            .collect();
        let mut acq = FreqAcquisition::new(true);
        let found = acq.push_iq(&flipped).expect("the mirrored pattern is found");
        assert!(found.inverted, "conjugated input reports an inverted spectrum");
    }
}
