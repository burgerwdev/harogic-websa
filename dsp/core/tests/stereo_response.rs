//! Keep WFM L−R level matched to mono through the 15 kHz broadcast audio band.
use websa_dsp::analog::build;
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

fn amplitude(x: &[f32], hz: f64, rate: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let phase = core::f64::consts::TAU * hz * i as f64 / rate;
        re += f64::from(v) * phase.cos();
        im -= f64::from(v) * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / n
}

#[test]
fn stereo_difference_tracks_mono_to_15khz() {
    let fs = 300_480.769_230_769_25;
    let out_rate = 48_000.0;
    for (hz, min_ratio) in [
        (1_000.0, 0.95), (5_000.0, 0.94), (8_000.0, 0.92),
        (12_000.0, 0.90), (15_000.0, 0.82),
    ] {
        let mut demod = build("wfm", DemodConfig::new(fs, 180_000.0), out_rate).unwrap();
        let mut iq = Vec::with_capacity(4_800 * 2);
        let mut audio = Vec::new();
        let mut side = Vec::new();
        let mut phase = 0.0_f64;
        for k in 0..(fs * 0.8) as usize {
            let t = k as f64 / fs;
            let tone = (core::f64::consts::TAU * hz * t).cos();
            let deviation = 8_000.0 * tone
                + 8_000.0 * tone * (core::f64::consts::TAU * 38_000.0 * t).cos()
                + 6_750.0 * (core::f64::consts::TAU * 19_000.0 * t).sin();
            phase = (phase + core::f64::consts::TAU * deviation / fs) % core::f64::consts::TAU;
            iq.push(phase.cos() as f32);
            iq.push(phase.sin() as f32);
            if iq.len() == 4_800 * 2 {
                let mut out = Vec::new();
                demod.process_into(&iq, &mut out);
                audio.extend_from_slice(&out);
                side.extend_from_slice(&demod.take_side());
                iq.clear();
            }
        }
        let n = audio.len().min(side.len());
        let start = n / 2;
        let m = amplitude(&audio[start..n], hz, out_rate);
        let s = amplitude(&side[start..n], hz, out_rate);
        let ratio = s / m;
        assert!(demod.stereo_detected(), "pilot must lock at {hz:.0} Hz programme audio");
        assert!(
            ratio > min_ratio && ratio < 1.08,
            "at {hz:.0} Hz the L−R/mono gain must preserve separation: {ratio:.3}"
        );
    }
}
