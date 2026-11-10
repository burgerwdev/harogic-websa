//! Meter only: quality must never decide whether a CRC-valid message is emitted.
use super::*;
use core::f64::consts::TAU;

#[derive(Debug, Clone, PartialEq)]
pub struct SnrEstimate {
    pub value: Option<f64>,
    /// Unavailable reason. Finite values carry independent quality reasons below.
    pub reason: Option<&'static str>,
    pub quality: &'static str,
    pub quality_reasons: Vec<&'static str>,
    /// No report clamp or substituted boundary is applied by this estimator.
    pub value_kind: &'static str,
    pub averaging_seconds: f64,
    pub enbw_hz: f64,
    pub symbols: usize,
    /// Noise-statistical approximation, excluding interference and model bias.
    pub uncertainty_db: Option<f64>,
    pub reference_samples: usize,
    pub reference_spread_db: Option<f64>,
}

impl Default for SnrEstimate {
    fn default() -> Self {
        Self {
            value: None,
            reason: Some("no_reliable_symbol_fit"),
            quality: "unavailable",
            quality_reasons: Vec::new(),
            value_kind: "unavailable",
            averaging_seconds: 0.0,
            enbw_hz: 0.0,
            symbols: 0,
            uncertainty_db: None,
            reference_samples: 0,
            reference_spread_db: None,
        }
    }
}

impl SnrEstimate {
    fn flag(&mut self, reason: &'static str) {
        if !self.quality_reasons.contains(&reason) {
            self.quality_reasons.push(reason);
        }
    }
}

/// Measure a CRC-validated, independently supplied codeword. This does not test
/// decoder sensitivity: only an actual Ft8Decoder message recovery does that.
/// Input is unmodified interleaved complex IQ, with no absolute amplitude unit.
pub fn measure_decoded_snr(
    samples: &[f32],
    rate: f64,
    codeword: &[u8; 174],
    frequency_hz: f64,
    start_seconds: f64,
) -> SnrEstimate {
    if !rate.is_finite()
        || !(6000.0..=192000.0).contains(&rate)
        || !frequency_hz.is_finite()
        || !start_seconds.is_finite()
        || samples.len() % 2 != 0
        || codeword.iter().any(|&b| b > 1)
        || !crc_ok(codeword)
    {
        return SnrEstimate {
            reason: Some("invalid_measurement_input"),
            ..SnrEstimate::default()
        };
    }
    let start = start_seconds * rate;
    if start < -((NUM_SYMBOLS * symbol_samples_at(rate)) as f64)
        || start > samples.len() as f64 / 2.0
    {
        return SnrEstimate {
            reason: Some("insufficient_complete_symbols"),
            ..SnrEstimate::default()
        };
    }
    let model = CancellationModel {
        frequency: frequency_hz,
        start: start.round() as i64,
        gains: [(0.0, 0.0); NUM_SYMBOLS],
    };
    estimate_snr(samples, rate, &tones_from_codeword(codeword), &model)
}

/// Under complex/positive-frequency real AWGN, individual matched-filter powers
/// are exponential. E[X_(k)]/mean = H_n-H_(n-k), so this finite-sample corrected
/// median estimates the mean noise power without choosing the quietest bins.
/// It bounds the influence of isolated strong off-tone observations. Persistent
/// majority contamination can still raise N0: report that limitation explicitly.
fn exponential_median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f64::total_cmp);
    let n = values.len();
    let lo = (n - 1) / 2;
    let hi = n / 2;
    let expected_order = |index: usize| ((n - index)..=n).map(|j| 1.0 / j as f64).sum::<f64>();
    Some((values[lo] + values[hi]) / (expected_order(lo) + expected_order(hi)))
}

/// Upper-tail rejection must compensate for the noise samples it excludes.
/// For X~Exp(mean), E[X | X<c*mean]/mean = 1-c/(exp(c)-1).
/// A bounded iteration of this conditional-mean equation rejects strong sparse
/// references without the low-noise selection bias of an uncorrected trim.
fn robust_noise(values: &mut [f64]) -> Option<f64> {
    let mut scale = exponential_median(values)?;
    let cutoff = 4.0_f64;
    let correction = 1.0 - cutoff / (cutoff.exp() - 1.0);
    for _ in 0..12 {
        if scale <= 0.0 {
            return Some(scale);
        }
        let (sum, count) = values
            .iter()
            .filter(|&&p| p < cutoff * scale)
            .fold((0.0, 0), |(sum, count), &p| (sum + p, count + 1));
        if count == 0 {
            return None;
        }
        let next = sum / count as f64 / correction;
        let converged = (next / scale - 1.0).abs() < 1e-6;
        scale = next;
        if converged {
            break;
        }
    }
    Some(scale)
}

/// Central 75% periodic Hann filters avoid GFSK transitions. ENBW is computed
/// from the actual samples. Six fixed off-tone references (+/-5,7,9 window bins)
/// stay in the receive/search band. A pooled corrected median initializes the
/// tail-corrected noise fit; both sides participate, preserving local slopes to
/// first order. Side medians diagnose asymmetry, never select the quiet side.
/// One-sided edge support is flagged. Nothing runs on cancelled IQ.
pub(super) fn estimate_snr(
    samples: &[f32],
    rate: f64,
    tones: &[u8; NUM_SYMBOLS],
    model: &CancellationModel,
) -> SnrEstimate {
    let sym = symbol_samples_at(rate);
    let n = sym * 3 / 4;
    let mut result = SnrEstimate::default();
    if n < 16 {
        return result;
    }
    // A floating IQ value beyond +/-1 can simply be scaled IQ, not clipping.
    // Repeated extrema diagnose possible rails without imposing amplitude units.
    let (mut peak, mut repeats) = (0.0_f32, 0_usize);
    for &x in samples {
        if !x.is_finite() {
            result.reason = Some("nonfinite_iq");
            return result;
        }
        if x.abs() > peak {
            peak = x.abs();
            repeats = 1;
        } else if x.abs() == peak {
            repeats += 1;
        }
    }
    if samples.len() < 4
        || !samples
            .chunks_exact(2)
            .any(|p| p[0] != samples[0] || p[1] != samples[1])
    {
        result.reason = Some("noise_not_resolved");
        return result;
    }
    if peak > 0.0 && repeats >= 8 && repeats > samples.len() / 1000 {
        result.flag("clipping_detected");
    }
    let window: Vec<_> = (0..n)
        .map(|i| 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos())
        .collect();
    let sum = window.iter().sum::<f64>();
    let enbw = rate * window.iter().map(|w| w * w).sum::<f64>() / (sum * sum);
    result.enbw_hz = enbw;
    let offsets = [0.0, -9.0, -7.0, -5.0, 5.0, 7.0, 9.0];
    let mut sides: [Vec<f64>; 2] = core::array::from_fn(|_| Vec::with_capacity(NUM_SYMBOLS * 3));
    let mut powers = [0.0; 7];
    let mut counts = [0; 7];
    let (mut sync_power, mut sync_count) = (0.0, 0);
    for (k, &tone) in tones.iter().enumerate() {
        let start = model.start + (k * sym + sym / 8) as i64;
        if start < 0 || start as usize + n > samples.len() / 2 {
            continue;
        }
        let frequency = model.frequency + f64::from(tone) * TONE_SPACING_HZ;
        if !(F_MIN_HZ..=F_MAX_HZ).contains(&frequency) {
            result.flag("limited_reference_support");
            continue;
        }
        for (j, &offset) in offsets.iter().enumerate() {
            let reference = frequency + offset * rate / n as f64;
            if !(F_MIN_HZ..=F_MAX_HZ).contains(&reference) {
                result.flag("limited_reference_support");
                continue;
            }
            let step = TAU * reference / rate;
            let (ds, dc) = step.sin_cos();
            let (mut wr, mut wi) = (1.0, 0.0);
            let (mut re, mut im) = (0.0, 0.0);
            for (i, &w) in window.iter().enumerate() {
                let index = (start as usize + i) * 2;
                let (yr, yi) = (f64::from(samples[index]), f64::from(samples[index + 1]));
                re += w * (yr * wr + yi * wi);
                im += w * (yi * wr - yr * wi);
                let next = wr * dc - wi * ds;
                wi = wr * ds + wi * dc;
                wr = next;
            }
            let p = (re * re + im * im) / (sum * sum);
            powers[j] += p;
            counts[j] += 1;
            if j > 0 {
                sides[usize::from(j > 3)].push(p);
            } else if is_sync_symbol(k) {
                sync_power += p;
                sync_count += 1;
            }
        }
        result.symbols += 1;
    }
    result.averaging_seconds = result.symbols as f64 * n as f64 / rate;
    result.reference_samples = sides.iter().map(Vec::len).sum();
    if result.symbols < 8 || result.reference_samples < 8 {
        result.reason = Some("insufficient_complete_symbols");
        return result;
    }
    if result.symbols < NUM_SYMBOLS {
        result.flag("partial_burst");
    }
    let left = exponential_median(&mut sides[0]);
    let right = exponential_median(&mut sides[1]);
    match (left, right) {
        (Some(a), Some(b)) => {
            if a > 0.0 && b > 0.0 && (10.0 * (a / b).log10()).abs() > 2.0 {
                result.flag("nonflat_background");
            }
        }
        (Some(_), None) | (None, Some(_)) => {
            result.flag("limited_reference_support");
        }
        (None, None) => {}
    }
    let mut references = Vec::with_capacity(result.reference_samples);
    references.extend_from_slice(&sides[0]);
    references.extend_from_slice(&sides[1]);
    let Some(background) = robust_noise(&mut references) else {
        result.reason = Some("noise_not_resolved");
        return result;
    };
    if background <= 0.0 || !background.is_finite() {
        result.reason = Some("noise_not_resolved");
        return result;
    }
    let means: Vec<_> = (1..7)
        .filter(|&i| counts[i] > 0)
        .map(|i| powers[i] / counts[i] as f64)
        .collect();
    let min = means.iter().copied().fold(f64::INFINITY, f64::min);
    let max = means.iter().copied().fold(0.0, f64::max);
    if min > 0.0 {
        result.reference_spread_db = Some(10.0 * (max / min).log10());
    }
    let reference_mean = powers[1..].iter().sum::<f64>() / result.reference_samples as f64;
    if max > 4.0 * min || reference_mean > 1.5 * background {
        result.flag("off_tone_interference");
    }
    let signal = powers[0] / result.symbols as f64 - background;
    if signal <= 0.0 || !signal.is_finite() {
        result.reason = Some("signal_not_resolved");
        return result;
    }
    if sync_count == 0 || sync_power <= sync_count as f64 * background {
        result.flag("sync_power_not_resolved");
    }
    let ratio = signal / background;
    let db = 10.0 * (ratio * enbw / 2500.0).log10();
    if !db.is_finite() {
        result.reason = Some("signal_not_resolved");
        return result;
    }
    let uncertainty = 10.0 / core::f64::consts::LN_10
        * ((1.0 + 2.0 * ratio) / result.symbols as f64
            + 1.0 / (result.reference_samples as f64 * core::f64::consts::LN_2.powi(2)))
        .sqrt()
        / ratio;
    result.uncertainty_db = Some(uncertainty);
    if uncertainty > 1.5 {
        result.flag("high_statistical_uncertainty");
    }
    result.value = Some(db);
    result.value_kind = "estimate";
    result.reason = None;
    result.quality = if result.quality_reasons.is_empty() {
        "good"
    } else {
        "degraded"
    };
    result
}
