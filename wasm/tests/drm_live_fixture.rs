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

/// The decode window reads the receiver through the plugin, not through the receiver type:
/// `websa_dsp_demod_message_at` calls `DigitalDemodulator::decoded`. A plugin that returns its
/// lines from `process_iq` but does not keep them makes a locked receiver show nothing, which
/// is exactly how the bench readout stayed empty while the offline tests passed.
#[test]
fn live_capture_reports_lines_through_the_plugin() {
    use websa_dsp::digital::drm::DrPlugin;
    use websa_dsp::plugin::DigitalDemodulator;

    let iq = load_iq();
    let mut resampler = ComplexResampler::new(PUBLISHED_RATE, CORE_RATE);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);
    let mut plugin = DrPlugin::new(CORE_RATE);
    let mut lines = Vec::new();
    for block in converted.chunks(3248 * 2) {
        let reported = plugin.process_iq(block);
        if !reported.is_empty() {
            lines = reported;
            break;
        }
    }
    assert!(!lines.is_empty(), "the plugin reported no lines after the capture");
    assert!(
        lines.iter().any(|line| line.contains("DRM")),
        "no DRM line in {lines:?}"
    );
    let decoded = plugin.decoded();
    assert!(
        decoded.iter().any(|(text, _)| text == &lines[0]),
        "decoded() returned {decoded:?}, the readout got {lines:?}"
    );
}

/// The worker feeds the receiver one baseband block at a time, so the lock has to arrive from
/// a short buffer: the FAC and the SDC need about one super frame. The MSC and the audio need
/// about three, which is why a caller that holds the whole capture must get them from the same
/// pass, and a streaming caller sees the station first.
#[test]
fn live_capture_locks_from_a_short_buffer() {
    let iq = load_iq();
    let mut resampler = ComplexResampler::new(PUBLISHED_RATE, CORE_RATE);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);

    let mut rx = DrmReceiver::new();
    let mut locked_at = None;
    for (index, block) in converted.chunks(3248 * 2).enumerate() {
        rx.push(block);
        rx.run();
        if rx.locked() {
            locked_at = Some(index * 3248);
            break;
        }
    }
    let locked_at = locked_at.expect("the receiver did not lock on the block feed");
    // The index counts input samples, which the resampler converts one for one within 0.02 %.
    let lock_seconds = locked_at as f64 / CORE_RATE;
    assert!(lock_seconds < 3.0, "locked after {lock_seconds:.1} s of baseband");
    assert_eq!(rx.station_label.as_deref(), Some(LABEL), "station label at the lock");
    assert!(!rx.facs.is_empty(), "no FAC block at the lock");

    // A caller with the whole capture gets the MSC and the audio from one pass.
    let mut full = DrmReceiver::new();
    full.push(&converted);
    full.run();
    assert!(full.locked(), "no lock on the whole capture");
    assert!(!full.msc_frames.is_empty(), "no MSC frame on the whole capture");
    assert!(!full.audio_access_units.is_empty(), "no audio access unit on the whole capture");
}

/// The worker streams forever, so the post-lock passes must produce the audio: the
/// long MSC interleaver spans five 400 ms frames, which is why the deinterleaver and
/// the carrier-offset corrections have to persist across passes. This test failed
/// twice on the way to DRM audio: a deinterleaver recreated per pass spent its whole
/// 3 s window on fill (au stayed 0), and a carrier-offset correction that replaced
/// instead of accumulated under-rotated every sample after the lock (the pass SNR
/// fell from 20 dB to 1.7 dB frame by frame).
#[test]
fn live_capture_streams_audio_after_the_lock() {
    let iq = load_iq();
    let mut resampler = ComplexResampler::new(PUBLISHED_RATE, CORE_RATE);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);

    let mut rx = DrmReceiver::new();
    for block in converted.chunks(3248 * 2) {
        rx.push(block);
        rx.run();
    }
    assert!(rx.locked(), "the receiver did not lock on the streamed capture");
    assert!(rx.snr_db.unwrap_or(0.0) > 10.0, "FAC SNR collapsed across the passes");
    assert!(
        !rx.audio_access_units.is_empty(),
        "the streaming passes produced no audio access units"
    );
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
