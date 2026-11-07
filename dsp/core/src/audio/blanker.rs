//! Impulse noise blanker for clicks and ignition-style bursts in decoded audio.
//!
//! Detection is relative to a running RMS estimate. At most `window` consecutive samples above
//! the onset threshold are repaired; a longer high-level segment passes through and updates the
//! estimate, so a legitimate volume step cannot leave the stage permanently blanked. Short
//! detected impulses do not raise their own threshold. The first non-held block seeds the RMS.
//!
//! A repair interpolates to a clean endpoint only when that endpoint is already present in the
//! current block and within the remaining repair budget. Otherwise it holds the preceding output.
//! The budget and onset threshold carry across blocks. This keeps output length and latency
//! unchanged; interpolation near block edges can differ because the available lookahead differs.
//!
//! This optional stage runs after the audio AGC; it repairs residual impulses but
//! cannot prevent those impulses from affecting the upstream gain estimate.

use crate::plugin::AudioStage;

const DEFAULT_THRESHOLD_K: f64 = 6.0;
const DEFAULT_WINDOW: usize = 12;
/// Preserve the old estimator's 0.9 decay per nominal 128-sample Wiener hop,
/// independently of input chunking: alpha = 1 - 0.9^(1/128). Smooth power so
/// `level()` remains an RMS estimate, rather than a rectified mean amplitude.
const LEVEL_SMOOTH: f64 = 0.000_822_790_350_809_454_7;

pub struct ImpulseBlanker {
    threshold_k: f64,
    window: usize,
    level_sq: f64,
    primed: bool,
    /// Repaired samples in the current high-level run, capped at `window`.
    burst_len: usize,
    burst_threshold: f64,
    /// Most recent output, used when no clean lookahead is available.
    from: f32,
    /// Lifetime number of repaired samples, excluding the clean ramp endpoint.
    pub blanked_samples: u64,
}

impl Default for ImpulseBlanker {
    fn default() -> Self {
        Self::new()
    }
}

impl ImpulseBlanker {
    pub fn new() -> Self {
        Self {
            threshold_k: DEFAULT_THRESHOLD_K,
            window: DEFAULT_WINDOW,
            level_sq: 0.0,
            primed: false,
            burst_len: 0,
            burst_threshold: 0.0,
            from: 0.0,
            blanked_samples: 0,
        }
    }

    /// `window` is the maximum consecutive above-threshold samples to repair
    /// (at least one), including samples split across input blocks.
    /// Invalid (non-finite or non-positive) multipliers use the default threshold.
    pub fn with_threshold(k: f64, window: usize) -> Self {
        let threshold_k = if k.is_finite() && k > 0.0 { k } else { DEFAULT_THRESHOLD_K };
        Self { threshold_k, window: window.max(1), ..Self::new() }
    }

    /// The level the threshold is derived from (diagnostics).
    pub fn level(&self) -> f64 {
        self.level_sq.sqrt()
    }
}

impl AudioStage for ImpulseBlanker {
    fn id(&self) -> &'static str {
        "blanker"
    }

    fn process_into(&mut self, input: &[f32], hold: bool, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());
        if input.is_empty() {
            return;
        }
        if !self.primed {
            if hold {
                out.extend_from_slice(input);
                self.from = *input.last().unwrap();
                return;
            }
            self.level_sq = input.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
                / input.len() as f64;
            self.primed = true;
        }

        let mut index = 0;
        while index < input.len() {
            let sample = input[index];
            let threshold = if self.burst_len == 0 {
                (self.level() * self.threshold_k).max(1e-4)
            } else {
                self.burst_threshold
            };
            if (sample as f64).abs() > threshold {
                self.burst_threshold = threshold;
                if self.burst_len < self.window {
                    let remaining = self.window - self.burst_len;
                    let last = index + remaining.min(input.len() - index - 1);
                    let endpoint = (index + 1..=last)
                        .find(|&end| (input[end] as f64).abs() <= threshold);
                    let count = if let Some(end) = endpoint {
                        let count = end - index;
                        let target = input[end];
                        for step in 0..count {
                            let ratio = (step + 1) as f32 / (count + 1) as f32;
                            // Finite f32 endpoints can have an overflowing f32
                            // difference. Interpolate in f64 before narrowing.
                            out.push((self.from as f64
                                + (target as f64 - self.from as f64) * ratio as f64) as f32);
                        }
                        count
                    } else {
                        let count = remaining.min(input.len() - index);
                        out.resize(out.len() + count, self.from);
                        count
                    };
                    self.burst_len += count;
                    self.blanked_samples = self.blanked_samples.saturating_add(count as u64);
                    self.from = *out.last().unwrap();
                    index += count;
                    continue;
                }
                // A sustained signal exceeds the repair budget. Pass it and
                // let the estimator adapt until the run returns below onset.
            } else {
                self.burst_len = 0;
            }
            out.push(sample);
            self.from = sample;
            if !hold {
                self.level_sq += ((sample as f64).powi(2) - self.level_sq) * LEVEL_SMOOTH;
            }
            index += 1;
        }
    }

    fn reset(&mut self) {
        self.level_sq = 0.0;
        self.primed = false;
        self.burst_len = 0;
        self.burst_threshold = 0.0;
        self.from = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::AudioStage;
    use core::f64::consts::PI;

    const RATE: f64 = 48_000.0;

    fn tone_with_impulses(n: usize) -> Vec<f32> {
        (0..n)
            .map(|k| {
                let tone = 0.3 * (2.0 * PI * 1_000.0 * k as f64 / RATE).sin();
                let impulse = if k % 2_400 == 0 { 3.0 } else { 0.0 };
                (tone + impulse) as f32
            })
            .collect()
    }

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

    #[test]
    fn impulses_are_removed_and_the_tone_survives() {
        let input = tone_with_impulses(24_000);
        let mut blanker = ImpulseBlanker::new();
        let mut out = Vec::new();
        blanker.process_into(&input, false, &mut out);

        let peak_before = input.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        let peak_after = out.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        println!("blanker: peak {peak_before} -> {peak_after}, repaired samples {}",
                 blanker.blanked_samples);
        assert!(peak_after < peak_before * 0.25, "impulses must be gone: {peak_before} -> {peak_after}");
        assert!(blanker.blanked_samples >= 8, "the impulses must have been detected");

        // The carrier must survive: a stage that merely lowered the gain would fail this.
        let ratio = amplitude_at(&out, 1_000.0) / amplitude_at(&input, 1_000.0);
        assert!(ratio > 0.7, "the tone must survive the repair: ratio {ratio}");
    }

    #[test]
    fn clean_audio_is_left_alone() {
        let input: Vec<f32> = (0..24_000)
            .map(|k| (0.3 * (2.0 * PI * 1_000.0 * k as f64 / RATE).sin()) as f32)
            .collect();
        let mut blanker = ImpulseBlanker::new();
        let mut out = Vec::new();
        blanker.process_into(&input, false, &mut out);
        assert_eq!(blanker.blanked_samples, 0, "clean audio must not be blanked");
        assert_eq!(out, input, "the signal must pass unchanged");
    }

    #[test]
    fn the_output_keeps_the_block_length() {
        let mut blanker = ImpulseBlanker::new();
        let mut out = Vec::new();
        for size in [1_usize, 960, 4096] {
            blanker.process_into(&tone_with_impulses(size), false, &mut out);
            assert_eq!(out.len(), size);
        }
    }

    fn process_chunks(blanker: &mut ImpulseBlanker, input: &[f32], size: usize) -> Vec<f32> {
        let mut result = Vec::with_capacity(input.len());
        let mut block = Vec::new();
        for chunk in input.chunks(size) {
            blanker.process_into(chunk, false, &mut block);
            assert_eq!(block.len(), chunk.len(), "the sample clock must not change");
            result.extend_from_slice(&block);
        }
        result
    }

    fn primed_constant(level: f32) -> ImpulseBlanker {
        let mut blanker = ImpulseBlanker::new();
        blanker.process_into(&[level; 512], false, &mut Vec::new());
        blanker
    }

    fn assert_constant_reference(input: &[f32], size: usize) {
        // Independent signal oracle: replacing samples of a known constant by
        // isolated high-amplitude interference must recover that same constant.
        // It does not duplicate the blanker's detector, lookahead or estimator.
        let mut blanker = primed_constant(0.125);
        let output = process_chunks(&mut blanker, input, size);
        for (index, actual) in output.into_iter().enumerate() {
            assert!((actual - 0.125).abs() < 1e-6,
                "block size {size}, sample {index}: {actual} differs from clean reference 0.125");
        }
        assert!(blanker.blanked_samples > 0, "interference must have been detected");
    }

    #[test]
    fn constant_reference_recovers_impulses_across_block_boundaries() {
        for size in [1_usize, 2, 7, 12, 13, 14, 64, 128, 255, 960, 4096] {
            let mut input = vec![0.125; size * 4 + 40];
            // The first burst straddles a boundary. The second lands exactly at
            // a block's last sample, where no clean lookahead is available.
            input[size - 1..size + 2].fill(4.0);
            input[size * 3 - 1] = -4.0;
            assert_constant_reference(&input, size);
        }
    }

    #[test]
    fn constant_reference_recovers_repeated_bursts_within_window() {
        let mut input = vec![0.125; 8192];
        // A dense train of short impulses must not raise its own threshold.
        for start in (31..4096).step_by(37) {
            for (index, sample) in input[start..start + DEFAULT_WINDOW].iter_mut().enumerate() {
                *sample = if index % 2 == 0 { 4.0 } else { -4.0 };
            }
        }
        for size in [1_usize, 7, 13, 64, 128, 255, 960, 4096, input.len()] {
            assert_constant_reference(&input, size);
        }
    }

    #[test]
    fn sustained_bursts_pass_after_the_bounded_repair_and_short_pulses_recover() {
        // A continuous high segment can also be a legitimate volume step. It
        // must resume unchanged after the bounded repair, rather than locking
        // the level estimate to the preceding quiet signal forever.
        let mut input = vec![0.125; 12_288];
        input[64..2112].fill(4.0);
        input[11_000..11_008].fill(-4.0);
        let mut expected = input.clone();
        expected[64..64 + DEFAULT_WINDOW].fill(0.125);
        expected[11_000..11_008].fill(0.125);
        for size in [1_usize, 7, 13, 64, 128, 255, 960, input.len()] {
            let mut blanker = primed_constant(0.125);
            let output = process_chunks(&mut blanker, &input, size);
            assert_eq!(output, expected, "block size {size}");
            assert_eq!(blanker.blanked_samples, (DEFAULT_WINDOW + 8) as u64);
        }
    }

    #[test]
    fn clean_envelope_changes_preserve_the_analytic_tone_at_any_partition() {
        let input: Vec<f32> = (0..8192).map(|index| {
            let amplitude = [0.12, 0.35, 0.08, 0.22][index / 2048];
            (amplitude * (2.0 * PI * 997.0 * index as f64 / RATE).sin()) as f32
        }).collect();
        for size in [1_usize, 2, 7, 13, 64, 128, 255, 960, input.len()] {
            let mut blanker = primed_constant(0.125);
            assert_eq!(process_chunks(&mut blanker, &input, size), input,
                "clean envelope changes must pass unchanged at block size {size}");
            assert_eq!(blanker.blanked_samples, 0);
        }
    }

    #[test]
    fn sinusoidal_reference_bounds_short_repairs_at_any_partition() {
        let reference: Vec<f32> = (0..8192).map(|index| {
            (0.2 * (2.0 * PI * 500.0 * index as f64 / RATE).sin()) as f32
        }).collect();
        let mut damaged = reference.clone();
        let mut corrupt = vec![false; reference.len()];
        for start in (127..reference.len()).step_by(1024) {
            damaged[start..start + 5].fill(4.0);
            corrupt[start..start + 5].fill(true);
        }
        // The analytic tone has |dx/dn| <= A * 2*pi*f/Fs. Even when a block
        // ends inside a pulse and no future clean endpoint is available, a
        // five-sample hold is bounded by five times that derivative.
        let bound = 5.0 * 0.2 * 2.0 * PI * 500.0 / RATE;
        for size in [1_usize, 2, 7, 13, 64, 128, 255, 960, reference.len()] {
            let mut blanker = primed_constant(0.125);
            let output = process_chunks(&mut blanker, &damaged, size);
            for (index, (&actual, &expected)) in output.iter().zip(&reference).enumerate() {
                if corrupt[index] {
                    assert!((actual as f64 - expected as f64).abs() <= bound + 1e-6,
                        "block size {size}, sample {index}: {actual} vs analytic tone {expected}");
                } else {
                    assert_eq!(actual, expected, "clean sample {index}, block size {size}");
                }
            }
        }
    }

    #[test]
    fn hold_freezes_level_and_reset_relearns_clean_input() {
        let mut blanker = primed_constant(0.125);
        let before = blanker.level();
        let mut output = Vec::new();
        blanker.process_into(&[0.25; 512], true, &mut output);
        assert_eq!(output, vec![0.25; 512]);
        assert_eq!(blanker.level(), before, "AudioStage::hold freezes adaptation");

        blanker.process_into(&[4.0; 3], false, &mut output);
        assert_eq!(output, vec![0.25; 3]);
        blanker.reset();
        blanker.process_into(&[0.25; 512], false, &mut output);
        assert_eq!(output, vec![0.25; 512]);
        assert_eq!(blanker.level(), 0.25);
    }

    #[test]
    fn invalid_thresholds_use_the_default_detection_policy() {
        for threshold in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            let mut blanker = ImpulseBlanker::with_threshold(threshold, DEFAULT_WINDOW);
            let mut output = Vec::new();
            blanker.process_into(&[0.125; 128], false, &mut output);
            assert_eq!(output, vec![0.125; 128], "threshold {threshold}");
            blanker.process_into(&[4.0, 0.125], false, &mut output);
            assert_eq!(output, vec![0.125; 2], "threshold {threshold}");
        }
    }

    #[test]
    fn interpolation_between_finite_endpoints_cannot_overflow() {
        let endpoint = f32::MAX * 0.6;
        let mut blanker = ImpulseBlanker::with_threshold(1.25, 12);
        let mut output = Vec::new();
        blanker.process_into(&[endpoint; 128], false, &mut output);
        assert_eq!(output, vec![endpoint; 128]);
        blanker.process_into(&[f32::MAX, -endpoint], false, &mut output);
        // The missing sample is the midpoint of equal and opposite finite
        // endpoints. Its exact value is zero even though their f32 difference
        // overflows; this oracle does not repeat the interpolation formula.
        assert_eq!(output, vec![0.0, -endpoint]);
        assert!(blanker.level().is_finite());
    }

    #[test]
    fn an_extreme_window_is_bounded_by_the_input_not_an_allocation() {
        let mut blanker = ImpulseBlanker::with_threshold(6.0, usize::MAX);
        let mut output = Vec::new();
        blanker.process_into(&[0.125; 128], false, &mut output);
        for _ in 0..4 {
            blanker.process_into(&[4.0; 17], false, &mut output);
            assert_eq!(output, vec![0.125; 17]);
        }
        blanker.process_into(&[4.0, 0.125], false, &mut output);
        assert_eq!(output, vec![0.125; 2]);

        let mut minimum = ImpulseBlanker::with_threshold(6.0, 0);
        minimum.process_into(&[0.125; 128], false, &mut output);
        minimum.process_into(&[4.0, 4.0, 0.125], false, &mut output);
        assert_eq!(output, vec![0.125, 4.0, 0.125]);
    }

    #[test]
    fn cold_start_and_reset_do_not_invent_a_previous_signal_level() {
        for size in [1_usize, 13, 128, 960] {
            let mut blanker = ImpulseBlanker::new();
            let mut output = vec![7.0];
            blanker.process_into(&[], false, &mut output);
            assert!(output.is_empty());
            blanker.process_into(&[4.0; 13], true, &mut output);
            assert_eq!(output, vec![4.0; 13]);
            assert_eq!(blanker.level(), 0.0, "hold cannot seed an unknown level");

            let input = vec![0.125; size];
            blanker.process_into(&input, false, &mut output);
            assert_eq!(output, input);
            assert_eq!(blanker.level(), 0.125);
            blanker.process_into(&[4.0; 2], false, &mut output);
            assert_eq!(output, vec![0.125; 2]);
            blanker.reset();

            // Reset must clear the partial burst and estimate. Lifetime
            // statistics deliberately survive it, just as before this change.
            blanker.process_into(&[0.25; 13], false, &mut output);
            assert_eq!(output, vec![0.25; 13]);
            assert_eq!(blanker.level(), 0.25);
            assert_eq!(blanker.blanked_samples, 2);
        }
    }

    #[test]
    fn cold_start_makes_the_available_history_boundary_explicit() {
        // With no preceding signal, one high sample could be an impulse or the
        // start of a legitimate source. A one-sample first block seeds its own
        // level; a 128-sample first block also supplies 127 following zeros.
        let mut impulse = vec![0.0; 128];
        impulse[0] = 4.0;
        let mut one_sample = ImpulseBlanker::new();
        assert_eq!(process_chunks(&mut one_sample, &impulse, 1), impulse);
        assert_eq!(one_sample.blanked_samples, 0);
        let mut complete_block = ImpulseBlanker::new();
        assert_eq!(process_chunks(&mut complete_block, &impulse, 128), vec![0.0; 128]);
        assert_eq!(complete_block.blanked_samples, 1);

        // The same first sample followed by a sustained level is a valid cold
        // source and must pass at every partition, with no invented pre-roll.
        let sustained = vec![4.0; 1024];
        for size in [1, 2, 7, 13, 64, 128, 960, 1024] {
            let mut blanker = ImpulseBlanker::new();
            assert_eq!(process_chunks(&mut blanker, &sustained, size), sustained);
            assert_eq!(blanker.blanked_samples, 0);
        }
    }

    #[test]
    fn ramp_reference_uses_only_the_endpoint_in_the_current_block() {
        // The two missing values of this linear ramp are exactly 1/4 and 3/8.
        // A split after the first damaged sample cannot see the final endpoint:
        // it holds 1/8, then repairs the second sample to the midpoint 5/16.
        let damaged = [0.125, 4.0, 4.0, 0.5];
        let mut whole = primed_constant(0.125);
        assert_eq!(process_chunks(&mut whole, &damaged, 4), [0.125, 0.25, 0.375, 0.5]);
        let mut split = primed_constant(0.125);
        assert_eq!(process_chunks(&mut split, &damaged, 2), [0.125, 0.125, 0.3125, 0.5]);
        assert_eq!(whole.blanked_samples, 2);
        assert_eq!(split.blanked_samples, 2);
    }
}
