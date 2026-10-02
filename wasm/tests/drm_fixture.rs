//! DRM fixture regression test: the receiver must lock onto the committed Mode B /
//! SO3 (10 kHz) synthesised DRM signal and produce a tight FAC constellation, an SNR
//! estimate and soft bits for the FAC/SDC/MSC channels.
//!
//! The fixture is generated externally by the DecDRM transmitter (see
//! `tests/fixtures/drm/README.md`) and its ground truth lives in
//! `tests/fixtures/drm/manifest.json`.

use websa_dsp::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
use websa_dsp::digital::drm::DrmReceiver;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_modeB_so3_48k.f32");

fn load_iq() -> Vec<f32> {
    let mut v = Vec::with_capacity(FIXTURE.len() / 4);
    for c in FIXTURE.chunks_exact(4) {
        v.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
    }
    v
}

#[test]
fn fixture_locks_frame_structure() {
    let iq = load_iq();
    assert_eq!(iq.len(), 576_000, "fixture must be 6 s of interleaved f32 at 48 kHz");

    let mut rx = DrmReceiver::new();
    rx.push(&iq);
    rx.run();

    assert!(rx.locked(), "receiver did not lock onto the DRM fixture");
    assert_eq!(rx.mode, Some(RobustnessMode::B));
    assert_eq!(rx.occupancy, Some(SpectrumOccupancy::SO_3));
    assert!(rx.frame_sync_score > 0.0, "time-pilot frame sync found no correlation");

    // A 6 s fixture has 15 frames (5 super frames); each frame carries 65 FAC cells
    // (4-QAM → 2 bits/cell), each super frame 322 SDC cells (16-QAM → 4 bits/cell)
    // and 2337·3+2 MSC cells (64-QAM SM → 6 bits/cell). FAC and SDC are exact: their
    // cells never fall in the final symbol, so the transmit channel filter's group
    // delay (which truncates the very last symbol) cannot remove any of them. MSC may
    // lose that one trailing symbol, so it is checked within one symbol's worth.
    assert_eq!(rx.fac_constellation.len(), 15 * 65);
    assert_eq!(rx.fac_soft_bits.len(), 15 * 65 * 2);
    assert_eq!(rx.sdc_soft_bits.len(), 5 * 322 * 4);
    let msc_bits = rx.msc_soft_bits.len();
    let msc_full = 5 * (2337 * 3 + 2) * 6;
    assert!(msc_bits <= msc_full, "MSC bits {msc_bits} > full {msc_full}");
    assert!(msc_bits >= msc_full - 6 * 207, "MSC bits {msc_bits} too far below {msc_full}");

    // Clean, noise-free fixture: the equalised FAC 4-QAM points must be tight.
    let snr = rx.snr_db.expect("FAC SNR must be computed");
    assert!(snr > 30.0, "FAC SNR {snr:.1} dB is too low for a clean fixture");
}
