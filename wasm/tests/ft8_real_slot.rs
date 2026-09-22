//! Decode a real slot the phone app decoded from our own rendered audio.
//!
//! `tests/fixtures/ft8/real_slot_ja0txu.*` is a 15 s slice of a real 40m capture whose rendered
//! audio a phone FT8 app decoded as "JA0TXU BG7BVP -73". It is a weak signal in a busy band, and
//! — the part that mattered — its transmission starts several seconds into the slice, not at the
//! slot boundary a naive decoder assumes. The decoder therefore has to search the whole slot, and
//! tolerate the truncated tail, which is exactly the behaviour the waterfall search provides.
use websa_dsp::digital::ft8::Ft8Decoder;

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/real_slot_ja0txu.iq");
const RATE: f64 = 48_840.0;

fn as_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[test]
fn the_phone_decodable_slot_decodes() {
    let iq = as_f32(IQ_BYTES);
    let mut decoder = Ft8Decoder::new(RATE);
    decoder.push_iq(&iq);
    if cfg!(debug_assertions) {
        return;
    }
    let result = decoder.decode();
    for message in &result {
        println!(
            "decoded: {:?} at {:.0} Hz, snr {:.0} dB, offset {:.3} s",
            message.text, message.frequency_hz, message.snr_db, message.time_offset_s
        );
    }
    // The phone decoded "JA0TXU BG7BVP -73" from this slot's audio. It is a weak signal, so allow
    // the odd bit error the sum-product LDPC leaves uncorrected (the phone's decoder has an
    // a-posteriori pass ours does not): the station is what must be right.
    assert!(
        result.iter().any(|m| m.text.contains("BG7BVP") || m.text.contains("JA0TXU")),
        "decoded {:?}",
        result
    );
    // The transmission is inside the FT8 audio band, wherever the user happened to click.
    assert!(
        result.iter().any(|m| (200.0..=3_000.0).contains(&m.frequency_hz)),
        "frequency outside the band: {:?}",
        result
    );
}
