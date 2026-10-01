//! The OSD fallback, measured below the plain-LDPC cliff.
//!
//! The committed `ft8_snr_sweep` baseline records what the pure max-log LDPC path did at low SNR:
//! 0/8 trials at -20 and -22 dB in-band. The scenarios here run the same harness -- same fixture,
//! same deterministic noise, same offsets -- below that cliff, with the ordered-statistics
//! fallback enabled behind the LDPC. The contract:
//!
//! * the fallback turns a real share of those trials into decodes (the numbers below are floors,
//!   measured with the fallback in place; the pre-fallback measurement is recorded alongside),
//! * no false positives: every decode below the cliff that is not the transmitted text counts,
//!   and the count must stay at zero. The fallback CRC-checks only its single best word, so a
//!   wrong codeword needs a 14-bit CRC coincidence on top of winning the distance race.
//!
//!     cd wasm && cargo test --release --test ft8_osd -- --nocapture
use websa_dsp::digital::ft8::Ft8Decoder;

mod common;
use common::{load_iq, noise, scenario, RATE, SLOT_SAMPLES, BAND_HZ};

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
const EXPECTED_CALL: &str = "JO1WKO";
/// Below the pure-LDPC cliff (~-19 dB in-band), where the fallback is the only path to a decode.
const LEVELS_DB: [f64; 2] = [-20.0, -22.0];
const TRIALS_PER_LEVEL: usize = 8;
const INSERT_S: f64 = 0.64;
const OFFSETS_HZ: [f64; 4] = [0.0, -3.125, 4.5, -11.25];

/// The assertion floors, one trial of slack under what the bench measured (2026-09-29) for
/// cross-platform float drift: 2 and 0 decoded trials per level with the OSD fallback in place.
/// (The pre-fallback pure-LDPC numbers were 0/8 at both levels, and -22 dB stays out of reach for
/// OSD-2's weight-2 sweep.)
const FLOOR_OSD: [usize; 2] = [1, 0];

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
    println!(" measured (with OSD): {rates:?}/{}, floors {FLOOR_OSD:?}; pre-fallback pure LDPC: 0/8 at both levels", TRIALS_PER_LEVEL);
    assert_eq!(
        false_positives, 0,
        "the OSD fallback must not add false decodes below the cliff"
    );
    // The fallback must beat plain max-log LDPC somewhere below the cliff; if it does not, it is
    // dead weight in the hot path and the pre-change baseline has caught up with it.
    let improved = rates
        .iter()
        .zip([0usize, 0usize])
        .any(|(now, pure_ldpc)| *now > pure_ldpc);
    assert!(
        improved,
        "OSD decodes nothing below the cliff that plain LDPC also missed ({rates:?})"
    );
}
