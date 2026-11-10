//! Independent encoded BT=2 GFSK calibration; these tests never claim decode success.
use std::f64::consts::{PI, TAU};
use websa_dsp::digital::ft8::{
    measure_decoded_snr,
    tables::{COSTAS, GRAY},
    SnrEstimate,
};
mod common;
use common::Rng;

fn bits() -> [u8; 174] {
    include_str!("../../../tests/fixtures/ft8/manifest.json")
        .split("\"codeword_bits\": [")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap()
        .split(',')
        .map(|s| s.trim().parse::<u8>().unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn tones() -> Vec<f64> {
    let bits = bits();
    let mut tones = Vec::new();
    for block in 0..3 {
        tones.extend(COSTAS.iter().map(|&t| f64::from(t)));
        if block < 2 {
            for k in 0..29 {
                let i = block * 87 + k * 3;
                tones.push(f64::from(
                    GRAY[(bits[i] * 4 + bits[i + 1] * 2 + bits[i + 2]) as usize],
                ));
            }
        }
    }
    tones
}

fn signal(rate: f64, frequency: f64, amplitude: f64, fade: bool) -> Vec<f32> {
    let tones = tones();
    let sym = (rate * 0.16).round() as usize;
    let mut deviations: Vec<_> = tones
        .iter()
        .flat_map(|&t| std::iter::repeat_n(t, sym))
        .collect();
    let sigma = sym as f64 * 2.0_f64.ln().sqrt() / (4.0 * PI);
    let radius = (sigma * 5.0).ceil() as i64;
    let gaussian: Vec<_> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
        .collect();
    let norm: f64 = gaussian.iter().sum();
    let mut cumulative = 0.0;
    for (j, g) in gaussian.iter().enumerate() {
        cumulative += g / norm;
        let offset = j as i64 - radius;
        for k in 1..tones.len() {
            let index = (k * sym) as i64 + offset;
            deviations[index as usize] +=
                (tones[k] - tones[k - 1]) * (cumulative - if offset >= 0 { 1.0 } else { 0.0 });
        }
    }
    let fades: Vec<_> = (0..79)
        .map(|k| {
            if fade {
                0.6 + 0.4 * (k as f64 * 0.4).cos()
            } else {
                1.0
            }
        })
        .collect();
    let norm = (fades.iter().map(|v| v * v).sum::<f64>() / 79.0).sqrt();
    let mut phase = 0.0_f64;
    deviations
        .iter()
        .enumerate()
        .flat_map(|(i, &tone)| {
            let amplitude = amplitude * fades[i / sym] / norm;
            let value = [
                (amplitude * phase.cos()) as f32,
                (amplitude * phase.sin()) as f32,
            ];
            phase = (phase + TAU * (frequency + tone * 6.25) / rate).rem_euclid(TAU);
            value
        })
        .collect()
}

fn input(rate: f64, db: f64, real: bool, frequency: f64, fade: bool, seed: u64) -> Vec<f32> {
    let amplitude = 0.002;
    // Real sine P=A²/2 and one-sided N0=2*sigma²/Fs; complex P=A²
    // and N0=2*sigma²/Fs. The real case therefore needs half the variance.
    let sigma = (amplitude * amplitude * rate
        / ((if real { 4.0 } else { 2.0 }) * 2500.0 * 10_f64.powf(db / 10.0)))
    .sqrt();
    let mut iq = signal(rate, frequency, amplitude, fade);
    let mut rng = Rng::new(seed);
    for pair in iq.chunks_exact_mut(2) {
        pair[0] += (sigma * rng.normal()) as f32;
        if real {
            pair[1] = 0.0;
        } else {
            pair[1] += (sigma * rng.normal()) as f32;
        }
    }
    iq
}

fn baseline() -> bool {
    std::env::var_os("WEBSA_FT8_BASELINE").is_some()
}
fn report(name: &str, expected: f64, snr: &SnrEstimate) {
    println!(
        "SNR_CASE,{name},{expected},{},{:?}",
        snr.value.map(|v| v.to_string()).unwrap_or("null".into()),
        snr
    );
}

#[test]
fn independent_gfsk_awgn_matrix_and_common_scaling() {
    for rate in [8000.0, 12000.0, 48000.0] {
        for real in [false, true] {
            for db in [-30.0, -27.0, -24.0, -20.0, -10.0, 0.0, 9.0, 15.0, 20.0] {
                let iq = input(rate, db, real, 1234.57, false, 0x6110);
                let measured = measure_decoded_snr(&iq, rate, &bits(), 1234.57, 0.0);
                report(&format!("awgn-{rate}-{real}-{db}"), db, &measured);
                if !baseline() {
                    let value = measured.value.expect("finite known signal and noise");
                    assert_eq!(measured.value_kind, "estimate");
                    assert_eq!(measured.reason, None);
                    if db >= -20.0 {
                        assert_eq!(measured.quality, "good", "{measured:?}");
                    }
                    assert!(
                        (value - db).abs() <= if db < -20.0 { 3.0 } else { 1.5 },
                        "{rate}/{real}/{db}: {measured:?}"
                    );
                    if db == -10.0 {
                        for scale in [0.01, 100.0] {
                            let scaled: Vec<_> = iq.iter().map(|v| v * scale).collect();
                            let scaled = measure_decoded_snr(&scaled, rate, &bits(), 1234.57, 0.0);
                            assert!((scaled.value.unwrap() - value).abs() < 0.01, "{scaled:?}");
                            assert_eq!(scaled.quality_reasons, measured.quality_reasons);
                        }
                    }
                }
            }
        }
    }
    let mut bias = 0.0;
    for seed in 0..32 {
        let snr = measure_decoded_snr(
            &input(12000.0, -10.0, false, 1234.57, false, 0xA00 + seed),
            12000.0,
            &bits(),
            1234.57,
            0.0,
        );
        bias += snr.value.unwrap() + 10.0;
    }
    println!("SNR_MEAN_BIAS,{}", bias / 32.0);
    assert!(bias.abs() / 32.0 <= 0.5);
}

#[test]
fn neighbours_fading_slope_edges_and_truncated_bursts() {
    let rate = 12000.0;
    let frequency = 1234.57;
    let expected = -10.0;
    let clean = input(rate, expected, false, frequency, false, 0x6210);
    for case in [
        "adjacent-cw",
        "crowded",
        "slope",
        "fading",
        "frequency-error",
        "low-edge",
        "high-edge",
        "partial",
        "clipped",
    ] {
        let f = if case == "low-edge" {
            110.0
        } else if case == "high-edge" {
            2948.0
        } else {
            frequency
        };
        let mut iq = if case == "fading" || f != frequency {
            input(rate, expected, false, f, case == "fading", 0x6210)
        } else {
            clean.clone()
        };
        if case == "adjacent-cw" {
            for (i, p) in iq.chunks_exact_mut(2).enumerate() {
                let phase = TAU * (frequency + 77.3) * i as f64 / rate;
                p[0] += (0.04 * phase.cos()) as f32;
                p[1] += (0.04 * phase.sin()) as f32;
            }
        }
        if case == "crowded" {
            for (j, offset) in [-110.3, 83.7, 170.9].into_iter().enumerate() {
                let neighbour = signal(rate, frequency + offset, 0.008, false);
                // Different data/timing phases prevent perfectly locked neighbours.
                let shift = 137 * (j + 1) * 2;
                for (i, value) in neighbour.iter().enumerate().take(iq.len() - shift) {
                    iq[i + shift] += value;
                }
            }
        }
        if case == "slope" {
            // Complex AR noise with known PSD. Local density at the target's
            // eight tones sets the truth independently of the meter filters.
            let radius = 0.88;
            let pole = frequency + 200.0;
            let theta = TAU * pole / rate;
            let local = tones()
                .iter()
                .map(|tone| {
                    1.0 / (1.0 + radius * radius
                        - 2.0 * radius * (TAU * (frequency + tone * 6.25 - pole) / rate).cos())
                })
                .sum::<f64>()
                / 79.0;
            let sigma = (0.002_f64.powi(2) * rate
                / (2.0 * 2500.0 * 10_f64.powf(expected / 10.0) * local))
                .sqrt();
            iq = signal(rate, frequency, 0.002, false);
            let mut rng = Rng::new(0x6210);
            let (mut re, mut im) = (0.0, 0.0);
            for p in iq.chunks_exact_mut(2) {
                let next = radius * (re * theta.cos() - im * theta.sin()) + sigma * rng.normal();
                im = radius * (re * theta.sin() + im * theta.cos()) + sigma * rng.normal();
                re = next;
                p[0] += re as f32;
                p[1] += im as f32;
            }
        }
        if case == "partial" {
            iq.truncate((rate * 0.16 * 30.0) as usize * 2);
        }
        if case == "clipped" {
            for x in &mut iq {
                *x = x.clamp(-0.004, 0.004);
            }
        }
        let snr = measure_decoded_snr(
            &iq,
            rate,
            &bits(),
            f + if case == "frequency-error" { 0.7 } else { 0.0 },
            0.0,
        );
        report(case, expected, &snr);
        if !baseline() {
            assert!(snr.value.is_some(), "{case}: {snr:?}");
            let flag = match case {
                "adjacent-cw" | "crowded" => Some("off_tone_interference"),
                "slope" => Some("nonflat_background"),
                "low-edge" | "high-edge" => Some("limited_reference_support"),
                "partial" => Some("partial_burst"),
                "clipped" => Some("clipping_detected"),
                _ => None,
            };
            if let Some(flag) = flag {
                assert!(snr.quality_reasons.contains(&flag), "{case}: {snr:?}");
                assert_eq!(snr.quality, "degraded");
            }
            if case != "clipped" {
                assert!(
                    (snr.value.unwrap() - expected).abs() <= 3.0,
                    "{case}: {snr:?}"
                );
            }
        }
    }
}

#[test]
fn truly_unavailable_inputs_have_no_fabricated_number() {
    let bits = bits();
    let iq = input(12000.0, -10.0, false, 1000.0, false, 123);
    for (name, samples, rate, freq, start) in [
        ("zero", vec![0.0; iq.len()], 12000.0, 1000.0, 0.0),
        ("short", iq[..1000].to_vec(), 12000.0, 1000.0, 0.0),
        ("nonfinite", vec![f32::NAN; iq.len()], 12000.0, 1000.0, 0.0),
        ("invalid-rate", iq.clone(), f64::NAN, 1000.0, 0.0),
        ("invalid-time", iq.clone(), 12000.0, 1000.0, f64::INFINITY),
        ("invalid-frequency", iq.clone(), 12000.0, f64::INFINITY, 0.0),
    ] {
        let snr = measure_decoded_snr(&samples, rate, &bits, freq, start);
        report(name, f64::NAN, &snr);
        assert!(snr.value.is_none(), "{name}: {snr:?}");
        assert!(snr.reason.is_some());
        if !baseline() {
            assert_eq!(snr.quality, "unavailable");
            assert_eq!(snr.value_kind, "unavailable");
        }
    }
}

#[test]
#[ignore = "serial performance run after compilation"]
fn meter_timing() {
    let iq = input(12000.0, -10.0, false, 1234.57, false, 0x6110);
    let codeword = bits();
    for _ in 0..5 {
        std::hint::black_box(measure_decoded_snr(&iq, 12000.0, &codeword, 1234.57, 0.0));
    }
    for repeat in 0..7 {
        let began = std::time::Instant::now();
        for _ in 0..32 {
            std::hint::black_box(measure_decoded_snr(&iq, 12000.0, &codeword, 1234.57, 0.0));
        }
        println!(
            "SNR_TIMING,{repeat},{}",
            began.elapsed().as_secs_f64() / 32.0
        );
    }
}
