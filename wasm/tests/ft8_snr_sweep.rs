//! The decoder's sensitivity floor, measured and committed.
//!
//! "Better decoding" has to mean something measurable. This test sweeps a real encoded transmission
//! (`ft8_cq_iq.bin`, the fixture the reference test uses) through additive noise from -24 to -14 dB
//! and counts decodes per level, then checks the numbers against the baseline committed below. The
//! same harness stacks the four-signal composite (`ft8_multi3_iq.bin`: three co-slot transmissions
//! at 800/875/950 Hz -- 25-75 Hz apart, i.e. genuinely crowded -- plus a fourth sharing the strong
//! signal's *exact* frequency, the co-channel pair only subtraction can untangle) under a noise
//! floor that puts the *weakest* signal at
//! -20 dB -- the busy-band case a single-pass decoder is expected to fail and the case subtraction
//! and OSD exist to fix.
//!
//! SNR here is **in-band**: signal mean power against noise power measured in the 100-3000 Hz band
//! the waterfall actually reads (the noise itself is white at 48 kHz, scaled so its in-band share is
//! exact). That is within ~0.6 dB of WSJT-X's convention of referencing SNR to 2500 Hz, so the
//! levels below can be compared with the literature: the reference decoder is quoted down to about
//! -21 dB. Everything is deterministic (fixed seeds, fixed frequency offsets), so a run reproduces
//! bit-for-bit and the baseline is a real floor, not a hope.
//!
//! Release-only like `ft8_real_slot`: each 15 s decode costs the better part of a second, which is
//! no test for a debug build.
//!
//!     cd wasm && cargo test --release --test ft8_snr_sweep -- --nocapture
use websa_dsp::digital::ft8::Ft8Decoder;

mod common;
use common::{noise, noise_power_for, scenario, BAND_HZ, RATE, SLOT_SAMPLES};

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
const MULTI_BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_multi3_iq.bin");
const EXPECTED_CALL: &str = "JO1WKO";
/// One FT8 slot: the buffer the decode runs over.
/// The waterfall's analysis band: what "in-band" noise means.
/// The sweep. Six levels, eight trials each (four frequency offsets x two noise seeds): enough for
/// a rate with resolution 1/8, cheap enough to keep the whole test around half a minute.
const LEVELS_DB: [f64; 6] = [-24.0, -22.0, -20.0, -18.0, -16.0, -14.0];
const TRIALS_PER_LEVEL: usize = 8;
/// Where the transmission starts inside the slot (seconds): not slot-aligned, like a real capture.
const INSERT_S: f64 = 0.64;
/// Per-trial frequency offsets: on-grid, half-tone low, off-grid high. A decoder that only works
/// on-grid shows it here.
const OFFSETS_HZ: [f64; 4] = [0.0, -3.125, 4.5, -11.25];

/// The multi fixture's four signals, marker -> intended in-band SNR. The two -16 dB signals sit at
/// MULTI_WEAK_SNR_DB; the mid one at MULTI_WEAK_SNR_DB + 8; the strong one MULTI_WEAK_SNR_DB + 24.
const MULTI_EXPECTED: [(&str, f64); 4] = [
    ("CQ JO1WKO", 4.0),
    ("JA1XYZ", -12.0),
    ("BG7BVP", -20.0),
    ("K1ABC", -20.0),
];
const MULTI_TRIALS: usize = 4;
const MULTI_WEAK_SNR_DB: f64 = -20.0;
/// The weak fixture signal is 16 dB below the strong one.
const MULTI_WEAK_AMP_DB: f64 = -16.0;

// ---------------------------------------------------------------------------
// The committed baseline. Measured on the pre-change decoder (single pass, no
// subtraction, no OSD) with this exact harness on 2026-09-29; recorded so the
// improvement tasks have a number to beat and so no later change can silently
// give the sensitivity back. Floors are the measured value minus one trial of
// slack, which absorbs cross-platform float drift without making the check
// cosmetic. The co-channel signal is the story: 0/4 before, 4/4 is the target
// of the subtraction pass.
// ---------------------------------------------------------------------------
/// Decoded trials (of `TRIALS_PER_LEVEL`) per level, in `LEVELS_DB` order: the
/// single-pass cliff sits at about -19 dB in-band.
const PRE_CHANGE_SINGLE: [usize; 6] = [0, 0, 0, 4, 8, 8];
const FLOOR_SINGLE: [usize; 6] = [0, 0, 0, 3, 7, 7];
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

    // --- the busy band: three co-slot signals, weakest at MULTI_WEAK_SNR_DB ---
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
        let slot = scenario(
            &noise(SLOT_SAMPLES, noise_power, seed),
            noise_power,
            &multi,
            0.0, // amplitudes are already in the fixture; snr_db=0 keeps them
            0.0,
            OFFSETS_HZ[trial % OFFSETS_HZ.len()],
        );
        let texts = decode_slot(&slot);
        let hits = MULTI_EXPECTED
            .iter()
            .filter(|(marker, _)| texts.iter().any(|t| t.starts_with(marker)))
            .count();
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

    println!("\n baseline (pre-change, single pass): single {PRE_CHANGE_SINGLE:?}/8, multi-two {PRE_CHANGE_MULTI_TWO}/4, multi-all {PRE_CHANGE_MULTI_ALL}/4");
    println!(" this run: multi-two {multi_two}/4, multi-all {multi_all}/4, floors single {FLOOR_SINGLE:?}/8, multi-two {FLOOR_MULTI_TWO}/4, multi-all {FLOOR_MULTI_ALL}/4");
    println!(" false-positive decodes across the sweep: {false_positives}");
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
