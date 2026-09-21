//! How long does one slot decode take, on a *noise* slot (the live case) and on the fixture?
//!
//! A decoder that cannot finish inside a slot falls behind the stream for ever, so the budget matters
//! as much as the result. This prints the wall time of one `decode()` call.
use std::time::Instant;
use websa_dsp::digital::ft8::Ft8Decoder;

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
const MANIFEST: &str = include_str!("../../tests/fixtures/ft8/manifest.json");

/// The fixture's own rate: decoding at any other rate reads the wrong symbol clock and finds nothing.
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
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Deterministic noise, scaled like a real baseband (the analyzer's noise floor).
fn noise(n: usize, level: f32) -> Vec<f32> {
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    (0..n * 2)
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            ((rng >> 11) as f64 / (1u64 << 53) as f64 - 0.5) as f32 * level
        })
        .collect()
}

#[test]
fn one_slot_decode_fits_the_slot() {
    let rate = manifest_number("fs_in");
    for (label, iq) in [
        ("fixture", as_f32(IQ_BYTES)),
        ("noise  ", noise(rate as usize * 15, 0.4)),
    ] {
        let mut decoder = Ft8Decoder::new(rate);
        decoder.push_iq(&iq);
        let started = Instant::now();
        let result = decoder.decode();
        let elapsed = started.elapsed();
        println!(
            "{label}: decode took {:.2} s ({})",
            elapsed.as_secs_f64(),
            if result.is_some() { "decoded" } else { "nothing" }
        );
        assert!(
            elapsed.as_secs_f64() < 12.0,
            "{label}: a slot decode must finish well inside a 15 s slot, took {:.2} s",
            elapsed.as_secs_f64()
        );
    }
}
