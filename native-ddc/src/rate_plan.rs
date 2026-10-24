//! Per-channel integer IQ clocks. Decoder-specific exact-rate conversion belongs in the demodulator.
//! Occupied bandwidth is the full, two-sided RF passband; no audio-rate floor is applied.

#[derive(Clone, Debug, PartialEq)]
pub struct RatePlan {
    pub decimate: i32,
    pub output_rate: f64,
    pub occupied_bandwidth_hz: f64,
    /// Width from the occupied passband edge to the final filter stopband.
    pub transition_hz: f64,
    /// One-sided final stopband edge.
    pub stopband_hz: f64,
    pub decoder_rate: Option<f64>,
}

impl RatePlan {
    pub fn new(coarse_rate: f64, mode: &str, occupied_bandwidth_hz: f64) -> Result<Self, &'static str> {
        if !coarse_rate.is_finite() || coarse_rate <= 0.0
            || !occupied_bandwidth_hz.is_finite() || occupied_bandwidth_hz <= 0.0 {
            return Err("invalid channel rate or occupied bandwidth");
        }
        let (decoder_rate, minimum_rate, relative_transition, minimum_transition): (Option<f64>, f64, f64, f64) = match mode {
            "am" | "dsb" | "usb" | "lsb" | "nfm" | "pm" => (None, 0.0, 0.125, 100.0),
            "cw" => (None, 0.0, 0.125, 25.0),
            "wfm" => (None, 300_000.0, 0.125, 2_000.0),
            "ft8" => (Some(12_000.0), 6_000.0, 0.125, 100.0),
            "sstv" => (Some(8_000.0), 8_000.0, 0.125, 100.0),
            "drm" => (Some(48_000.0), 0.0, 0.125, 250.0),
            "drmplus" => (Some(96_000.0), 0.0, 0.125, 1_000.0),
            _ => return Err("unsupported channel mode"),
        };
        let transition = (occupied_bandwidth_hz * relative_transition).max(minimum_transition);
        let needed = (occupied_bandwidth_hz + 2.0 * transition).max(minimum_rate);
        if needed > coarse_rate {
            return Err("occupied bandwidth and transition exceed coarse IQ rate");
        }
        let ceiling = (coarse_rate / needed).floor().min(65_536.0) as i32;
        let decimate = (1..=ceiling).rev().find(|&d| supported_decimation(d))
            .ok_or("no supported integer channel rate")?;
        let output_rate = coarse_rate / decimate as f64;
        Ok(Self {
            decimate,
            output_rate,
            occupied_bandwidth_hz,
            transition_hz: output_rate / 2.0 - occupied_bandwidth_hz / 2.0,
            stopband_hz: output_rate / 2.0,
            decoder_rate,
        })
    }

    pub(crate) fn valid_for(&self, fs: f64) -> bool {
        fs.is_finite() && fs > 0.0 && supported_decimation(self.decimate)
            && self.output_rate.is_finite() && self.output_rate > 0.0
            && (self.output_rate - fs / self.decimate as f64).abs() <= fs * 1e-12
            && self.occupied_bandwidth_hz.is_finite() && self.occupied_bandwidth_hz > 0.0
            && self.transition_hz.is_finite() && self.transition_hz >= self.output_rate / 1_024.0
            && self.stopband_hz.is_finite() && self.stopband_hz <= self.output_rate / 2.0
            && (self.stopband_hz - self.occupied_bandwidth_hz / 2.0 - self.transition_hz).abs() <= fs * 1e-12
    }
}

fn supported_decimation(decimate: i32) -> bool {
    if !(1..=65_536).contains(&decimate) { return false; }
    let mut remaining = decimate;
    for factor in [2, 3, 5, 7] {
        while remaining % factor == 0 { remaining /= factor; }
    }
    remaining == 1
}

/// Small integer factors bound each anti-alias filter's work, including very narrow channels.
pub(crate) fn stage_factors(decimate: i32) -> Option<Vec<usize>> {
    if !(1..=65_536).contains(&decimate) { return None; }
    let mut remaining = decimate as usize;
    let mut factors = Vec::new();
    for factor in [8, 7, 5, 4, 3, 2] {
        while remaining % factor == 0 {
            factors.push(factor);
            remaining /= factor;
        }
    }
    (remaining == 1).then_some(factors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_edges_and_decoder_clocks_survive_narrow_and_wide_plans() {
        for mode in ["am", "dsb", "usb", "lsb", "cw", "nfm", "wfm", "pm", "ft8", "sstv", "drm", "drmplus"] {
            for bandwidth in [500.0, 3_000.0, 12_500.0, 100_000.0, 300_000.0] {
                let plan = RatePlan::new(976_562.5, mode, bandwidth).unwrap();
                assert!(plan.valid_for(976_562.5));
                assert!(plan.stopband_hz > bandwidth / 2.0);
                let minimum = match mode { "ft8" => 6_000.0, "sstv" => 8_000.0,
                    "wfm" => 300_000.0, _ => 0.0 };
                assert!(plan.output_rate >= minimum);
            }
        }
        assert!(RatePlan::new(976_562.5, "cw", 250.0).unwrap().output_rate < 1_000.0);
        assert!(RatePlan::new(976_562.5, "sstv", 6_000.0).unwrap().output_rate < 10_000.0);
        assert!(RatePlan::new(976_562.5, "ft8", 6_000.0).unwrap().output_rate < 10_000.0);
        assert!(RatePlan::new(976_562.5, "drm", 10_000.0).unwrap().output_rate < 20_000.0);
        assert!(RatePlan::new(976_562.5, "am", 800_000.0).is_err());
    }
}
