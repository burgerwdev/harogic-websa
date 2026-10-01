//! Subtraction, isolated: two transmissions one pass cannot both have.
//!
//! Both scenarios put a weak signal *above* the single-signal sensitivity cliff (`ft8_snr_sweep`
//! measures it at about -19 dB in-band; here the weak signal sits at -16 dB, where it decodes
//! reliably *alone*) and underneath a strong transmission at 0 dB:
//!
//! 1. co-channel: the weak burst shares the strong one's exact audio frequency, 0.8 s later in the
//!    slot. Their waterfall bins superpose, so one pass decodes the strong and loses the weak.
//! 2. same frame, 12.5 Hz apart (two sub-bins): different frequencies, same time — the sync peaks
//!    merge and the weaker extraction is corrupted by the neighbour's skirt.
//!
//! The contract is that both scenarios report both messages: the strong one first, the weak one
//! once the subtracted re-search can see it. The scenario harness is `tests/common`,
//! deterministic, so this is a standing regression test, not a demo.
//!
//!     cd wasm && cargo test --release --test ft8_subtract -- --nocapture
use websa_dsp::digital::ft8::Ft8Decoder;

mod common;
use common::{at_amp_db, insert, load_iq, noise, scenario, RATE, SLOT_SAMPLES, BAND_HZ};

const FIRST: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
const SECOND: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_second_iq.bin");
const FIRST_TEXT: &str = "CQ JO1WKO";
const SECOND_TEXT: &str = "K1ABC";
/// Strong at 0 dB in-band, weak at -16 dB: above the single-signal cliff, low enough that the
/// neighbour's presence is what kills it.
const WEAK_SNR_DB: f64 = -16.0;
const TRIALS: usize = 3;
/// Trial frequency offsets: the pair moves together, so the separation stays what it is. The
/// offsets are chosen on the waterfall's sub-bin grid (3.125 Hz) on purpose: a signal halfway
/// between sub-bins splits its sync peak and loses several dB of contrast — a real frequency-
/// granularity limitation of the 2x-oversampled waterfall (raising FREQ_OSR backfires: the FFT
/// frame grows to 4 symbols and the Hann window dilutes the tone), to be solved with time-domain
/// refinement, not here. These scenarios isolate the subtraction, not that limitation.
const OFFSETS_HZ: [f64; 3] = [0.0, -3.125, 6.25];

fn decode_texts(iq: &[f32]) -> Vec<String> {
    let mut decoder = Ft8Decoder::new(RATE);
    decoder.push_iq(iq);
    decoder
        .decode()
        .into_iter()
        .map(|m| m.text)
        .collect::<Vec<_>>()
}

fn run_pair(label: &str, second_hz_off: f64, weak_insert_s: f64) {
    let first = load_iq(FIRST);
    let second = load_iq(SECOND);
    // Noise at unit in-band power; the scenario call places the strong signal at 0 dB.
    let noise_iq = noise(SLOT_SAMPLES, RATE / BAND_HZ, 0x5EED_0001);
    println!("\n {label}, weak at {WEAK_SNR_DB} dB in-band, {TRIALS} trials:");
    for trial in 0..TRIALS {
        let hz = OFFSETS_HZ[trial];
        let mut slot = scenario(&noise_iq, 1.0, &first, 0.0, 0.64, hz);
        let second_shifted = common::shift(&second, second_hz_off);
        insert(&mut slot, &at_amp_db(&second_shifted, -16.0), weak_insert_s, hz);
        let texts = decode_texts(&slot);
        let got_first = texts.iter().any(|t| t.starts_with(FIRST_TEXT));
        let got_second = texts.iter().any(|t| t.starts_with(SECOND_TEXT));
        println!("  trial {trial}: first={got_first} second={got_second} -> {texts:?}");
        assert!(got_first, "{label}: the strong transmission must decode");
        assert!(
            got_second,
            "{label}: the weak transmission must decode once the strong one is subtracted"
        );
    }
}

#[test]
fn the_weak_transmission_survives_subtraction() {
    if cfg!(debug_assertions) {
        return;
    }
    run_pair("co-channel pair (same frequency, 0.8 s later)", 0.0, 0.8);
    run_pair("same-frame pair (12.5 Hz apart)", 12.5, 0.64);
}
