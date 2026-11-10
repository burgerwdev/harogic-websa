//! Independent BT=2 Gaussian FSK synthesis, with SNR referenced to 2500 Hz.
//! No stored audio, external executable or callsign hints are needed for this gate.
use websa_dsp::digital::ft8::{
    tables::{COSTAS, GRAY},
    Ft8Decoder,
};

mod common;
use common::Rng;

const RATE: f64 = 12_000.0;
const SYMBOL: usize = 1920;

fn waveform(frequency: f64, start: usize) -> Vec<f32> {
    let manifest = include_str!("../../../tests/fixtures/ft8/manifest.json");
    let bits: Vec<usize> = manifest
        .split("\"codeword_bits\": [")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap()
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect();
    let mut tones = Vec::new();
    for block in 0..3 {
        tones.extend(COSTAS.iter().map(|&x| f64::from(x)));
        if block < 2 {
            for k in 0..29 {
                let i = block * 87 + k * 3;
                tones.push(f64::from(GRAY[bits[i] * 4 + bits[i + 1] * 2 + bits[i + 2]]));
            }
        }
    }
    let mut deviation: Vec<f64> = tones
        .iter()
        .flat_map(|&x| std::iter::repeat_n(x, SYMBOL))
        .collect();
    // Gaussian low-pass step response. BT=2 implies sigma/T = sqrt(ln 2)/(4*pi).
    // Integrate a sampled Gaussian to avoid relying on an erf implementation.
    let sigma = SYMBOL as f64 * 2.0_f64.ln().sqrt() / (4.0 * std::f64::consts::PI);
    let radius = (sigma * 5.0).ceil() as i64;
    let gaussian: Vec<f64> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
        .collect();
    let norm: f64 = gaussian.iter().sum();
    let mut cumulative = 0.0;
    let step: Vec<f64> = gaussian
        .iter()
        .map(|g| {
            cumulative += g / norm;
            cumulative
        })
        .collect();
    for k in 1..tones.len() {
        let delta = tones[k] - tones[k - 1];
        for (j, &cdf) in step.iter().enumerate() {
            let offset = j as i64 - radius;
            let i = k as i64 * SYMBOL as i64 + offset;
            deviation[i as usize] += delta * (cdf - if offset >= 0 { 1.0 } else { 0.0 });
        }
    }
    let mut signal = vec![0.0; 180_000];
    let mut phase = 0.0_f64;
    for (i, tone) in deviation.into_iter().enumerate() {
        signal[start + i] = (0.002 * phase.cos()) as f32;
        phase = (phase + std::f64::consts::TAU * (frequency + tone * 6.25) / RATE)
            .rem_euclid(std::f64::consts::TAU);
    }
    signal
}

#[test]
fn weak_gfsk_decodes_without_prior_payload_bits_and_noise_stays_empty() {
    if cfg!(debug_assertions) {
        return;
    }
    let mut hits = 0;
    for (trial, (frequency, start)) in [(1000.37, 6000), (843.1, 7530), (1777.8, 8280)]
        .into_iter()
        .enumerate()
    {
        let signal = waveform(frequency, start);
        // Real noise has one-sided N0=2*sigma^2/Fs; active sine power=A^2/2.
        let sigma = (0.002_f64.powi(2) / 2.0 * RATE / (2.0 * 2500.0 * 0.01)).sqrt();
        let mut rng = Rng::new(0x6F_5A_2026 + trial as u64 * 2);
        let noise: Vec<f32> = (0..signal.len())
            .map(|_| (sigma * rng.normal()) as f32)
            .collect();
        for known in [false, true] {
            let iq: Vec<f32> = signal
                .iter()
                .zip(&noise)
                .flat_map(|(&s, &n)| [n + if known { s } else { 0.0 }, 0.0])
                .collect();
            let mut decoder = Ft8Decoder::new(RATE);
            decoder.push_iq(&iq);
            let began = std::time::Instant::now();
            let messages = decoder.decode();
            println!(
                "GFSK trial={trial} signal={known} time={:?} messages={messages:?}",
                began.elapsed()
            );
            assert!(
                began.elapsed().as_secs_f64() < 4.0,
                "decode must fit its worker budget"
            );
            assert!(
                messages.iter().all(|m| known && m.text == "CQ JO1WKO PM95"),
                "false decode"
            );
            if known && !messages.is_empty() {
                hits += 1;
            }
        }
    }
    assert!(hits >= 2, "-20 dB GFSK decoded {hits}/3 independent trials");
}
