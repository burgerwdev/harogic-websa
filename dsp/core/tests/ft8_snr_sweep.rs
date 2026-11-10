//! Actual FT8 message recovery with fixed-seed noise and frequency offsets.
//! Single-signal cases cover -24 through -14 dB. The crowded fixture contains
//! signals at 800/875/950 Hz and a fourth cochannel signal; its weakest signals
//! have nominal -20 dB in-band SNR.
//!
//! SNR is active signal power relative to expected AWGN power in 100-3000 Hz,
//! about 0.645 dB below the 2500 Hz convention. Sensitivity assertions require
//! decoded payloads and zero unrelated messages. Release-only execution keeps
//! full-slot searches within the test's computation budget.
//!
//! cargo test --release --test ft8_snr_sweep -- --nocapture
use websa_dsp::digital::ft8::Ft8Decoder;

mod common;
use common::{insert, noise, noise_power_for, scenario, BAND_HZ, RATE, SLOT_SAMPLES};

const IQ_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/ft8/ft8_cq_iq.bin");
const MULTI_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/ft8/ft8_multi3_iq.bin");
const EXPECTED_CALL: &str = "JO1WKO";
/// Six levels and four distinct frequency offsets with one fixed noise realization.
const LEVELS_DB: [f64; 6] = [-24.0, -22.0, -20.0, -18.0, -16.0, -14.0];
const TRIALS_PER_LEVEL: usize = 4;
/// Where the transmission starts inside the slot (seconds): not slot-aligned, like a real capture.
const INSERT_S: f64 = 0.64;
/// Per-trial frequency offsets: on-grid, half-tone low, off-grid high. A decoder that only works
/// on-grid shows it here.
const OFFSETS_HZ: [f64; 4] = [0.0, -3.125, 4.5, -11.25];

/// The multi fixture's four signals, marker -> intended in-band SNR. The two -16 dB signals sit at
/// MULTI_WEAK_SNR_DB; the mid one at MULTI_WEAK_SNR_DB + 8; the strong one MULTI_WEAK_SNR_DB + 16.
const MULTI_EXPECTED: [(&str, f64); 4] = [
    ("CQ JO1WKO", -4.0),
    ("JA1XYZ", -12.0),
    ("BG7BVP", -20.0),
    ("K1ABC", -20.0),
];
const MULTI_TRIALS: usize = 4;
const MULTI_WEAK_SNR_DB: f64 = -20.0;
/// The weak fixture signal is 16 dB below the strong one.
const MULTI_WEAK_AMP_DB: f64 = -16.0;

/// Single-pass reference recovery counts, ordered by LEVELS_DB.
/// FLOOR_SINGLE requires four unique decodes at every level from -20 dB upward.
const PRE_CHANGE_SINGLE: [usize; 6] = [0, 0, 0, 2, 4, 4];
const FLOOR_SINGLE: [usize; 6] = [0, 0, 4, 4, 4, 4];
/// Decoded trials (of `MULTI_TRIALS`) where all four signals came out at once.
const PRE_CHANGE_MULTI_ALL: usize = 0;
/// Decoded trials where at least two signals came out.
const PRE_CHANGE_MULTI_TWO: usize = 4;
const FLOOR_MULTI_TWO: usize = 3;
const FLOOR_MULTI_ALL: usize = 3;

fn decode_slot(iq: &[f32]) -> Vec<String> {
    let mut decoder = Ft8Decoder::new(RATE);
    decoder.push_iq(iq);
    decoder
        .decode()
        .into_iter()
        .map(|m| m.text)
        .collect::<Vec<_>>()
}

fn multi_slot(noise_iq: &[f32], multi: &[f32], offset_hz: f64) -> Vec<f32> {
    let mut slot = noise_iq.to_vec();
    insert(&mut slot, multi, 0.0, offset_hz);
    slot
}

#[test]
fn noise_harness_preserves_the_multi_fixture_signal_power() {
    let background = vec![0.125; 512];
    let signal: Vec<f32> = [1.0, 0.5].into_iter().cycle().take(200).collect();
    let slot = multi_slot(&background, &signal, 0.0);
    let added: Vec<f32> = slot[..signal.len()].iter().zip(&background)
        .map(|(combined, background)| combined - background).collect();
    // Multi-fixture amplitudes encode the power ratios; adding a separately
    // calibrated noise floor must preserve those amplitudes as well as ratios.
    assert!((common::power_of(&added) / common::power_of(&signal) - 1.0).abs() < 1e-6);
    assert_eq!(&slot[signal.len()..], &background[signal.len()..]);
}

#[test]
fn the_sensitivity_floor_holds() {
    if cfg!(debug_assertions) {
        return;
    }
    let signal: Vec<f32> = IQ_BYTES
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // In-band noise power 1.0 at every level: the signal amplitude carries the whole sweep.
    let noise_iq = noise(SLOT_SAMPLES, 1.0 * RATE / BAND_HZ, 0x5EED_2026);

    let mut rates = Vec::new();
    let mut false_positives = 0usize;
    println!("\n single signal, {TRIALS_PER_LEVEL} trials per level (in-band SNR):");
    println!("  SNR dB | decoded | rate");
    for (level, floor) in LEVELS_DB.iter().zip(FLOOR_SINGLE.iter()) {
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
            let texts = decode_slot(&slot);
            if texts.iter().any(|t| t.contains(EXPECTED_CALL)) {
                decoded += 1;
            }
            false_positives += texts.iter().filter(|t| !t.contains(EXPECTED_CALL)).count();
        }
        println!("  {:>6.0} | {decoded:>7} | {:.0}%", level, 100.0 * decoded as f64 / TRIALS_PER_LEVEL as f64);
        rates.push(decoded);
        assert!(
            decoded >= *floor,
            "sensitivity regression at {level} dB: {decoded} decoded, floor is {floor}"
        );
    }

    // --- the busy band: four co-slot signals, weakest at MULTI_WEAK_SNR_DB ---
    let multi: Vec<f32> = MULTI_BYTES
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // The fixture's signals have amplitude 1/-8/-16 dB; in power the weak one is
    // 10^(MULTI_WEAK_AMP_DB/10) of the strong one's unit power. Pick the noise so the *weak*
    // signal sits at MULTI_WEAK_SNR_DB.
    let weak_power = 10.0_f64.powf(MULTI_WEAK_AMP_DB / 10.0);
    let noise_power = weak_power * noise_power_for(MULTI_WEAK_SNR_DB);
    let mut multi_all = 0;
    let mut multi_two = 0;
    println!("\n four signals (weakest at {MULTI_WEAK_SNR_DB} dB in-band), {MULTI_TRIALS} trials:");
    for trial in 0..MULTI_TRIALS {
        let seed = 0xA11C_0FF1 + trial as u64;
        let slot = multi_slot(
            &noise(SLOT_SAMPLES, noise_power, seed),
            &multi,
            OFFSETS_HZ[trial % OFFSETS_HZ.len()],
        );
        let texts = decode_slot(&slot);
        let hits = MULTI_EXPECTED
            .iter()
            .filter(|(marker, _)| texts.iter().any(|t| t.starts_with(marker)))
            .count();
        false_positives += texts.iter().filter(|text| {
            !MULTI_EXPECTED.iter().any(|(marker, _)| text.starts_with(marker))
        }).count();
        println!("  trial {trial}: {hits}/4 signals -> {texts:?}");
        multi_all += (hits == 4) as usize;
        multi_two += (hits >= 2) as usize;
    }
    assert!(
        multi_two >= FLOOR_MULTI_TWO,
        "busy-band regression: two or more signals decoded in {multi_two}/{MULTI_TRIALS} trials, floor is {FLOOR_MULTI_TWO}"
    );
    assert!(
        multi_all >= FLOOR_MULTI_ALL,
        "busy-band regression: all four signals decoded in {multi_all}/{MULTI_TRIALS} trials, floor is {FLOOR_MULTI_ALL}"
    );

    println!("\n baseline (pre-change, single pass): single {PRE_CHANGE_SINGLE:?}/4, multi-two {PRE_CHANGE_MULTI_TWO}/4, multi-all {PRE_CHANGE_MULTI_ALL}/4");
    println!(" this run: multi-two {multi_two}/4, multi-all {multi_all}/4, floors single {FLOOR_SINGLE:?}/4, multi-two {FLOOR_MULTI_TWO}/4, multi-all {FLOOR_MULTI_ALL}/4");
    println!(" false-positive decodes across the sweep: {false_positives}");
    assert_eq!(false_positives, 0, "sensitivity sweep emitted unrelated messages");
    // The baseline documents the pre-change decoder; if this run is strictly *better* somewhere,
    // the improvement tasks have landed and the constants above should be re-recorded (see the
    // comment block at the constants).
    let improved = rates
        .iter()
        .zip(PRE_CHANGE_SINGLE.iter())
        .any(|(now, before)| now > before)
        || multi_two > PRE_CHANGE_MULTI_TWO
        || multi_all > PRE_CHANGE_MULTI_ALL;
    if improved {
        println!(" NOTE: every level is at or above the pre-change baseline -- consider re-recording it.");
    }
}
