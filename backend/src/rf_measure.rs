//! Passive, bounded IQ measurements. No receiver/listener samples are modified.
//! SNR is conditional on sparse occupancy and a flat, additive background. A
//! spectrally inseparable interferer is included in that background, never proven noise.
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;

const FRAMES: usize = 32;
#[derive(Clone, Debug, Serialize)]
pub struct RfMeasurement {
    pub metric_type: &'static str,
    pub unit: &'static str,
    pub value: Option<f64>,
    pub state: &'static str,
    pub reason: Option<&'static str>,
    pub reference_bandwidth_hz: f64,
    pub band_low_hz: f64,
    pub band_high_hz: f64,
    pub sample_rate_hz: f64,
    pub flat_passband_hz: f64,
    pub location: &'static str,
    pub method: &'static str,
    pub background: &'static str,
    pub window: &'static str,
    pub fft_size: usize,
    pub bin_spacing_hz: f64,
    pub enbw_hz: f64,
    pub averaging_seconds: f64,
    pub observation_seconds: f64,
    pub max_age_seconds: f64,
    pub signal_plus_noise_power: Option<f64>,
    pub background_power: Option<f64>,
    pub signal_power: Option<f64>,
    pub power_unit: &'static str,
    pub clean_reference_fraction: Option<f64>,
    pub uncertainty_db: Option<f64>,
}
pub struct RfMeter {
    rate: f64,
    low: f64,
    high: f64,
    flat: f64,
    n: usize,
    fft: Arc<dyn Fft<f64>>,
    buffer: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    window: Vec<f64>,
    spectrum: Vec<f64>,
    filled: usize,
    frames: usize,
    skip: usize,
    stride: usize,
    elapsed: usize,
    overloaded: bool,
    last: Option<RfMeasurement>,
    measured_at: Option<Instant>,
}
impl RfMeter {
    /// `flat` is the certified full flat passband of the upstream channel FIR,
    /// NOT its stopband, output sample rate, or UI span. Only that region can
    /// supply noise references. Offline unfiltered IQ may use `0.9 * rate`.
    pub fn new(rate: f64, low: f64, high: f64, flat: f64) -> Self {
        let n = ((rate / (high - low) * 512.0).clamp(512.0, 4096.0).ceil() as usize)
            .next_power_of_two()
            .clamp(512, 4096);
        let fft = FftPlanner::new().plan_fft_forward(n);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        let window = (0..n)
            .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())
            .collect();
        // At most 64 FFTs/s for wide channels. Sample positions, not input block
        // boundaries, select windows; there is no queue and no backlog.
        let stride = n.max((rate / 64.0).ceil() as usize);
        Self {
            rate,
            low,
            high,
            flat,
            n,
            fft,
            buffer: vec![Complex::default(); n],
            scratch,
            window,
            spectrum: vec![0.0; n],
            filled: 0,
            frames: 0,
            skip: 0,
            stride,
            elapsed: 0,
            overloaded: false,
            last: None,
            measured_at: None,
        }
    }
    pub fn reset(&mut self) {
        self.spectrum.fill(0.0);
        self.filled = 0;
        self.frames = 0;
        self.skip = 0;
        self.elapsed = 0;
        self.overloaded = false;
        self.last = None;
        self.measured_at = None;
    }
    fn empty(&self) -> RfMeasurement {
        RfMeasurement {metric_type:"rf_snr",unit:"dB",value:None,state:"unavailable",reason:Some("warming_up"),
            reference_bandwidth_hz:self.high-self.low,band_low_hz:self.low,band_high_hz:self.high,
            sample_rate_hz:self.rate,flat_passband_hz:self.flat,
            location:"channel_iq_before_demod_agc_squelch_nr_eq",
            method:"welch_hann_sparse_flat_background_v1",
            background:"assumed additive flat noise plus unresolved interference; sparse occupancy required",
            window:"periodic_hann",fft_size:self.n,bin_spacing_hz:self.rate/self.n as f64,
            enbw_hz:1.5*self.rate/self.n as f64,averaging_seconds:0.0,observation_seconds:0.0,
            max_age_seconds:(FRAMES*self.stride) as f64/self.rate*2.0+0.5,
            signal_plus_noise_power:None,background_power:None,signal_power:None,power_unit:"adc_full_scale_squared",
            clean_reference_fraction:None,uncertainty_db:None }
    }
    pub fn snapshot(&self) -> RfMeasurement {
        let mut value = self.last.clone().unwrap_or_else(|| self.empty());
        if self
            .measured_at
            .is_some_and(|at| at.elapsed().as_secs_f64() > value.max_age_seconds)
        {
            value.value = None;
            value.state = "unavailable";
            value.reason = Some("stale");
            value.signal_plus_noise_power = None;
            value.background_power = None;
            value.signal_power = None;
        }
        value
    }
    /// Return a new result only at the end of an observation. O(1) retained
    /// windows; FFT storage is reused. Overload scans include skipped samples.
    pub fn push(&mut self, iq: &[f32]) -> bool {
        let mut updated = false;
        for pair in iq.chunks_exact(2) {
            self.elapsed += 1;
            self.overloaded |= pair.iter().any(|v| !v.is_finite() || v.abs() >= 0.999);
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            let w = self.window[self.filled];
            self.buffer[self.filled] = Complex::new(f64::from(pair[0]) * w, f64::from(pair[1]) * w);
            self.filled += 1;
            if self.filled != self.n {
                continue;
            }
            self.fft
                .process_with_scratch(&mut self.buffer, &mut self.scratch);
            // Complex two-sided PSD: |FFT(x*w)|² / (Fs * sum(w²)).
            // Integrating it with df conserves mean complex power; never divide
            // by coherent gain or ENBW a second time.
            let norm = self.rate * self.window.iter().map(|w| w * w).sum::<f64>();
            for (p, z) in self.spectrum.iter_mut().zip(&self.buffer) {
                *p += z.norm_sqr() / norm;
            }
            self.filled = 0;
            self.frames += 1;
            self.skip = self.stride - self.n;
            if self.frames == FRAMES {
                self.last = Some(self.estimate());
                self.measured_at = Some(Instant::now());
                updated = true;
                self.spectrum.fill(0.0);
                self.frames = 0;
                self.elapsed = 0;
                self.overloaded = false;
            }
        }
        updated
    }
    fn estimate(&self) -> RfMeasurement {
        let mut result = self.empty();
        result.averaging_seconds = (self.n * FRAMES) as f64 / self.rate;
        result.observation_seconds = self.elapsed as f64 / self.rate;
        let df = self.rate / self.n as f64;
        let invalid = if self.overloaded {
            Some("overload_or_nonfinite_iq")
        } else if self.high <= self.low
            || self.low < -self.flat / 2.0
            || self.high > self.flat / 2.0
            || self.flat > self.rate
        {
            Some("measurement_band_outside_known_flat_passband")
        } else {
            None
        };
        if let Some(reason) = invalid {
            result.reason = Some(reason);
            return result;
        }
        let frequency = |k: usize| {
            if k < self.n / 2 {
                k as f64 * df
            } else {
                (k as f64 - self.n as f64) * df
            }
        };
        let psd: Vec<_> = self.spectrum.iter().map(|p| p / FRAMES as f64).collect();
        let indices: Vec<_> = (0..self.n)
            .filter(|&k| frequency(k).abs() < self.flat / 2.0 - 4.0 * df)
            .collect();
        if indices.len() < 64 {
            result.reason = Some("insufficient_flat_reference_bins");
            return result;
        }
        let mut sorted: Vec<_> = indices.iter().map(|&k| psd[k]).collect();
        sorted.sort_unstable_by(f64::total_cmp);
        // Median of Gamma(M,1/M): Wilson-Hilferty approximation. Used only
        // to mask strong bins, NOT as the final noise power estimator.
        let floor = sorted[sorted.len() / 2] / (1.0 - 1.0 / (9.0 * FRAMES as f64)).powi(3);
        if floor <= 0.0 {
            result.reason = Some("no_measurable_background");
            return result;
        }
        let mut clean = vec![true; self.n];
        for &k in &indices {
            if psd[k] > floor * 1.8 {
                // Hann leakage guard, including non-bin-centred tones.
                for delta in -4_isize..=4 {
                    clean[(k as isize + delta).rem_euclid(self.n as isize) as usize] = false;
                }
            }
        }
        let refs: Vec<_> = indices.iter().copied().filter(|&k| clean[k]).collect();
        let fraction = refs.len() as f64 / indices.len() as f64;
        result.clean_reference_fraction = Some(fraction);
        if fraction < 0.6 || refs.len() < 64 {
            result.reason = Some("occupied_or_contaminated_noise_reference");
            return result;
        }
        // Broadband response slope, coloured noise and broad neighbours cannot
        // silently become a single noise density. Require references throughout
        // the passband and agreement between eight frequency regions.
        let mut sums = [0.0; 8];
        let mut counts = [0_usize; 8];
        let mut totals = [0_usize; 8];
        for &k in &indices {
            let region = ((frequency(k) / self.flat + 0.5) * 8.0)
                .floor()
                .clamp(0.0, 7.0) as usize;
            totals[region] += 1;
            if clean[k] {
                sums[region] += psd[k];
                counts[region] += 1;
            }
        }
        if (0..8).any(|r| counts[r] < 4 || counts[r] * 5 < totals[r]) {
            result.reason = Some("noise_reference_missing_across_passband");
            return result;
        }
        let levels: Vec<_> = (0..8).map(|r| sums[r] / counts[r] as f64).collect();
        if levels.iter().copied().fold(0.0, f64::max)
            / levels.iter().copied().fold(f64::INFINITY, f64::min)
            > 10_f64.powf(1.5 / 10.0)
        {
            result.reason = Some("nonflat_background_or_filter_response");
            return result;
        }
        let density = refs.iter().map(|&k| psd[k]).sum::<f64>() / refs.len() as f64;
        let total = (0..self.n)
            .map(|k| {
                let f = frequency(k);
                let width =
                    (self.high.min(f + df / 2.0) - self.low.max(f - df / 2.0)).clamp(0.0, df);
                psd[k] * width
            })
            .sum::<f64>();
        let noise = density * (self.high - self.low);
        let signal = total - noise;
        result.signal_plus_noise_power = Some(total);
        result.background_power = Some(noise);
        // Conservative 3-sigma floor including covariance between adjacent Hann
        // bins. Pure noise has no identifiable positive signal power.
        let relative_sigma =
            (3.0 / (FRAMES as f64 * ((self.high - self.low) / df).min(refs.len() as f64))).sqrt();
        if signal <= 3.0 * relative_sigma * noise {
            result.reason = Some("signal_not_resolved_from_background");
            return result;
        }
        let snr = 10.0 * (signal / noise).log10();
        // Range established by the deterministic calibration, not a clamp.
        if !(-11.0..=36.0).contains(&snr) {
            result.reason = Some("outside_validated_range_minus10_to35_db");
            return result;
        }
        result.value = Some(snr);
        result.signal_power = Some(signal);
        result.state = "conditional";
        result.reason = None;
        result.uncertainty_db =
            Some(10.0 / std::f64::consts::LN_10 * relative_sigma * (1.0 + noise / signal));
        result
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    struct Rng(u64);
    impl Rng {
        fn unit(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 + 0.5) / 9007199254740992.0
        }
        fn gaussian(&mut self) -> f64 {
            (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
        }
    }
    pub(crate) fn fixture(rate: f64, bw: f64, snr: f64, count: usize) -> Vec<f32> {
        let mut rng = Rng(42);
        let amplitude = 0.002;
        let sigma = (amplitude * amplitude / 10_f64.powf(snr / 10.0) * rate / bw / 2.0).sqrt();
        (0..count)
            .flat_map(|i| {
                let phase = std::f64::consts::TAU * (bw * 0.137 + 0.73) * i as f64 / rate;
                [
                    (amplitude * phase.cos() + sigma * rng.gaussian()) as f32,
                    (amplitude * phase.sin() + sigma * rng.gaussian()) as f32,
                ]
            })
            .collect()
    }
    #[test]
    fn calibration_rates_bandwidths_negative_positive_nonbin_and_chunking() {
        let mut worst = 0.0_f64;
        for (rate, bw) in [
            (12_000.0, 3000.0),
            (48_828.125, 12_000.0),
            (390_625.0, 100_000.0),
            (2000.0, 250.0),
        ] {
            for snr in [-10.0, -5.0, 0.0, 10.0, 25.0, 35.0] {
                let mut meter = RfMeter::new(rate, -bw / 2.0, bw / 2.0, bw);
                let iq = fixture(rate, bw, snr, FRAMES * meter.stride);
                for chunk in iq.chunks(274) {
                    meter.push(chunk);
                }
                let result = meter.snapshot();
                let measured = result
                    .value
                    .unwrap_or_else(|| panic!("{rate}/{bw}/{snr}: {result:?}"));
                let error = (measured - snr).abs();
                worst = worst.max(error);
                println!(
                    "RF Fs={rate} B={bw} expected={snr} measured={measured:.4} error={error:.4} dB"
                );
                assert!(error < 1.0, "{result:?}");
                let mut second = RfMeter::new(rate, -bw / 2.0, bw / 2.0, bw);
                second.push(&iq);
                assert_eq!(
                    result.value,
                    second.snapshot().value,
                    "chunk boundaries must not select windows"
                );
            }
        }
        println!("RF worst error {worst:.4} dB");
    }
    #[test]
    fn pure_noise_overload_response_and_reset_are_unavailable() {
        let rate = 48_000.0;
        let bw = 12_000.0;
        let mut meter = RfMeter::new(rate, -bw / 2.0, bw / 2.0, bw);
        let mut rng = Rng(987);
        let noise: Vec<f32> = (0..FRAMES * meter.stride * 2)
            .map(|_| (rng.gaussian() * 0.01) as f32)
            .collect();
        meter.push(&noise);
        assert_eq!(
            meter.snapshot().reason,
            Some("signal_not_resolved_from_background")
        );
        meter.reset();
        assert_eq!(meter.snapshot().value, None);
        assert_eq!(meter.snapshot().reason, Some("warming_up"));
        meter.push(&vec![1.0; noise.len()]);
        assert_eq!(meter.snapshot().reason, Some("overload_or_nonfinite_iq"));
        let mut outside = RfMeter::new(rate, -bw / 2.0, bw / 2.0, bw * 0.9);
        outside.push(&noise);
        assert_eq!(
            outside.snapshot().reason,
            Some("measurement_band_outside_known_flat_passband")
        );
        // A broad spectral slope cannot be treated as a uniform N0. Smooth
        // coloured noise by a one-pole filter, leaving receiver input untouched.
        let mut coloured = noise.clone();
        let (mut re, mut im) = (0.0, 0.0);
        for pair in coloured.chunks_exact_mut(2) {
            re += 0.02 * (pair[0] - re);
            im += 0.02 * (pair[1] - im);
            pair[0] = re;
            pair[1] = im;
        }
        meter.reset();
        meter.push(&coloured);
        assert_eq!(meter.snapshot().value, None);
    }
    #[test]
    fn neighbouring_tone_is_excluded_from_background_but_inchannel_interference_is_counted() {
        let rate = 48_000.0;
        let bw = 6000.0;
        let mut meter = RfMeter::new(rate, -bw / 2.0, bw / 2.0, 24_000.0);
        let mut iq = fixture(rate, bw, 10.0, FRAMES * meter.stride);
        for (i, pair) in iq.chunks_exact_mut(2).enumerate() {
            let phase = std::f64::consts::TAU * 8501.37 * i as f64 / rate;
            pair[0] += (0.02 * phase.cos()) as f32;
            pair[1] += (0.02 * phase.sin()) as f32;
        }
        meter.push(&iq);
        let result = meter.snapshot();
        println!("adjacent: {result:?}");
        assert!((result.value.unwrap() - 10.0).abs() < 0.5);
    }
    #[test]
    fn bounded_work_and_memory_benchmark() {
        let rate = 390_625.0;
        let mut meter = RfMeter::new(rate, -50_000.0, 50_000.0, 100_000.0);
        let iq = fixture(rate, 100_000.0, 10.0, rate as usize * 2);
        let start = Instant::now();
        meter.push(&iq);
        let elapsed = start.elapsed().as_secs_f64();
        let retained = (meter.buffer.capacity() + meter.scratch.capacity()) * 16
            + (meter.window.capacity() + meter.spectrum.capacity()) * 8;
        println!("RF benchmark: 2 s IQ in {elapsed:.6} s; persistent FFT buffers {retained} bytes; <=64 FFT/s");
        assert!(retained < 512 * 1024);
        assert!(elapsed < 0.5);
    }
}

#[cfg(test)]
mod filter_validation {
    use super::tests::fixture;
    use super::*;
    #[test]
    fn real_channel_fir_does_not_turn_its_stopband_into_noise() {
        let fs = 976_562.5;
        let bw = 12000.0;
        let plan = crate::rate_plan::RatePlan::new(fs, "am", bw).unwrap();
        let mut meter = RfMeter::new(plan.output_rate, -bw / 2.0, bw / 2.0, bw);
        let handle = crate::channel_open_planned(fs, 0.0, &plan);
        assert!(!handle.is_null());
        let required = (FRAMES * meter.stride + 2048) * plan.decimate as usize;
        let input = fixture(fs, bw, -5.0, required);
        let mut emitted = 0;
        for chunk in input.chunks(32768) {
            let count = chunk.len() / 2;
            let mut output = vec![0.0; 2 * (count / plan.decimate as usize + 2)];
            let n = unsafe {
                crate::channel_process_float(
                    handle,
                    chunk.as_ptr(),
                    count as i32,
                    output.as_mut_ptr(),
                    (output.len() / 2) as i32,
                )
            };
            assert!(n >= 0);
            let skip = 1024_usize.saturating_sub(emitted).min(n as usize);
            emitted += n as usize;
            meter.push(&output[skip * 2..n as usize * 2]);
        }
        unsafe {
            crate::channel_close(handle);
        }
        let result = meter.snapshot();
        println!("real FIR Fs={} result {result:?}", plan.output_rate);
        assert!((result.value.unwrap() + 5.0).abs() < 1.0);
        meter.measured_at = Some(Instant::now() - std::time::Duration::from_secs(100));
        assert_eq!(meter.snapshot().reason, Some("stale"));
        assert_eq!(meter.snapshot().value, None);
    }
}

#[cfg(test)]
mod impairment_validation {
    use super::tests::fixture;
    use super::*;
    #[test]
    fn frequency_offset_and_slow_fading_keep_average_rf_power() {
        let rate = 48_000.0;
        let bw = 12000.0;
        let mut meter = RfMeter::new(rate, -bw / 2.0, bw / 2.0, bw);
        let n = FRAMES * meter.stride;
        let mut iq = fixture(rate, bw, 0.0, n);
        // Add a second off-bin, slowly fading tone. The independently known
        // average added power is 0.5*A² for this integer-cycle envelope.
        for (i, pair) in iq.chunks_exact_mut(2).enumerate() {
            let a = 0.002 * (std::f64::consts::TAU * 3.0 * i as f64 / n as f64).sin();
            let phase = std::f64::consts::TAU * 3543.137 * i as f64 / rate;
            pair[0] += (a * phase.cos()) as f32;
            pair[1] += (a * phase.sin()) as f32;
        }
        meter.push(&iq);
        let result = meter.snapshot();
        let expected = 10.0 * 1.5_f64.log10();
        println!(
            "RF fading CFO expected={expected:.4} actual={:?}",
            result.value
        );
        assert!((result.value.unwrap() - expected).abs() < 0.5);
    }
}
