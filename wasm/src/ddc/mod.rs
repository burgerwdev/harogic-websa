//! The DDC base layer: NCO -> anti-alias FIR -> integer decimation -> resampling -> level.
//!
//! Every analog and digital mode receives the output of this module, which is why it holds no
//! mode-specific knowledge: it only moves a channel to DC, limits its bandwidth and sets its
//! rate. The demodulators (analog, digital) and their filters sit above it.
//!
//! Numerics are the Python reference's (`web_sa/demod/filters.py` + `sdr.py::_mix`); the
//! committed fixtures under `tests/fixtures/dsp/` pin them down stage by stage.
//!
//! The DDC itself runs on the backend now (the analyzer's DSP_DDC plus a software NCO), so the
//! browser only sees its output. What stays here are the kernels the demodulators need (FIR,
//! resampler, AGC) plus [`Ddc`], the orchestrator they were verified as a chain against — kept
//! because the parity fixtures compare exactly that chain, and because a future wideband path would
//! need it again.

pub mod agc;
pub mod fir;
pub mod nco;
pub mod resampler;

pub use agc::RmsAgc;
pub use fir::{design_complex_bandpass, design_lowpass, FirState, IqFilter};
pub use nco::Nco;
pub use resampler::{ComplexResampler, LinearResampler};

/// Sample scale of the int16 IQ the analyzer delivers (`IQS_ScaleToV`-normalised stream).
const IQ_FULL_SCALE: f64 = 32768.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DdcConfig {
    /// Input rate of the IQ block (Hz).
    pub fs_in: f64,
    /// NCO frequency: a component here is mixed down to DC.
    pub offset_hz: f64,
    /// Anti-alias cutoff at `fs_in` (Hz), normally just below `fs_in / decimate / 2`.
    pub cutoff_hz: f64,
    /// Integer decimation applied after the filter.
    pub decimate: usize,
    /// Output rate after the resampler. `0` (or equal to the decimated rate) skips it.
    pub out_rate: f64,
    /// FIR length (forced odd). 0 disables the filter.
    pub ntaps: usize,
}

impl DdcConfig {
    /// A sane default for a channel of `out_rate` inside an `fs_in` stream.
    pub fn for_channel(fs_in: f64, offset_hz: f64, out_rate: f64, ntaps: usize) -> Self {
        let decimate = ((fs_in / out_rate).floor().max(1.0)) as usize;
        let cutoff = (fs_in / decimate as f64) * 0.4;
        Self { fs_in, offset_hz, cutoff_hz: cutoff, decimate, out_rate, ntaps }
    }

    /// Rate after the integer decimation (the resampler's input rate).
    pub fn decimated_rate(&self) -> f64 {
        self.fs_in / self.decimate.max(1) as f64
    }

    pub fn resampling(&self) -> bool {
        self.out_rate > 0.0 && (self.out_rate - self.decimated_rate()).abs() > 1e-9
    }
}

/// The shared channelizer. Holds all the state that must survive a block boundary.
pub struct Ddc {
    config: DdcConfig,
    nco: Nco,
    filter: IqFilter,
    resampler: Option<ComplexResampler>,
    agc: RmsAgc,
    mixed: Vec<f64>,
    filtered: Vec<f64>,
    decimated: Vec<f64>,
    resampled: Vec<f32>,
    levelled: Vec<f32>,
}

impl Ddc {
    pub fn new(config: DdcConfig) -> Self {
        let taps = Self::taps_for(&config);
        let filter = IqFilter::new(taps);
        let resampler = if config.resampling() {
            Some(ComplexResampler::new(config.decimated_rate(), config.out_rate))
        } else {
            None
        };
        let mut nco = Nco::new();
        nco.set_frequency(config.offset_hz, config.fs_in);
        Self {
            config,
            nco,
            filter,
            resampler,
            agc: RmsAgc::reference(),
            mixed: Vec::new(),
            filtered: Vec::new(),
            decimated: Vec::new(),
            resampled: Vec::new(),
            levelled: Vec::new(),
        }
    }

    fn taps_for(config: &DdcConfig) -> Vec<f64> {
        if config.ntaps == 0 {
            vec![1.0]
        } else if config.ntaps == 1 {
            vec![1.0]
        } else {
            design_lowpass(config.fs_in, config.cutoff_hz, config.ntaps)
        }
    }

    pub fn config(&self) -> DdcConfig {
        self.config
    }

    /// Re-apply a configuration (a bandwidth or rate change). Clears the filter tails: the new
    /// geometry's history describes another channel.
    pub fn configure(&mut self, config: DdcConfig) {
        self.config = config;
        self.nco.set_frequency(config.offset_hz, config.fs_in);
        self.filter = IqFilter::new(Self::taps_for(&config));
        self.resampler = if config.resampling() {
            Some(ComplexResampler::new(config.decimated_rate(), config.out_rate))
        } else {
            None
        };
    }

    /// Full reset: state *and* the AGC gain (used when the stream restarts).
    pub fn reset(&mut self) {
        self.nco.reset();
        self.filter.reset();
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        self.agc.reset();
    }

    /// Retune within the same capture: clear the channel history but KEEP the AGC gain.
    ///
    /// Resetting the gain here is what produced a loud burst after every tune; the reference
    /// does the same (`AnalogDemod.retune`).
    pub fn retune(&mut self, offset_hz: f64) {
        self.config.offset_hz = offset_hz;
        self.nco.set_frequency(offset_hz, self.config.fs_in);
        self.nco.reset();
        self.filter.clear_tail();
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
    }

    pub fn agc(&self) -> &RmsAgc {
        &self.agc
    }

    /// Scale int16 IQ to f64, then mix the channel to DC.
    pub fn mix_i16_into(&mut self, iq: &[i16], out: &mut Vec<f64>) {
        let n = iq.len() & !1;              // a torn block would desync I/Q
        out.clear();
        out.reserve(n);
        for k in 0..n {
            out.push(iq[k] as f64 / IQ_FULL_SCALE);
        }
        self.nco.mix(out);
    }

    pub fn filter_into(&mut self, iq: &[f64], out: &mut Vec<f64>) {
        self.filter.process_complex_into(iq, out);
    }

    pub fn decimate_into(&mut self, iq: &[f64], out: &mut Vec<f64>) {
        let step = self.config.decimate.max(1);
        let n = iq.len() / 2;
        out.clear();
        out.reserve((n / step + 1) * 2);
        let mut k = 0;
        while k < n {
            out.push(iq[2 * k]);
            out.push(iq[2 * k + 1]);
            k += step;
        }
    }

    pub fn resample_into(&mut self, iq: &[f64], out: &mut Vec<f32>) {
        match self.resampler.as_mut() {
            Some(resampler) => resampler.process_into(iq, out),
            None => {
                out.clear();
                out.reserve(iq.len());
                for value in iq {
                    out.push(*value as f32);
                }
            }
        }
    }

    /// The level stage. Only the analog path enables it (see the module docs).
    pub fn agc_into(&mut self, x: &[f32], hold: bool, out: &mut Vec<f32>) {
        self.agc.process_into(x, hold, out);
    }

    /// Full chain over one int16 IQ block: complex f32 at the configured output rate.
    pub fn process_i16_into(&mut self, iq: &[i16], out: &mut Vec<f32>) {
        let mut mixed = core::mem::take(&mut self.mixed);
        let mut filtered = core::mem::take(&mut self.filtered);
        let mut decimated = core::mem::take(&mut self.decimated);
        self.mix_i16_into(iq, &mut mixed);
        self.filter_into(&mixed, &mut filtered);
        self.decimate_into(&filtered, &mut decimated);
        self.resample_into(&decimated, out);
        self.mixed = mixed;
        self.filtered = filtered;
        self.decimated = decimated;
    }

    /// Full chain plus the level stage, for the analog path.
    pub fn process_i16_to_real_into(&mut self, iq: &[i16], out: &mut Vec<f32>) {
        let mut resampled = core::mem::take(&mut self.resampled);
        let mut levelled = core::mem::take(&mut self.levelled);
        self.process_i16_into(iq, &mut resampled);
        // The real part is the audio-side signal; AGC is a real-signal stage like the reference.
        let real: Vec<f32> = resampled.chunks(2).map(|pair| pair[0]).collect();
        self.agc_into(&real, false, &mut levelled);
        out.clear();
        out.extend_from_slice(&levelled);
        self.resampled = resampled;
        self.levelled = levelled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::PI;

    fn tone_block(fs: f64, hz: f64, n: usize, amp: f64) -> Vec<i16> {
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            let ph = 2.0 * PI * hz / fs * k as f64;
            iq.push((amp * ph.cos() * IQ_FULL_SCALE) as i16);
            iq.push((amp * ph.sin() * IQ_FULL_SCALE) as i16);
        }
        iq
    }

    #[test]
    fn a_channel_tone_lands_on_dc_after_the_chain() {
        let fs = 1.0e6;
        let channel_hz = 100_000.0;
        let config = DdcConfig {
            fs_in: fs,
            offset_hz: channel_hz,
            cutoff_hz: 40_000.0,
            decimate: 10,
            out_rate: 100_000.0,     // no resampling in this test
            ntaps: 129,
        };
        let mut ddc = Ddc::new(config);
        let mut out = Vec::new();
        ddc.process_i16_into(&tone_block(fs, channel_hz, 4096, 0.5), &mut out);
        // Skip the filter transient, then the block must sit at +1 on I and ~0 on Q.
        assert!(out.len() > 200);
        let skip = 100;   // 100 complex samples past the tail split
        let mut sum_i = 0.0_f64;
        let mut sum_q = 0.0_f64;
        let mut count = 0.0_f64;
        for k in skip..(out.len() / 2) {
            sum_i += out[2 * k] as f64;
            sum_q += out[2 * k + 1] as f64;
            count += 1.0;
        }
        assert!((sum_i / count - 0.5).abs() < 0.01, "I mean {}", sum_i / count);
        assert!((sum_q / count).abs() < 0.01, "Q mean {}", sum_q / count);
    }

    #[test]
    fn an_out_of_band_tone_is_rejected() {
        // 300 kHz folds onto DC when the block is decimated by 10 (300 = 3 x 100 kHz), which is
        // exactly what the anti-alias filter exists for. The reference puts the filter's response
        // there at about -75 dB, and asserts a conservative -60 dB.
        //
        // The first decimated samples are skipped: a block that starts mid-tone makes the filter
        // ramp from its zeroed tail, and that step response (~-27 dB) is larger than the stopband
        // leakage the test is about.
        let fs = 1.0e6;
        let ntaps = 129;
        let decimate = 10;
        let config = DdcConfig {
            fs_in: fs,
            offset_hz: 0.0,
            cutoff_hz: 40_000.0,
            decimate,
            out_rate: 100_000.0,
            ntaps,
        };
        let mut ddc = Ddc::new(config);
        let mut out = Vec::new();
        ddc.process_i16_into(&tone_block(fs, 300_000.0, 4096, 0.5), &mut out);
        let settle = (ntaps / decimate) + 5;
        let peak = out
            .chunks(2)
            .skip(settle)
            .fold(0.0_f32, |m, pair| m.max(pair[0].abs().max(pair[1].abs())));
        let suppression_db = 20.0 * ((peak as f64) / 0.5).log10();
        assert!(
            suppression_db < -60.0,
            "300 kHz must be filtered out: peak {peak} is only {suppression_db:.1} dB down"
        );
    }

    #[test]
    fn retune_keeps_the_agc_gain_but_reset_drops_it() {
        let mut ddc = Ddc::new(DdcConfig::for_channel(1.0e6, 0.0, 48_000.0, 129));
        let mut out = Vec::new();
        ddc.process_i16_into(&tone_block(1.0e6, 0.0, 4096, 0.01), &mut out);
        let real = vec![0.01_f32; 960];
        let mut levelled = Vec::new();
        ddc.agc_into(&real, false, &mut levelled);
        let gain = ddc.agc().gain();
        assert!(gain > 1.0, "a quiet block must have raised the gain: {gain}");
        ddc.retune(12_500.0);
        assert_eq!(ddc.agc().gain(), gain, "retune must keep the gain");
        ddc.reset();
        assert_eq!(ddc.agc().gain(), 1.0, "reset must drop it");
    }

    #[test]
    fn the_resampler_runs_only_when_the_rates_differ() {
        let same = DdcConfig {
            fs_in: 1.0e6, offset_hz: 0.0, cutoff_hz: 40_000.0,
            decimate: 10, out_rate: 100_000.0, ntaps: 33,
        };
        assert!(!same.resampling());
        let mut ddc = Ddc::new(same);
        let mut out = Vec::new();
        ddc.process_i16_into(&tone_block(1.0e6, 0.0, 1000, 0.5), &mut out);
        // No resampler: the output length is the decimated length.
        assert_eq!(out.len() / 2, 100);
    }
}
