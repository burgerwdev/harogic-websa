//! STFT Wiener denoiser for narrowband voice — the analog path's noise-reduction stage.
//!
//! Standard short-time spectral subtraction with a Wiener-shaped gain: a Hann-windowed STFT at 75%
//! overlap, a smoothed low quantile of the receive band's bin powers as the noise floor, and
//! `g = max(floor, 1 - noise/(power + eps))`. The floor is what keeps the residual from turning
//! into isolated "musical" tones: without it, bins that happen to dip below the estimate get
//! slammed to zero and the result sounds like a ring modulator.
//!
//! Streaming: the frame overlap, the noise estimate and the output FIFO all carry across blocks.
//! The equal-length [`AudioStage`] interface has a fixed `FRAME - 1` (511 sample) delay: the
//! first sample cannot be emitted before its 512-sample analysis frame is complete. Native
//! callers that account for completed hops use [`Wiener::process_available_into`] instead;
//! that interface emits 128 samples after the first 512, retaining 384 samples of overlap.
//!
//! This is deliberately *not* an ML speech enhancer: it must run in the wasm audio budget with no
//! dependencies, and it must be provably safe to disable (the RAW/digital path never sees it).

use crate::fft::Fft;
use crate::plugin::AudioStage;

const FRAME: usize = 512;
const HOP: usize = FRAME / 4;
const GAIN_FLOOR: f64 = 0.1;        // -20 dB: the musical-noise guard (strength 1.0)
/// The gentlest suppression a strength of 0 asks for (-6 dB): a knob that goes to "off" is the
/// switch's job, so the weakest setting still removes something audible.
const GAIN_FLOOR_WEAK: f64 = 0.501_187_233_627_296_4;   // 10^(-6/20), -6 dB

pub struct Wiener {
    fft: Fft,
    window: Vec<f64>,
    /// Running input frame (the last `FRAME` samples seen).
    frame: Vec<f64>,
    /// Input samples collected towards a complete frame, including retained overlap.
    filled: usize,
    /// Exclusive end of the last held input within `frame`; zero means it is safe to adapt.
    held_until: usize,
    /// Current frame's power in each FFT bin.
    power: Vec<f64>,
    noise_bins: core::ops::Range<usize>,
    noise_scratch: Vec<f64>,
    floor: f64,
    warmed: bool,
    /// Overlap-add accumulator; its first `HOP` samples are complete after each frame.
    out_buf: Vec<f64>,
    /// Samples produced by the STFT but not yet handed to the caller.
    pending: Vec<f64>,
    /// Startup delay for the equal-length interface, consumed once per stream.
    padding_remaining: usize,
    fft_re: Vec<f64>,
    fft_im: Vec<f64>,
    /// COLA normalisation: the sum of the squared window over the overlapping frames.
    norm: f64,
    /// The lowest gain the Wiener shape may apply (the musical-noise guard).
    gain_floor: f64,
    pub frames: u64,
}

impl Default for Wiener {
    fn default() -> Self {
        Self::new()
    }
}

impl Wiener {
    pub fn new() -> Self {
        Self::with_strength(1.0)
    }

    /// `strength` (0..1) sets how far the noise floor may be pushed down: -6 dB at 0, -20 dB at 1.
    /// The musical-noise guard is this same floor, so one number controls both.
    pub fn with_strength(strength: f64) -> Self {
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
            held_until: 0,
            power: vec![0.0; n],
            noise_bins: 1..n / 2,
            noise_scratch: Vec::with_capacity(n / 2),
            floor: 0.0,
            warmed: false,
            out_buf: vec![0.0; n],
            pending: Vec::new(),
            padding_remaining: FRAME - 1,
            fft_re: vec![0.0; n],
            fft_im: vec![0.0; n],
            norm: norm.max(1e-9),
            gain_floor: Self::floor_for(strength),
            frames: 0,
        }
    }

    /// The gain floor a strength asks for, interpolated in dB between -6 and -20.
    fn floor_for(strength: f64) -> f64 {
        let s = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 1.0 };
        GAIN_FLOOR_WEAK * (GAIN_FLOOR / GAIN_FLOOR_WEAK).powf(s)
    }

    /// Change how hard the reducer pushes the noise down (0..1).
    pub fn set_strength(&mut self, strength: f64) {
        self.gain_floor = Self::floor_for(strength);
    }

    pub fn gain_floor(&self) -> f64 {
        self.gain_floor
    }

    /// Estimate noise inside the receive band: out-of-band filter rejection is
    /// not a measurement of the in-band noise. Very narrow bands cannot separate
    /// a Hann-spread CW tone from noise reliably; retain the conservative full
    /// spectrum estimate when fewer than eight bins are available.
    pub fn set_noise_band(&mut self, rate: f64, low_hz: f64, high_hz: f64) {
        let first = ((low_hz * FRAME as f64 / rate).ceil() as usize).clamp(1, FRAME / 2 - 1);
        let end = ((high_hz * FRAME as f64 / rate).floor() as usize + 1).clamp(first + 1, FRAME / 2);
        self.noise_bins = if end - first >= 8 { first..end } else { 1..FRAME / 2 };
    }

    /// Per-bin gain: the Wiener shape against the estimated noise floor.
    pub fn gains(&self) -> Vec<f64> {
        let mut gains = vec![0.0; self.power.len()];
        for (k, gain) in gains.iter_mut().enumerate() {
            *gain = (1.0 - self.floor / (self.power[k] + 1e-12)).clamp(self.gain_floor, 1.0);
        }
        gains
    }

    /// Exact order statistic, with reusable scratch and no max-power histogram
    /// bins: one strong carrier must not set the resolution of the noise floor.
    fn quantile(power: &mut [f64], q: f64) -> f64 {
        let target = ((power.len() - 1) as f64 * q) as usize;
        *power.select_nth_unstable_by(target, f64::total_cmp).1
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
            self.power[k] = power;
        }
        if self.held_until == 0 {
            self.noise_scratch.clear();
            self.noise_scratch.extend_from_slice(&self.power[self.noise_bins.clone()]);
            // Gaussian noise has exponential FFT-bin power. Its lower quartile
            // is -ln(0.75) times the mean noise PSD used in the Wiener gain.
            let floor = Self::quantile(&mut self.noise_scratch, 0.25) / -0.75_f64.ln();
            self.floor = if self.warmed {
                self.floor * 0.8 + floor * 0.2
            } else {
                self.warmed = true;
                floor
            };
        }
        for k in 0..FRAME {
            let gain = (1.0 - self.floor / (self.power[k] + 1e-12)).clamp(self.gain_floor, 1.0);
            self.fft_re[k] *= gain;
            self.fft_im[k] *= gain;
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

    fn ingest(&mut self, input: &[f32], hold: bool) {
        let mut index = 0;
        while index < input.len() {
            let take = (FRAME - self.filled).min(input.len() - index);
            for k in 0..take {
                self.frame[self.filled + k] = input[index + k] as f64;
            }
            self.filled += take;
            if hold {
                self.held_until = self.filled;
            }
            index += take;
            if self.filled == FRAME {
                self.frame_into();
                // Slide by one hop: keep the newest FRAME-HOP samples at the front.
                self.frame.copy_within(HOP.., 0);
                self.filled = FRAME - HOP;
                self.held_until = self.held_until.saturating_sub(HOP);
            }
        }
    }

    /// Emit completed 128-sample hops without padding. The first hop needs 512 input samples;
    /// each subsequent hop needs 128. Input partitioning does not change the emitted sequence.
    /// A frame containing any held sample advances the stream without updating its noise floor.
    /// Use this interface or the equal-length `AudioStage` interface consistently until reset.
    pub fn process_available_into(&mut self, input: &[f32], hold: bool, out: &mut Vec<f32>) {
        self.ingest(input, hold);
        out.clear();
        out.extend(self.pending.drain(..).map(|sample| sample as f32));
    }
}

impl AudioStage for Wiener {
    fn id(&self) -> &'static str {
        "wiener"
    }

    fn process_into(&mut self, input: &[f32], hold: bool, out: &mut Vec<f32>) {
        self.ingest(input, hold);
        out.clear();
        let padding = self.padding_remaining.min(input.len());
        out.resize(padding, 0.0);
        self.padding_remaining -= padding;
        // Waiting for FRAME-1 samples once makes every subsequent output causal, including
        // one-sample callbacks. Padding at each callback's end would insert gaps into the PCM.
        let available = input.len() - padding;
        out.extend(self.pending.drain(..available).map(|sample| sample as f32));
    }

    fn reset(&mut self) {
        self.frame.iter_mut().for_each(|v| *v = 0.0);
        self.filled = 0;
        self.held_until = 0;
        self.power.iter_mut().for_each(|v| *v = 0.0);
        self.floor = 0.0;
        self.warmed = false;
        self.out_buf.iter_mut().for_each(|v| *v = 0.0);
        self.pending.clear();
        self.padding_remaining = FRAME - 1;
        self.frames = 0;
    }

    fn set_strength(&mut self, strength: f64) -> bool {
        Wiener::set_strength(self, strength);
        true
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
    fn noise_quantile_is_not_scaled_by_a_strong_carrier() {
        let mut power = vec![1.0; 255];
        power[10] = 1e12;
        power[11] = 2e11;
        assert_eq!(Wiener::quantile(&mut power, 0.25), 1.0);
    }

    #[test]
    fn receive_band_noise_is_reduced_without_erasing_a_continuous_tone() {
        use crate::ddc::fir::{design_lowpass, FirState};
        let input = tone_plus_noise(48_000);
        let noisy: Vec<f64> = input.iter().enumerate().map(|(i, x)|
            *x as f64 - 0.3 * (2.0 * PI * 1_000.0 * i as f64 / RATE).sin()).collect();
        let mut filter = FirState::new(design_lowpass(RATE, 3_000.0, 257));
        let mut filtered = Vec::new(); filter.process_into(&noisy, &mut filtered);
        let noise: Vec<f32> = filtered.iter().map(|x| *x as f32).collect();
        let mut wiener = Wiener::new(); wiener.set_noise_band(RATE, 300.0, 2_400.0);
        let mut output = Vec::new(); wiener.process_into(&noise, false, &mut output);
        let rms = |x: &[f32]| (x[24_000..].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 24_000.0).sqrt();
        assert!(rms(&output) < 0.8 * rms(&noise), "in-band noise: {} -> {}", rms(&noise), rms(&output));
        let tone: Vec<f32> = (0..48_000).map(|i| (0.3 * (2.0*PI*1_000.0*i as f64/RATE).sin()) as f32).collect();
        wiener.reset(); wiener.process_into(&tone, false, &mut output);
        assert!(rms(&output) > 0.98 * rms(&tone), "a continuous carrier must survive");
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

    #[test]
    fn held_audio_advances_frames_without_learning_its_noise_floor() {
        let mut wiener = Wiener::new();
        let input = tone_plus_noise(FRAME);
        let mut output = Vec::new();
        wiener.process_into(&input, false, &mut output);
        let floor = wiener.floor;
        let frames = wiener.frames;
        let transient: Vec<f32> = input.iter().map(|sample| sample * 10.0).collect();
        wiener.process_into(&transient, true, &mut output);
        assert_eq!(wiener.floor, floor, "AudioStage::hold freezes noise adaptation");
        assert!(wiener.frames > frames, "hold still advances the stream clock");
    }

    #[test]
    fn callback_partition_does_not_insert_gaps_into_the_pcm_clock() {
        let input = tone_plus_noise(4096);
        let render = |size| {
            let mut wiener = Wiener::new();
            let mut output = Vec::new();
            let mut block = Vec::new();
            for chunk in input.chunks(size) {
                wiener.process_into(chunk, false, &mut block);
                assert_eq!(block.len(), chunk.len());
                output.extend_from_slice(&block);
            }
            output
        };
        let reference = render(input.len());
        for size in [1, 7, 127, 128, 511, 512, 960] {
            let output = render(size);
            let mismatch = output.iter().zip(&reference).position(|(a, b)| a != b);
            assert!(mismatch.is_none(), "block size {size} changes PCM at sample {mismatch:?}");
        }
    }

    #[test]
    fn unity_gain_follows_the_independent_overlap_add_and_causal_delay_reference() {
        let input = tone_plus_noise(2048);
        let mut wiener = Wiener::new();
        wiener.gain_floor = 1.0;
        let mut output = Vec::new();
        wiener.process_into(&input, false, &mut output);
        assert!(output[..FRAME - 1].iter().all(|sample| *sample == 0.0));
        for (n, actual) in output[FRAME - 1..].iter().enumerate() {
            // With unit spectral gain, analysis and synthesis multiply each sample by
            // Hann twice. Only frames starting at 0, HOP, ... exist during startup.
            let weight: f64 = (0..=n).step_by(HOP).map(|start| n - start)
                .filter(|&index| index < FRAME)
                .map(|index| (0.5 - 0.5 * (2.0 * PI * index as f64 / FRAME as f64).cos()).powi(2))
                .sum();
            let expected = input[n] as f64 * weight / 1.5;
            assert!((*actual as f64 - expected).abs() < 2e-7,
                "source sample {n}: {actual} vs independent OLA {expected}");
        }
    }

    #[test]
    fn completed_hops_match_the_equal_length_sequence_after_its_delay() {
        let input = tone_plus_noise(4096);
        let mut equal = Wiener::new();
        let mut delayed = Vec::new();
        equal.process_into(&input, false, &mut delayed);
        for size in [1, 7, 128, 512, 960, 4096] {
            let mut native = Wiener::new();
            let mut available = Vec::new();
            let mut block = Vec::new();
            for chunk in input.chunks(size) {
                native.process_available_into(chunk, false, &mut block);
                assert_eq!(block.len() % HOP, 0);
                available.extend_from_slice(&block);
            }
            assert_eq!(available.len(), input.len() - (FRAME - HOP));
            assert_eq!(delayed[FRAME - 1..], available[..delayed.len() - (FRAME - 1)]);
        }
    }

    #[test]
    fn held_samples_freeze_every_overlapping_frame_until_they_leave() {
        let mut wiener = Wiener::new();
        let mut output = Vec::new();
        wiener.process_available_into(&tone_plus_noise(FRAME), false, &mut output);
        let initial_floor = wiener.floor;
        wiener.process_available_into(&[9.0], true, &mut output);
        // Repeating one hop creates a line spectrum with empty bins, not a
        // broadband noise floor. Keep the noise continuous across these hops.
        let loud: Vec<f32> = tone_plus_noise(5 * HOP).iter().map(|sample| sample * 10.0).collect();
        let mut offset = 0;
        for hop in 0..4 {
            let count = if hop == 0 { HOP - 1 } else { HOP };
            wiener.process_available_into(&loud[offset..offset + count], false, &mut output);
            offset += count;
            assert_eq!(output.len(), HOP);
            assert_eq!(wiener.floor, initial_floor, "held input still appears in overlap {hop}");
        }
        wiener.process_available_into(&loud[offset..offset + HOP], false, &mut output);
        assert!(wiener.floor > initial_floor, "learning resumes after the last held sample leaves");
    }
}
