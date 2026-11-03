//! Native listening AGC. Measurement windows and gain envelopes advance on the PCM clock,
//! independently of the channelizer's packet sizes. No PCM is buffered or removed.

use crate::ddc::RmsAgc;

/// Keep the historical, fixture-backed DDC demodulator separate from native listening.
pub(super) enum AnalogAgc {
    Reference(RmsAgc),
    Listening(ListeningAgc),
}

impl AnalogAgc {
    pub fn new(rate: f64, native: bool) -> Self {
        if native {
            Self::Listening(ListeningAgc::new(rate))
        } else {
            Self::Reference(RmsAgc::reference())
        }
    }

    pub fn reset(&mut self) {
        match self {
            Self::Reference(agc) => agc.reset(),
            Self::Listening(agc) => agc.reset(),
        }
    }

    pub fn settle_for(&mut self, samples: usize) {
        if let Self::Listening(agc) = self {
            agc.settling = samples;
            agc.energy = 0.0;
            agc.measured = 0;
            agc.overloaded = 0;
            agc.desired = agc.gain;
            agc.fade = 0;
        }
    }

    pub fn bypass(&mut self, samples: usize) {
        if let Self::Listening(agc) = self {
            agc.settling = agc.settling.saturating_sub(samples);
        }
    }

    pub fn process_into(&mut self, input: &[f32], out: &mut Vec<f32>) {
        match self {
            Self::Reference(agc) => agc.process_into(input, false, out),
            Self::Listening(agc) => agc.process_into(input, out),
        }
    }
}

pub(super) struct ListeningAgc {
    window: usize,
    fade_samples: usize,
    attack: f64,
    release: f64,
    limiter_release: f64,
    settling: usize,
    energy: f64,
    measured: usize,
    overloaded: usize,
    gain: f64,
    desired: f64,
    limit: f64,
    primed: bool,
    fade: usize,
}

impl ListeningAgc {
    const TARGET: f64 = 0.1;
    const MAX_GAIN: f64 = 10_000.0;
    const CEILING: f64 = 0.95;
    // Preserve approximately the former native 35 ms packet's response, rather than making
    // the listener twice as fast by interpreting its per-packet coefficients as 20 ms.
    const ATTACK_SECONDS: f64 = 0.160;
    const RELEASE_SECONDS: f64 = 1.750;

    fn new(rate: f64) -> Self {
        Self {
            window: (rate * 0.020).round().max(1.0) as usize,
            fade_samples: (rate * 0.005).round().max(1.0) as usize,
            attack: -(-1.0 / (rate * Self::ATTACK_SECONDS)).exp_m1(),
            release: -(-1.0 / (rate * Self::RELEASE_SECONDS)).exp_m1(),
            // A single impulse must not suppress the following syllables for seconds. The
            // peak guard releases separately from the much slower RMS gain recovery.
            limiter_release: -(-1.0 / (rate * 0.050)).exp_m1(),
            settling: 0,
            energy: 0.0,
            measured: 0,
            overloaded: 0,
            gain: 1.0,
            desired: 1.0,
            limit: 1.0,
            primed: false,
            fade: 0,
        }
    }

    fn reset(&mut self) {
        self.energy = 0.0;
        self.measured = 0;
        self.overloaded = 0;
        self.gain = 1.0;
        self.desired = 1.0;
        self.limit = 1.0;
        self.primed = false;
        self.fade = 0;
        // Filters continue running with AGC off. Their remaining settling time is independent
        // of this switch; a full demodulator reset explicitly starts that interval again.
    }

    fn process_into(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());
        for &value in input {
            if self.settling > 0 {
                self.settling -= 1;
                out.push(0.0);
                continue;
            }
            let sample = value as f64;
            if self.primed {
                if sample.abs() * self.gain > 1.0 {
                    self.overloaded += 1;
                }
                let coefficient = if self.desired < self.gain {
                    self.attack
                } else {
                    self.release
                };
                if sample != 0.0 {
                    self.gain += (self.desired - self.gain) * coefficient;
                }
                self.fade = (self.fade + 1).min(self.fade_samples);
                let scaled = sample * self.gain * self.fade as f64 / self.fade_samples as f64;
                let wanted = if scaled.abs() > Self::CEILING {
                    Self::CEILING / scaled.abs()
                } else {
                    1.0
                };
                if wanted < self.limit {
                    self.limit = wanted;
                } else {
                    self.limit =
                        (self.limit + (1.0 - self.limit) * self.limiter_release).min(wanted);
                }
                out.push((scaled * self.limit) as f32);
            } else {
                // First measure a complete window of settled audio, then fade in. These zeros
                // occupy their original sample positions; there is no added presentation delay.
                out.push(0.0);
            }

            // Exact silence cannot prime an enormous gain on the very last sample of a window.
            if !self.primed && self.measured == 0 && sample == 0.0 {
                continue;
            }
            self.energy += sample * sample;
            self.measured += 1;
            if self.measured == self.window {
                if self.energy > 0.0 {
                    let rms = (self.energy / self.measured as f64).sqrt();
                    self.desired = (Self::TARGET / rms).min(Self::MAX_GAIN);
                    if !self.primed {
                        self.gain = self.desired;
                        self.primed = true;
                    } else if rms * self.gain > 1.0 && self.overloaded > self.window / 20 {
                        // Sustained overload needs the old clip guard's immediate correction;
                        // the ordinary attack would otherwise leave loud audio at the ceiling
                        // for hundreds of ms. Require >5% overloaded samples so one impulse
                        // uses only the short peak limiter, without a long RMS gain collapse.
                        self.limit = (self.limit * self.gain / self.desired).min(1.0);
                        self.gain = self.desired;
                    }
                } else {
                    // Silence freezes adaptation, rather than requesting maximum gain.
                    self.desired = self.gain;
                }
                self.energy = 0.0;
                self.measured = 0;
                self.overloaded = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(rate: usize) -> Vec<f32> {
        (0..rate * 5)
            .map(|k| {
                let time = k as f64 / rate as f64;
                let amplitude = if time < 0.5 {
                    0.0002
                } else if time < 0.8 {
                    0.2
                } else if time < 1.2 {
                    0.002
                } else if time < 2.0 {
                    0.0
                } else {
                    0.0005
                };
                if k == rate * 3 {
                    12.0
                } else {
                    (amplitude * (core::f64::consts::TAU * 1000.0 * time).sin()) as f32
                }
            })
            .collect()
    }

    fn replay(input: &[f32], rate: f64, block: usize) -> (Vec<f32>, ListeningAgc) {
        let mut agc = ListeningAgc::new(rate);
        let mut output = Vec::new();
        let mut scratch = Vec::new();
        for chunk in input.chunks(block) {
            agc.process_into(chunk, &mut scratch);
            assert_eq!(scratch.len(), chunk.len());
            output.extend_from_slice(&scratch);
        }
        (output, agc)
    }

    #[test]
    fn envelopes_and_impulse_limiter_are_independent_of_packet_sizes() {
        let input = signal(48_000);
        let (expected, _) = replay(&input, 48_000.0, input.len());
        assert!(expected
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 0.95 + f32::EPSILON));
        let strong = &expected[26_400..31_200]; // 50-150 ms after a 60 dB rise.
        let strong_rms =
            (strong.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / strong.len() as f64).sqrt();
        assert!(
            (0.08..0.14).contains(&strong_rms),
            "a sustained strong station must reach the target promptly: {strong_rms}"
        );
        for size in [1, 17, 240, 960, 1699, 8192] {
            assert_eq!(
                replay(&input, 48_000.0, size).0,
                expected,
                "packet size {size}"
            );
        }
        let recovered = &expected[48_000 * 4..];
        let rms = (recovered.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
            / recovered.len() as f64)
            .sqrt();
        assert!(
            rms > 0.07 && rms < 0.13,
            "speech must recover after the pause and impulse: {rms}"
        );
    }

    #[test]
    fn attack_and_release_use_elapsed_sample_time_at_each_audio_rate() {
        for rate in [8000, 24000, 44100, 48000, 96000] {
            for (initial, next, seconds) in [
                (0.2_f32, 0.002_f32, ListeningAgc::RELEASE_SECONDS),
                (0.02, 0.1, ListeningAgc::ATTACK_SECONDS),
            ] {
                let mut agc = ListeningAgc::new(rate as f64);
                let mut out = Vec::new();
                agc.process_into(&vec![initial; rate], &mut out);
                let before = agc.gain;
                agc.process_into(&vec![next; rate], &mut out);
                let target = ListeningAgc::TARGET / next as f64;
                // The detector measures a complete 20 ms window before changing its request.
                let elapsed = (rate - agc.window) as f64 / rate as f64;
                let expected = target + (before - target) * (-elapsed / seconds).exp();
                assert!(
                    (agc.gain - expected).abs() < 1e-8,
                    "rate {rate}, {initial}->{next}: {} vs {expected}",
                    agc.gain
                );
            }
        }
    }

    #[test]
    fn settled_measurement_and_short_fade_prevent_startup_gain_windup() {
        let mut agc = AnalogAgc::new(48_000.0, true);
        agc.settle_for(2725);
        let mut input = vec![1e-12_f32; 2725];
        input.extend(vec![0.001; 48000]);
        let mut all = Vec::new();
        let mut out = Vec::new();
        for chunk in input.chunks(1699) {
            agc.process_into(chunk, &mut out);
            all.extend_from_slice(&out);
        }
        assert_eq!(all.len(), input.len());
        assert!(all[..2725 + 960].iter().all(|v| *v == 0.0));
        assert!(all.iter().all(|v| *v >= 0.0 && *v <= 0.10001));
        assert!((all[2725 + 960 + 240] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn silence_does_not_prime_or_maximize_gain_and_weak_noise_stays_bounded() {
        let mut agc = ListeningAgc::new(48000.0);
        let mut out = Vec::new();
        agc.process_into(&vec![0.0; 48000], &mut out);
        assert!(!agc.primed);
        assert_eq!(agc.gain, 1.0);
        // One nonzero sample at the end of silence starts a fresh full measurement window.
        agc.process_into(&[0.001], &mut out);
        assert!(!agc.primed);
        agc.process_into(&vec![0.001; 48000], &mut out);
        let gain = agc.gain;
        agc.process_into(&vec![0.0; 48000], &mut out);
        assert!((agc.gain - gain).abs() < 1e-9);
        agc.reset();
        agc.process_into(&vec![1e-12; 48000], &mut out);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0001e-8));
        assert!(agc.gain <= ListeningAgc::MAX_GAIN);
    }

    #[test]
    fn bypass_advances_filter_settling_and_retune_excludes_old_measurement() {
        let mut agc = AnalogAgc::new(48000.0, true);
        agc.settle_for(2725);
        agc.bypass(48000);
        agc.reset();
        let mut out = Vec::new();
        agc.process_into(&vec![0.001; 1201], &mut out);
        assert!(
            (out[1200] - 0.1).abs() < 1e-6,
            "enabling AGC on settled filters should only measure and fade"
        );
        agc.settle_for(2725);
        agc.process_into(&vec![1e-12; 2725], &mut out);
        assert!(out.iter().all(|v| *v == 0.0));
        if let AnalogAgc::Listening(agc) = agc {
            assert!(
                (agc.gain - 100.0).abs() < 1e-4,
                "retune preserves gain without measuring filter startup"
            );
            assert_eq!(agc.measured, 0);
        } else {
            unreachable!();
        }
    }
}
