//! Python-vs-WASM cross-check for the analog demodulators.
//!
//! `tools/gen_dsp_fixtures.py` runs the Python reference (`web_sa/demod/demod.py::AnalogDemod`,
//! the DSP the browser is replacing) over a committed complex baseband and writes the audio it
//! produced. These tests run the Rust kernels over the same bytes and require agreement within the
//! manifest's tolerance, plus the signal-level property the mode exists for (the tone comes back at
//! the right frequency). Keeping both halves matters: a kernel that agrees with the reference but
//! demodulates nothing would be a faithful port of nothing.
use websa_dsp::analog::{build, AUDIO_RATE};
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

const USB_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_usb_iq.bin");
const USB_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_usb.bin");
const LSB_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_lsb_iq.bin");
const LSB_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_lsb.bin");
const CW_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_cw_iq.bin");
const CW_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_cw.bin");
const MANIFEST: &str = include_str!("../../tests/fixtures/dsp/manifest.json");

const FS: f64 = 96_000.0;
const PITCH: f64 = 700.0;

fn tolerance() -> f64 {
    let key = "\"tolerance\":";
    let start = MANIFEST.find(key).expect("manifest has a tolerance") + key.len();
    let rest = &MANIFEST[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    rest[..end].trim().parse().expect("tolerance is a number")
}

fn as_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|quad| f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]))
        .collect()
}

fn rms(values: &[f32]) -> f64 {
    (values.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

/// Hann-windowed amplitude at one frequency (coherent gain corrected).
fn amplitude_at(audio: &[f32], rate: f64, hz: f64) -> f64 {
    let n = audio.len() as f64;
    let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
    for (k, value) in audio.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * core::f64::consts::PI * k as f64 / n).cos();
        let ph = 2.0 * core::f64::consts::PI * hz * k as f64 / rate;
        re += *value as f64 * w * ph.cos();
        im -= *value as f64 * w * ph.sin();
        wsum += w;
    }
    (re * re + im * im).sqrt() / wsum
}

/// Dominant audio frequency, searched around `expect`.
fn recovered_hz(audio: &[f32], expect: f64) -> f64 {
    let mut best = (expect, f64::MIN);
    let mut hz = expect - 60.0;
    while hz <= expect + 60.0 {
        let level = amplitude_at(audio, AUDIO_RATE, hz);
        if level > best.1 {
            best = (hz, level);
        }
        hz += 0.5;
    }
    best.0
}

/// Run one mode over the fixture baseband, compare with the reference and check the tone.
fn cross_check(mode: &str, if_bw: f64, iq_bytes: &[u8], audio_bytes: &[u8], expect_hz: f64) {
    let iq = as_f32(iq_bytes);
    let reference = as_f32(audio_bytes);

    let mut config = DemodConfig::new(FS, if_bw);
    config.pitch = PITCH;
    let mut demod = build(mode, config).unwrap_or_else(|| panic!("{mode} must build"));
    let mut audio = Vec::new();
    demod.process_into(&iq, &mut audio);

    assert_eq!(audio.len(), reference.len(), "{mode}: audio length differs from the reference");
    let mut worst = 0.0_f64;
    let mut at = 0;
    for (index, (got, want)) in audio.iter().zip(reference.iter()).enumerate() {
        let diff = (*got as f64 - *want as f64).abs();
        if diff > worst {
            worst = diff;
            at = index;
        }
    }
    let tol = tolerance();
    assert!(
        worst <= tol,
        "{mode}: worst difference {worst:e} at sample {at} ({} vs {}) exceeds {tol:e}",
        audio[at],
        reference[at]
    );
    println!("{mode}: worst difference vs the Python reference {worst:e}");

    // The signal-level half: the tone must come back where the mode promises it.
    let found = recovered_hz(&audio, expect_hz);
    assert!(
        (found - expect_hz).abs() <= 1.0,
        "{mode}: recovered {found} Hz, expected {expect_hz} Hz"
    );
    assert!(rms(&audio) > 0.05, "{mode}: the audio level is too low ({})", rms(&audio));
}

#[test]
fn usb_matches_the_python_reference_and_recovers_its_tone() {
    cross_check("usb", 2_400.0, USB_IQ, USB_AUDIO, 1_200.0);
}

#[test]
fn lsb_matches_the_python_reference_and_recovers_its_tone() {
    cross_check("lsb", 2_400.0, LSB_IQ, LSB_AUDIO, 1_200.0);
}

#[test]
fn cw_matches_the_python_reference_and_recovers_its_sidetone() {
    cross_check("cw", 500.0, CW_IQ, CW_AUDIO, 700.0);
}

#[test]
fn the_cross_check_covers_every_demod_fixture_in_the_manifest() {
    // Guards against a fixture being added without a test: every `demod` entry names files that
    // must exist (they are include_bytes! above) and a mode the registry implements.
    for mode in ["usb", "lsb", "cw"] {
        let entry = format!("\"mode\": \"{mode}\"");
        assert!(MANIFEST.contains(&entry), "manifest is missing the {mode} demod case");
    }
    assert_eq!(tolerance(), 1.0e-5);
    assert_eq!(AUDIO_RATE, 48_000.0);
}
