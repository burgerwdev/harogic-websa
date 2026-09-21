//! Time-domain audio-enhancement stages: low-pass, AGC and squelch.
//!
//! All three are analog-PCM-only (see `pipeline::AudioChain`) and all three are streaming: the
//! filter tail, the AGC gain and the squelch gate carry across blocks, which is what stops a
//! 20 ms block boundary from becoming an audible seam.

use crate::audio::dc_block::DcBlock;
use crate::ddc::fir::{design_lowpass, FirState};
use crate::ddc::RmsAgc;
use crate::plugin::AudioStage;

/// The rate the chain ran at before the consumer's rate became a parameter (the tests' reference
/// and the fallback when a caller does not know it).
pub const DEFAULT_AUDIO_RATE: f64 = 48_000.0;

/// Anti-alias / hiss low-pass. Default corner is speech bandwidth, not the full 15 kHz: the
/// narrow modes benefit and a wider setting is one constructor argument away.
pub struct Lpf {
    filter: FirState,
    cutoff_hz: f64,
    rate: f64,
}

impl Lpf {
    pub fn new(cutoff_hz: f64, rate: f64) -> Self {
        let rate = if rate > 0.0 { rate } else { DEFAULT_AUDIO_RATE };
        let cutoff = cutoff_hz.clamp(100.0, 0.45 * rate);
        Self {
            filter: FirState::new(design_lowpass(rate, cutoff, 129)),
            cutoff_hz: cutoff,
            rate,
        }
    }

    pub fn speech(rate: f64) -> Self {
        Self::new(8_000.0, rate)
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn cutoff_hz(&self) -> f64 {
        self.cutoff_hz
    }
}

impl AudioStage for Lpf {
    fn id(&self) -> &'static str {
        "lpf"
    }

    fn process_into(&mut self, input: &[f32], _hold: bool, out: &mut Vec<f32>) {
        let x: Vec<f64> = input.iter().map(|v| *v as f64).collect();
        let mut y = Vec::new();
        self.filter.process_into(&x, &mut y);
        out.clear();
        out.extend(y.iter().map(|v| *v as f32));
    }

    fn reset(&mut self) {
        self.filter.reset();
    }
}

/// The reference AGC (target 0.2, attack 0.2, release 0.08) as a chain stage.
pub struct AgcStage {
    agc: RmsAgc,
}

impl AgcStage {
    pub fn reference() -> Self {
        Self { agc: RmsAgc::reference() }
    }

    pub fn gain(&self) -> f64 {
        self.agc.gain()
    }
}

impl AudioStage for AgcStage {
    fn id(&self) -> &'static str {
        "agc"
    }

    fn process_into(&mut self, input: &[f32], hold: bool, out: &mut Vec<f32>) {
        // `hold` freezes the adaptation while a reconfiguration transient is being discarded, so
        // the gain cannot wind up on audio that is never published (the audibility defect the
        // Python path fixed by the same means).
        self.agc.process_into(input, hold, out);
    }

    fn reset(&mut self) {
        self.agc.reset();
    }
}

/// Level squelch: mutes the channel while it is only noise, with hysteresis, a hold time and a
/// smoothed gate (a bare comparator chatters at the threshold, and a hard step is audible).
///
/// The measurement is the block's level in dBFS, which is what the Python path used
/// (`sdr.py`: `sdr_level_dbfs` against `sdr_squelch`). An SNR-referenced decision needs a noise
/// estimate band; the guard-band estimator belongs with the demodulator that knows its bandwidth,
/// not with a stage that only sees PCM.
pub struct Squelch {
    threshold_dbfs: f64,
    /// The rate the gate's time constants are expressed at.
    rate: f64,
    hysteresis_db: f64,
    hold_s: f64,
    attack_s: f64,
    release_s: f64,
    gain: f64,
    open: bool,
    hold_until_s: f64,
    now_s: f64,
}

impl Squelch {
    pub fn new(threshold_dbfs: f64, rate: f64) -> Self {
        Self {
            threshold_dbfs,
            rate: if rate > 0.0 { rate } else { DEFAULT_AUDIO_RATE },
            hysteresis_db: 3.0,
            hold_s: 0.15,
            attack_s: 0.01,
            release_s: 0.08,
            gain: 0.0,
            open: false,
            hold_until_s: 0.0,
            now_s: 0.0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// Feed a block and advance the internal clock by its duration.
    pub fn process_block(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len();
        let level = if n == 0 {
            -200.0
        } else {
            let mean_sq = input.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / n as f64;
            10.0 * (mean_sq + 1e-20).log10()
        };
        let dt = n as f64 / self.rate;
        if level >= self.threshold_dbfs {
            self.open = true;
            self.hold_until_s = self.now_s + self.hold_s;
        } else if level < self.threshold_dbfs - self.hysteresis_db && self.now_s >= self.hold_until_s {
            self.open = false;
        }
        let target = if self.open { 1.0 } else { 0.0 };
        let tau = if target > self.gain { self.attack_s } else { self.release_s }.max(1e-4);
        let alpha = 1.0 - (-dt / tau).exp();
        let start = self.gain;
        self.gain += (target - self.gain) * alpha;
        // The gate ramps over its own time constant, not over the whole block: a block is 20 ms in
        // the live path, but a caller that hands over a second of audio must not get a one-second
        // fade (measured: -6 dB on a steady tone).
        let ramp = (tau * self.rate).max(1.0);
        out.clear();
        out.reserve(n);
        for (index, sample) in input.iter().enumerate() {
            let progress = (index as f64 / ramp).min(1.0);
            let gate = start + (self.gain - start) * progress;
            out.push((*sample as f64 * gate) as f32);
        }
        self.now_s += dt;
    }

    /// Diagnostics for the UI: level, gate and state in one call.
    pub fn status(&self) -> (f64, f64, bool) {
        (self.threshold_dbfs, self.gain, self.open)
    }

    /// The gate's threshold in dBFS (the panel's squelch control).
    pub fn set_threshold_dbfs(&mut self, dbfs: f64) {
        if dbfs.is_finite() {
            self.threshold_dbfs = dbfs.clamp(-160.0, 0.0);
        }
    }
}

impl AudioStage for Squelch {
    fn id(&self) -> &'static str {
        "squelch"
    }

    fn set_threshold(&mut self, dbfs: f64) -> bool {
        self.set_threshold_dbfs(dbfs);
        true
    }

    fn process_into(&mut self, input: &[f32], _hold: bool, out: &mut Vec<f32>) {
        self.process_block(input, out);
    }

    fn reset(&mut self) {
        self.gain = 0.0;
        self.open = false;
        self.hold_until_s = 0.0;
        self.now_s = 0.0;
    }
}

/// The default chain, in `plugin::AUDIO_PLUGINS` order. Each factory takes the consumer's rate.
pub fn stage_factories() -> Vec<(&'static str, fn(f64) -> Box<dyn AudioStage>)> {
    vec![
        ("dc_block", |_rate| Box::new(DcBlock::reference()) as Box<dyn AudioStage>),
        ("lpf", |rate| Box::new(Lpf::speech(rate)) as Box<dyn AudioStage>),
        ("agc", |_rate| Box::new(AgcStage::reference()) as Box<dyn AudioStage>),
        ("squelch", |rate| Box::new(Squelch::new(-110.0, rate)) as Box<dyn AudioStage>),
        ("wiener", |_rate| Box::new(crate::audio::wiener::Wiener::new()) as Box<dyn AudioStage>),
        ("notch", |rate| Box::new(crate::audio::notch::AdaptiveNotch::new(rate)) as Box<dyn AudioStage>),
        ("blanker", |_rate| Box::new(crate::audio::blanker::ImpulseBlanker::new()) as Box<dyn AudioStage>),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::PI;

    const AUDIO_RATE: f64 = DEFAULT_AUDIO_RATE;

    fn tone(n: usize, hz: f64, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|k| amp * (2.0 * PI * hz * k as f64 / AUDIO_RATE).sin() as f32)
            .collect()
    }

    fn rms(values: &[f32]) -> f64 {
        (values.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / values.len().max(1) as f64).sqrt()
    }

    #[test]
    fn the_low_pass_attenuates_what_is_above_its_corner() {
        let mut lpf = Lpf::new(3_000.0, AUDIO_RATE);
        let mut out = Vec::new();
        // 1 kHz passes; 12 kHz does not.
        lpf.process_into(&tone(4_800, 1_000.0, 0.5), false, &mut out);
        let pass = rms(&out[960..]);
        lpf.reset();
        lpf.process_into(&tone(4_800, 12_000.0, 0.5), false, &mut out);
        let stop = rms(&out[960..]);
        // A 0.5-amplitude sine has 0.354 RMS; the passband must keep essentially all of it.
        assert!(pass > 0.3, "the passband must survive: {pass}");
        assert!(stop < 0.03, "12 kHz must be attenuated: {stop}");
    }

    #[test]
    fn the_agc_stage_brings_a_quiet_block_to_the_target() {
        let mut stage = AgcStage::reference();
        let mut out = Vec::new();
        stage.process_into(&tone(960, 1_000.0, 0.01), false, &mut out);
        assert!((rms(&out) - 0.1).abs() < 0.03, "level {}", rms(&out));
        assert!(stage.gain() > 5.0, "the gain must have come up: {}", stage.gain());
    }

    #[test]
    fn the_squelch_mutes_noise_and_opens_on_signal() {
        let mut squelch = Squelch::new(-40.0, AUDIO_RATE);
        let mut out = Vec::new();
        // -60 dBFS noise: closed.
        squelch.process_block(&tone(960, 1_000.0, 0.001), &mut out);
        let muted = rms(&out);
        assert!(muted < 0.001, "the gate must stay closed: {muted}");
        // A strong tone: open, and the gate ramps rather than stepping.
        for _ in 0..6 {
            squelch.process_block(&tone(960, 1_000.0, 0.3), &mut out);
        }
        assert!(squelch.is_open(), "a strong signal must open the gate");
        assert!(rms(&out) > 0.2, "the signal must pass: {}", rms(&out));
    }

    #[test]
    fn the_squelch_hysteresis_holds_the_gate_through_a_fade() {
        let mut squelch = Squelch::new(-40.0, AUDIO_RATE);
        let mut out = Vec::new();
        for _ in 0..6 {
            squelch.process_block(&tone(960, 1_000.0, 0.3), &mut out);
        }
        assert!(squelch.is_open());
        // A short dip below the threshold must not close the gate (hold + hysteresis).
        squelch.process_block(&tone(200, 1_000.0, 0.001), &mut out);
        assert!(squelch.is_open(), "the hold window must absorb a short fade");
    }

    #[test]
    fn the_chain_factories_match_the_registry_order() {
        let ids: Vec<&str> = stage_factories().iter().map(|(id, _)| *id).collect();
        let registered: Vec<&str> = crate::plugin::AUDIO_PLUGINS
            .iter()
            .filter(|p| p.implemented)
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, registered, "the chain must follow the registry's order and set");
    }
}
