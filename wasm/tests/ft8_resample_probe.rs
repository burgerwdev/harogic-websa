//! The real 40m capture survives the digital path's rate conversion.
//!
//! The browser pipeline resamples the backend's DDC baseband (48840.13 Hz for this capture) to
//! the decoder's 48 kHz; this pins the resampled -- not just the native-rate -- decode of the
//! weak real slot, so a resampler regression cannot hide behind the direct-rate test in
//! `ft8_real_slot.rs`.
use websa_dsp::ddc::ComplexResampler;
use websa_dsp::digital::ft8::Ft8Decoder;

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/real_slot_ja0txu.iq");
const RATE_IN: f64 = 48_840.130_763_116_205;
const RATE_OUT: f64 = 48_000.0;

#[test]
fn the_real_slot_survives_the_rate_conversion() {
    // Release-only like the other heavy integration tests: a resample plus two full decodes.
    if cfg!(debug_assertions) {
        return;
    }
    let iq: Vec<f32> = IQ_BYTES
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut decoder = Ft8Decoder::new(RATE_IN);
    decoder.push_iq(&iq);
    let direct = decoder.decode();
    println!("direct @48840.13: {} decodes {:?}", direct.len(), direct.iter().map(|m| &m.text).collect::<Vec<_>>());

    let mut resampler = ComplexResampler::new(RATE_IN, RATE_OUT);
    let mut out = Vec::new();
    resampler.process_f32_into(&iq, &mut out);
    println!("resampled: {} complex samples (from {})", out.len() / 2, iq.len() / 2);

    let mut decoder = Ft8Decoder::new(RATE_OUT);
    decoder.push_iq(&out);
    let through = decoder.decode();
    println!("through resampler @48000: {} decodes {:?}", through.len(), through.iter().map(|m| &m.text).collect::<Vec<_>>());
    assert!(!through.is_empty(), "the resampled real slot must still decode");
}
