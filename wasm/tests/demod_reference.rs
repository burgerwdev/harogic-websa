//! Python-vs-WASM cross-check for the analog demodulators, every mode in the registry.
//!
//! `tools/fixtures/gen_dsp_fixtures.py` runs the Python reference (`web_sa/demod/demod.py::AnalogDemod`, the
//! DSP the browser is replacing) over a committed complex baseband for each mode and writes the
//! audio it produced. These tests run the Rust kernels over the same bytes and require agreement
//! within the manifest's tolerance, plus the signal-level property the mode exists for (the tone
//! comes back at the right frequency). Keeping both halves matters: a kernel that agrees with the
//! reference but demodulates nothing would be a faithful port of nothing.
//!
//! The table is the coverage contract: adding a mode to the registry without a fixture here leaves
//! the previous `every_mode_in_the_registry_has_a_reference_fixture` test failing.
use websa_dsp::analog::{build, DEFAULT_AUDIO_RATE as AUDIO_RATE};
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

/// Build a demodulator that produces audio at the reference rate (every fixture is 48 kHz).
fn build_at(id: &str, config: DemodConfig) -> Option<websa_dsp::analog::AnalogDemod> {
    build(id, config, AUDIO_RATE)
}

const AM_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_am_iq.bin");
const AM_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_am.bin");
const DSB_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_dsb_iq.bin");
const DSB_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_dsb.bin");
const USB_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_usb_iq.bin");
const USB_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_usb.bin");
const LSB_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_lsb_iq.bin");
const LSB_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_lsb.bin");
const CW_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_cw_iq.bin");
const CW_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_cw.bin");
const NFM_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_nfm_iq.bin");
const NFM_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_nfm.bin");
const WFM_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_wfm_iq.bin");
const WFM_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_wfm.bin");
const PM_IQ: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_pm_iq.bin");
const PM_AUDIO: &[u8] = include_bytes!("../../tests/fixtures/dsp/demod_pm.bin");
const MANIFEST: &str = include_str!("../../tests/fixtures/dsp/manifest.json");

const FS: f64 = 96_000.0;
const PITCH: f64 = 700.0;

struct Case {
    mode: &'static str,
    if_bw: f64,
    expect_hz: f64,
    iq: &'static [u8],
    audio: &'static [u8],
    /// Bound on the absolute difference against the reference (see the fixture manifest).
    ///
    /// The three modes whose detector is followed by the one-pole DC blocker are bounded two orders
    /// looser, and deliberately so: that blocker's noise gain is `1/(1-0.9995) = 2000`, so the ~1e-7
    /// difference between this crate's FIR and numpy's convolution — which the modes *without* a
    /// blocker show directly at 1.8e-7 — is amplified to ~1e-3 there. The measured worst is 7.4e-4;
    /// the bound sits just above it and the reason is recorded in the manifest, not hidden.
    tolerance: f64,
}

const DIRECT: f64 = 1.0e-5;
const DC_BLOCKED: f64 = 2.0e-3;

const CASES: &[Case] = &[
    Case { mode: "am", if_bw: 12_000.0, expect_hz: 1_000.0, iq: AM_IQ, audio: AM_AUDIO, tolerance: DC_BLOCKED },
    Case { mode: "dsb", if_bw: 12_000.0, expect_hz: 1_000.0, iq: DSB_IQ, audio: DSB_AUDIO, tolerance: DC_BLOCKED },
    Case { mode: "usb", if_bw: 2_400.0, expect_hz: 1_200.0, iq: USB_IQ, audio: USB_AUDIO, tolerance: DIRECT },
    Case { mode: "lsb", if_bw: 2_400.0, expect_hz: 1_200.0, iq: LSB_IQ, audio: LSB_AUDIO, tolerance: DIRECT },
    Case { mode: "cw", if_bw: 500.0, expect_hz: 700.0, iq: CW_IQ, audio: CW_AUDIO, tolerance: DIRECT },
    Case { mode: "nfm", if_bw: 12_000.0, expect_hz: 1_000.0, iq: NFM_IQ, audio: NFM_AUDIO, tolerance: DIRECT },
    Case { mode: "wfm", if_bw: 180_000.0, expect_hz: 1_000.0, iq: WFM_IQ, audio: WFM_AUDIO, tolerance: DIRECT },
    Case { mode: "pm", if_bw: 12_000.0, expect_hz: 1_000.0, iq: PM_IQ, audio: PM_AUDIO, tolerance: DC_BLOCKED },
];

/// The top-level (DDC-stage) tolerance from the manifest.
///
/// It is matched at the start of a line: the demod entries carry their own `tolerance` field, and a
/// plain first-occurrence search picked one of those up instead (the JSON is dumped with an indent,
/// so a top-level key starts one space in).
fn tolerance() -> f64 {
    let key = "\n \"tolerance\": ";
    let start = MANIFEST.find(key).expect("manifest has a top-level tolerance") + key.len();
    let rest = &MANIFEST[start..];
    let end = rest
        .find(|c: char| c == ',' || c == '\n')
        .unwrap_or(rest.len());
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

/// Compare one mode with its reference; returns the worst difference or a description of what is
/// wrong. Reporting every mode (instead of stopping at the first failure) is what makes a
/// divergence diagnosable: one run shows which modes agree and by how much.
fn cross_check(case: &Case) -> Result<f64, String> {
    let iq = as_f32(case.iq);
    let reference = as_f32(case.audio);

    let mut config = DemodConfig::new(FS, case.if_bw);
    config.pitch = PITCH;
    let Some(mut demod) = build_at(case.mode, config) else {
        return Err(format!("{}: no kernel", case.mode));
    };
    let mut audio = Vec::new();
    demod.process_into(&iq, &mut audio);

    if audio.len() != reference.len() {
        return Err(format!("{}: audio length {} vs {}", case.mode, audio.len(), reference.len()));
    }
    let mut worst = 0.0_f64;
    let mut at = 0;
    for (index, (got, want)) in audio.iter().zip(reference.iter()).enumerate() {
        let diff = (*got as f64 - *want as f64).abs();
        if diff > worst {
            worst = diff;
            at = index;
        }
    }
    println!("{}: worst difference vs the reference {worst:e}", case.mode);
    println!("{}:   at sample {at}: {} vs {}", case.mode, audio[at], reference[at]);
    if worst > case.tolerance {
        return Err(format!(
            "{}: worst difference {worst:e} exceeds its bound {:e}",
            case.mode, case.tolerance
        ));
    }
    let found = recovered_hz(&audio, case.expect_hz);
    if (found - case.expect_hz).abs() > 1.0 {
        return Err(format!("{}: recovered {found} Hz, expected {} Hz", case.mode, case.expect_hz));
    }
    if rms(&audio) <= 0.05 {
        return Err(format!("{}: audio level too low ({})", case.mode, rms(&audio)));
    }
    Ok(worst)
}

#[test]
fn every_analog_mode_matches_the_python_reference_and_recovers_its_tone() {
    let mut failures = Vec::new();
    let mut worst_overall = 0.0_f64;
    for case in CASES {
        match cross_check(case) {
            Ok(worst) => worst_overall = worst_overall.max(worst),
            Err(problem) => failures.push(problem),
        }
    }
    println!(
        "{} modes cross-checked against their per-mode bounds, worst {worst_overall:e}",
        CASES.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn every_mode_in_the_registry_has_a_reference_fixture() {
    // Coverage contract: a mode added to the registry without a fixture (and therefore without a
    // comparison) fails here instead of quietly shipping unverified.
    for descriptor in websa_dsp::plugin::ANALOG_PLUGINS.iter().filter(|p| p.implemented) {
        assert!(
            CASES.iter().any(|case| case.mode == descriptor.id),
            "{} is implemented but has no Python reference fixture in this test",
            descriptor.id
        );
    }
    assert_eq!(CASES.len(), websa_dsp::plugin::ANALOG_PLUGINS.len());
    // The DDC stages (a separate comparison) are held to the strict bound; only the DC-blocked
    // detectors get the wider one, and only because of their amplifier gain.
    assert_eq!(tolerance(), DIRECT);
    for case in CASES.iter().filter(|case| case.tolerance > DIRECT) {
        assert_eq!(case.tolerance, DC_BLOCKED, "{}", case.mode);
    }
    assert_eq!(AUDIO_RATE, 48_000.0);
}
