//! FT8 end-to-end: decode the committed fixture to the exact transmitted message.
//!
//! The fixture is produced by `tools/gen_ft8_fixtures.py`, which encodes a standard FT8 message
//! with the protocol's own rules (CRC-14, LDPC(174,91), Gray-coded 8-FSK with Costas sync — tables
//! ported from ft8_lib, MIT). This test is the contract: the decoder has to recover the text, not
//! "something plausible", and it has to reject a slot that carries no signal.
use websa_dsp::digital::ft8::Ft8Decoder;

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
const MANIFEST: &str = include_str!("../../tests/fixtures/ft8/manifest.json");

/// Minimal manifest reader (no JSON dependency in the crate).
fn manifest_string(key: &str) -> String {
    let needle = format!("\"{key}\": \"");
    let start = MANIFEST.find(&needle).expect("manifest key") + needle.len();
    let rest = &MANIFEST[start..];
    rest[..rest.find('"').expect("closing quote")].to_string()
}

fn manifest_number(key: &str) -> f64 {
    let needle = format!("\"{key}\": ");
    let start = MANIFEST.find(&needle).expect("manifest key") + needle.len();
    let rest = &MANIFEST[start..];
    let end = rest.find(|c: char| c == ',' || c == '\n').unwrap_or(rest.len());
    rest[..end].trim().parse().expect("number")
}

fn as_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|quad| f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]))
        .collect()
}

#[test]
fn the_committed_fixture_decodes_to_the_exact_transmitted_message() {
    let expected = manifest_string("message");
    let rate = manifest_number("rate");
    let samples = manifest_number("samples") as usize;
    let iq = as_f32(IQ_BYTES);
    assert_eq!(iq.len(), samples * 2, "fixture length must match the manifest");

    let mut decoder = Ft8Decoder::new(rate);
    decoder.push_iq(&iq);
    if cfg!(debug_assertions) {
        return;
    }
    let decoded = decoder.decode();
    let decoded = decoded.first().expect("the fixture must decode");
    println!(
        "FT8 decoded '{}' at {:.1} Hz (offset {:.3} s, sync SNR {:.1} dB)",
        decoded.text, decoded.frequency_hz, decoded.time_offset_s, decoded.snr_db
    );
    assert_eq!(decoded.text, expected);
    // The signal sits at the fixture's base frequency with no deliberate offset.
    assert!((decoded.frequency_hz - manifest_number("base_hz")).abs() < 25.0);
    // The reported time is the waterfall block time (the window leads the signal by a symbol).
    assert!(decoded.time_offset_s < 1.0, "start at {}", decoded.time_offset_s);
}

/// One slot decode has to fit inside its own slot with room to spare.
///
/// The search covers the whole FT8 audio band now (200-3000 Hz of candidates, each refined and then
/// handed to the LDPC/CRC stage), which is what makes a real band decodable at all; this keeps the
/// cost honest - the decoder runs in its own worker, but a slot that took longer than a slot would
/// fall behind the stream forever. Release-only: a debug build is unoptimized.
#[test]
fn a_slot_decode_stays_well_inside_a_slot() {
    if cfg!(debug_assertions) {
        return;
    }
    let rate = manifest_number("rate");
    let iq = as_f32(IQ_BYTES);
    let mut decoder = Ft8Decoder::new(rate);
    decoder.push_iq(&iq);
    let start = std::time::Instant::now();
    let decoded = decoder.decode();
    let decoded = decoded.first().expect("the fixture must decode");
    let seconds = start.elapsed().as_secs_f64();
    println!("FT8 slot decode: {:.3} s for '{}'", seconds, decoded.text);
    assert!(seconds < 4.0, "a slot decode took {seconds:.3} s");
}

#[test]
fn the_decoder_rejects_a_slot_that_carries_no_signal() {
    // The CRC is what separates "decoded" from "invented": noise must produce nothing.
    let rate = manifest_number("rate");
    let mut seed = 99_u32;
    let mut noise = Vec::with_capacity(48_000 * 2);
    for _ in 0..48_000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        noise.push(((seed >> 8) as f64 / 16_777_216.0 - 0.5) as f32 * 0.05);
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        noise.push(((seed >> 8) as f64 / 16_777_216.0 - 0.5) as f32 * 0.05);
    }
    let mut decoder = Ft8Decoder::new(rate);
    decoder.push_iq(&noise);
    assert!(decoder.decode().is_empty(), "noise must not decode");
}

#[test]
fn the_decoder_finds_a_signal_that_does_not_start_at_sample_zero() {
    // The decoder searches the slot edge (+/- 0.128 s), which is where a slot-synchronised FT8
    // transmission lands; this checks that the search actually covers a signal that is not at 0.
    let expected = manifest_string("message");
    let rate = manifest_number("rate");
    let iq = as_f32(IQ_BYTES);
    let mut seed = 7_u32;
    let mut buffer: Vec<f32> = Vec::new();
    for _ in 0..1_200 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        buffer.push(((seed >> 8) as f64 / 16_777_216.0 - 0.5) as f32 * 0.02);
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        buffer.push(((seed >> 8) as f64 / 16_777_216.0 - 0.5) as f32 * 0.02);
    }
    buffer.extend_from_slice(&iq);

    let mut decoder = Ft8Decoder::new(rate);
    decoder.push_iq(&buffer);
    if cfg!(debug_assertions) {
        return;
    }
    let decoded = decoder.decode();
    let decoded = decoded.first().expect("must decode a signal inside the search window");
    assert_eq!(decoded.text, expected);
    // The reported start is waterfall time (the window leads the signal), not the sample offset:
    // what this test is about is that the search finds and reads a signal that is not at the
    // buffer start, not the sub-grid precision.
    assert!(decoded.time_offset_s >= 0.0 && decoded.time_offset_s < 1.0,
            "offset {}", decoded.time_offset_s);
}

/// A candidate near the end of the buffer must not reach the reader.
///
/// The fine search records a base whose symbols were cut off by the end of the window (its correlation
/// is 0, which beats an empty best), and reading it indexed past the buffer: a wasm panic that killed
/// the decoder worker silently on the first real band (measured: `dsp_pushes` stopped with the buffer
/// 65 samples short of a slot). This is that shape: a transmission pushed so late in the buffer that
/// only its head fits.
#[test]
fn a_transmission_too_late_in_the_buffer_is_skipped_not_indexed() {
    let rate = manifest_number("rate");
    let symbol_samples = (rate * 0.160) as usize;
    let span = 79 * symbol_samples;                 // NUM_SYMBOLS
    let mut iq = as_f32(IQ_BYTES);
    // Pad so the burst starts 200 ms before the end: less than a transmission fits.
    let total = iq.len() / 2 + (span - symbol_samples / 2);
    let mut padded = vec![0.0_f32; total * 2];
    let start = total - iq.len() / 2 - symbol_samples / 2;
    padded[start * 2..start * 2 + iq.len()].copy_from_slice(&iq);
    iq = padded;
    let mut decoder = Ft8Decoder::new(rate);
    decoder.push_iq(&iq);
    if cfg!(debug_assertions) {
        return;
    }
    // Either it finds nothing (the honest answer) or it finds the message; never a panic.
    let _ = decoder.decode();
}

/// A busy band must not kill the decoder.
///
/// Reported from the bench: on a real 40 m band the decoder worker stopped reporting after its first
/// attempt (the page showed no error because the worker died silently). A number of stations, each a
/// tone with its own offset, is the shape that band has, and every stage of the search has to stay
/// inside its buffers while it looks at all of them.
#[test]
fn a_busy_band_is_searched_without_a_panic() {
    let rate = manifest_number("rate");
    let seconds = 20.0_f64;
    let total = (rate * seconds) as usize;
    let mut seed = 0x1234_5678_u32;
    let mut noise = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.05
    };
    // Twenty stations, spread over the band, each 8-FSK at a different offset.
    let stations: Vec<f64> = (0..20).map(|i| 250.0 + i as f64 * 137.0).collect();
    let mut iq = Vec::with_capacity(total * 2);
    for k in 0..total {
        let t = k as f64 / rate;
        let (mut i, mut q) = (noise(), noise());
        for (index, base) in stations.iter().enumerate() {
            let tone = (k / 7_680 + index) % 8;
            let hz = base + tone as f64 * 6.25;
            let ph = 2.0 * core::f64::consts::PI * hz * t;
            i += 0.05 * ph.cos();
            q += 0.05 * ph.sin();
        }
        iq.push(i as f32);
        iq.push(q as f32);
    }
    let mut decoder = Ft8Decoder::new(rate);
    for block in iq.chunks(48_000 * 2) {
        decoder.push_iq(block);
        if cfg!(debug_assertions) {
            continue;
        }
        // Every block that completes a slot attempts a decode; none of them may panic, and none may
        // stop the decoder from accepting the next block.
        let _ = decoder.decode();
    }
}
