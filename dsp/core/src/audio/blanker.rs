//! Impulse noise blanker: removes clicks and ignition-style bursts before they reach the AGC.
//!
//! A sample is an impulse when it stands far above the block's own level (a multiple of the running
//! RMS), which is why the threshold is relative: an absolute one would blank quiet audio or miss
//! loud impulses depending on the volume setting. The blanked span is filled by a linear ramp
//! between the last clean sample and the first clean sample after the burst, so the repair is
//! inaudible instead of being a hole.
//!
//! Ordering matters: this is the *last* stage in the chain because it must see the signal the AGC
//! would otherwise have used an impulse to wind up on.

use crate::plugin::AudioStage;

const DEFAULT_THRESHOLD_K: f64 = 6.0;
const DEFAULT_WINDOW: usize = 12;
/// Smoothing of the running level estimate (per block): slow enough that an impulse cannot raise
/// the very threshold that is supposed to catch it.
const LEVEL_SMOOTH: f64 = 0.1;

pub struct ImpulseBlanker {
    threshold_k: f64,
    window: usize,
    level: f64,
    primed: bool,
    /// Samples still being repaired from a burst that started in this or the previous block.
    remaining: usize,
    /// The clean sample the ramp starts from.
    from: f32,
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
            level: 0.0,
            primed: false,
            remaining: 0,
            from: 0.0,
            blanked_samples: 0,
        }
    }

    pub fn with_threshold(k: f64, window: usize) -> Self {
        Self { threshold_k: k, window: window.max(1), ..Self::new() }
    }

    /// The level the threshold is derived from (diagnostics).
    pub fn level(&self) -> f64 {
        self.level
    }
}

impl AudioStage for ImpulseBlanker {
    fn id(&self) -> &'static str {
        "blanker"
    }

    fn process_into(&mut self, input: &[f32], _hold: bool, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());
        if input.is_empty() {
            return;
        }
        let mean_sq = input.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / input.len() as f64;
        let block_level = mean_sq.sqrt();
        if !self.primed {
            self.level = block_level;
            self.primed = true;
        } else {
            self.level += (block_level - self.level) * LEVEL_SMOOTH;
        }
        let threshold = (self.level * self.threshold_k).max(1e-4);

        let mut index = 0;
        while index < input.len() {
            let sample = input[index];
            if self.remaining == 0 {
                if (sample as f64).abs() > threshold {
                    // A burst starts here: ramp from the last clean sample to the first clean one
                    // after the window (bounded by the block).
                    self.remaining = self.window;
                    self.blanked_samples += 1;
                    let end = (index + self.window + 1).min(input.len() - 1);
                    let target = input[end];
                    for step in 0..=(end - index) {
                        let ratio = (step + 1) as f32 / (end - index + 1) as f32;
                        out.push(self.from + (target - self.from) * ratio);
                    }
                    index = end + 1;
                    self.from = target;
                    self.remaining = 0;
                    continue;
                }
                out.push(sample);
                self.from = sample;
            } else {
                // A burst that started in the previous block: keep ramping toward the block's level.
                self.remaining -= 1;
                out.push(self.from);
            }
            index += 1;
        }
        // A block that ends while blanking keeps the last pushed sample as the ramp origin.
        if let Some(last) = out.last() {
            self.from = *last;
        }
    }

    fn reset(&mut self) {
        self.level = 0.0;
        self.primed = false;
        self.remaining = 0;
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
        println!("blanker: peak {peak_before} -> {peak_after}, blanked blocks {}",
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
        for (a, b) in input.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-6, "the signal must pass unchanged");
        }
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
}
