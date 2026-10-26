// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

pub(crate) const WORK_SAMPLE_RATE: u32 = 12_000;

#[derive(Debug)]
pub(crate) struct LinearResampler {
    input_rate: u32,
    source_index: u64,
    next_output_position: f64,
    previous: f32,
    initialized: bool,
}

impl LinearResampler {
    pub(crate) fn new(input_rate: u32) -> Self {
        Self {
            input_rate,
            source_index: 0,
            next_output_position: 0.0,
            previous: 0.0,
            initialized: false,
        }
    }

    pub(crate) fn push(&mut self, input: f32, mut output: impl FnMut(f32)) -> usize {
        if !self.initialized {
            self.initialized = true;
            self.previous = input;
            self.source_index = 0;
            output(input);
            self.next_output_position = f64::from(self.input_rate) / f64::from(WORK_SAMPLE_RATE);
            return 1;
        }
        self.source_index += 1;
        let current_position = self.source_index as f64;
        let previous_position = current_position - 1.0;
        let step = f64::from(self.input_rate) / f64::from(WORK_SAMPLE_RATE);
        let mut count = 0;
        while self.next_output_position <= current_position {
            let fraction = (self.next_output_position - previous_position).clamp(0.0, 1.0);
            output(self.previous + (input - self.previous) * fraction as f32);
            self.next_output_position += step;
            count += 1;
        }
        self.previous = input;
        count
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.input_rate);
    }
}

#[derive(Debug)]
pub(crate) struct FrequencyDemodulator {
    center_hz: f32,
    oscillator_sin: f64,
    oscillator_cos: f64,
    oscillator_sin_step: f64,
    oscillator_cos_step: f64,
    oscillator_until_normalize: u16,
    i_stage1: f64,
    q_stage1: f64,
    i_stage2: f64,
    q_stage2: f64,
    previous_i: f64,
    previous_q: f64,
    has_previous_vector: bool,
    current_frequency: f32,
    amplitude_ema: f32,
    carrier_level_ema: f32,
}

impl Default for FrequencyDemodulator {
    fn default() -> Self {
        Self::new(1900.0)
    }
}

impl FrequencyDemodulator {
    pub(crate) fn new(center_hz: f32) -> Self {
        let step = std::f64::consts::TAU * f64::from(center_hz) / f64::from(WORK_SAMPLE_RATE);
        let (oscillator_sin_step, oscillator_cos_step) = step.sin_cos();
        Self {
            center_hz,
            oscillator_sin: 0.0,
            oscillator_cos: 1.0,
            oscillator_sin_step,
            oscillator_cos_step,
            oscillator_until_normalize: 4096,
            i_stage1: 0.0,
            q_stage1: 0.0,
            i_stage2: 0.0,
            q_stage2: 0.0,
            previous_i: 0.0,
            previous_q: 0.0,
            has_previous_vector: false,
            current_frequency: center_hz,
            amplitude_ema: 0.0,
            carrier_level_ema: 0.0,
        }
    }

    pub(crate) fn process(&mut self, sample: f32, _index: u64) -> f32 {
        self.amplitude_ema += (sample.abs() - self.amplitude_ema) * 0.001;
        let sample = f64::from(sample);
        let mixed_i = sample * self.oscillator_cos;
        let mixed_q = -sample * self.oscillator_sin;
        let next_sin = self.oscillator_sin * self.oscillator_cos_step
            + self.oscillator_cos * self.oscillator_sin_step;
        let next_cos = self.oscillator_cos * self.oscillator_cos_step
            - self.oscillator_sin * self.oscillator_sin_step;
        self.oscillator_sin = next_sin;
        self.oscillator_cos = next_cos;
        self.oscillator_until_normalize -= 1;
        if self.oscillator_until_normalize == 0 {
            let magnitude = self.oscillator_sin.hypot(self.oscillator_cos);
            if magnitude > f64::EPSILON {
                self.oscillator_sin /= magnitude;
                self.oscillator_cos /= magnitude;
            }
            self.oscillator_until_normalize = 4096;
        }

        const ALPHA: f64 = 0.42;
        self.i_stage1 += ALPHA * (mixed_i - self.i_stage1);
        self.q_stage1 += ALPHA * (mixed_q - self.q_stage1);
        self.i_stage2 += ALPHA * (self.i_stage1 - self.i_stage2);
        self.q_stage2 += ALPHA * (self.q_stage1 - self.q_stage2);

        let magnitude = self.i_stage2.hypot(self.q_stage2);
        self.carrier_level_ema += ((2.0 * magnitude) as f32 - self.carrier_level_ema) * 0.02;
        if self.has_previous_vector && magnitude > 0.0005 {
            let cross = self.previous_i * self.q_stage2 - self.previous_q * self.i_stage2;
            let dot = self.previous_i * self.i_stage2 + self.previous_q * self.q_stage2;
            let delta = cross.atan2(dot);
            let measured = f64::from(self.center_hz)
                + delta * f64::from(WORK_SAMPLE_RATE) / std::f64::consts::TAU;
            if (50.0..f64::from(WORK_SAMPLE_RATE) * 0.5 - 50.0).contains(&measured) {
                self.current_frequency += (measured as f32 - self.current_frequency) * 0.22;
            }
        }
        self.previous_i = self.i_stage2;
        self.previous_q = self.q_stage2;
        self.has_previous_vector = magnitude > 0.0005;
        self.current_frequency
    }

    pub(crate) const fn carrier_level(&self) -> f32 {
        self.carrier_level_ema
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.center_hz);
    }
}
