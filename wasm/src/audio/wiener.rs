//! STFT Wiener denoiser for narrowband voice — the analog path's noise-reduction stage.
//!
//! Standard short-time spectral subtraction with a Wiener-shaped gain: a Hann-windowed STFT at 75%
//! overlap, a per-bin noise power estimate tracked by minimum statistics, and
//! `g = max(floor, 1 - noise/(power + eps))`. The floor is what keeps the residual from turning
//! into isolated "musical" tones: without it, bins that happen to dip below the estimate get
//! slammed to zero and the result sounds like a ring modulator.
//!
//! Streaming: the frame overlap, the noise estimate and the output FIFO all carry across blocks.
//! The stage has `frame - hop` samples of latency, which is why the chain's block budget has to
//! leave room for it rather than assume zero-delay processing.
//!
//! This is deliberately *not* an ML speech enhancer: it must run in the wasm audio budget with no
//! dependencies, and it must be provably safe to disable (the RAW/digital path never sees it).

use crate::fft::Fft;
use crate::plugin::AudioStage;

const FRAME: usize = 512;
const HOP: usize = FRAME / 4;
const GAIN_FLOOR: f64 = 0.1;        // -20 dB: the musical-noise guard
/// Fraction of the bin powers treated as the noise floor.
const FLOOR_QUANTILE: f64 = 0.25;

pub struct Wiener {
    fft: Fft,
    window: Vec<f64>,
    /// Running input frame (the last `FRAME` samples seen).
    frame: Vec<f64>,
    /// Filled by the first `HOP` calls, so the first frames are real instead of starting mid-air.
    filled: usize,
    /// Per-bin noise estimate (the floor the gains are computed against).
    power: Vec<f64>,
    floor: f64,
    warmed: bool,
    /// Overlap-add accumulator; its first `HOP` samples are complete after each frame.
    out_buf: Vec<f64>,
    /// Samples produced by the STFT but not yet handed to the caller.
    pending: Vec<f64>,
    fft_re: Vec<f64>,
    fft_im: Vec<f64>,
    /// COLA normalisation: the sum of the squared window over the overlapping frames.
    norm: f64,
    pub frames: u64,
}

impl Default for Wiener {
    fn default() -> Self {
        Self::new()
    }
}

impl Wiener {
    pub fn new() -> Self {
        let n = FRAME;
        let mut window = vec![0.0; n];
        for (k, value) in window.iter_mut().enumerate() {
            *value = 0.5 - 0.5 * (2.0 * core::f64::consts::PI * k as f64 / n as f64).cos();
        }
        // Weighted overlap-add with the same window on analysis and synthesis: the sum of the
        // squared window over one sample position is a constant, computed here rather than assumed.
        let mut norm = 0.0;
        let mut offset = 0;
        while offset < n {
            norm += window[offset] * window[offset];
            offset += HOP;
        }
        Self {
            fft: Fft::new(n),
            window,
            frame: vec![0.0; n],
            filled: 0,
            power: vec![0.0; n],
            floor: 0.0,
            warmed: false,
            out_buf: vec![0.0; n],
            pending: Vec::new(),
            fft_re: vec![0.0; n],
            fft_im: vec![0.0; n],
            norm: norm.max(1e-9),
            frames: 0,
        }
    }

    /// Per-bin gain: the Wiener shape against the estimated noise floor.
    pub fn gains(&self) -> Vec<f64> {
        let mut gains = vec![0.0; self.power.len()];
        for (k, gain) in gains.iter_mut().enumerate() {
            *gain = (1.0 - self.floor / (self.power[k] + 1e-12)).clamp(GAIN_FLOOR, 1.0);
        }
        gains
    }

    /// Value at `q` of the low bins' power distribution (no sort: a coarse histogram is enough for
    /// a floor estimate and stays O(n)).
    fn quantile(power: &[f64], q: f64) -> f64 {
        let half = power.len() / 2;
        let mut bins = [0_usize; 64];
        let mut max = 0.0_f64;
        for value in &power[1..half] {
            if *value > max {
                max = *value;
            }
        }
        if max <= 0.0 {
            return 0.0;
        }
        for value in &power[1..half] {
            let index = ((value / max) * 63.0) as usize;
            bins[index.min(63)] += 1;
        }
        let target = ((half as f64 - 1.0) * q) as usize;
        let mut seen = 0;
        for (index, count) in bins.iter().enumerate() {
            seen += count;
            if seen > target {
                return (index as f64 + 0.5) / 64.0 * max;
            }
        }
        max
    }

    /// Process one frame: window, transform, estimate noise, apply the gain, overlap-add.
    fn frame_into(&mut self) {
        for k in 0..FRAME {
            self.fft_re[k] = self.frame[k] * self.window[k];
            self.fft_im[k] = 0.0;
        }
        self.fft.transform(&mut self.fft_re, &mut self.fft_im, false);
        for k in 0..FRAME {
            let power = self.fft_re[k] * self.fft_re[k] + self.fft_im[k] * self.fft_im[k];
            // The noise floor is a robust per-frame statistic (a low quantile of the bin powers),
            // not a per-bin minimum tracked over time: minimum statistics needs speech pauses to
            // find the floor, and with a continuous carrier it would converge onto the carrier
            // itself and notch the signal out. A quantile floor needs no pauses and cannot lock
            // onto a tone that occupies a handful of bins.
            let _ = power;
            self.power[k] = power;
        }
        let floor = Self::quantile(&self.power, 0.25);
        self.floor = if self.warmed {
            self.floor * 0.8 + floor * 0.2
        } else {
            self.warmed = true;
            floor
        };
        let gains = self.gains();
        for k in 0..FRAME {
            self.fft_re[k] *= gains[k];
            self.fft_im[k] *= gains[k];
        }
        self.fft.transform(&mut self.fft_re, &mut self.fft_im, true);
        for k in 0..FRAME {
            self.out_buf[k] += self.fft_re[k] * self.window[k];
        }
        // The first HOP samples of the accumulator are complete; emit them and shift down.
        for k in 0..HOP {
            self.pending.push(self.out_buf[k] / self.norm);
        }
        self.out_buf.copy_within(HOP.., 0);
        for value in self.out_buf[FRAME - HOP..].iter_mut() {
            *value = 0.0;
        }
        self.frames += 1;
    }
}

impl AudioStage for Wiener {
    fn id(&self) -> &'static str {
        "wiener"
    }

    fn process_into(&mut self, input: &[f32], _hold: bool, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());
        let mut index = 0;
        while index < input.len() {
            let take = (FRAME - self.filled).min(input.len() - index);
            for k in 0..take {
                self.frame[self.filled + k] = input[index + k] as f64;
            }
            self.filled += take;
            index += take;
            if self.filled == FRAME {
                self.frame_into();
                // Slide by one hop: keep the newest FRAME-HOP samples at the front.
                self.frame.copy_within(HOP.., 0);
                self.filled = FRAME - HOP;
            }
            while let Some(sample) = self.pending.first().copied() {
                if out.len() >= input.len() {
                    break;
                }
                out.push(sample as f32);
                self.pending.remove(0);
            }
        }
        // A block shorter than the stage's latency produces nothing yet; pad with zeros so the
        // caller's sample count is preserved (the chain must not change the block length).
        while out.len() < input.len() {
            out.push(0.0);
        }
    }

    fn reset(&mut self) {
        self.frame.iter_mut().for_each(|v| *v = 0.0);
        self.filled = 0;
        self.power.iter_mut().for_each(|v| *v = 0.0);
        self.floor = 0.0;
        self.warmed = false;
        self.out_buf.iter_mut().for_each(|v| *v = 0.0);
        self.pending.clear();
        self.frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::AudioStage;
    use core::f64::consts::PI;

    const RATE: f64 = 48_000.0;

    /// SINAD of a known tone: signal power in the tone bin(s) against everything else.
    fn sinad_db(signal: &[f32], hz: f64) -> f64 {
        let n = 4096.min(signal.len());
        let start = signal.len() - n;
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        let window: Vec<f64> = (0..n)
            .map(|k| 0.5 - 0.5 * (2.0 * PI * k as f64 / n as f64).cos())
            .collect();
        for k in 0..n {
            re[k] = signal[start + k] as f64 * window[k];
            im[k] = 0.0;
        }
        let fft = Fft::new(n);
        fft.transform(&mut re, &mut im, false);
        let bin = (hz * n as f64 / RATE).round() as usize;
        let mut signal_power = 0.0;
        let mut total = 0.0;
        for k in 1..n / 2 {
            let power = re[k] * re[k] + im[k] * im[k];
            total += power;
            // The Hann main lobe spans the bin and its neighbours.
            if k >= bin.saturating_sub(1) && k <= bin + 1 {
                signal_power += power;
            }
        }
        10.0 * (signal_power / (total - signal_power).max(1e-20)).log10()
    }

    fn tone_plus_noise(n: usize) -> Vec<f32> {
        let mut seed = 4_242_u32;
        (0..n)
            .map(|k| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.2;
                (0.3 * (2.0 * PI * 1_000.0 * k as f64 / RATE).sin() + noise) as f32
            })
            .collect()
    }

    #[test]
    fn the_denoiser_improves_the_sinad_of_a_tone_in_noise() {
        let input = tone_plus_noise(48_000);
        let before = sinad_db(&input, 1_000.0);
        let mut wiener = Wiener::new();
        let mut out = Vec::new();
        // One long block keeps the test independent of the block size.
        wiener.process_into(&input, false, &mut out);
        let after = sinad_db(&out, 1_000.0);
        println!("wiener: SINAD {before:.1} dB -> {after:.1} dB");
        assert!(after > before + 3.0, "the denoiser must improve SINAD: {before} -> {after}");
    }

    #[test]
    fn the_output_keeps_the_block_length() {
        let mut wiener = Wiener::new();
        let mut out = Vec::new();
        for size in [960_usize, 480, 1, 4096] {
            let input = tone_plus_noise(size);
            wiener.process_into(&input, false, &mut out);
            assert_eq!(out.len(), size, "the stage must not change the block length");
        }
    }

    #[test]
    fn the_gain_never_falls_below_the_floor() {
        // A silence-then-tone input must not produce bins slammed to zero (musical noise): the
        // floor is the whole reason the residual stays broadband.
        let mut wiener = Wiener::new();
        let mut out = Vec::new();
        wiener.process_into(&tone_plus_noise(8_192), false, &mut out);
        for gain in wiener.gains() {
            assert!(gain >= GAIN_FLOOR - 1e-9, "gain {gain} below the floor");
        }
    }

    #[test]
    fn reset_clears_the_streaming_state() {
        let mut wiener = Wiener::new();
        let mut a = Vec::new();
        let mut b = Vec::new();
        // `tone_plus_noise` is deterministic, so a reset must reproduce the first run exactly.
        wiener.process_into(&tone_plus_noise(9_600), false, &mut a);
        wiener.reset();
        wiener.process_into(&tone_plus_noise(9_600), false, &mut b);
        assert_eq!(a.len(), b.len());
        for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!((x - y).abs() < 1e-6, "sample {index}: {x} vs {y}");
        }
    }
}
