//! Analog demodulators: one framework, and every mode as a plugin on top of the shared DDC.
//!
//! The chain is the reference's (`web_sa/demod/demod.py::AnalogDemod`), in the same order:
//!
//! ```text
//!   complex baseband ─▶ complex band filter ─▶ detector ─▶ audio LPF ─▶ resample ─▶ de-emphasis ─▶ AGC ─▶ PCM
//!                                                          (@channel rate)   (48 kHz)   (WFM only)
//! ```
//!
//! A mode is a *specification*, not a code path: which band it selects, which detector turns the
//! complex signal into audio, where its audio filter sits and whether it has de-emphasis. Adding
//! a mode is adding a spec (and, for a genuinely new detector, a match arm) — the mode list the UI
//! shows is the registry's, and a spec's absence is what `implemented: false` means.
//!
//! The framework is deliberately streaming: every filter keeps its tail and every detector keeps
//! its last sample, because a per-block reset is audible as a click at every block boundary.

use crate::ddc::fir::{design_complex_bandpass, design_lowpass, ComplexBandFilter, FirState};
use crate::ddc::resampler::LinearResampler;
use crate::ddc::RmsAgc;
use crate::plugin::{AnalogDemodulator, DemodConfig};

/// Output rate of every analog mode, in Hz (the audio path's rate).
pub const AUDIO_RATE: f64 = 48_000.0;
const BAND_TAPS: usize = 257;
const AUDIO_TAPS: usize = 129;
/// DC-block pole from the reference's AM branch (~3.3 Hz corner at 48 kHz).
const DC_A: f64 = 0.9995;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detector {
    /// `|z|` then DC block: AM and DSB (a DSB signal needs a residual carrier to be detected this
    /// way; with a fully suppressed carrier there is no envelope to follow).
    Envelope,
    /// Phase-difference discriminator: NFM and WFM.
    Fm,
    /// The phase itself, DC blocked: PM.
    Phase,
}

/// Everything that distinguishes one analog mode from another.
#[derive(Debug, Clone, Copy)]
pub struct ModeSpec {
    pub id: &'static str,
    pub detector: Detector,
    /// Audio low-pass corner as a function of the mode's IF bandwidth.
    audio_cut: fn(f64) -> f64,
    /// De-emphasis time constant in microseconds (0 = none).
    pub deemph_us: f64,
}

impl ModeSpec {
    pub fn audio_cut_hz(&self, if_bw: f64) -> f64 {
        (self.audio_cut)(if_bw)
    }
}

fn am_cut(if_bw: f64) -> f64 {
    (if_bw / 2.0).min(15_000.0)
}

fn nfm_cut(if_bw: f64) -> f64 {
    (if_bw / 2.0).min(5_000.0)
}

fn wfm_cut(_if_bw: f64) -> f64 {
    15_000.0
}

/// The mode table. `None` means the kernel is not written yet, which is exactly what
/// `plugin::ANALOG_PLUGINS[*].implemented == false` must say; the registry test asserts the two
/// agree, so a mode cannot be advertised without being buildable.
pub fn spec_for(id: &str) -> Option<ModeSpec> {
    Some(match id {
        "am" => ModeSpec { id: "am", detector: Detector::Envelope, audio_cut: am_cut, deemph_us: 0.0 },
        "dsb" => ModeSpec { id: "dsb", detector: Detector::Envelope, audio_cut: am_cut, deemph_us: 0.0 },
        "nfm" => ModeSpec { id: "nfm", detector: Detector::Fm, audio_cut: nfm_cut, deemph_us: 0.0 },
        "wfm" => ModeSpec { id: "wfm", detector: Detector::Fm, audio_cut: wfm_cut, deemph_us: 50.0 },
        "pm" => ModeSpec { id: "pm", detector: Detector::Phase, audio_cut: am_cut, deemph_us: 0.0 },
        _ => return None,
    })
}

/// Build a demodulator for `id`, or `None` when its kernel does not exist.
pub fn build(id: &str, config: DemodConfig) -> Option<AnalogDemod> {
    spec_for(id).map(|spec| AnalogDemod::new(spec, config))
}

/// The streaming demodulator: one instance per active mode.
pub struct AnalogDemod {
    spec: ModeSpec,
    fs: f64,
    band: ComplexBandFilter,
    audio_lp: FirState,
    resampler: LinearResampler,
    deemph: Option<FirState>,
    agc: RmsAgc,
    // Detector state
    prev_z: Option<(f64, f64)>,
    env_prev: f64,
    dc_y: f64,
    // Scratch buffers reused per block (no allocation in the hot path).
    z: Vec<f64>,
    det: Vec<f32>,
    lp: Vec<f64>,
    rs: Vec<f32>,
    levelled: Vec<f32>,
}

impl AnalogDemod {
    pub fn new(spec: ModeSpec, config: DemodConfig) -> Self {
        let fs = config.fs.max(1.0);
        let if_bw = config.if_bw.max(50.0).min(fs * 0.45);
        let (lo, hi) = (-if_bw / 2.0, if_bw / 2.0);
        let (re, im) = design_complex_bandpass(fs, lo, hi, BAND_TAPS);
        let audio_cut = spec.audio_cut_hz(if_bw).max(100.0).min(0.45 * AUDIO_RATE).min(0.45 * fs);
        Self {
            spec,
            fs,
            band: ComplexBandFilter::new(re, im),
            audio_lp: FirState::new(design_lowpass(fs, audio_cut, AUDIO_TAPS)),
            resampler: LinearResampler::new(fs, AUDIO_RATE),
            deemph: Self::deemph_taps(spec.deemph_us),
            agc: RmsAgc::reference(),
            prev_z: None,
            env_prev: 0.0,
            dc_y: 0.0,
            z: Vec::new(),
            det: Vec::new(),
            lp: Vec::new(),
            rs: Vec::new(),
            levelled: Vec::new(),
        }
    }

    pub fn spec(&self) -> ModeSpec {
        self.spec
    }

    pub fn audio_cut_hz(&self) -> f64 {
        // The filter length is the observable; the design constant is what the tests pin.
        self.audio_lp.taps().len() as f64
    }

    fn deemph_taps(tau_us: f64) -> Option<FirState> {
        if tau_us <= 0.0 {
            return None;
        }
        let alpha = (-1.0 / (AUDIO_RATE * tau_us * 1e-6)).exp();
        if !(0.0 < alpha && alpha < 1.0) {
            return None;
        }
        let span = ((-1e-4_f64.ln()) / -alpha.ln()).ceil() as usize;
        let span = span.clamp(1, 2048);
        let mut taps: Vec<f64> = (0..=span).map(|k| (1.0 - alpha) * alpha.powi(k as i32)).collect();
        let sum: f64 = taps.iter().sum();
        for tap in taps.iter_mut() {
            *tap /= sum;
        }
        Some(FirState::new(taps.into_iter().map(|t| t as f32 as f64).collect()))
    }

    /// One-pole DC block, `y[n] = a*y[n-1] + a*(x[n] - x[n-1])`, block-crossing.
    fn dc_block(&mut self, x: &[f32], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(x.len());
        let mut prev = if x.is_empty() { 0.0 } else { x[0] as f64 };
        for (index, sample) in x.iter().enumerate() {
            let delta = if index == 0 {
                (*sample as f64 - self.env_prev) * DC_A
            } else {
                (*sample as f64 - prev) * DC_A
            };
            self.dc_y = DC_A * self.dc_y + delta;
            prev = *sample as f64;
            out.push(self.dc_y as f32);
        }
        if let Some(last) = x.last() {
            self.env_prev = *last as f64;
        }
    }

    fn detect(&mut self, z: &[f64], out: &mut Vec<f32>) {
        let n = z.len() / 2;
        out.clear();
        out.reserve(n);
        match self.spec.detector {
            Detector::Envelope => {
                let mut env = Vec::with_capacity(n);
                for k in 0..n {
                    let (i, q) = (z[2 * k], z[2 * k + 1]);
                    env.push((i * i + q * q).sqrt() as f32);
                }
                let mut blocked = Vec::new();
                self.dc_block(&env, &mut blocked);
                out.extend_from_slice(&blocked);
            }
            Detector::Phase => {
                let mut phase = Vec::with_capacity(n);
                for k in 0..n {
                    phase.push(z[2 * k + 1].atan2(z[2 * k]) as f32);
                }
                let mut blocked = Vec::new();
                self.dc_block(&phase, &mut blocked);
                out.extend_from_slice(&blocked);
            }
            Detector::Fm => {
                let scale = self.fs / (2.0 * core::f64::consts::PI);
                for k in 0..n {
                    let (i, q) = (z[2 * k], z[2 * k + 1]);
                    if let Some((pi, pq)) = self.prev_z {
                        // angle(z * conj(prev)): the phase advance between two samples.
                        let re = i * pi + q * pq;
                        let im = q * pi - i * pq;
                        out.push((im.atan2(re) * scale) as f32);
                    }
                    self.prev_z = Some((i, q));
                }
            }
        }
    }
}

impl AnalogDemodulator for AnalogDemod {
    fn id(&self) -> &'static str {
        self.spec.id
    }

    fn process_into(&mut self, iq: &[f32], out: &mut Vec<f32>) {
        out.clear();
        let n = iq.len() & !1;
        if n == 0 {
            return;
        }
        let mut z: Vec<f64> = Vec::with_capacity(n);
        z.extend(iq[..n].iter().map(|v| *v as f64));
        let mut filtered = core::mem::take(&mut self.z);
        self.band.process_complex_into(&z, &mut filtered);

        let mut det = core::mem::take(&mut self.det);
        self.detect(&filtered, &mut det);

        // The audio LPF and the resampler run in f64 (the reference does), the AGC takes the f32
        // PCM it produces.
        let det_f64: Vec<f64> = det.iter().map(|v| *v as f64).collect();
        let mut lp = core::mem::take(&mut self.lp);
        self.audio_lp.process_into(&det_f64, &mut lp);

        let mut rs = core::mem::take(&mut self.rs);
        self.resampler.process_into(&lp, &mut rs);
        if let Some(deemph) = self.deemph.as_mut() {
            let input: Vec<f64> = rs.iter().map(|v| *v as f64).collect();
            let mut shaped = Vec::new();
            deemph.process_into(&input, &mut shaped);
            rs.clear();
            rs.extend(shaped.iter().map(|v| *v as f32));
        }

        let mut levelled = core::mem::take(&mut self.levelled);
        self.agc.process_into(&rs, false, &mut levelled);
        out.extend_from_slice(&levelled);

        self.z = filtered;
        self.det = det;
        self.lp = lp;
        self.rs = rs;
        self.levelled = levelled;
    }

    fn retune(&mut self) {
        // Keep the AGC gain: resetting it made every tune start with a loud burst.
        self.band.clear_tail();
        self.prev_z = None;
        self.env_prev = 0.0;
        self.dc_y = 0.0;
        if let Some(deemph) = self.deemph.as_mut() {
            deemph.reset();
        }
    }

    fn reset(&mut self) {
        self.band.reset();
        self.audio_lp.reset();
        self.resampler.reset();
        if let Some(deemph) = self.deemph.as_mut() {
            deemph.reset();
        }
        self.agc.reset();
        self.prev_z = None;
        self.env_prev = 0.0;
        self.dc_y = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 100_000.0;
    const IF_BW: f64 = 12_000.0;
    const TONE_HZ: f64 = 1_000.0;

    /// Hann-windowed amplitude of `audio` at one frequency (coherent gain corrected).
    fn amplitude_at(audio: &[f32], rate: f64, hz: f64) -> f64 {
        let n = audio.len() as f64;
        let mut re = 0.0;
        let mut im = 0.0;
        let mut wsum = 0.0;
        for (k, value) in audio.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * core::f64::consts::PI * k as f64 / n).cos();
            let ph = 2.0 * core::f64::consts::PI * hz * k as f64 / rate;
            re += *value as f64 * w * ph.cos();
            im -= *value as f64 * w * ph.sin();
            wsum += w;
        }
        (re * re + im * im).sqrt() / wsum
    }

    /// Dominant frequency of the recovered audio, searched around `expect`.
    fn recovered_hz(audio: &[f32], expect: f64) -> f64 {
        let mut best = (expect, f64::MIN);
        let mut hz = expect - 60.0;
        while hz <= expect + 60.0 {
            let level = amplitude_at(audio, AUDIO_RATE, hz);
            if level > best.1 {
                best = (hz, level);
            }
            hz += 0.5;
        }
        best.0
    }

    fn rms(audio: &[f32]) -> f64 {
        (audio.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / audio.len() as f64).sqrt()
    }

    /// Demodulate one block of `n` samples built by `modulate(time_seconds)`.
    fn run(id: &str, n: usize, modulate: impl Fn(f64) -> (f64, f64)) -> Vec<f32> {
        let demod = build(id, DemodConfig::new(FS, IF_BW)).expect("mode must exist");
        let mut demod = demod;
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            let (i, q) = modulate(k as f64 / FS);
            iq.push(i as f32);
            iq.push(q as f32);
        }
        let mut audio = Vec::new();
        demod.process_into(&iq, &mut audio);
        audio
    }

    fn tau() -> f64 {
        2.0 * core::f64::consts::PI
    }

    #[test]
    fn am_recovers_the_modulation_tone() {
        let audio = run("am", 20_000, |t| (1.0 + 0.5 * (tau() * TONE_HZ * t).cos(), 0.0));
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "AM audio tone");
        let level = rms(&audio);
        assert!((0.1..0.35).contains(&level), "AM audio level {level} (AGC target 0.2)");
    }

    #[test]
    fn dsb_recovers_the_modulation_tone() {
        // DSB needs a residual carrier for envelope detection; 0.8 carrier + 0.5 modulation keeps
        // the envelope positive, which is what a real transmitter's carrier insertion does.
        let audio = run("dsb", 20_000, |t| (0.8 + 0.5 * (tau() * TONE_HZ * t).cos(), 0.0));
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "DSB audio tone");
        assert!(rms(&audio) > 0.05, "DSB must produce audio");
    }

    #[test]
    fn nfm_recovers_the_modulation_tone() {
        let audio = run("nfm", 20_000, |t| {
            let ph = 1.0 * (tau() * TONE_HZ * t).sin();
            (ph.cos(), ph.sin())
        });
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "NFM audio tone");
        assert!(rms(&audio) > 0.05, "NFM must produce audio");
    }

    #[test]
    fn wfm_recovers_the_modulation_tone_through_de_emphasis() {
        let audio = run("wfm", 20_000, |t| {
            let ph = 3.0 * (tau() * TONE_HZ * t).sin();
            (ph.cos(), ph.sin())
        });
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "WFM audio tone");
        // The 50 us de-emphasis is nearly flat at 1 kHz: the tone must not be gutted.
        assert!(amplitude_at(&audio, AUDIO_RATE, TONE_HZ) > 0.05, "de-emphasis killed the tone");
    }

    #[test]
    fn pm_recovers_the_modulation_tone() {
        let audio = run("pm", 20_000, |t| {
            let ph = 0.6 * (tau() * TONE_HZ * t).cos();
            (ph.cos(), ph.sin())
        });
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "PM audio tone");
        assert!(rms(&audio) > 0.05, "PM must produce audio");
    }

    #[test]
    fn the_output_rate_is_the_audio_rate_and_blocks_are_continuous() {
        let demod = build("am", DemodConfig::new(FS, IF_BW)).unwrap();
        let mut demod = demod;
        let modulate = |t: f64| (1.0 + 0.5 * (tau() * TONE_HZ * t).cos(), 0.0);
        let mut iq = Vec::new();
        for k in 0..10_000 {
            let (i, q) = modulate(k as f64 / FS);
            iq.push(i as f32);
            iq.push(q as f32);
        }
        let mut first = Vec::new();
        let mut second = Vec::new();
        demod.process_into(&iq, &mut first);
        demod.process_into(&iq, &mut second);
        let expected = (10_000.0 * AUDIO_RATE / FS) as usize;
        assert!(first.len().abs_diff(expected) <= 2, "len {} vs {expected}", first.len());
        assert!(second.len().abs_diff(expected) <= 2, "the second block must keep the rate");
        // A block boundary must not reset the chain: the level stays comparable.
        assert!((rms(&first) - rms(&second)).abs() < 0.15, "level jumped at the block boundary");
    }

    #[test]
    fn unimplemented_modes_are_not_buildable() {
        for descriptor in crate::plugin::ANALOG_PLUGINS {
            let built = build(descriptor.id, DemodConfig::new(FS, IF_BW)).is_some();
            assert_eq!(
                built, descriptor.implemented,
                "{}: registry says implemented={} but build() says {built}",
                descriptor.id, descriptor.implemented
            );
        }
    }

    #[test]
    fn every_implemented_mode_reports_its_registry_id() {
        for descriptor in crate::plugin::ANALOG_PLUGINS.iter().filter(|p| p.implemented) {
            let demod = build(descriptor.id, DemodConfig::new(FS, IF_BW)).expect("implemented");
            assert_eq!(demod.id(), descriptor.id);
        }
    }
}
