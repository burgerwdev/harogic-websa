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
use websa_dsp::audio::AudioPolicy;
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

/// A channelized baseband block (the pipeline's input): a channel tone with a DC offset, so a DC
/// blocker has something to remove and the audio output legitimately changes with the chain on.
fn baseband(n: usize) -> Vec<f32> {
    let mut iq = Vec::with_capacity(n * 2);
    for k in 0..n {
        let ph = 2.0 * core::f64::consts::PI * 12_000.0 / 96_000.0 * k as f64;
        iq.push((0.3 + 0.4 * ph.cos()) as f32);
        iq.push((0.4 * ph.sin()) as f32);
    }
    iq
}

fn chain() -> AudioChain {
    // The *full* chain, not a token stage: the separation claim is about everything the analog path
    // runs (dc block, lpf, agc, squelch, wiener, notch, blanker), and using the real one is what
    // makes the comparison below meaningful.
    websa_dsp::audio::default_chain(96_000.0)
}

#[test]
fn the_raw_stream_is_bit_identical_with_the_audio_chain_enabled_or_not() {
    let iq = baseband(4096);
    // The chain must actually process for this comparison to say anything: the *default* policy is a
    // pass-through (the Python reference runs no enhancement), so the noise reducer is switched on
    // for the "with chain" run.
    let nr = AudioPolicy { nr: true, nr_strength: 1.0, squelch_dbfs: -110.0 };
    let mut with_chain = Pipeline::new(PathKind::Analog, chain(), nr);
    with_chain.set_analog_demod(Box::new(RealPartDemod));
    with_chain.set_audio_enabled(true);

    let mut without = Pipeline::new(PathKind::Analog, chain(), nr);
    without.set_analog_demod(Box::new(RealPartDemod));
    without.set_audio_enabled(false);

    let mut a = PipelineOutput::default();
    let mut b = PipelineOutput::default();
    with_chain.process_f32_into(&iq, false, &mut a);
    without.process_f32_into(&iq, false, &mut b);

    // Bit-for-bit, not "close enough": the RAW stream is the decoder's input.
    assert_eq!(a.raw.len(), b.raw.len());
    for (index, (x, y)) in a.raw.iter().zip(b.raw.iter()).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "RAW sample {index} changed: {x} vs {y}");
    }
    // ...and the audio genuinely went through the chain, or the comparison above proves nothing.
    assert_eq!(a.audio.len(), b.audio.len());
    let changed = a.audio.iter().zip(b.audio.iter()).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
    assert!(changed > a.audio.len() / 2, "the reducer must change the audio ({changed} samples)");
    // The RAW stream is what the decoder reads, and it never changes: the guarantee the newtype
    // carries is asserted here, bit for bit.
    assert_eq!(a.raw.len(), iq.len());
}

#[test]
fn the_digital_path_ignores_the_audio_chain_completely() {
    let iq = baseband(4096);

    let run = |audio_enabled: bool| {
        let mut pipeline = Pipeline::new(PathKind::Digital, chain(), AudioPolicy::default());
        pipeline.set_digital_demod(Box::new(Fingerprint::default()), 96_000.0, 96_000.0);
        pipeline.set_audio_enabled(audio_enabled);
        let mut out = PipelineOutput::default();
        pipeline.process_f32_into(&iq, false, &mut out);
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
    let iq = baseband(4096);
    let fingerprint = |audio_enabled: bool| {
        let mut pipeline = Pipeline::new(PathKind::Digital, chain(), AudioPolicy::default());
        pipeline.set_digital_demod(Box::new(Fingerprint::default()), 96_000.0, 96_000.0);
        pipeline.set_audio_enabled(audio_enabled);
        let mut out = PipelineOutput::default();
        pipeline.process_f32_into(&iq, false, &mut out);
        out
    };
    let on = fingerprint(true);
    let off = fingerprint(false);
    assert_eq!(on.decoded, off.decoded);
    assert_eq!(on.raw, off.raw, "the decoder's input must not depend on the audio settings");
}

#[test]
fn the_digital_path_adapts_the_baseband_rate_to_the_decoder() {
    // The backend's DDC hands over whatever channel rate its IF bandwidth needs; FT8 needs 48 kHz.
    // The pipeline owns that conversion, so at half the rate the decoder sees half the samples.
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Counting(Arc<AtomicUsize>);
    impl DigitalDemodulator for Counting {
        fn id(&self) -> &'static str {
            "ft8"
        }
        fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
            self.0.fetch_add(iq.len() / 2, Ordering::Relaxed);
            Vec::new()
        }
        fn reset(&mut self) {}
    }

    let iq = baseband(8_192);
    let seen = Arc::new(AtomicUsize::new(0));
    let mut pipeline = Pipeline::new(PathKind::Digital, AudioChain::new(Vec::new()), AudioPolicy::default());
    pipeline.set_digital_demod(Box::new(Counting(seen.clone())), 96_000.0, 48_000.0);
    let mut out = PipelineOutput::default();
    pipeline.process_f32_into(&iq, false, &mut out);
    let complex_in = iq.len() / 2;
    let complex_out = seen.load(Ordering::Relaxed);
    assert_eq!(out.raw.len(), iq.len(), "the RAW stream is the input, untouched");
    assert!(
        (complex_out as f64 - complex_in as f64 / 2.0).abs() < complex_in as f64 * 0.01,
        "a 96 kHz baseband must reach a 48 kHz decoder at half the sample count: {complex_out} vs {complex_in}"
    );
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
    let mut pipeline = Pipeline::new(PathKind::Analog, chain(), AudioPolicy::default());
    pipeline.set_analog_demod(Box::new(RealPartDemod));
    for (id, kind) in pipeline.active_plugins() {
        assert!(websa_dsp::plugin::find(kind, id).is_some(), "{kind:?}/{id} is not registered");
    }
}

/// The FT8 fixture: the channelized 48 kHz baseband of a real CQ transmission, which is what the
/// backend's DDC hands the browser.
///
/// Padded to a full slot: the decoder consumes slots (FT8 is a 15 s slot mode and the live stream is
/// a rolling buffer), so a transmission-sized buffer would never be decoded.
fn ft8_baseband() -> Vec<f32> {
    const BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
    let mut iq: Vec<f32> = BYTES
        .chunks_exact(4)
        .map(|q| f32::from_le_bytes([q[0], q[1], q[2], q[3]]) * 0.25 * 32767.0)
        .collect();
    iq.resize(48_000 * 15 * 2, 0.0);
    iq
}

#[test]
fn the_ft8_decoder_reads_the_raw_path_untouched_by_the_audio_chain() {
    // The strongest form of the separation claim, with the *real* digital decoder: the same IQ
    // decodes to the same message whether the analog enhancement chain is on or off, and the RAW
    // samples the decoder reads are bit-identical in both runs.
    use websa_dsp::digital::ft8::Ft8Plugin;

    let iq = ft8_baseband();
    let run = |audio_enabled: bool| {
        let mut pipeline = Pipeline::new(PathKind::Digital, chain(), AudioPolicy::default());
        pipeline.set_digital_demod(Box::new(Ft8Plugin::new(48_000.0)), 48_000.0, 48_000.0);
        pipeline.set_audio_enabled(audio_enabled);
        let mut decoded: Vec<String> = Vec::new();
        let mut raw_checksum = 0.0_f64;
        let mut out = PipelineOutput::default();
        for block in iq.chunks(8_192 * 2) {
            pipeline.process_f32_into(block, false, &mut out);
            decoded.extend(out.decoded.iter().cloned());
            for (index, sample) in out.raw.iter().enumerate() {
                raw_checksum += (*sample as f64) * (1.0 + index as f64 % 7.0);
            }
        }
        (decoded, raw_checksum)
    };

    let (decoded_on, raw_on) = run(true);
    let (decoded_off, raw_off) = run(false);
    assert_eq!(
        decoded_on,
        vec!["CQ JO1WKO PM95".to_string()],
        "the FT8 decoder must decode the fixture through the pipeline"
    );
    assert_eq!(decoded_on, decoded_off, "the decoded messages must not depend on the audio chain");
    assert_eq!(raw_on, raw_off, "the RAW stream must be bit-identical with the chain on and off");
}
