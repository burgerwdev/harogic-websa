//! RMS AGC (`Agc` in `web_sa/demod/filters.py`), used as the level stage of the DDC chain.
//!
//! The reference behaviour is preserved down to the priming rule and the ceiling, because those
//! two are what stopped the "loud burst after every retune" defect:
//!
//!   * a silence gate keeps the gain from winding up on empty input;
//!   * `hold` applies the current gain without adapting (used while a reconfiguration transient
//!     is being discarded);
//!   * the first block after a configure jumps straight to the safe gain, and any block whose
//!     current gain would push it past full scale does the same — ramping there made FM audio
//!     clip for tens of blocks;
//!   * a peak ceiling bounds the block even when the RMS target alone would let it clip.
//!
//! The digital path must run with the AGC **off**: an automatic level control changes the
//! amplitude a decoder relies on. That is why the stage is optional here rather than always on.

#[derive(Debug, Clone)]
pub struct RmsAgc {
    target: f64,
    attack: f64,
    release: f64,
    max_gain: f64,
    silence_floor: f64,
    ceiling: f64,
    gain: f64,
    limit: f64,
    primed: bool,
}

impl RmsAgc {
    pub fn new(target: f64, attack: f64, release: f64) -> Self {
        Self {
            target,
            attack,
            release,
            max_gain: 1.0e4,
            silence_floor: 1.0e-4,
            ceiling: 0.95,
            gain: 1.0,
            limit: 1.0,
            primed: false,
        }
    }

    /// The reference configuration (`Agc(target=0.1, attack=0.2, release=0.02)`).
    ///
    /// The target is deliberately well below full scale: a noise floor has a block crest factor of
    /// about 4.5, so a 0.2 target puts the block peak at the 0.95 ceiling and the ceiling then works
    /// continuously - on noise it was the level itself that moved (2.2x measured, AM). At 0.1 the
    /// ceiling stays out of the way of ordinary noise and only catches a real burst.
    ///
    /// The release is slow (~1 s) for the same reason: on a noise floor a fast release makes the gain
    /// follow the noise's own fluctuation instead of ignoring it, which the listener hears as the
    /// level breathing (irregular, because the noise is). A second is the usual AGC release for AM/SSB.
    pub fn reference() -> Self {
        Self::new(0.1, 0.2, 0.02)
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.limit = 1.0;
        self.primed = false;
    }

    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// The RMS the AGC drives the output to (the tests read it rather than repeating the number).
    pub fn target(&self) -> f64 {
        self.target
    }

    /// Apply the AGC to a real block, writing into `out` (cleared first).
    pub fn process_into(&mut self, x: &[f32], hold: bool, out: &mut Vec<f32>) {
        out.clear();
        if x.is_empty() {
            return;
        }
        if !hold {
            let sum_sq: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum();
            let rms = ((sum_sq / x.len() as f64) + 1e-20).sqrt();
            if rms >= self.silence_floor {
                let desired = self.max_gain.min(self.target / rms.max(1e-9));
                if !self.primed || rms * self.gain > 1.0 {
                    self.gain = desired;
                    self.primed = true;
                } else {
                    let coef = if desired < self.gain { self.attack } else { self.release };
                    self.gain += (desired - self.gain) * coef;
                }
            }
        }
        let mut peak = 0.0_f64;
        out.reserve(x.len());
        for value in x {
            let scaled = (*value as f64) * self.gain;
            let abs = scaled.abs();
            if abs > peak {
                peak = abs;
            }
            out.push(scaled as f32);
        }
        if self.ceiling > 0.0 {
            // A limiter *gain* (instant down, slow up) rather than a per-block rescale. A per-block
            // rescale modulates the level by that block's crest factor: on a noise floor the crest
            // factor varies block to block, so the level swung by up to 2.2x (measured on AM noise)
            // with a step at every block boundary - the periodic sound heard on a quiet frequency.
            // The limiter holds a steady value on steady input, so the level holds steady too.
            let wanted = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            if wanted < self.limit {
                self.limit = wanted;
            } else {
                self.limit += (wanted - self.limit) * self.release;
            }
            if self.limit < 1.0 {
                for value in out.iter_mut() {
                    *value = ((*value as f64) * self.limit) as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|k| amp * (2.0 * core::f32::consts::PI * 1000.0 * k as f32 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn a_quiet_block_is_brought_up_to_the_target() {
        let mut agc = RmsAgc::reference();
        let mut out = Vec::new();
        agc.process_into(&sine(960, 0.01), false, &mut out);
        let rms = (out.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt();
        assert!((rms - agc.target()).abs() < 0.02, "rms {rms}");
    }

    #[test]
    fn silence_never_raises_the_gain() {
        let mut agc = RmsAgc::reference();
        let mut out = Vec::new();
        agc.process_into(&vec![0.0; 960], false, &mut out);
        assert_eq!(agc.gain(), 1.0);
    }

    #[test]
    fn hold_applies_the_gain_without_adapting() {
        let mut agc = RmsAgc::reference();
        let mut out = Vec::new();
        agc.process_into(&sine(960, 0.05), false, &mut out);
        let gain = agc.gain();
        // A held block small enough that the ceiling does not engage: what is asserted is that
        // the frozen gain is still applied, linearly.
        agc.process_into(&sine(960, 0.1), true, &mut out);
        assert_eq!(agc.gain(), gain, "hold must freeze the adaptation");
        let peak = out.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(
            (peak as f64 - 0.1 * gain).abs() < 1e-3,
            "a held block must still be scaled by the frozen gain: peak {peak}, expected {}",
            0.1 * gain
        );
    }

    #[test]
    fn the_limiter_releases_instead_of_scaling_each_block() {
        // The ceiling used to scale each block by its own peak, so the level followed the block's
        // crest factor - a breathing, rough noise floor with a step at every block boundary
        // (measured 2.2x on AM noise through the browser, and reproduced offline from a capture).
        // It is a limiter *gain* now: instant down, slow up, so this state exists and holds steady
        // on steady input. (The old code had no such state to assert, which is the point.)
        let mut agc = RmsAgc::reference();
        let mut out = Vec::new();
        agc.process_into(&sine(960, 0.3), false, &mut out);
        assert_eq!(agc.limit, 1.0, "a block inside the ceiling is not limited");
        let mut peaky = sine(960, 0.3);
        for (i, v) in peaky.iter_mut().enumerate() {
            if i % 97 == 0 {
                *v = 12.0;
            }
        }
        agc.process_into(&peaky, false, &mut out);
        let caught = agc.limit;
        assert!(caught < 0.5, "a burst past the ceiling must pull the limiter down: {caught}");
        // Recovery is slow (the AGC's release), not instant: it takes several blocks.
        agc.process_into(&sine(960, 0.3), false, &mut out);
        assert!(agc.limit > caught, "the limiter recovers");
        assert!(agc.limit < 1.0, "but not within one block: {caught} -> {}", agc.limit);
    }

    #[test]
    fn the_ceiling_bounds_the_block() {
        let mut agc = RmsAgc::reference();
        let mut out = Vec::new();
        agc.process_into(&sine(960, 50.0), false, &mut out);
        let peak = out.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(peak <= 0.95 + 1e-6, "peak {peak} must stay under the ceiling");
    }

    #[test]
    fn the_first_block_jumps_to_the_safe_gain() {
        // A configured (unprimed) AGC must not ramp: the first block already has the right level.
        let mut agc = RmsAgc::reference();
        let mut out = Vec::new();
        agc.process_into(&sine(960, 0.001), false, &mut out);
        let rms = (out.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt();
        assert!((rms - agc.target()).abs() < 0.02, "first block rms {rms}");
    }
}
