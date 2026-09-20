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
            primed: false,
        }
    }

    /// The reference configuration (`Agc(target=0.2, attack=0.2, release=0.08)`).
    pub fn reference() -> Self {
        Self::new(0.2, 0.2, 0.08)
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.primed = false;
    }

    pub fn gain(&self) -> f64 {
        self.gain
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
        if self.ceiling > 0.0 && peak > self.ceiling {
            let scale = self.ceiling / peak;
            for value in out.iter_mut() {
                *value = ((*value as f64) * scale) as f32;
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
        assert!((rms - 0.2).abs() < 0.02, "rms {rms}");
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
        assert!((rms - 0.2).abs() < 0.02, "first block rms {rms}");
    }
}
