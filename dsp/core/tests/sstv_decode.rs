//! SSTV through the shipped plugin: a Python-encoded waveform in, the picture out.
//!
//! Three claims, and this is where each is checked:
//!
//! 1. **The port decodes what this repository's own reference encoder sends.** `robot36_iq.bin` is
//!    written by `tools/fixtures/gen_sstv_fixtures.py` from the Python encoder whose mode tables are
//!    ported from the vendored Rust tree — so this is Python → Rust, not a round trip through one
//!    implementation. `tools/fixtures/gen_sstv_fixtures.py --check` keeps the fixture in step with
//!    both sides.
//! 2. **Every registered mode decodes its own transmission.** The vendored encoder synthesises one
//!    per mode and the plugin decodes it, so a mode cannot be listed, buildable and unable to decode.
//! 3. **The audio-enhancement chain cannot reach the decoder.** The picture must be bit-identical
//!    whether the chain is on or off; a denoiser on an SSTV line would destroy it, and the failure
//!    would be a subtly wrong picture rather than an error.
use websa_dsp::audio::AudioPolicy;
use websa_dsp::digital::sstv::{mode_index, mode_sequence_ns, mode_vis, SstvPlugin, SSTV_MODES};
use websa_dsp::digital::sstv::vendor::decode::Decoder;
use websa_dsp::digital::sstv::vendor::encode::{Encoder, Synthesizer};
use websa_dsp::digital::sstv::vendor::modes::Mode;
use websa_dsp::digital::sstv::vendor::RgbPixel;
use websa_dsp::pipeline::{PathKind, Pipeline, PipelineOutput};
use websa_dsp::plugin::DigitalDemodulator;

const IQ_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/sstv/robot36_iq.bin");
const RGB_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/sstv/robot36_rgb.bin");
const MANIFEST: &str = include_str!("../../../tests/fixtures/sstv/manifest.json");
const MODES_JSON: &str = include_str!("../../../tests/fixtures/sstv/modes.json");

/// Minimal manifest readers (the crate has no JSON dependency), as `ft8_reference.rs` does it.
fn manifest_string(key: &str) -> String {
    let needle = format!("\"{key}\": \"");
    let start = MANIFEST.find(&needle).expect("manifest key") + needle.len();
    let end = MANIFEST[start..].find('"').expect("manifest value") + start;
    MANIFEST[start..end].to_string()
}

fn manifest_number(key: &str) -> f64 {
    let needle = format!("\"{key}\": ");
    let start = MANIFEST.find(&needle).expect("manifest key") + needle.len();
    let end = MANIFEST[start..]
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .map_or(MANIFEST.len(), |offset| start + offset);
    MANIFEST[start..end].parse().expect("manifest number")
}

/// The fixture as complex baseband (interleaved f32 pairs).
fn fixture_iq() -> Vec<f32> {
    let samples = manifest_number("samples") as usize;
    let bytes = samples * 2 * 4;
    assert!(IQ_BYTES.len() >= bytes, "fixture is shorter than its manifest says");
    IQ_BYTES[..bytes]
        .chunks_exact(4)
        .map(|word| f32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect()
}

/// Feed a block sequence to a digital demodulator, in the pipeline the worker drives.
fn run_blocks(demod: Box<dyn DigitalDemodulator>, iq: &[f32], rate: f64, block: usize) -> Pipeline {
    let mut pipeline = Pipeline::new(PathKind::Digital, websa_dsp::audio::default_chain(rate),
                                     AudioPolicy::default());
    pipeline.set_digital_demod(demod, rate, rate);
    let mut out = PipelineOutput::default();
    for chunk in iq.chunks(block * 2) {
        pipeline.process_f32_into(chunk, false, &mut out);
    }
    pipeline
}

/// A Python-encoded transmission at 8 kHz: the send/receive check.
fn decode_fixture() -> (websa_dsp::plugin::ImageStatus, Vec<u8>) {
    let rate = manifest_number("rate");
    let iq = fixture_iq();
    let pipeline = run_blocks(Box::new(SstvPlugin::new(rate)), &iq, rate, 2048);
    let status = pipeline.digital_image_status().expect("a decoded picture");
    let image = pipeline.digital_image().expect("picture pixels");
    (status, image.rgb.to_vec())
}

#[test]
fn decodes_the_python_encoded_transmission() {
    let (status, rgb) = decode_fixture();
    let expected_mode = manifest_string("mode");
    let width = manifest_number("width") as u32;
    let height = manifest_number("height") as u32;
    assert_eq!(
        SSTV_MODES[status.mode_index as usize].0, expected_mode,
        "the VIS header must identify the mode the fixture was sent in"
    );
    assert_eq!(status.vis, manifest_number("vis") as u32);
    assert_eq!((status.width, status.height), (width, height));
    assert!(status.complete, "the picture stopped at {} of {} lines", status.lines, status.height);
    assert_eq!(status.progress, 100);
    assert_eq!(rgb.len(), RGB_BYTES.len());

    let error = mean_absolute_error(&rgb, RGB_BYTES);
    let tolerance = manifest_number("tolerance_mean_abs_error");
    assert!(error < tolerance, "mean absolute error {error} per channel exceeds {tolerance}");
}

fn mean_absolute_error(actual: &[u8], expected: &[u8]) -> f64 {
    let sum: u64 = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    sum as f64 / actual.len() as f64
}

/// A picture to send: red brightens to the right, green downwards, blue diagonally. A row order or a
/// channel swap shows up as error rather than passing.
fn gradient(width: usize, height: usize) -> Vec<RgbPixel> {
    (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                RgbPixel::new(
                    (x * 255 / width.max(1)) as u8,
                    (y * 255 / height.max(1)) as u8,
                    ((x + y) * 255 / (width + height).max(1)) as u8,
                )
            })
        })
        .collect()
}

/// Encode `mode`'s picture, decode it, and report the picture against what was sent.
///
/// The decode here is the vendored `Decoder` on the synthesised audio, not the plugin around it. The
/// plugin's front end is a 129-tap complex band pass, and these eighteen transmissions are about
/// 26 M samples at the decoder's 8 kHz — minutes of debug-build FIR for no extra coverage, because
/// the front end is what the two Robot 36 fixture tests (`decodes_the_python_encoded_transmission`
/// here, and the plugin tests in `digital::sstv`) already drive, from a Rust- and a Python-encoded
/// waveform. What this test owns is the claim "every registered mode decodes its own transmission".
fn round_trip(mode: Mode, pixels: Vec<RgbPixel>) -> (Mode, bool, Vec<u8>) {
    const RATE: u32 = 8_000;
    let encoder = Encoder::new(mode, pixels.into_iter()).expect("encoder");
    let samples: Vec<i16> = Synthesizer::new(encoder, RATE).collect();
    let image = Decoder::new(samples.iter().copied(), RATE)
        .decode()
        .expect("a decoded picture");
    let rgb = image.pixels().iter().flat_map(|p| [p.red(), p.green(), p.blue()]).collect();
    (image.mode(), image.complete(), rgb)
}

#[test]
fn every_registered_mode_decodes_its_own_transmission() {
    let mut failures: Vec<String> = Vec::new();
    for (index, (name, mode)) in SSTV_MODES.iter().enumerate() {
        assert_eq!(mode_index(*mode), Some(index), "{name} is not at its own index");
        let (width, height) = (mode.resolution().0 as usize, mode.resolution().1 as usize);
        let sent = gradient(width, height);
        let (decoded_mode, complete, rgb) = round_trip(*mode, sent.clone());
        if decoded_mode != *mode {
            failures.push(format!("{name}: decoded as {decoded_mode:?}"));
            continue;
        }
        if !complete {
            failures.push(format!("{name}: the picture is incomplete"));
            continue;
        }
        // A synthesised transmission of our own picture: the decoder's own tolerance is a few levels
        // per channel, so anything above this is a real defect (row order, channel, timing).
        let expected: Vec<u8> = sent.iter().flat_map(|p| [p.red(), p.green(), p.blue()]).collect();
        let error = mean_absolute_error(&rgb, &expected);
        if error >= 24.0 {
            failures.push(format!("{name}: mean absolute error {error:.1} per channel"));
        }
    }
    assert!(failures.is_empty(), "modes that did not decode:\n  {}", failures.join("\n  "));
}

#[test]
fn the_audio_chain_cannot_reach_the_sstv_decoder() {
    let rate = manifest_number("rate");
    let iq = fixture_iq();
    // The chain is switched on for one run and off for the other. Both must produce the same picture:
    // the digital path never sees the chain, and this is the assertion that keeps that true.
    let run = |audio_enabled: bool| {
        let policy = AudioPolicy { nr: true, nr_strength: 1.0, squelch_dbm: -110.0 };
        let mut pipeline = Pipeline::new(PathKind::Digital, websa_dsp::audio::default_chain(rate), policy);
        pipeline.set_digital_demod(Box::new(SstvPlugin::new(rate)), rate, rate);
        pipeline.set_audio_enabled(audio_enabled);
        let mut out = PipelineOutput::default();
        for chunk in iq.chunks(4096) {
            pipeline.process_f32_into(chunk, false, &mut out);
        }
        // No PCM at all on this path: the chain has no input to work on.
        assert!(out.audio.is_empty(), "the digital path produced PCM");
        pipeline.digital_image().expect("a decoded picture").rgb.to_vec()
    };
    let on = run(true);
    let off = run(false);
    assert!(!on.is_empty());
    assert!(on == off, "the picture changed when the audio chain was enabled");
}

/// Every value of `key` in the fixture table, in file order (no JSON dependency in this crate).
fn json_numbers(text: &str, key: &str) -> Vec<u64> {
    let needle = format!("\"{key}\": ");
    text.match_indices(&needle)
        .map(|(at, _)| {
            let rest = &text[at + needle.len()..];
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            rest[..end].parse().expect("manifest number")
        })
        .collect()
}

#[test]
fn the_transmitter_table_matches_the_vendored_decoder_table() {
    // The Python transmitter (`tools/fixtures/sstv_encode.py`) gets its timing from
    // `tools/fixtures/port_sstv_modes.py`, which reads this tree. The chain is only honest if its two
    // ends agree, so the table it wrote is compared here, mode by mode, against the vendored one:
    // VIS code, resolution and the length of one pass of the timing sequence.
    let index = json_numbers(MODES_JSON, "index");
    let vis = json_numbers(MODES_JSON, "vis");
    let width = json_numbers(MODES_JSON, "width");
    let height = json_numbers(MODES_JSON, "height");
    let total = json_numbers(MODES_JSON, "total_ns");
    assert_eq!(index.len(), SSTV_MODES.len(), "the fixture table lists every mode");
    for (position, (name, mode)) in SSTV_MODES.iter().enumerate() {
        assert_eq!(index[position] as usize, position, "{name}: the table's own order");
        assert_eq!(vis[position] as u32, u32::from(mode_vis(*mode)), "{name}: VIS code");
        assert_eq!(
            (width[position] as u32, height[position] as u32),
            mode.resolution(),
            "{name}: resolution"
        );
        assert_eq!(total[position], mode_sequence_ns(*mode), "{name}: one pass");
    }
}

#[test]
fn a_second_transmission_replaces_the_picture_instead_of_extending_it() {
    // Two transmissions in one stream, as a station sending twice produces: the fixture (10 s of tail
    // silence included, which is the gap a station leaves) followed by a differently patterned picture.
    // The plugin clears its buffer only on a *complete* picture, so the second one must arrive as a new
    // serial and a new picture — not as the first picture with the second's lines stitched on.
    const RATE: u32 = 8_000;
    let mode = SSTV_MODES[5].1; // Robot 36, the fixture's mode
    let (width, height) = (mode.resolution().0 as usize, mode.resolution().1 as usize);
    let second = gradient(width, height);
    let encoder = Encoder::new(mode, second.clone().into_iter()).expect("encoder");
    let mut iq = fixture_iq();
    // A gap, in case the fixture's own tail silence is shorter than a station's pause, then the second
    // transmission at the same rate.
    iq.extend(core::iter::repeat(0.0f32).take(RATE as usize * 2 * 2));
    for sample in Synthesizer::new(encoder, RATE) {
        iq.push(f32::from(sample) / 32_767.0);
        iq.push(0.0);
    }
    // Trailing silence as well: the final attempt has to run after the last scanline arrived, and a
    // live stream always has audio after a transmission (a recording cut at the last sample would
    // leave the last attempt where it was).
    iq.extend(core::iter::repeat(0.0f32).take(RATE as usize * 5 * 2));

    let mut pipeline = Pipeline::new(PathKind::Digital, websa_dsp::audio::default_chain(f64::from(RATE)),
                                     AudioPolicy::default());
    pipeline.set_digital_demod(Box::new(SstvPlugin::new(f64::from(RATE))), f64::from(RATE),
                               f64::from(RATE));
    let mut out = PipelineOutput::default();
    let mut seen: Vec<(u64, u32, bool, Vec<u8>)> = Vec::new();
    for chunk in iq.chunks(2048 * 2) {
        pipeline.process_f32_into(chunk, false, &mut out);
        if let (Some(status), Some(image)) =
            (pipeline.digital_image_status(), pipeline.digital_image())
        {
            let last = seen.last().map(|entry| (entry.0, entry.1, entry.2));
            if last != Some((status.serial, status.lines, status.complete)) {
                seen.push((status.serial, status.lines, status.complete, image.rgb.to_vec()));
            }
        }
    }
    let first = seen.iter().find(|entry| entry.0 == 1 && entry.2).expect("the fixture completed");
    let second_entry = seen.iter().find(|entry| entry.0 == 2 && entry.2).expect("the second completed");
    // The first picture is the fixture's; the second is the pattern encoded for it.
    let fixture_error = mean_absolute_error(&first.3, RGB_BYTES);
    assert!(fixture_error < 24.0, "first picture error {fixture_error}");
    let expected: Vec<u8> = second.iter().flat_map(|p| [p.red(), p.green(), p.blue()]).collect();
    let second_error = mean_absolute_error(&second_entry.3, &expected);
    assert!(second_error < 24.0, "second picture error {second_error}");
    // ...and they are different pictures, which is what proves the second was not the first held over.
    let difference = mean_absolute_error(&first.3, &second_entry.3);
    assert!(difference > 5.0, "the two pictures look the same (error {difference})");
}
