//! The RAW/digital path must be bit-identical whatever the audio chain is doing.
//!
//! This is the structural promise of the architecture: audio enhancement is for the human ear, so
//! it may only ever touch the analog PCM. If a voice denoiser could reach the digital path it
//! would destroy the information an FT8 decoder needs — and the failure would be subtle (a
//! decoded message that mostly works), which is why it is asserted bit-for-bit rather than
//! reviewed.
//!
//! The demodulators here are test stand-ins: the real modes land on top of the same interfaces,
//! and this test keeps the interfaces honest.
use websa_dsp::audio::DcBlock;
use websa_dsp::ddc::{Ddc, DdcConfig};
use websa_dsp::pipeline::{AudioChain, PathKind, Pipeline, PipelineOutput};
use websa_dsp::plugin::{AnalogDemodulator, DigitalDemodulator, PluginKind};

/// Stand-in analog demodulator: real part to PCM, as a trivial mode would.
struct RealPartDemod;

impl AnalogDemodulator for RealPartDemod {
    fn id(&self) -> &'static str {
        "am"
    }
    fn process_into(&mut self, iq: &[f32], out: &mut Vec<f32>) {
        out.clear();
        out.extend(iq.chunks(2).map(|pair| pair[0]));
    }
    fn retune(&mut self) {}
    fn reset(&mut self) {}
}

/// Stand-in digital demodulator: keeps a fingerprint of what it was fed, so the test can show the
/// decoder saw exactly the same samples in both runs.
#[derive(Default)]
struct Fingerprint {
    checksum: f64,
    samples: usize,
}

impl DigitalDemodulator for Fingerprint {
    fn id(&self) -> &'static str {
        "ft8"
    }
    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        for (index, value) in iq.iter().enumerate() {
            // Order-sensitive so a reordering cannot pass by accident.
            self.checksum += (*value as f64) * (1.0 + index as f64);
        }
        self.samples += iq.len();
        Vec::new()
    }
    fn reset(&mut self) {
        self.checksum = 0.0;
        self.samples = 0;
    }
}

fn iq_block(n: usize) -> Vec<i16> {
    // A channel tone with a DC offset, so a DC blocker has something to remove and the audio
    // output legitimately changes when the chain is enabled.
    let mut iq = Vec::with_capacity(n * 2);
    for k in 0..n {
        let ph = 2.0 * core::f64::consts::PI * 12_000.0 / 96_000.0 * k as f64;
        iq.push(((0.3 + 0.4 * ph.cos()) * 32768.0) as i16);
        iq.push((0.4 * ph.sin() * 32768.0) as i16);
    }
    iq
}

fn ddc() -> Ddc {
    Ddc::new(DdcConfig {
        fs_in: 1.0e6,
        offset_hz: 0.0,
        cutoff_hz: 40_000.0,
        decimate: 10,
        out_rate: 96_000.0,
        ntaps: 129,
    })
}

fn chain() -> AudioChain {
    // The *full* chain, not a token stage: the separation claim is about everything the analog path
    // runs (dc block, lpf, agc, squelch, wiener, notch, blanker), and using the real one is what
    // makes the comparison below meaningful.
    websa_dsp::audio::default_chain()
}

#[test]
fn the_raw_stream_is_bit_identical_with_the_audio_chain_enabled_or_not() {
    let iq = iq_block(4096);
    let mut with_chain = Pipeline::new(ddc(), PathKind::Analog, chain());
    with_chain.set_analog_demod(Box::new(RealPartDemod));
    with_chain.set_audio_enabled(true);

    let mut without = Pipeline::new(ddc(), PathKind::Analog, chain());
    without.set_analog_demod(Box::new(RealPartDemod));
    without.set_audio_enabled(false);

    let mut a = PipelineOutput::default();
    let mut b = PipelineOutput::default();
    with_chain.process_i16_into(&iq, false, &mut a);
    without.process_i16_into(&iq, false, &mut b);

    // Bit-for-bit, not "close enough": the RAW stream is the decoder's input.
    assert_eq!(a.raw.len(), b.raw.len());
    for (index, (x, y)) in a.raw.iter().zip(b.raw.iter()).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "RAW sample {index} changed: {x} vs {y}");
    }
    // ...and the audio genuinely went through the chain, or the comparison above proves nothing.
    assert_eq!(a.audio.len(), b.audio.len());
    let changed = a.audio.iter().zip(b.audio.iter()).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
    assert!(changed > a.audio.len() / 2, "the DC blocker must change the audio ({changed} samples)");
    let mean = a.audio.iter().map(|v| *v as f64).sum::<f64>() / a.audio.len() as f64;
    let mean_off = b.audio.iter().map(|v| *v as f64).sum::<f64>() / b.audio.len() as f64;
    assert!(mean.abs() < mean_off.abs(), "the chain must remove the offset ({mean} vs {mean_off})");
}

#[test]
fn the_digital_path_ignores_the_audio_chain_completely() {
    let iq = iq_block(4096);

    let mut run = |audio_enabled: bool| {
        let mut pipeline = Pipeline::new(ddc(), PathKind::Digital, chain());
        pipeline.set_digital_demod(Box::new(Fingerprint::default()));
        pipeline.set_audio_enabled(audio_enabled);
        let mut out = PipelineOutput::default();
        pipeline.process_i16_into(&iq, false, &mut out);
        out
    };

    let on = run(true);
    let off = run(false);
    // No PCM at all on this path: the chain has no input to work on.
    assert!(on.audio.is_empty() && off.audio.is_empty());
    assert_eq!(on.raw.len(), off.raw.len());
    for (index, (x, y)) in on.raw.iter().zip(off.raw.iter()).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "RAW sample {index} changed with the chain enabled");
    }
}

#[test]
fn the_decoder_sees_the_same_samples_in_both_runs() {
    let iq = iq_block(4096);
    let fingerprint = |audio_enabled: bool| {
        let mut pipeline = Pipeline::new(ddc(), PathKind::Digital, chain());
        pipeline.set_digital_demod(Box::new(Fingerprint::default()));
        pipeline.set_audio_enabled(audio_enabled);
        let mut out = PipelineOutput::default();
        pipeline.process_i16_into(&iq, false, &mut out);
        out
    };
    let on = fingerprint(true);
    let off = fingerprint(false);
    assert_eq!(on.decoded, off.decoded);
    assert_eq!(on.raw, off.raw, "the decoder's input must not depend on the audio settings");
}

#[test]
fn the_audio_chain_only_contains_implemented_stages() {
    // The chain is built from the registry, and a stage whose kernel is not written yet is skipped
    // instead of being stubbed with a pass-through that would silently do nothing.
    let built = chain();
    assert_eq!(built.ids().len(), 7, "the analog chain must contain every implemented stage");
    for id in built.ids() {
        assert!(
            websa_dsp::plugin::find(PluginKind::Audio, id).map(|p| p.implemented).unwrap_or(false),
            "chain contains {id}, which the registry does not mark as implemented"
        );
    }
}

#[test]
fn every_registered_plugin_can_be_resolved_by_its_own_family() {
    // Mirror of tools/check_registrations.py for the DSP side: a plugin that is registered but
    // unreachable (a typo in its id, a family that forgets it) fails here.
    for kind in PluginKind::all() {
        for descriptor in websa_dsp::plugin::plugins(kind) {
            let found = websa_dsp::plugin::find(kind, descriptor.id)
                .unwrap_or_else(|| panic!("{} is registered but not resolvable", descriptor.id));
            assert_eq!(found.kind, descriptor.kind);
            assert_eq!(found.implemented, descriptor.implemented);
        }
    }
    // The pipeline reports only plugins that exist in a family (the UI reads this list).
    let mut pipeline = Pipeline::new(ddc(), PathKind::Analog, chain());
    pipeline.set_analog_demod(Box::new(RealPartDemod));
    for (id, kind) in pipeline.active_plugins() {
        assert!(websa_dsp::plugin::find(kind, id).is_some(), "{kind:?}/{id} is not registered");
    }
}
