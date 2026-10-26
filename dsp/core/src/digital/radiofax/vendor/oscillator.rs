// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

use std::f64::consts::TAU;

/// Phase-continuous oscillator using a recurrence per output sample.
///
/// `sin_cos` is evaluated once per tone segment rather than once per sample.
#[derive(Clone, Debug)]
pub(crate) struct Oscillator {
    sin_phase: f64,
    cos_phase: f64,
    samples_until_normalize: u16,
}

impl Default for Oscillator {
    fn default() -> Self {
        Self {
            sin_phase: 0.0,
            cos_phase: 1.0,
            samples_until_normalize: 4096,
        }
    }
}

impl Oscillator {
    #[inline]
    pub(crate) fn fill(
        &mut self,
        output: &mut [f32],
        frequency_hz: f64,
        sample_rate: u32,
        amplitude: f32,
    ) {
        let step = TAU * frequency_hz / f64::from(sample_rate);
        let (sin_step, cos_step) = step.sin_cos();
        let amplitude = f64::from(amplitude);
        let mut sin_phase = self.sin_phase;
        let mut cos_phase = self.cos_phase;
        let mut samples_until_normalize = self.samples_until_normalize;

        for sample in output {
            *sample = (sin_phase * amplitude) as f32;
            let next_sin = sin_phase * cos_step + cos_phase * sin_step;
            let next_cos = cos_phase * cos_step - sin_phase * sin_step;
            sin_phase = next_sin;
            cos_phase = next_cos;
            samples_until_normalize -= 1;
            if samples_until_normalize == 0 {
                let magnitude = sin_phase.hypot(cos_phase);
                if magnitude > f64::EPSILON {
                    sin_phase /= magnitude;
                    cos_phase /= magnitude;
                } else {
                    sin_phase = 0.0;
                    cos_phase = 1.0;
                }
                samples_until_normalize = 4096;
            }
        }

        self.sin_phase = sin_phase;
        self.cos_phase = cos_phase;
        self.samples_until_normalize = samples_until_normalize;
    }

    #[inline]
    pub(crate) fn fill_ramped(
        &mut self,
        output: &mut [f32],
        frequency_hz: f64,
        sample_rate: u32,
        amplitude: f32,
        sample_range: std::ops::Range<usize>,
        ramp_samples: usize,
    ) {
        let step = TAU * frequency_hz / f64::from(sample_rate);
        let (sin_step, cos_step) = step.sin_cos();
        let amplitude = f64::from(amplitude);
        let mut sin_phase = self.sin_phase;
        let mut cos_phase = self.cos_phase;
        let mut samples_until_normalize = self.samples_until_normalize;

        for (offset, sample) in output.iter_mut().enumerate() {
            let index = sample_range.start + offset;
            let attack = if ramp_samples == 0 {
                1.0
            } else {
                (index as f64 / ramp_samples as f64).min(1.0)
            };
            let remaining = sample_range.end.saturating_sub(index + 1);
            let release = if ramp_samples == 0 {
                1.0
            } else {
                (remaining as f64 / ramp_samples as f64).min(1.0)
            };
            *sample = (sin_phase * amplitude * attack.min(release)) as f32;
            let next_sin = sin_phase * cos_step + cos_phase * sin_step;
            let next_cos = cos_phase * cos_step - sin_phase * sin_step;
            sin_phase = next_sin;
            cos_phase = next_cos;
            samples_until_normalize -= 1;
            if samples_until_normalize == 0 {
                let magnitude = sin_phase.hypot(cos_phase);
                if magnitude > f64::EPSILON {
                    sin_phase /= magnitude;
                    cos_phase /= magnitude;
                } else {
                    sin_phase = 0.0;
                    cos_phase = 1.0;
                }
                samples_until_normalize = 4096;
            }
        }

        self.sin_phase = sin_phase;
        self.cos_phase = cos_phase;
        self.samples_until_normalize = samples_until_normalize;
    }
}
