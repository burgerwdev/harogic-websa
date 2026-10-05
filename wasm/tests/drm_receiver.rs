//! DRM receiver integration tests (the ported drm chain): the committed Mode B / SO3
//! synthesised fixtures must lock, decode the FAC/SDC metadata back to the ground truth in
//! `tests/fixtures/drm/manifest.json`, deframe the real AAC audio, decode the MSC bit-exactly
//! and survive the impairments a real link has (noise, an echo, a carrier offset).
//!
//! The fixtures are generated externally by the DecDRM transmitter (see
//! `tests/fixtures/drm/README.md`); the MSC payload of one fixture is a deterministic xorshift
//! stream and of the other a re-serialised DRM AAC super frame, so the decode is checked
//! against the content, not just "it decoded".

use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::fac::{Interleaving, MscMode, SdcMode};
use websa_dsp::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
use websa_dsp::digital::drm::DrmReceiver;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_modeB_so3_48k.f32");
const FIXTURE_AAC: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_modeB_so3_48k_aac.f32");
const LIVE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
const LABEL: &str = "SAN90 DRM TEST";
const LIVE_LABEL: &str = "SAN90 DRM BENCH";

fn load(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Decode a baseband vector and return the receiver state.
fn decode(iq: &[f32]) -> DrmReceiver {
    let mut rx = DrmReceiver::new();
    rx.push(iq);
    rx.run();
    rx
}

#[test]
fn fixture_locks_and_decodes_the_fac_and_sdc_metadata() {
    let iq = load(FIXTURE);
    assert_eq!(iq.len(), 576_000, "fixture must be 6 s of interleaved f32 at 48 kHz");
    let rx = decode(&iq);

    assert!(rx.locked(), "receiver did not lock onto the DRM fixture");
    assert_eq!(rx.mode, Some(RobustnessMode::B));
    assert_eq!(rx.occupancy(), SpectrumOccupancy::SO_3);

    // FAC: one valid block per frame, no CRC errors.
    assert_eq!(rx.fac_errors, 0, "every FAC frame must pass its CRC");
    assert!(rx.facs.len() >= 13, "one FAC block per 400 ms frame, got {}", rx.facs.len());
    let fac0 = &rx.facs[0];
    assert_eq!(fac0.channel.occupancy, SpectrumOccupancy::SO_3);
    assert_eq!(fac0.channel.msc_mode, MscMode::Qam64Sm);
    assert_eq!(fac0.channel.sdc_mode, SdcMode::Qam16);
    assert_eq!(fac0.channel.interleaving, Interleaving::Long);
    assert_eq!(fac0.channel.num_audio, 1);
    assert_eq!(fac0.channel.num_data, 0);
    assert_eq!(fac0.service.service_id, 0x123456);
    // The frame index advances 0 -> 1 -> 2 -> 0 across the super frames (the receiver may
    // start part-way, so only the cycle is pinned, not the first value).
    let indices: Vec<u8> = rx.facs.iter().map(|f| f.channel.frame_index).collect();
    for pair in indices.windows(2) {
        assert_eq!((pair[1] + 3 - pair[0]) % 3, 1, "frame indices must cycle: {indices:?}");
    }

    // SDC: every super frame decodes with a valid CRC and the station label matches.
    assert!(rx.sdc_ok >= 4, "one SDC block per super frame, got {}", rx.sdc_ok);
    assert_eq!(rx.station_label.as_deref(), Some(LABEL));

    // Audio service: AAC, SBR, mono, 24 kHz.
    let audio = rx.audio.as_ref().expect("audio entity in SDC");
    assert_eq!(audio.coding, 0, "AAC");
    assert!(audio.sbr, "SBR enabled");
    assert_eq!(audio.mode, 0, "mono");
    assert_eq!(audio.sample_rate, 3, "24 kHz sample-rate code");

    // Multiplex: one audio stream, EEP protection 0/1.
    let mux = rx.multiplex.as_ref().expect("multiplex entity in SDC");
    assert_eq!(mux.protection_a, 0);
    assert_eq!(mux.protection_b, 1);
    assert_eq!(mux.streams.len(), 1);

    // Clean, noise-free fixture: the FAC constellation is tight (each point near the
    // +/-1/sqrt(2) 4-QAM grid).
    assert!(!rx.fac_constellation.is_empty(), "the FAC constellation must be populated");
    let mean = rx.fac_constellation.iter().map(|(re, im)| (re * re + im * im).sqrt()).sum::<f64>()
        / rx.fac_constellation.len() as f64;
    assert!(mean > 0.5 && mean < 1.5, "FAC constellation mean |cell| {mean:.2} is not tight");
}

#[test]
fn fixture_deframes_real_aac_audio() {
    let iq = load(FIXTURE_AAC);
    assert_eq!(iq.len(), 576_000, "AAC fixture must be 6 s at 48 kHz");
    let rx = decode(&iq);

    assert!(rx.locked(), "receiver did not lock onto the AAC fixture");
    assert_eq!(rx.station_label.as_deref(), Some(LABEL));
    // 15 multiplex frames x 10 access units; the depth-5 interleaver and the estimator
    // warm-up delay the first frames, so a handful of complete multiplex frames deframe.
    assert!(
        rx.audio_access_units.len() >= 40,
        "expected >= 40 AAC access units, got {}",
        rx.audio_access_units.len()
    );
    for au in &rx.audio_access_units {
        assert!(!au.is_empty(), "empty access unit");
    }
}

/// The fixture's MSC payload is a deterministic xorshift bit stream (seed 1, one continuous
/// stream across frames), so the receiver's MSC decode must recover it bit-exactly.
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
    let rx = decode(&load(FIXTURE));
    let n = 8390usize; // MSC information bits per multiplex frame
    assert!(rx.msc_frames.len() >= 4, "complete MSC frames expected, got {}", rx.msc_frames.len());
    for bits in &rx.msc_frames {
        assert_eq!(bits.len(), n, "MSC frame length");
    }
    // The receiver's frames are a contiguous window of the deterministic stream; the window
    // offset depends on the estimator warm-up, so find it and require a bit-exact match.
    let stream = xorshift_bits(30 * n);
    let shift = (0..20).find(|&s| {
        rx.msc_frames.iter().enumerate().all(|(f, b)| {
            let off = (s + f) * n;
            off + n <= stream.len() && &b[..] == &stream[off..off + n]
        })
    });
    assert!(shift.is_some(), "the MSC bits must match the xorshift stream bit-exactly");
    eprintln!("[msc] {} frames bit-exact at window offset {}", rx.msc_frames.len(), shift.unwrap());
}

/// A deterministic uniform generator: the test crate has no random dependency.
fn pseudo_random(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
}

/// Add white noise for a target SNR in the 10 kHz DRM band.
fn add_noise(iq: &[f32], snr_db: f64) -> Vec<f32> {
    let power = iq.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / iq.len() as f64;
    let band = 10_000.0 / 48_000.0;
    let noise_power = power * band / 10f64.powf(snr_db / 10.0);
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut out = Vec::with_capacity(iq.len());
    for value in iq {
        out.push((f64::from(*value) + pseudo_random(&mut state) * (3.0 * noise_power).sqrt()) as f32);
    }
    out
}

/// Add one echo: a delayed copy of the signal, `echo_db` below it.
fn add_echo(iq: &[f32], delay_us: f64, echo_db: f64) -> Vec<f32> {
    let delay = (delay_us * 1e-6 * 48_000.0).round() as usize;
    let gain = 10f64.powf(echo_db / 20.0);
    let samples = iq.len() / 2;
    let mut out = iq.to_vec();
    for i in 0..samples.saturating_sub(delay) {
        for channel in 0..2 {
            out[i * 2 + channel] = (f64::from(iq[i * 2 + channel])
                + gain * f64::from(iq[(i + delay) * 2 + channel])) as f32;
        }
    }
    out
}

/// Inject `hz` of carrier offset. A live signal always carries some: the transmitter and the
/// analyzer use different clocks, and the bench capture measured -19.7 Hz.
fn inject_offset(iq: &[f32], hz: f64) -> Vec<f32> {
    let n = iq.len() / 2;
    let mut out = Vec::with_capacity(iq.len());
    for i in 0..n {
        let ph = 2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0;
        let (s, c) = ph.sin_cos();
        let re = f64::from(iq[i * 2]);
        let im = f64::from(iq[i * 2 + 1]);
        out.push((re * c - im * s) as f32);
        out.push((re * s + im * c) as f32);
    }
    out
}

/// The same assertions for every impairment: lock, no FAC error, label, MSC frames.
fn expect_decode(name: &str, iq: &[f32]) {
    let rx = decode(iq);
    assert!(rx.locked(), "{name}: no lock");
    assert_eq!(rx.fac_errors, 0, "{name}: FAC blocks with a CRC error");
    assert_eq!(rx.station_label.as_deref(), Some(LABEL), "{name}: station label");
    assert!(!rx.msc_frames.is_empty(), "{name}: no MSC frame");
}

#[test]
fn survives_a_realistic_signal_to_noise_ratio() {
    // The bench signal measures about 21 dB, so 15 dB is a margin below it.
    expect_decode("AWGN 15 dB", &add_noise(&load(FIXTURE), 15.0));
}

#[test]
fn survives_a_long_echo() {
    // A radiated bench loop has reflections. Mode B tolerates a delay spread of a few
    // milliseconds, so a 200 us echo at -6 dB is a mild case.
    expect_decode("echo 200 us", &add_echo(&load(FIXTURE), 200.0, -6.0));
}

#[test]
fn removes_a_carrier_offset_before_the_fft() {
    for hz in [-19.7, -10.0, -5.0, 5.0, 10.0, 19.7] {
        expect_decode(&format!("offset {hz:+.1} Hz"), &inject_offset(&load(FIXTURE), hz));
    }
}

// ---------------------------------------------------------------------------------------------
// The committed live bench capture (the baseband the browser DSP worker receives): it arrives at
// the channelizer's rate, starts mid-super-frame and carries real level and noise.
// ---------------------------------------------------------------------------------------------

/// Feed the capture through the pipeline's own rate conversion, then the receiver.
fn decode_live() -> DrmReceiver {
    let iq = load(LIVE);
    let mut resampler = ComplexResampler::new(48_828.125, 48_000.0);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);
    let mut rx = DrmReceiver::new();
    rx.push(&converted);
    rx.run();
    rx
}

fn live_baseband() -> Vec<f32> {
    let iq = load(LIVE);
    let mut resampler = ComplexResampler::new(48_828.125, 48_000.0);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);
    converted
}

#[test]
fn live_capture_locks_and_decodes_metadata() {
    let rx = decode_live();
    assert!(rx.locked(), "the receiver did not lock on the live capture");
    assert_eq!(rx.mode, Some(RobustnessMode::B), "robustness mode");
    assert_eq!(rx.occupancy(), SpectrumOccupancy::SO_3, "occupancy");
    assert_eq!(rx.fac_errors, 0, "every FAC block must pass its CRC");
    assert!(rx.facs.len() >= 10, "decoded {} FAC blocks, expected >= 10", rx.facs.len());
    let fac = rx.facs.last().expect("a decoded FAC block");
    assert_eq!(fac.channel.msc_mode, MscMode::Qam64Sm);
    assert_eq!(fac.channel.sdc_mode, SdcMode::Qam16);
    assert_eq!(rx.station_label.as_deref(), Some(LIVE_LABEL), "station label from the SDC");
    assert!(rx.sdc_ok > 0, "SDC blocks with a valid CRC");
    assert!(rx.multiplex.is_some(), "multiplex description decoded");
}

/// The decode window reads the receiver through the plugin: a plugin that returns its lines
/// from `process_iq` but does not keep them makes a locked receiver show nothing, which is how
/// the bench readout stayed empty while the offline tests passed.
#[test]
fn live_capture_reports_lines_through_the_plugin() {
    use websa_dsp::digital::drm::DrPlugin;
    use websa_dsp::plugin::DigitalDemodulator;

    let converted = live_baseband();
    let mut plugin = DrPlugin::new(48_000.0);
    let mut lines = Vec::new();
    for block in converted.chunks(3248 * 2) {
        let reported = plugin.process_iq(block);
        if !reported.is_empty() {
            lines = reported;
            break;
        }
    }
    assert!(!lines.is_empty(), "the plugin reported no lines after the capture");
    assert!(lines.iter().any(|line| line.contains("DRM")), "no DRM line in {lines:?}");
    let decoded = plugin.decoded();
    assert!(
        decoded.iter().any(|(text, _)| text == &lines[0]),
        "decoded() returned {decoded:?}, the readout got {lines:?}"
    );
}

#[test]
fn live_capture_locks_from_a_short_buffer() {
    let converted = live_baseband();
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
    let lock_seconds = locked_at as f64 / 48_000.0;
    assert!(lock_seconds < 3.0, "locked after {lock_seconds:.1} s of baseband");
    assert_eq!(rx.mode, Some(RobustnessMode::B), "mode at the lock");
    assert!(!rx.facs.is_empty(), "no FAC block at the lock");

    let full = decode_live();
    assert_eq!(full.station_label.as_deref(), Some(LIVE_LABEL), "station label on the whole capture");
    assert!(!full.msc_frames.is_empty(), "no MSC frame on the whole capture");
    assert!(!full.audio_access_units.is_empty(), "no audio access unit on the whole capture");
}

#[test]
fn live_capture_streams_audio_after_the_lock() {
    let converted = live_baseband();
    let mut rx = DrmReceiver::new();
    for block in converted.chunks(3248 * 2) {
        rx.push(block);
        rx.run();
    }
    assert!(rx.locked(), "the receiver did not lock on the streamed capture");
    assert!(!rx.msc_frames.is_empty(), "the streaming passes produced no MSC frames");
    assert!(!rx.audio_access_units.is_empty(), "the streaming passes produced no audio access units");
}
