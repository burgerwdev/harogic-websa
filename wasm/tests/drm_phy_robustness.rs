//! Physical-layer robustness checks on the synthesised DRM fixture.
//!
//! Purpose: separate a receiver defect from a signal condition. The live bench capture fails
//! the FAC decode (see `drm_live_fixture.rs` and `docs/en/DRM_BENCH.md`). These tests add the
//! impairments a real link has, one at a time, to the known-good fixture. The receiver must
//! survive all of them. When it does, those impairments cannot be the cause of the live
//! failure.
//!
//! The reference fixture decodes with a FAC SNR of about 36.6 dB. The live signal has about
//! 21 dB SNR.

use websa_dsp::digital::drm::DrmReceiver;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_modeB_so3_48k.f32");
const LABEL: &str = "SAN90 DRM TEST";

fn load_iq() -> Vec<f32> {
    FIXTURE
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

/// Inject `hz` of carrier offset. A live signal always carries some: the transmitter and
/// the analyzer use different clocks, and the bench capture measured -19.7 Hz.
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
fn expect_decode(name: &str, iq: &[f32], min_snr_db: f64) {
    let rx = decode(iq);
    assert!(rx.locked(), "{name}: no lock");
    assert_eq!(rx.fac_errors, 0, "{name}: FAC blocks with a CRC error");
    assert_eq!(rx.station_label.as_deref(), Some(LABEL), "{name}: station label");
    assert!(!rx.msc_frames.is_empty(), "{name}: no MSC frame");
    let snr = rx.snr_db.expect("FAC SNR");
    assert!(snr >= min_snr_db, "{name}: FAC SNR {snr:.1} dB below {min_snr_db:.1} dB");
}

#[test]
fn reference_decodes_without_impairment() {
    expect_decode("clean fixture", &load_iq(), 30.0);
}

#[test]
fn survives_a_realistic_signal_to_noise_ratio() {
    // The bench signal measures about 21 dB, so 15 dB is a margin below it.
    expect_decode("AWGN 15 dB", &add_noise(&load_iq(), 15.0), 20.0);
}

#[test]
fn survives_a_long_echo() {
    // A radiated bench loop has reflections. Mode B tolerates a delay spread of a few
    // milliseconds, so a 200 us echo at -6 dB is a mild case.
    expect_decode("echo 200 us", &add_echo(&load_iq(), 200.0, -6.0), 15.0);
}

#[test]
fn removes_a_carrier_offset_before_the_fft() {
    // Before the AFC this failed from about +20 Hz: the constellation SNR fell to -1.7 dB
    // at 20 Hz and -14.4 dB at half a carrier spacing (23.4 Hz), and every FAC block
    // failed its CRC. The bench capture measured -19.7 Hz.
    for hz in [-19.7, -10.0, -5.0, 5.0, 10.0, 19.7] {
        let iq = inject_offset(&load_iq(), hz);
        let rx = decode(&iq);
        assert!(rx.locked(), "offset {hz:+.1} Hz: no lock");
        assert_eq!(rx.fac_errors, 0, "offset {hz:+.1} Hz: FAC blocks with a CRC error");
        assert_eq!(
            rx.station_label.as_deref(),
            Some(LABEL),
            "offset {hz:+.1} Hz: station label"
        );
        let measured = rx.carrier_offset_hz;
        assert!(
            (measured - hz).abs() < 0.5,
            "offset {hz:+.1} Hz: measured {measured:+.2} Hz"
        );
    }
}

#[test]
fn survives_the_ddc_level_of_a_live_capture() {
    // A bench capture arrives far above full scale. The receiver must not depend on the level.
    let scaled: Vec<f32> = load_iq().iter().map(|v| v * 400.0).collect();
    expect_decode("level x400", &scaled, 30.0);
}
