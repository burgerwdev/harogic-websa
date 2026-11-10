//! Actual weak-signal recovery with BP/OSD at -20 and -22 dB in-band SNR.
//! Four fixed frequency offsets exercise independent scenarios. Acceptance
//! requires the transmitted message, the specified recovery counts and zero
//! unrelated decodes. The decoder receives no prior payload bits.
//!
//! cargo test --release --test ft8_osd -- --nocapture
use websa_dsp::digital::ft8::Ft8Decoder;

mod common;
use common::{load_iq, noise, scenario, RATE, SLOT_SAMPLES, BAND_HZ};

const IQ_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/ft8/ft8_cq_iq.bin");
const EXPECTED_CALL: &str = "JO1WKO";
/// Below the pure-LDPC cliff (~-19 dB in-band), where the fallback is the only path to a decode.
const LEVELS_DB: [f64; 2] = [-20.0, -22.0];
const TRIALS_PER_LEVEL: usize = 4;
const INSERT_S: f64 = 0.64;
const OFFSETS_HZ: [f64; 4] = [0.0, -3.125, 4.5, -11.25];

/// Minimum successful recoveries among four trials, ordered by LEVELS_DB.
const FLOOR_OSD: [usize; 2] = [4, 1];

fn decode_texts(iq: &[f32]) -> Vec<String> {
    let mut decoder = Ft8Decoder::new(RATE);
    decoder.push_iq(iq);
    decoder
        .decode()
        .into_iter()
        .map(|m| m.text)
        .collect::<Vec<_>>()
}

#[test]
fn the_osd_fallback_decodes_below_the_ldpc_cliff() {
    if cfg!(debug_assertions) {
        return;
    }
    let signal = load_iq(IQ_BYTES);
    // In-band noise power 1.0; the signal amplitude carries the level.
    let noise_iq = noise(SLOT_SAMPLES, RATE / BAND_HZ, 0x05D_0001);

    let mut rates = Vec::new();
    let mut false_positives = 0usize;
    for (level, floor) in LEVELS_DB.iter().zip(FLOOR_OSD.iter()) {
        let mut decoded = 0;
        for trial in 0..TRIALS_PER_LEVEL {
            let slot = scenario(
                &noise_iq,
                1.0,
                &signal,
                *level,
                INSERT_S,
                OFFSETS_HZ[trial % OFFSETS_HZ.len()],
            );
            let texts = decode_texts(&slot);
            if texts.iter().any(|t| t.contains(EXPECTED_CALL)) {
                decoded += 1;
            }
            false_positives += texts.iter().filter(|t| !t.contains(EXPECTED_CALL)).count();
        }
        println!("  {level:>5.0} dB: {decoded}/{} decoded", TRIALS_PER_LEVEL);
        rates.push(decoded);
        assert!(
            decoded >= *floor,
            "OSD sensitivity regression at {level} dB: {decoded} decoded, floor is {floor}"
        );
    }
    println!(" measured (with OSD): {rates:?}/{}, floors {FLOOR_OSD:?}; pre-fallback pure LDPC: 0/4 at both levels", TRIALS_PER_LEVEL);
    assert_eq!(
        false_positives, 0,
        "the OSD fallback must not add false decodes below the cliff"
    );
}
