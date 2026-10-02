//! DRM fixture regression test: the receiver must lock onto the committed Mode B /
//! SO3 (10 kHz) synthesised DRM signal, produce a tight FAC constellation, and decode
//! the FAC/SDC metadata back to the ground truth recorded in
//! `tests/fixtures/drm/manifest.json`.
//!
//! The fixture is generated externally by the DecDRM transmitter (see
//! `tests/fixtures/drm/README.md`).

use websa_dsp::digital::drm::fac::{Interleaving, MscMode, SdcMode};
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

#[test]
fn fixture_decodes_fac_and_sdc_metadata() {
    let iq = load_iq();
    let mut rx = DrmReceiver::new();
    rx.push(&iq);
    rx.run();

    // FAC: one valid block per frame, no CRC errors.
    assert_eq!(rx.fac_errors, 0, "every FAC frame must pass its CRC");
    assert_eq!(rx.facs.len(), 15, "one FAC block per 400 ms frame");

    let fac0 = &rx.facs[0];
    assert_eq!(fac0.channel.occupancy, SpectrumOccupancy::SO_3);
    assert_eq!(fac0.channel.msc_mode, MscMode::Qam64Sm);
    assert_eq!(fac0.channel.sdc_mode, SdcMode::Qam16);
    assert_eq!(fac0.channel.interleaving, Interleaving::Long);
    assert_eq!(fac0.channel.num_audio, 1);
    assert_eq!(fac0.channel.num_data, 0);
    assert_eq!(fac0.service.service_id, 0x123456);
    assert_eq!(fac0.channel.frame_index, 0);

    // The frame index cycles 0,1,2 across the super frames.
    let indices: Vec<u8> = rx.facs.iter().map(|f| f.channel.frame_index).collect();
    assert_eq!(indices, vec![0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2]);

    // SDC: every super frame decodes with a valid CRC and the station label matches.
    assert_eq!(rx.sdc_ok, 5);
    assert_eq!(rx.sdc_errors, 0);
    assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM TEST"));

    // Audio service: AAC, SBR, mono, 24 kHz.
    let audio = rx.audio.as_ref().expect("audio entity in SDC");
    assert_eq!(audio.coding, 0, "AAC");
    assert!(audio.sbr, "SBR enabled");
    assert_eq!(audio.mode, 0, "mono");
    assert_eq!(audio.sample_rate, 3, "24 kHz sample-rate code");

    // MSC information bitrate: 20.975 kbps (Mode B / SO3 / 64-QAM SM / EEP level 1).
    let bitrate = rx.msc_bitrate_kbps.expect("bitrate computed from FAC + SDC");
    assert!((bitrate - 20.975).abs() < 0.01, "MSC bitrate {bitrate:.3} kbps");
}

/// The fixture's MSC payload is a deterministic xorshift bit stream (seed 1, one
/// continuous stream across frames), so the MSC decode must recover it bit-exactly.
fn xorshift_bits(n: usize) -> Vec<u8> {
    let mut seed = 1u32;
    (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed & 1) as u8
        })
        .collect()
}

#[test]
fn fixture_decodes_msc_bit_exact() {
    let iq = load_iq();
    let mut rx = DrmReceiver::new();
    rx.push(&iq);
    rx.run();

    // 4 complete super frames → 12 multiplex frames; the depth-5 cell interleaver
    // delays by 4 frames, so 8 frames are complete.
    let n = 8390usize; // MSC information bits per multiplex frame
    assert_eq!(rx.msc_frames.len(), 8, "8 complete MSC frames expected");
    let stream = xorshift_bits(12 * n);
    for (f, bits) in rx.msc_frames.iter().enumerate() {
        assert_eq!(bits.len(), n, "MSC frame {f} length");
        assert_eq!(&bits[..], &stream[f * n..(f + 1) * n], "MSC frame {f} bit-exact");
    }
}
