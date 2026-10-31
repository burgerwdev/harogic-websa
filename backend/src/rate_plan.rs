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
    /// Independent IQ plan. The caller aggregates processor/consumer minimum rates;
    /// an empty processor set is valid and does not create an analog demodulator.
    pub fn baseband(coarse_rate: f64, passband_hz: f64, transition_hz: f64, minimum_rate_hz: f64) -> Result<Self, &'static str> {
        if ![coarse_rate, passband_hz, transition_hz, minimum_rate_hz].iter().all(|v| v.is_finite() && *v > 0.0) {
            return Err("invalid independent baseband requirements");
        }
        let needed = (passband_hz + 2.0 * transition_hz).max(minimum_rate_hz);
        if needed > coarse_rate { return Err("baseband requirements exceed coarse rate"); }
        let ceiling = (coarse_rate / needed).floor().min(65_536.0) as i32;
        let decimate = (1..=ceiling).rev().find(|&d| supported_decimation(d)).ok_or("no supported IQ clock")?;
        let plan = Self { decimate, output_rate: coarse_rate / decimate as f64,
            occupied_bandwidth_hz: passband_hz, transition_hz, stopband_hz: passband_hz / 2.0 + transition_hz,
            decoder_rate: None };
        if !plan.valid_for(coarse_rate) { return Err("baseband filter exceeds supported bounds"); }
        Ok(plan)
    }

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
            "radiofax" => (Some(12_000.0), 12_000.0, 0.125, 100.0),
            "drmplus" => (Some(96_000.0), 0.0, 0.125, 1_000.0),
            _ => match crate::catalog::external_rates(mode) {
                Some((rate, minimum)) => (Some(rate as f64), minimum as f64, 0.125, 100.0),
                None => return Err("unsupported channel mode"),
            },
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
        let plan = Self {
            decimate,
            output_rate,
            occupied_bandwidth_hz,
            transition_hz: transition,
            stopband_hz: occupied_bandwidth_hz / 2.0 + transition,
            decoder_rate,
        };
        if !plan.valid_for(coarse_rate) { return Err("channel filter geometry exceeds supported bounds"); }
        Ok(plan)
    }

    pub(crate) fn valid_for(&self, fs: f64) -> bool {
        fs.is_finite() && fs > 0.0 && supported_decimation(self.decimate)
            && self.output_rate.is_finite() && self.output_rate > 0.0
            && (self.output_rate - fs / self.decimate as f64).abs() <= fs * 1e-12
            && self.occupied_bandwidth_hz.is_finite() && self.occupied_bandwidth_hz > 0.0
            && self.transition_hz.is_finite() && self.transition_hz > 0.0
            && self.stopband_hz.is_finite() && self.stopband_hz <= self.output_rate / 2.0
            && self.occupied_bandwidth_hz / 2.0 > 0.0
            && self.stopband_hz - self.occupied_bandwidth_hz / 2.0 > 0.0
            && filter_taps(self.output_rate, self.occupied_bandwidth_hz / 2.0, self.stopband_hz).is_some()
            && (self.stopband_hz - self.occupied_bandwidth_hz / 2.0 - self.transition_hz).abs() <= fs * 1e-12
    }
}

/// Bound FIR memory and work before converting the floating-point tap estimate.
pub(crate) fn filter_taps(fs: f64, pass: f64, stop: f64) -> Option<usize> {
    let width = stop - pass;
    if !fs.is_finite() || fs <= 0.0 || !pass.is_finite() || pass <= 0.0
        || !stop.is_finite() || !width.is_finite() || width <= 0.0 { return None; }
    let estimate = (72.0 / (2.285 * 2.0 * std::f64::consts::PI * (width / fs))).ceil();
    if !estimate.is_finite() || !(0.0..65_536.0).contains(&estimate) { return None; }
    let n = (estimate as usize).checked_add(1)?.max(8).checked_next_multiple_of(8)?;
    (n <= 65_536 && n.checked_mul(2).is_some()).then_some(n)
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
    fn independent_iq_preserves_actual_clock_and_consumer_floor() {
        let plan = RatePlan::baseband(976_562.5, 3_000.0, 1_000.0, 6_000.0).unwrap();
        assert_eq!(plan.output_rate, 976_562.5 / plan.decimate as f64);
        assert!(plan.output_rate >= 6_000.0);
        assert_eq!(plan.decoder_rate, None);
        assert!(RatePlan::baseband(976_562.5, 1e6, 1e3, 6e3).is_err());
        assert!(RatePlan::baseband(976_562.5, 3e3, f64::NAN, 6e3).is_err());
    }

    #[test]
    fn malformed_public_plan_rejects_unrealizable_or_unbounded_filter() {
        let mut plan = RatePlan { decimate: 1, output_rate: 976_562.5,
            occupied_bandwidth_hz: 1_000.0, transition_hz: 1e-300,
            stopband_hz: 500.0, decoder_rate: None };
        assert!(!plan.valid_for(976_562.5));
        assert!(crate::narrow::channel_open_planned(976_562.5, 0.0, &plan).is_null());
        plan.stopband_hz = f64::from_bits(500.0f64.to_bits() + 1);
        plan.transition_hz = plan.stopband_hz - 500.0;
        assert!(!plan.valid_for(976_562.5));
        assert!(crate::narrow::channel_open_planned(976_562.5, 0.0, &plan).is_null());
    }

    #[test]
    fn occupied_edges_and_decoder_clocks_survive_narrow_and_wide_plans() {
        for mode in ["am", "dsb", "usb", "lsb", "cw", "nfm", "wfm", "pm", "ft8", "sstv", "drm", "drmplus", "radiofax"] {
            for bandwidth in [500.0, 3_000.0, 12_500.0, 100_000.0, 300_000.0] {
                let plan = RatePlan::new(976_562.5, mode, bandwidth).unwrap();
                assert!(plan.valid_for(976_562.5));
                assert!(plan.stopband_hz > bandwidth / 2.0);
                let minimum = match mode { "ft8" => 6_000.0, "sstv" => 8_000.0, "radiofax" => 12_000.0,
                    "wfm" => 300_000.0, _ => 0.0 };
                assert!(plan.output_rate >= minimum);
            }
        }
        assert!(RatePlan::new(976_562.5, "cw", 250.0).unwrap().output_rate < 1_000.0);
        assert!(RatePlan::new(976_562.5, "sstv", 6_000.0).unwrap().output_rate < 10_000.0);
        assert!(RatePlan::new(976_562.5, "ft8", 6_000.0).unwrap().output_rate < 10_000.0);
        assert!(RatePlan::new(976_562.5, "drm", 10_000.0).unwrap().output_rate < 20_000.0);
        assert!(RatePlan::new(976_562.5, "am", 800_000.0).is_err());

        let narrow = RatePlan::new(976_562.5, "wfm", 5_000.0).unwrap();
        assert!(narrow.output_rate >= 300_000.0);
        assert!(narrow.stopband_hz < 5_000.0,
            "WFM processing cadence must not widen the requested RF filter");
    }
    #[test]
    fn external_clocks_preserve_exact_decoder_contract_and_reject_wideband() {
        for (mode, width, exact) in [("pocsag1200", 12_500.0, 22_050.0),
            ("ism", 200_000.0, 250_000.0), ("ais", 125_000.0, 250_000.0),
            ("meteor", 150_000.0, 288_000.0)] {
            let plan = RatePlan::new(976_562.5, mode, width).unwrap();
            assert_eq!(plan.decoder_rate, Some(exact));
            assert!(plan.output_rate >= exact);
            assert!(crate::rational::Rational::from_rates(plan.output_rate, exact as u32, width / 2.0).is_ok());
        }
        assert!(RatePlan::new(976_562.5, "adsb", 2_000_000.0).is_err());
        assert!(RatePlan::new(200_000.0, "ism", 200_000.0).is_err());
    }

    #[test]
    fn direct_gps_and_adsb_plans_retain_filter_edges_and_actual_clocks() {
        for (mode,width,minimum,exact) in [("gps",2_046_000.0,2_600_000.0,4_000_000),
            ("adsb",1_800_000.0,2_500_000.0,2_000_000)] {
            assert!(RatePlan::new(976_562.5,mode,width).is_err());
            for fs in [3_906_250.0,7_812_500.0,15_625_000.0,31_250_000.0,62_500_000.0] {
                let plan=RatePlan::new(fs,mode,width).unwrap();
                assert!(plan.output_rate>=minimum);
                assert_eq!(plan.output_rate,fs/plan.decimate as f64);
                assert_eq!(plan.decoder_rate,Some(exact as f64));
                assert!(plan.stopband_hz<=plan.output_rate/2.0);
                assert!(crate::rational::Rational::from_rates(plan.output_rate,exact,width/2.0).is_ok());
            }
        }
        let gps=RatePlan::new(3_906_250.0,"gps",2_046_000.0).unwrap();
        assert_eq!(gps.output_rate,3_906_250.0);
        assert_eq!(gps.stopband_hz,1_278_750.0);
        assert!(crate::rational::Rational::from_rates(gps.output_rate,4_000_000,1_023_000.0).is_ok());
        assert!(RatePlan::new(2_500_000.0,"gps",2_046_000.0).is_err());
    }
}
