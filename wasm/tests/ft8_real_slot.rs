//! Decode a real slot the phone app decoded from our own rendered audio.
//!
//! `tests/fixtures/ft8/real_slot_ja0txu.*` is a 15 s slice of a real 40m capture (7.074 MHz, an
//! arbitrary slot) whose audio, played to a phone running an FT8 app, decoded as "JA0TXU BG7BVP -73"
//! at 799 Hz audio. It is a *weak* signal (its Costas correlation is 0.193 against a chance level of
//! 0.125) in a band that also carries a steady spur, which is exactly the case our decoder missed
//! while the phone decoded it. Everything here runs offline, so the decoder can be iterated against a
//! known-good signal without a radio.
use websa_dsp::digital::ft8::Ft8Decoder;

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/real_slot_ja0txu.iq");
const RATE: f64 = 48_840.0;

fn as_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Ignored while it does not pass: the vector is committed so the work can continue offline, and a
/// failing test in CI would hide everything else. `cargo test --release --test ft8_real_slot -- --ignored
/// --nocapture` prints where the decoder loses the signal.
#[test]
#[ignore = "known gap: the sync is found, the time alignment is ~0.3 s off, so the LDPC never converges"]
fn the_phone_decodable_slot_decodes() {
    let iq = as_f32(IQ_BYTES);
    let mut decoder = Ft8Decoder::new(RATE);
    decoder.push_iq(&iq);
    let sync = decoder.debug_sync();
    println!("{} candidates survived the fine search:", sync.len());
    for (hz, correlation, seconds) in &sync {
        println!("  {hz:7.1} Hz  correlation {correlation:.3}  start {seconds:.3} s");
    }
    let result = decoder.decode();
    match &result {
        Some(message) => println!(
            "decoded: {:?} at {:.0} Hz, snr {:.0} dB, offset {:.3} s",
            message.text, message.frequency_hz, message.snr_db, message.time_offset_s
        ),
        None => println!("no decode (the phone decoded \"JA0TXU BG7BVP -73\" from this slot's audio)"),
    }
    // The expectation is the phone's message. It is a weak signal, so this is a sensitivity test: if it
    // cannot pass, the loss is in the sync gate or the soft-decision stage, and this is where to look.
    let message = result.expect("the phone decodes this slot; we must too");
    assert!(
        message.text.contains("BG7BVP") || message.text.contains("JA0TXU"),
        "decoded something else: {:?}",
        message.text
    );
}
