//! Live-signal regression: the receiver must decode the DRM bench capture.
//!
//! The file is the baseband the browser DSP worker receives. It comes from the bench loop
//! (DecDRM transmitter → PlutoSDR → SAN-90), so it differs from the synthesised fixtures in
//! three ways that matter:
//!
//!   * it arrives at the channelizer's rate (48828.125 Hz published, 48833.85 Hz measured),
//!     not at the 48 kHz the DRM core works at;
//!   * it starts at an arbitrary point of the transmission super frame;
//!   * it carries real level, noise and channel conditions.
//!
//! The external Dream decoder decodes the same samples (see
//! `tests/fixtures/drm/live_manifest.json`). The expectations below therefore describe the
//! bench signal, not a synthetic one.
//!
//! This test failed until three fixes landed, and each failure was the reproduction for
//! the next one:
//!
//!   1. the rate: the capture does not lock at its delivered rate, because the DRM core is
//!      fixed at 48 kHz;
//!   2. the carrier offset: the capture sits at +121 Hz, which is 2.58 carrier spacings.
//!      The guard correlation only sees the fraction of that (it wraps every 47.7 Hz), so
//!      the whole carriers must come from the detected band edges;
//!   3. the super-frame phase: the capture starts in the middle of a super frame, and the
//!      SDC and MSC cells move with that phase. The receiver tries each phase and keeps the
//!      one whose SDC block passes its CRC.
//!
//! See `docs/en/DRM_BENCH.md` for the measurements.

use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::fac::{MscMode, SdcMode};
use websa_dsp::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
use websa_dsp::digital::drm::DrmReceiver;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
/// The rate the backend publishes in the IQBF header for this capture.
const PUBLISHED_RATE: f64 = 48_828.125;
/// The rate the DRM core works at.
const CORE_RATE: f64 = 48_000.0;
/// The station label the bench transmitter sends (tools/drm_bench/station_iq.toml).
const LABEL: &str = "SAN90 DRM BENCH";

fn load_iq() -> Vec<f32> {
    FIXTURE
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Feed the capture through the pipeline's own rate conversion, then the receiver.
fn decode_live() -> DrmReceiver {
    let iq = load_iq();
    let mut resampler = ComplexResampler::new(PUBLISHED_RATE, CORE_RATE);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);
    let mut rx = DrmReceiver::new();
    rx.push(&converted);
    rx.run();
    rx
}

#[test]
fn live_capture_locks_and_decodes_metadata() {
    let rx = decode_live();

    assert!(rx.locked(), "the receiver did not lock on the live capture");
    assert_eq!(rx.mode, Some(RobustnessMode::B), "robustness mode");
    assert_eq!(rx.occupancy, Some(SpectrumOccupancy::SO_3), "occupancy");

    let snr = rx.snr_db.expect("FAC SNR must be measured");
    assert!(snr > 10.0, "FAC SNR {snr:.1} dB is too low for a 21 dB bench signal");

    assert_eq!(rx.fac_errors, 0, "every FAC block must pass its CRC");
    assert!(rx.facs.len() >= 10, "decoded {} FAC blocks, expected >= 10", rx.facs.len());

    // FAC: one audio service, no data service, 64-QAM MSC with 16-QAM SDC (the bench config).
    let fac = rx.facs.last().expect("a decoded FAC block");
    assert_eq!(fac.channel.msc_mode, MscMode::Qam64Sm);
    assert_eq!(fac.channel.sdc_mode, SdcMode::Qam16);

    assert_eq!(rx.station_label.as_deref(), Some(LABEL), "station label from the SDC");
    assert!(rx.sdc_ok > 0, "SDC blocks with a valid CRC");
    assert!(rx.multiplex.is_some(), "multiplex description decoded");
    assert!(rx.msc_bitrate_kbps.unwrap_or(0.0) > 20.0, "MSC bitrate from the FAC/SDC");
}

#[test]
fn live_capture_decodes_msc_and_audio() {
    let rx = decode_live();
    assert!(
        !rx.msc_frames.is_empty(),
        "no MSC frame passed its CRC (msc bits: {})",
        rx.msc_soft_bits.len()
    );
    assert!(
        !rx.audio_access_units.is_empty(),
        "no audio access unit came out of the MSC deframing"
    );
}
