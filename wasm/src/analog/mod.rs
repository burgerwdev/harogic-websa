//! Analog demodulators: one framework, and every mode as a plugin on top of the shared DDC.
//!
//! The chain is the reference's (`web_sa/demod/demod.py::AnalogDemod`), in the same order:
//!
//! ```text
//!   complex baseband ─▶ complex band filter ─▶ detector ─▶ audio LPF ─▶ resample ─▶ de-emphasis ─▶ AGC ─▶ PCM
//!                                                          (@baseband rate)  (@out_rate)  (WFM only)
//! ```
//!
//! `fs` is the rate of the baseband it is handed (the backend's DDC output) and `out_rate` is what
//! the consumer plays at (the AudioWorklet's `sampleRate`). Both are explicit: a demodulator that
//! assumed one rate for both played 8.8% slow on a 44.1 kHz device.
//!
//! A mode is a *specification*, not a code path: which band it selects, which detector turns the
//! complex signal into audio, where its audio filter sits and whether it has de-emphasis. Adding
//! a mode is adding a spec (and, for a genuinely new detector, a match arm) — the mode list the UI
//! shows is the registry's, and a spec's absence is what `implemented: false` means.
//!
//! The band is complex on purpose: `usb`, `lsb` and `cw` select an *asymmetric* slice of the
//! spectrum, which a real low-pass cannot express. That is why the same filter serves the
//! symmetric modes (a band around DC) and the sideband modes (a band on one side of it).
//!
//! The framework is deliberately streaming: every filter keeps its tail and every detector keeps
//! its last sample, because a per-block reset is audible as a click at every block boundary.

use crate::ddc::fir::{design_complex_bandpass, design_lowpass, ComplexBandFilter, FirState};
use crate::ddc::resampler::LinearResampler;
use crate::ddc::RmsAgc;
use crate::plugin::{AnalogDemodulator, DemodConfig};

/// The rate every analog mode produced before the consumer's rate became configurable (the
/// AudioWorklet's rate is passed in as `out_rate` now; this stays the default and the tests'
/// reference).
pub const DEFAULT_AUDIO_RATE: f64 = 48_000.0;
const BAND_TAPS: usize = 257;
const AUDIO_TAPS: usize = 129;
/// DC-block pole from the reference's AM branch (~3.3 Hz corner at 48 kHz).
const DC_A: f64 = 0.9995;
const TAU: f64 = 2.0 * core::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detector {
    /// `|z|` then DC block: AM and DSB (a DSB signal needs a residual carrier to be detected this
    /// way; with a fully suppressed carrier there is no envelope to follow).
    Envelope,
    /// Phase-difference discriminator: NFM and WFM.
    Fm,
    /// The phase itself, DC blocked: PM.
    Phase,
    /// The real part of the sideband-selected signal: USB and LSB.
    Ssb,
    /// Filter at zero IF, then shift to the sidetone pitch: CW.
    Cw,
}

/// A frequency band `(lo, hi)` in Hz at the demodulator's input rate.
pub type BandSelector = fn(f64, f64) -> (f64, f64);

/// Everything that distinguishes one analog mode from another.
#[derive(Debug, Clone, Copy)]
pub struct ModeSpec {
    pub id: &'static str,
    pub detector: Detector,
    /// Audio low-pass corner as a function of (IF bandwidth, sidetone pitch).
    audio_cut: fn(f64, f64) -> f64,
    /// The band this mode selects, as a function of (IF bandwidth, sidetone pitch).
    band: BandSelector,
    /// De-emphasis time constant in microseconds (0 = none).
    pub deemph_us: f64,
}

impl ModeSpec {
    pub fn audio_cut_hz(&self, if_bw: f64, pitch: f64) -> f64 {
        (self.audio_cut)(if_bw, pitch)
    }

    pub fn band_hz(&self, if_bw: f64, pitch: f64) -> (f64, f64) {
        (self.band)(if_bw, pitch)
    }
}

fn am_cut(if_bw: f64, _pitch: f64) -> f64 {
    (if_bw / 2.0).min(15_000.0)
}

fn nfm_cut(if_bw: f64, _pitch: f64) -> f64 {
    (if_bw / 2.0).min(5_000.0)
}

fn wfm_cut(_if_bw: f64, _pitch: f64) -> f64 {
    15_000.0
}

fn ssb_cut(if_bw: f64, _pitch: f64) -> f64 {
    if_bw.min(20_000.0)
}

fn cw_cut(if_bw: f64, pitch: f64) -> f64 {
    (pitch + if_bw / 2.0 + 200.0).max(1_000.0).min(5_000.0)
}

/// A band centred on DC: AM, DSB, NFM, WFM, PM.
fn band_symmetric(if_bw: f64, _pitch: f64) -> (f64, f64) {
    (-if_bw / 2.0, if_bw / 2.0)
}

/// The upper sideband, with a guard against the carrier.
fn band_usb(if_bw: f64, _pitch: f64) -> (f64, f64) {
    ((if_bw * 0.1).clamp(100.0, 300.0), if_bw)
}

/// The lower sideband (negative frequencies), with the same guard.
fn band_lsb(if_bw: f64, _pitch: f64) -> (f64, f64) {
    (-if_bw, -(if_bw * 0.1).clamp(100.0, 300.0))
}

/// CW: a narrow band around zero IF, shifted to the sidetone by the detector.
fn band_cw(if_bw: f64, _pitch: f64) -> (f64, f64) {
    let half = if_bw.max(50.0) / 2.0;
    (-half, half)
}

/// The mode table. `None` means the kernel is not written yet, which is exactly what
/// `plugin::ANALOG_PLUGINS[*].implemented == false` must say; the registry test asserts the two
/// agree, so a mode cannot be advertised without being buildable.
pub fn spec_for(id: &str) -> Option<ModeSpec> {
    Some(match id {
        "am" => ModeSpec {
            id: "am", detector: Detector::Envelope, audio_cut: am_cut,
            band: band_symmetric, deemph_us: 0.0,
        },
        "dsb" => ModeSpec {
            id: "dsb", detector: Detector::Envelope, audio_cut: am_cut,
            band: band_symmetric, deemph_us: 0.0,
        },
        "usb" => ModeSpec {
            id: "usb", detector: Detector::Ssb, audio_cut: ssb_cut,
            band: band_usb, deemph_us: 0.0,
        },
        "lsb" => ModeSpec {
            id: "lsb", detector: Detector::Ssb, audio_cut: ssb_cut,
            band: band_lsb, deemph_us: 0.0,
        },
        "cw" => ModeSpec {
            id: "cw", detector: Detector::Cw, audio_cut: cw_cut,
            band: band_cw, deemph_us: 0.0,
        },
        "nfm" => ModeSpec {
            id: "nfm", detector: Detector::Fm, audio_cut: nfm_cut,
            band: band_symmetric, deemph_us: 0.0,
        },
        "wfm" => ModeSpec {
            id: "wfm", detector: Detector::Fm, audio_cut: wfm_cut,
            band: band_symmetric, deemph_us: 50.0,
        },
        "pm" => ModeSpec {
            id: "pm", detector: Detector::Phase, audio_cut: am_cut,
            band: band_symmetric, deemph_us: 0.0,
        },
        _ => return None,
    })
}

/// Build a demodulator for `id` (audio at `out_rate`), or `None` when its kernel does not exist.
pub fn build(id: &str, config: DemodConfig, out_rate: f64) -> Option<AnalogDemod> {
    spec_for(id).map(|spec| AnalogDemod::new(spec, config, out_rate))
}

/// The streaming demodulator: one instance per active mode.
pub struct AnalogDemod {
    spec: ModeSpec,
    fs: f64,
    /// The de-emphasis in force, in microseconds (0 = none). Starts at the mode's default and can be
    /// overridden by the listener (the panel's De-emph control).
    deemph_us: f64,
    /// Rate the PCM is produced at (the consumer's rate).
    out_rate: f64,
    pitch: f64,
    band: ComplexBandFilter,
    audio_lp: FirState,
    resampler: LinearResampler,
    deemph: Option<FirState>,
    agc: RmsAgc,
    // Detector state
    prev_z: Option<(f64, f64)>,
    env_prev: f64,
    dc_y: f64,
    cw_phase: f64,
    // Scratch buffers reused per block (no allocation in the hot path).
    z: Vec<f64>,
    det: Vec<f32>,
    lp: Vec<f64>,
    rs: Vec<f32>,
    levelled: Vec<f32>,
}

impl AnalogDemod {
    pub fn new(spec: ModeSpec, config: DemodConfig, out_rate: f64) -> Self {
        let fs = config.fs.max(1.0);
        let out_rate = if out_rate > 0.0 { out_rate } else { DEFAULT_AUDIO_RATE };
        let if_bw = config.if_bw.max(50.0).min(fs * 0.45);
        let pitch = if config.pitch > 0.0 { config.pitch } else { 700.0 };
        let (lo, hi) = spec.band_hz(if_bw, pitch);
        let (re, im) = design_complex_bandpass(fs, lo, hi, BAND_TAPS);
        // The audio low-pass runs at the baseband rate but must also stay below the output rate's
        // Nyquist: it is designed before the resampler, so both bounds apply.
        let audio_cut = spec
            .audio_cut_hz(if_bw, pitch)
            .max(100.0)
            .min(0.45 * out_rate)
            .min(0.45 * fs);
        Self {
            spec,
            fs,
            deemph_us: spec.deemph_us,
            out_rate,
            pitch,
            band: ComplexBandFilter::new(re, im),
            audio_lp: FirState::new(design_lowpass(fs, audio_cut, AUDIO_TAPS)),
            resampler: LinearResampler::new(fs, out_rate),
            deemph: Self::deemph_taps(spec.deemph_us, out_rate),
            agc: RmsAgc::reference(),
            prev_z: None,
            env_prev: 0.0,
            dc_y: 0.0,
            cw_phase: 0.0,
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

    pub fn pitch(&self) -> f64 {
        self.pitch
    }

    /// The rate this demodulator produces PCM at.
    pub fn out_rate(&self) -> f64 {
        self.out_rate
    }

    /// The de-emphasis time constant in microseconds in force (0 = none).
    pub fn deemph_us(&self) -> f64 {
        self.deemph_us
    }

    /// Change the de-emphasis at runtime.
    ///
    /// `tau_us < 0` restores the *mode's* default (the panel's Auto: 50 us for broadcast WFM, none
    /// for the modes where it is meaningless), `0` switches it off, and a positive value sets it.
    /// Keeping the "auto" resolution here means the mode table stays in one place.
    ///
    /// The FIR is rebuilt rather than reset, because the time constant *is* the filter: a WFM
    /// listener switching 50 -> 75 us is asking for a different filter, not for different state.
    pub fn set_deemph_us(&mut self, tau_us: f64) {
        if !tau_us.is_finite() {
            return;
        }
        let tau = if tau_us < 0.0 {
            self.spec.deemph_us
        } else {
            tau_us.max(0.0)
        };
        if (tau - self.deemph_us).abs() < 1e-9 {
            return;
        }
        self.deemph_us = tau;
        self.deemph = Self::deemph_taps(tau, self.out_rate);
    }

    /// Number of audio-filter taps (the design constant the tests pin).
    pub fn audio_taps(&self) -> usize {
        self.audio_lp.taps().len()
    }

    fn deemph_taps(tau_us: f64, out_rate: f64) -> Option<FirState> {
        if tau_us <= 0.0 {
            return None;
        }
        let alpha = (-1.0 / (out_rate * tau_us * 1e-6)).exp();
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
    ///
    /// The input stays in f64 (the reference computes the envelope and the phase in float64 and only
    /// narrows the *result* to float32). Narrowing the detector output first — as an earlier version
    /// did — destroys the small differences the block works on, which measured as ~1e-4 disagreement
    /// on exactly the modes that use it and left the others at ~1e-7.
    fn dc_block(&mut self, x: &[f64], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(x.len());
        let mut prev = if x.is_empty() { 0.0 } else { x[0] };
        for (index, sample) in x.iter().enumerate() {
            let delta = if index == 0 {
                (*sample - self.env_prev) * DC_A
            } else {
                (*sample - prev) * DC_A
            };
            self.dc_y = DC_A * self.dc_y + delta;
            prev = *sample;
            out.push(self.dc_y as f32);
        }
        if let Some(last) = x.last() {
            self.env_prev = *last;
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
                    env.push((i * i + q * q).sqrt());
                }
                let mut blocked = Vec::new();
                self.dc_block(&env, &mut blocked);
                out.extend_from_slice(&blocked);
            }
            Detector::Phase => {
                let mut phase = Vec::with_capacity(n);
                for k in 0..n {
                    phase.push(z[2 * k + 1].atan2(z[2 * k]));
                }
                let mut blocked = Vec::new();
                self.dc_block(&phase, &mut blocked);
                out.extend_from_slice(&blocked);
            }
            Detector::Fm => {
                let scale = self.fs / TAU;
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
            Detector::Ssb => {
                // A sideband signal is already audio once the band filter has removed the other
                // sideband: the real part *is* the message (no envelope, no discriminator).
                for k in 0..n {
                    out.push(z[2 * k] as f32);
                }
            }
            Detector::Cw => {
                // A narrow band around zero IF, shifted up to the sidetone pitch. The phase is kept
                // across blocks so the sidetone does not restart at every block boundary.
                let inc = TAU * self.pitch / self.fs;
                for k in 0..n {
                    let ph = self.cw_phase + inc * k as f64;
                    let (i, q) = (z[2 * k], z[2 * k + 1]);
                    out.push((i * ph.cos() - q * ph.sin()) as f32);
                }
                self.cw_phase = (self.cw_phase + inc * n as f64) % TAU;
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

    fn deemph_us(&self) -> f64 {
        self.deemph_us
    }

    fn retune(&mut self) {
        // Keep the AGC gain: resetting it made every tune start with a loud burst.
        self.band.clear_tail();
        self.prev_z = None;
        self.env_prev = 0.0;
        self.dc_y = 0.0;
        self.cw_phase = 0.0;
        if let Some(deemph) = self.deemph.as_mut() {
            deemph.reset();
        }
    }

    fn reset(&mut self) {
        self.band.reset();
        self.audio_lp.reset();
        self.resampler.reset();
        // The de-emphasis setting is the listener's, not the stream's: reset() keeps it.
        if let Some(deemph) = self.deemph.as_mut() {
            deemph.reset();
        }
        self.agc.reset();
        self.prev_z = None;
        self.env_prev = 0.0;
        self.dc_y = 0.0;
        self.cw_phase = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 100_000.0;
    const IF_BW: f64 = 12_000.0;
    const TONE_HZ: f64 = 1_000.0;
    const PITCH: f64 = 700.0;
    const AUDIO_RATE: f64 = DEFAULT_AUDIO_RATE;

    /// Hann-windowed amplitude of `audio` at one frequency (coherent gain corrected).
    pub(crate) fn amplitude_at(audio: &[f32], rate: f64, hz: f64) -> f64 {
        let n = audio.len() as f64;
        let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
        for (k, value) in audio.iter().enumerate() {
            let w = 0.5 - 0.5 * (TAU * k as f64 / n).cos();
            let ph = TAU * hz * k as f64 / rate;
            re += *value as f64 * w * ph.cos();
            im -= *value as f64 * w * ph.sin();
            wsum += w;
        }
        (re * re + im * im).sqrt() / wsum
    }

    /// Dominant frequency of the recovered audio, searched around `expect`.
    pub(crate) fn recovered_hz(audio: &[f32], expect: f64) -> f64 {
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
    fn run_with(id: &str, n: usize, if_bw: f64, pitch: f64, modulate: impl Fn(f64) -> (f64, f64)) -> Vec<f32> {
        run_at(id, n, if_bw, pitch, AUDIO_RATE, modulate)
    }

    /// Demodulate `n` samples of a baseband at `FS` and produce audio at `out_rate`.
    fn run_at(id: &str, n: usize, if_bw: f64, pitch: f64, out_rate: f64,
              modulate: impl Fn(f64) -> (f64, f64)) -> Vec<f32> {
        let mut config = DemodConfig::new(FS, if_bw);
        config.pitch = pitch;
        let mut demod = build(id, config, out_rate).expect("mode must exist");
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

    fn run(id: &str, n: usize, modulate: impl Fn(f64) -> (f64, f64)) -> Vec<f32> {
        run_with(id, n, IF_BW, PITCH, modulate)
    }

    #[test]
    fn am_recovers_the_modulation_tone() {
        let audio = run("am", 20_000, |t| (1.0 + 0.5 * (TAU * TONE_HZ * t).cos(), 0.0));
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "AM audio tone");
        let level = rms(&audio);
        assert!((0.05..0.2).contains(&level), "AM audio level {level} (AGC target 0.1)");
    }

    #[test]
    fn dsb_recovers_the_modulation_tone() {
        // DSB needs a residual carrier for envelope detection; 0.8 carrier + 0.5 modulation keeps
        // the envelope positive, which is what a real transmitter's carrier insertion does.
        let audio = run("dsb", 20_000, |t| (0.8 + 0.5 * (TAU * TONE_HZ * t).cos(), 0.0));
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "DSB audio tone");
        assert!(rms(&audio) > 0.05, "DSB must produce audio");
    }

    #[test]
    fn nfm_recovers_the_modulation_tone() {
        let audio = run("nfm", 20_000, |t| {
            let ph = 1.0 * (TAU * TONE_HZ * t).sin();
            (ph.cos(), ph.sin())
        });
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "NFM audio tone");
        assert!(rms(&audio) > 0.05, "NFM must produce audio");
    }

    #[test]
    fn wfm_recovers_the_modulation_tone_through_de_emphasis() {
        let audio = run("wfm", 20_000, |t| {
            let ph = 3.0 * (TAU * TONE_HZ * t).sin();
            (ph.cos(), ph.sin())
        });
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "WFM audio tone");
        // The 50 us de-emphasis is nearly flat at 1 kHz: the tone must not be gutted.
        assert!(amplitude_at(&audio, AUDIO_RATE, TONE_HZ) > 0.05, "de-emphasis killed the tone");
    }

    #[test]
    fn pm_recovers_the_modulation_tone() {
        let audio = run("pm", 20_000, |t| {
            let ph = 0.6 * (TAU * TONE_HZ * t).cos();
            (ph.cos(), ph.sin())
        });
        assert_eq!(recovered_hz(&audio, TONE_HZ), TONE_HZ, "PM audio tone");
        assert!(rms(&audio) > 0.05, "PM must produce audio");
    }

    /// Two mirrored tones, used to show which sideband a mode accepts.
    fn single_upper(t: f64) -> (f64, f64) {
        (0.5 * (TAU * 1_200.0 * t).cos(), 0.5 * (TAU * 1_200.0 * t).sin())
    }

    fn single_lower(t: f64) -> (f64, f64) {
        (0.5 * (TAU * -1_200.0 * t).cos(), 0.5 * (TAU * -1_200.0 * t).sin())
    }

    /// Steady-state gain of a mode's band filter at `hz` for a unit complex tone.
    ///
    /// Sideband rejection is measured here, not on the demodulated audio: the audio is real, so a
    /// DFT at +f and -f is identical by construction, and the AGC would boost whatever leaks
    /// through back to its target level anyway.
    fn band_gain(id: &str, if_bw: f64, pitch: f64, hz: f64) -> f64 {
        let spec = spec_for(id).expect("mode spec");
        let (lo, hi) = spec.band_hz(if_bw, pitch);
        let (re, im) = design_complex_bandpass(FS, lo, hi, BAND_TAPS);
        let mut filter = ComplexBandFilter::new(re, im);
        let n = 8_192;
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            let ph = TAU * hz * k as f64 / FS;
            iq.push(ph.cos());
            iq.push(ph.sin());
        }
        let mut out = Vec::new();
        filter.process_complex_into(&iq, &mut out);
        let skip = BAND_TAPS;   // past the filter transient
        let mut sum = 0.0;
        let mut count = 0.0;
        for k in skip..(out.len() / 2) {
            sum += (out[2 * k].powi(2) + out[2 * k + 1].powi(2)).sqrt();
            count += 1.0;
        }
        sum / count
    }

    #[test]
    fn usb_selects_the_upper_sideband_only() {
        let wanted = band_gain("usb", 2_400.0, PITCH, 1_200.0);
        let image = band_gain("usb", 2_400.0, PITCH, -1_200.0);
        assert!(wanted > 0.5, "USB must pass the upper sideband: gain {wanted}");
        assert!(wanted / image > 100.0, "USB must reject the lower sideband: {wanted} vs {image}");
        // The accepted sideband becomes audio at its own frequency.
        let audio = run_with("usb", 20_000, 2_400.0, PITCH, single_upper);
        assert_eq!(recovered_hz(&audio, 1_200.0), 1_200.0, "USB audio tone");
    }

    #[test]
    fn lsb_selects_the_lower_sideband_only() {
        let wanted = band_gain("lsb", 2_400.0, PITCH, -1_200.0);
        let image = band_gain("lsb", 2_400.0, PITCH, 1_200.0);
        assert!(wanted > 0.5, "LSB must pass the lower sideband: gain {wanted}");
        assert!(wanted / image > 100.0, "LSB must reject the upper sideband: {wanted} vs {image}");
        // A lower-sideband tone demodulates to the same positive audio frequency (it is the
        // mirror image that ends up at +1200 Hz).
        let audio = run_with("lsb", 20_000, 2_400.0, PITCH, single_lower);
        assert_eq!(recovered_hz(&audio, 1_200.0), 1_200.0, "LSB audio tone");
    }

    #[test]
    fn cw_puts_the_carrier_on_the_sidetone_pitch() {
        // A carrier exactly at zero IF (the CW signal the operator tuned onto) plus an off-channel
        // interferer 3 kHz away that the narrow band must reject.
        let audio = run_with("cw", 20_000, 500.0, PITCH, |t| {
            (
                1.0 + 0.5 * (TAU * 3_000.0 * t).cos(),
                0.5 * (TAU * 3_000.0 * t).sin(),
            )
        });
        assert_eq!(recovered_hz(&audio, PITCH), PITCH, "CW sidetone");
        let sidetone = amplitude_at(&audio, AUDIO_RATE, PITCH);
        assert!(sidetone > 0.02, "the sidetone must be present, level {sidetone}");
        // The 3 kHz interferer is outside the band: it cannot add energy at its own offset.
        let interferer = amplitude_at(&audio, AUDIO_RATE, 3_000.0);
        assert!(interferer < sidetone, "CW band filter must reject the interferer");
    }

    /// The consumer's rate is what the PCM comes out at (44.1 kHz devices played 8.8% slow when
    /// the output rate was a constant).
    #[test]
    fn the_output_rate_follows_the_configured_rate() {
        let modulate = |t: f64| (1.0 + 0.5 * (TAU * TONE_HZ * t).cos(), 0.0);
        for out_rate in [48_000.0, 44_100.0, 96_000.0] {
            let audio = run_at("am", 20_000, IF_BW, PITCH, out_rate, modulate);
            let expected = 20_000.0 * out_rate / FS;
            assert!(audio.len().abs_diff(expected as usize) <= 2,
                    "out_rate {out_rate}: produced {} samples, expected {expected:.0}",
                    audio.len());
            // The modulation tone is a frequency, not a sample count: it must land where it was
            // sent whatever the output rate is.
            let mut hz = TONE_HZ - 60.0;
            let mut best = (TONE_HZ, f64::MIN);
            while hz <= TONE_HZ + 60.0 {
                let level = amplitude_at(&audio, out_rate, hz);
                if level > best.1 { best = (hz, level); }
                hz += 0.5;
            }
            assert_eq!(best.0, TONE_HZ, "tone at {out_rate} Hz output");
        }
    }

    /// The panel's De-emph control: Auto (the mode's default), Off, or an explicit constant.
    #[test]
    fn de_emphasis_is_a_setting_the_listener_owns() {
        let tone = 3_000.0;
        let modulate = |t: f64| {
            let ph = 3.0 * (TAU * tone * t).sin();
            (ph.cos(), ph.sin())
        };
        let mut demod = build("wfm", DemodConfig::new(FS, IF_BW), AUDIO_RATE).unwrap();
        assert_eq!(demod.deemph_us(), 50.0, "WFM's default is 50 us");

        demod.set_deemph_us(0.0);
        let mut audio = Vec::new();
        let mut iq: Vec<f32> = Vec::new();
        for k in 0..20_000 {
            let (i, q) = modulate(k as f64 / FS);
            iq.push(i as f32);
            iq.push(q as f32);
        }
        demod.process_into(&iq, &mut audio);
        let flat = amplitude_at(&audio, AUDIO_RATE, tone);

        demod.set_deemph_us(300.0);
        let mut shaped = Vec::new();
        demod.process_into(&iq, &mut shaped);
        let rolled = amplitude_at(&shaped, AUDIO_RATE, tone);
        assert!(
            rolled < flat * 0.9,
            "300 us must roll the tone off (flat {flat:.4}, shaped {rolled:.4})"
        );
        // ...and the choice survives a stream reset: it is a setting, not stream state.
        demod.reset();
        assert_eq!(demod.deemph_us(), 300.0);
        // Auto (`< 0`) restores the mode's own default, which is where a fresh demodulator starts.
        demod.set_deemph_us(0.0);
        demod.set_deemph_us(-1.0);
        assert_eq!(demod.deemph_us(), 50.0, "Auto means the mode's default");
        demod.set_deemph_us(50.0);
        let mut again = Vec::new();
        demod.process_into(&iq, &mut again);
        assert!(amplitude_at(&again, AUDIO_RATE, tone) > rolled);
    }

    #[test]
    fn the_output_rate_is_the_audio_rate_and_blocks_are_continuous() {
        let mut demod = build("am", DemodConfig::new(FS, IF_BW), AUDIO_RATE).unwrap();
        let modulate = |t: f64| (1.0 + 0.5 * (TAU * TONE_HZ * t).cos(), 0.0);
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
    fn every_mode_builds_and_reports_its_registry_id() {
        for descriptor in crate::plugin::ANALOG_PLUGINS {
            let built = build(descriptor.id, DemodConfig::new(FS, IF_BW), AUDIO_RATE);
            assert_eq!(
                built.is_some(), descriptor.implemented,
                "{}: registry says implemented={} but build() says {}",
                descriptor.id, descriptor.implemented, built.is_some()
            );
            if let Some(demod) = built {
                assert_eq!(demod.id(), descriptor.id);
                assert_eq!(demod.audio_taps(), AUDIO_TAPS);
            }
        }
    }
}
