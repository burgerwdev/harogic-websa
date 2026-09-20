//! Adaptive notch for a single interfering tone.
//!
//! Detection is spectral, rejection is temporal: the block is transformed (512-point FFT) to find
//! the strongest bin, and a second-order RBJ notch is placed there. Doing the rejection in the time
//! domain keeps the rest of the spectrum untouched — the point of a notch is that it removes *one*
//! tone, not a band — while the spectral estimate is what makes it adaptive without a training
//! signal. A 512-point frame at 48 kHz resolves about 94 Hz, so the notch is widened by the biquad's
//! Q (default 12: roughly +/-400 Hz at 1 kHz) rather than pretending to bin precision.
//!
//! Only single-tone interference: a second tone would need a second section, and the stage is
//! registered as one plugin per concern.

use crate::fft::Fft;
use crate::plugin::AudioStage;

const FRAME: usize = 512;
/// Recompute the coefficients only when the estimate moves more than this (avoids coefficient
/// churn on noise-driven bin jitter).
const RETUNE_HZ: f64 = 20.0;
/// Ignore the lowest bins: DC and rumble dominate the peak search otherwise.
const MIN_HZ: f64 = 200.0;
const MAX_HZ: f64 = 8_000.0;

pub struct AdaptiveNotch {
    rate: f64,
    fft: Fft,
    window: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    /// Current notch frequency (0 = disabled).
    hz: f64,
    /// Biquad coefficients, normalised by a0.
    b0: f64, b1: f64, b2: f64, a1: f64, a2: f64,
    x1: f64, x2: f64, y1: f64, y2: f64,
    /// Rejection depth: 1 - (this much at the notch frequency).
    pub q: f64,
}

impl Default for AdaptiveNotch {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveNotch {
    pub fn new() -> Self {
        let mut window = vec![0.0; FRAME];
        for (k, value) in window.iter_mut().enumerate() {
            *value = 0.5 - 0.5 * (2.0 * core::f64::consts::PI * k as f64 / FRAME as f64).cos();
        }
        let mut notch = Self {
            rate: 48_000.0,
            fft: Fft::new(FRAME),
            window,
            re: vec![0.0; FRAME],
            im: vec![0.0; FRAME],
            hz: 0.0,
            b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
            q: 12.0,
        };
        notch.set_frequency(1_000.0);
        notch.hz = 0.0;
        notch
    }

    pub fn frequency_hz(&self) -> f64 {
        self.hz
    }

    /// The strongest tone in the block, or 0 when nothing stands out above the noise.
    fn estimate_hz(&mut self, input: &[f32]) -> f64 {
        let n = FRAME.min(input.len());
        if n < FRAME {
            return self.hz;
        }
        // Analyse the middle of the block so a block boundary cannot shift the estimate.
        let start = (input.len() - FRAME) / 2;
        for k in 0..FRAME {
            self.re[k] = input[start + k] as f64 * self.window[k];
            self.im[k] = 0.0;
        }
        self.fft.transform(&mut self.re, &mut self.im, false);
        let mut peak_bin = 0;
        let mut peak_power = 0.0;
        let mut mean_power = 0.0;
        let mut bins = 0.0;
        let lo = (MIN_HZ * FRAME as f64 / self.rate).ceil() as usize;
        let hi = (MAX_HZ * FRAME as f64 / self.rate).floor() as usize;
        let hi = hi.min(FRAME / 2);
        for bin in lo..hi {
            let power = self.re[bin] * self.re[bin] + self.im[bin] * self.im[bin];
            mean_power += power;
            bins += 1.0;
            if power > peak_power {
                peak_power = power;
                peak_bin = bin;
            }
        }
        if peak_bin == 0 || bins == 0.0 {
            return 0.0;
        }
        // A tone must stand out from the block's mean bin power, or the notch would chase noise.
        if peak_power < 40.0 * (mean_power / bins).max(1e-20) {
            return self.hz;
        }
        // Sub-bin interpolation on the log magnitudes. Without it the estimate is quantised to the
        // bin width (93.75 Hz here) and a Q of 12 - an 86 Hz notch - would miss the tone it was
        // aimed at by more than its own bandwidth, which measured as only a few dB of rejection.
        let mut delta = 0.0;
        if peak_bin > lo && peak_bin + 1 < hi {
            let log_at = |bin: usize| {
                (self.re[bin] * self.re[bin] + self.im[bin] * self.im[bin] + 1e-30).ln()
            };
            let (y0, y1, y2) = (log_at(peak_bin - 1), log_at(peak_bin), log_at(peak_bin + 1));
            let denom = y0 - 2.0 * y1 + y2;
            if denom.abs() > 1e-12 {
                delta = (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5);
            }
        }
        (peak_bin as f64 + delta) * self.rate / FRAME as f64
    }

    /// RBJ notch at `hz` (constant Q).
    fn set_frequency(&mut self, hz: f64) {
        let w0 = 2.0 * core::f64::consts::PI * hz.clamp(20.0, 0.45 * self.rate) / self.rate;
        let alpha = w0.sin() / (2.0 * self.q);
        let a0 = 1.0 + alpha;
        self.b0 = 1.0 / a0;
        self.b1 = -2.0 * w0.cos() / a0;
        self.b2 = 1.0 / a0;
        self.a1 = -2.0 * w0.cos() / a0;
        self.a2 = (1.0 - alpha) / a0;
    }
}

impl AudioStage for AdaptiveNotch {
    fn id(&self) -> &'static str {
        "notch"
    }

    fn process_into(&mut self, input: &[f32], _hold: bool, out: &mut Vec<f32>) {
        let estimate = self.estimate_hz(input);
        if estimate > 0.0 && (estimate - self.hz).abs() > RETUNE_HZ {
            self.hz = estimate;
            self.set_frequency(estimate);
        }
        out.clear();
        out.reserve(input.len());
        for sample in input {
            let x0 = *sample as f64;
            let y0 = self.b0 * x0 + self.b1 * self.x1 + self.b2 * self.x2
                - self.a1 * self.y1 - self.a2 * self.y2;
            self.x2 = self.x1;
            self.x1 = x0;
            self.y2 = self.y1;
            self.y1 = y0;
            out.push(y0 as f32);
        }
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
        self.hz = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::AudioStage;
    use core::f64::consts::PI;

    const RATE: f64 = 48_000.0;

    fn amplitude_at(values: &[f32], hz: f64) -> f64 {
        let n = values.len() as f64;
        let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
        for (k, value) in values.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * k as f64 / n).cos();
            let ph = 2.0 * PI * hz * k as f64 / RATE;
            re += *value as f64 * w * ph.cos();
            im -= *value as f64 * w * ph.sin();
            wsum += w;
        }
        (re * re + im * im).sqrt() / wsum
    }

    fn band_rms(values: &[f32]) -> f64 {
        (values.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
    }

    /// Broadband noise with one strong tone at `tone_hz`.
    fn noise_plus_tone(n: usize, tone_hz: f64) -> Vec<f32> {
        let mut seed = 999_u32;
        (0..n)
            .map(|k| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.1;
                (noise + 0.3 * (2.0 * PI * tone_hz * k as f64 / RATE).sin()) as f32
            })
            .collect()
    }

    /// Median spectral magnitude away from DC: the broadband noise floor, unaffected by whether a
    /// single tone is present (which is what "the channel survived" has to mean here — total RMS
    /// necessarily drops when a strong tone is removed).
    fn noise_floor(values: &[f32]) -> f64 {
        let n = 2048.min(values.len());
        let start = values.len() - n;
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        for k in 0..n {
            let w = 0.5 - 0.5 * (2.0 * PI * k as f64 / n as f64).cos();
            re[k] = values[start + k] as f64 * w;
        }
        let fft = crate::fft::Fft::new(n);
        fft.transform(&mut re, &mut im, false);
        let mut magnitudes: Vec<f64> = (4..n / 2)
            .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
            .collect();
        magnitudes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        magnitudes[magnitudes.len() / 2]
    }

    #[test]
    fn a_single_tone_is_notched_out_without_taking_the_channel_with_it() {
        let input = noise_plus_tone(24_000, 1_000.0);
        let mut notch = AdaptiveNotch::new();
        let mut out = Vec::new();
        notch.process_into(&input, false, &mut out);

        let before = amplitude_at(&input, 1_000.0);
        let after = amplitude_at(&out, 1_000.0);
        let depth_db = 20.0 * (after / before).log10();
        println!("notch: 1 kHz tone {before:.4} -> {after:.4} ({depth_db:.1} dB)");
        assert!(depth_db < -12.0, "the tone must be rejected, got {depth_db:.1} dB");
        assert!((notch.frequency_hz() - 1_000.0).abs() < 200.0, "estimated {}", notch.frequency_hz());

        // The channel (broadband noise) must survive: a stage that simply turned the gain down
        // would drop the floor with the tone.
        let floor_change_db = 20.0 * (noise_floor(&out) / noise_floor(&input)).log10();
        println!("notch: noise floor {floor_change_db:.2} dB");
        assert!(floor_change_db.abs() < 1.5, "the noise floor must be preserved: {floor_change_db:.2} dB");
    }

    #[test]
    fn a_different_tone_is_left_alone() {
        // The stage removes single-tone *interference*: here 1 kHz is 6x the wanted 3 kHz tone, so
        // the peak search has an unambiguous target and the wanted tone has to survive.
        let mut seed = 1234_u32;
        let input: Vec<f32> = (0..24_000)
            .map(|k| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.02;
                (noise
                    + 0.9 * (2.0 * PI * 1_000.0 * k as f64 / RATE).sin()
                    + 0.15 * (2.0 * PI * 3_000.0 * k as f64 / RATE).sin()) as f32
            })
            .collect();
        let mut notch = AdaptiveNotch::new();
        let mut out = Vec::new();
        notch.process_into(&input, false, &mut out);
        let wanted = amplitude_at(&out, 3_000.0) / amplitude_at(&input, 3_000.0);
        let rejected = amplitude_at(&out, 1_000.0) / amplitude_at(&input, 1_000.0);
        assert!((notch.frequency_hz() - 1_000.0).abs() < 50.0,
                "the notch must sit on the interferer, got {}", notch.frequency_hz());
        assert!(wanted > 0.5, "the 3 kHz tone must survive: ratio {wanted}");
        assert!(rejected < wanted / 4.0, "the interferer must be the one notched");
    }

    #[test]
    fn noise_alone_does_not_move_the_notch() {
        // Well-distributed noise (an LCG with a poor low-bit pattern looks like a tone to a peak
        // search, which would make this test pass or fail for the wrong reason).
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let input: Vec<f32> = (0..24_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 40) as f64 / 16_777_216.0 - 0.5) as f32 * 0.2
            })
            .collect();
        let mut notch = AdaptiveNotch::new();
        let mut out = Vec::new();
        notch.process_into(&input, false, &mut out);
        assert_eq!(notch.frequency_hz(), 0.0, "noise must not be treated as a tone");
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn the_output_keeps_the_block_length() {
        let mut notch = AdaptiveNotch::new();
        let mut out = Vec::new();
        for size in [1_usize, 960, 4096] {
            notch.process_into(&noise_plus_tone(size, 1_000.0), false, &mut out);
            assert_eq!(out.len(), size);
        }
    }
}
