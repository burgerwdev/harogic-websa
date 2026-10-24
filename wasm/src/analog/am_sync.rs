//! AM carrier recovery and RF measurements, before every listener gain stage.
//!
//! A polarity-insensitive differential FLL acquires the carrier, including overmodulated
//! AM. Once coherent evidence appears, a type-II phase loop removes acquisition bias
//! without pulling toward noise or asymmetric sidebands. All timing uses source samples.
use super::TAU;

#[derive(Clone, Copy, Debug)]
pub struct AmStatus {
    pub sync_enabled: bool,
    pub sync_locked: bool,
    pub carrier_offset_hz: Option<f64>,
    /// Sine-equivalent envelope depth: sqrt(2 * variance(envelope)) / mean(envelope).
    pub modulation_pct: Option<f64>,
    pub input_dbfs: Option<f64>,
}

pub(super) struct AmSync {
    fs: f64,
    pull_hz: f64,
    enabled: bool,
    phase: f64,
    frequency: f64,
    history: Vec<(f64, f64)>,
    history_at: usize,
    history_count: usize,
    differential: (f64, f64),
    carrier: (f64, f64),
    phase_axis: (f64, f64),
    power: f64,
    reference_power: f64,
    phase_error: f64,
    good_samples: usize,
    bad_samples: usize,
    offset: f64,
    modulation_power: Option<f64>,
    mean_input_power: f64,
    valid: bool,
    elapsed: usize,
    window: usize,
    env_sum: f64,
    env_square_sum: f64,
    input_sum: f64,
    input_count: usize,
    depth: Option<f64>,
    input_dbfs: Option<f64>,
    product_previous: f64,
    product_polarity: f64,
    product_dc: f64,
    blend: f64,
    fast_alpha: f64,
    frequency_alpha: f64,
    carrier_alpha: f64,
    reference_decay: f64,
    power_alpha: f64,
    measurement_alpha: f64,
    offset_alpha: f64,
}

impl AmSync {
    pub fn new(fs: f64, if_bw: f64) -> Self {
        let pull_hz = (if_bw * 0.2).min(fs * 0.1).min(3_000.0);
        Self { fs, pull_hz,
            enabled: false, phase: 0.0, frequency: 0.0,
            history: vec![(0.0, 0.0); (fs / (4.0 * pull_hz)).floor().max(1.0) as usize],
            history_at: 0, history_count: 0,
            differential: (0.0, 0.0), carrier: (0.0, 0.0), phase_axis: (0.0, 0.0), power: 0.0,
            reference_power: 0.0, phase_error: 1.0, good_samples: 0, bad_samples: 0,
            offset: 0.0, modulation_power: None, mean_input_power: 0.0,
            valid: false, elapsed: 0, window: (fs * 0.1).round().max(1.0) as usize,
            env_sum: 0.0, env_square_sum: 0.0, input_sum: 0.0, input_count: 0,
            depth: None, input_dbfs: None, product_previous: 0.0, product_polarity: 1.0, product_dc: 0.0,
            blend: 0.0, fast_alpha: 1.0 - (-1.0 / (fs * 0.01)).exp(),
            frequency_alpha: 1.0 - (-1.0 / (fs * 0.02)).exp(),
            carrier_alpha: 1.0 - (-TAU * 10.0 / fs).exp(),
            power_alpha: 1.0 - (-1.0 / (fs * 0.01)).exp(),
            reference_decay: (-1.0 / (fs * 10.0)).exp(),
            measurement_alpha: 1.0 - (-0.1_f64 / 0.5).exp(),
            offset_alpha: 1.0 - (-1.0 / (fs * 0.25)).exp() }
    }

    pub fn set_enabled(&mut self, enabled: bool) { self.enabled = enabled; }

    pub fn reset(&mut self) {
        let enabled = self.enabled;
        *self = Self::new(self.fs, self.pull_hz / 0.2);
        self.enabled = enabled;
    }

    pub fn observe_input(&mut self, i: f64, q: f64) {
        let power = i * i + q * q;
        if power.is_finite() { self.input_sum += power; }
        self.input_count += 1;
        if self.input_count >= self.window {
            let power = self.input_sum / self.input_count as f64;
            self.mean_input_power = if self.mean_input_power == 0.0 { power } else {
                self.mean_input_power + self.measurement_alpha * (power - self.mean_input_power)
            };
            self.input_dbfs = if self.mean_input_power > 0.0 {
                Some(10.0 * self.mean_input_power.log10())
            } else { None };
            self.input_sum = 0.0;
            self.input_count = 0;
        }
    }

    pub fn status(&self) -> AmStatus {
        AmStatus { sync_enabled: self.enabled, sync_locked: self.enabled && self.valid,
            carrier_offset_hz: self.valid.then_some(self.offset * self.fs / TAU),
            modulation_pct: if self.valid { self.depth } else { None },
            input_dbfs: self.input_dbfs }
    }

    /// Return the coherent detector's DC-blocked sample and the current crossfade weight.
    pub fn process(&mut self, i: f64, q: f64) -> (f64, f64) {
        let (i, q) = if i.is_finite() && q.is_finite() { (i, q) } else { (0.0, 0.0) };
        let power = i * i + q * q;
        let envelope = power.sqrt();
        self.power += self.power_alpha * (power - self.power);
        self.reference_power = self.power.max(self.reference_power * self.reference_decay);
        if self.history_count == self.history.len() {
            let (pi, pq) = self.history[self.history_at];
            let re = i * pi + q * pq;
            let im = q * pi - i * pq;
            let magnitude = re.hypot(im);
            // Squaring removes the pi flips of an overmodulated envelope. Weight by actual
            // RF power: near envelope zeros and noise must not dominate acquisition.
            let (dr, di) = if magnitude > (self.power * 0.01).max(1e-30) {
                ((re * re - im * im) / magnitude, 2.0 * re * im / magnitude)
            } else { (0.0, 0.0) };
            self.differential.0 += self.fast_alpha * (dr - self.differential.0);
            self.differential.1 += self.fast_alpha * (di - self.differential.1);
        }
        self.history[self.history_at] = (i, q);
        self.history_at = (self.history_at + 1) % self.history.len();
        self.history_count = (self.history_count + 1).min(self.history.len());
        // A lag spanning a quarter cycle at the pull limit retains an unambiguous
        // estimate and avoids the severe zero-frequency bias of adjacent samples
        // in narrow-band receiver noise.
        let measured = 0.5 * self.differential.1.atan2(self.differential.0) / self.history.len() as f64;
        let limit = TAU * self.pull_hz / self.fs;
        let (sin, cos) = self.phase.sin_cos();
        let real = i * cos + q * sin;
        let imag = q * cos - i * sin;
        self.carrier.0 += self.carrier_alpha * (real - self.carrier.0);
        self.carrier.1 += self.carrier_alpha * (imag - self.carrier.1);
        // Estimate the carrier axis, modulo pi, from the squared mixer output.
        // Signed AM envelope reversals must never steer the NCO by pi. Normalizing
        // by magnitude retains RF amplitude weighting without squaring its range.
        let (axis_re, axis_im) = if power > (self.power * 0.01).max(1e-30) {
            ((real * real - imag * imag) / envelope, 2.0 * real * imag / envelope)
        } else { (0.0, 0.0) };
        self.phase_axis.0 += self.fast_alpha * (axis_re - self.phase_axis.0);
        self.phase_axis.1 += self.fast_alpha * (axis_im - self.phase_axis.1);
        let error = 0.5 * self.phase_axis.1.atan2(self.phase_axis.0);
        self.phase_error += self.fast_alpha * (error.abs() - self.phase_error);
        // The differential discriminator is useful for capture, but channel-filtered
        // noise and unequal sidebands bias it. Integral phase feedback removes this
        // bias once a coherent axis exists.
        let axis_power = self.phase_axis.0 * self.phase_axis.0 + self.phase_axis.1 * self.phase_axis.1;
        let tracking = axis_power > self.power * 0.12;
        if tracking {
            self.frequency = (self.frequency + (TAU * 3.0 / self.fs).powi(2) * error)
                .clamp(-limit, limit);
        } else if !self.valid {
            self.frequency += self.frequency_alpha * (measured.clamp(-limit, limit) - self.frequency);
        }
        self.phase = (self.phase + self.frequency + TAU * 20.0 / self.fs * error).rem_euclid(TAU);
        if self.valid { self.offset += self.offset_alpha * (self.frequency - self.offset); }
        else { self.offset = self.frequency; }
        let carrier_power = self.carrier.0 * self.carrier.0 + self.carrier.1 * self.carrier.1;
        let differential_power = self.differential.0.hypot(self.differential.1);
        let evidence = self.power > 1e-14 && self.power > self.reference_power * 1e-3
            && carrier_power > self.power * 0.12 && self.phase_error < 0.3
            && differential_power > self.power * 0.05
            && self.frequency.abs() < limit * 0.98;
        // The squared phase detector locks modulo pi. Resolve that ambiguity from
        // the recovered carrier while acquiring, then hold polarity through signed
        // modulation minima. Both detectors must agree in polarity when crossfading.
        if !self.valid && evidence {
            self.product_polarity = if self.carrier.0 < 0.0 { -1.0 } else { 1.0 };
        }
        if evidence { self.good_samples = self.good_samples.saturating_add(1); }
        else { self.good_samples = 0; }
        // Carrier evidence must persist to acquire; brief modulation minima and phase
        // disturbances are tolerated. A genuine deep fade invalidates immediately.
        let faded = self.power < 1e-14 || self.power < self.reference_power * 1e-3;
        let lost = carrier_power < self.power * 0.05 || self.phase_error > 0.65
            || differential_power < self.power * 0.025 || self.frequency.abs() >= limit * 0.98;
        if lost { self.bad_samples = self.bad_samples.saturating_add(1); }
        else { self.bad_samples = 0; }
        if faded || self.bad_samples >= (self.fs * 0.15) as usize {
            self.valid = false;
            self.depth = None;
        } else if self.good_samples >= (self.fs * 0.15) as usize { self.valid = true; }

        self.env_sum += envelope;
        self.env_square_sum += power;
        self.elapsed += 1;
        if self.elapsed >= self.window {
            let mean = self.env_sum / self.elapsed as f64;
            let mean_power = self.env_square_sum / self.elapsed as f64;
            // Normalize each integration window before smoothing. Mixing moments
            // across changing RF levels incorrectly counts propagation fades as
            // audio modulation and makes the meter jump when the station fades.
            let variance = (mean_power - mean * mean).max(0.0);
            if mean > 1e-7 {
                let normalized = 2.0 * variance / (mean * mean);
                let alpha = 1.0 - (-(self.window as f64) / self.fs).exp();
                self.modulation_power = Some(match self.modulation_power {
                    Some(previous) => previous + alpha * (normalized - previous),
                    None => normalized,
                });
            }
            self.depth = if self.valid {
                self.modulation_power.map(|power| 100.0 * power.sqrt())
            } else { None };
            self.elapsed = 0;
            self.env_sum = 0.0;
            self.env_square_sum = 0.0;
        }
        self.product_dc = super::DC_A * self.product_dc
            + super::DC_A * (real - self.product_previous);
        self.product_previous = real;
        let target = if self.enabled { 1.0 } else { 0.0 };
        let step = 1.0 / (self.fs * 0.02).max(1.0);
        self.blend += (target - self.blend).clamp(-step, step);
        (self.product_dc * self.product_polarity, self.blend)
    }
}
