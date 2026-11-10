use super::*;

const FINE_RATE: f64 = 6781.684027777777;

fn iq(samples: usize, offset: usize) -> Vec<f32> {
    (offset..offset + samples)
        .flat_map(|k| {
            let time = k as f64 / FINE_RATE;
            let amplitude = if time < 0.4 {
                0.0002
            } else if time < 0.55 {
                0.02
            } else if time < 0.8 {
                0.0001
            } else {
                0.0004
            };
            let phase = TAU * 1000.0 * time;
            [
                (amplitude * phase.cos()) as f32,
                (amplitude * phase.sin()) as f32,
            ]
        })
        .collect()
}

fn demod(out_rate: f64) -> AnalogDemod {
    AnalogDemod::new_prechannelized(
        spec_for("usb").unwrap(),
        DemodConfig::new(FINE_RATE, 2700.0),
        out_rate,
    )
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

#[test]
fn native_wfm_stereo_matrix_preserves_samples_across_iq_partitions() {
    // The two resamplers see different input clocks (IQ and IQ/2). A one-frame
    // disagreement must wait for its matching sample, rather than become a zero
    // or get dropped at the L/R matrix. Include the live SAN-90 clock/packet and
    // odd packets reached by other acquisition and multichannel geometries.
    for (fs, rate) in [(325_520.833_333_333_3, 48_000.0),
        (325_520.833_333_333_3, 44_100.0), (300_480.769_230_769_25, 48_000.0),
        (488_281.25, 192_000.0)] {
        let mut phase = 0.0;
        let input: Vec<f32> = (0..(fs * 0.09) as usize).flat_map(|k| {
            let t = k as f64 / fs;
            let composite = 0.16 * (TAU * 1_137.0 * t).sin()
                + 0.24 * (TAU * 4_321.0 * t).cos() * (TAU * 38_000.0 * t).cos()
                + 0.09 * (TAU * 19_000.0 * t).sin();
            phase += TAU * 75_000.0 * composite / fs;
            [phase.cos() as f32, phase.sin() as f32]
        }).collect();
        let replay = |size: usize| {
            let mut demod = AnalogDemod::new_prechannelized(
                spec_for("wfm").unwrap(), DemodConfig::new(fs, 180_000.0), rate);
            demod.set_agc_enabled(false);
            let mut matrix = Vec::new();
            let mut pcm = Vec::new();
            for chunk in input.chunks(2 * size) {
                let limit = demod.output_length_hint(chunk.len() / 2).unwrap();
                demod.process_into(chunk, &mut pcm);
                let side = demod.take_side();
                assert!(pcm.len() <= limit);
                assert_eq!(pcm.len(), side.len(), "unmatched stereo frames: fs={fs} rate={rate} block={size}");
                for (&m, &s) in pcm.iter().zip(&side) { matrix.extend([m + s, m - s]); }
            }
            matrix
        };
        let expected = replay(input.len() / 2);
        for size in [1, 17, 9_765, 11_165, 12_180] {
            let actual = replay(size);
            assert_eq!(actual.len(), expected.len());
            let peak = actual.iter().zip(&expected).map(|(a,b)| (a-b).abs()).fold(0.0_f32, f32::max);
            assert!(peak < 2e-6, "partition created stereo transient: fs={fs} rate={rate} block={size} peak={peak}");
        }
    }
}

#[test]
fn native_wfm_retune_discards_unmatched_stereo_history() {
    let fs = 325_520.833_333_333_3;
    let new = || {
        let mut demod = AnalogDemod::new_prechannelized(
            spec_for("wfm").unwrap(), DemodConfig::new(fs, 180_000.0), 48_000.0);
        demod.set_agc_enabled(false);
        demod
    };
    let mut demod = new();
    let mut pcm = Vec::new();
    let old: Vec<f32> = (0..3001).flat_map(|k| {
        let phase = TAU * 30_000.0 * k as f64 / fs;
        [phase.cos() as f32, phase.sin() as f32]
    }).collect();
    demod.process_into(&old, &mut pcm);
    demod.take_side();
    for _ in 0..32 {
        if demod.stereo.as_ref().unwrap().pending_mono_samples() > 0 { break; }
        demod.process_into(&[0.8, 0.6], &mut pcm);
        demod.take_side();
    }
    assert!(demod.stereo.as_ref().unwrap().pending_mono_samples() > 0);
    demod.reset();
    let mut fresh = new();
    let input = vec![0.2; 24_360];
    let mut expected = Vec::new();
    fresh.process_into(&input, &mut expected);
    demod.process_into(&input, &mut pcm);
    assert_eq!(pcm, expected, "old-station mono tail survived retuning");
    assert_eq!(demod.take_side(), fresh.take_side(), "old-station side tail survived retuning");
}

#[test]
fn native_wfm_agc_measures_both_stereo_channels_without_clipping_or_pumping() {
    let fs = 325_520.833_333_333_3;
    let mut receiver = AnalogDemod::new_prechannelized(
        spec_for("wfm").unwrap(), DemodConfig::new(fs, 180_000.0), 48_000.0,
    );
    let mut phase = 0.0;
    let mut pcm = Vec::new();
    let mut rendered = Vec::new();
    // A stereo programme can have very little L+R and substantial L−R.
    // Measuring only mono asks the AGC to amplify the difference into clipping.
    for start in (0..(fs * 1.2) as usize).step_by(4096) {
        let input: Vec<f32> = (start..(start + 4096).min((fs * 1.2) as usize)).flat_map(|k| {
            let t = k as f64 / fs;
            let mono = 0.001 * (TAU * 1_000.0 * t).sin();
            let side = 0.35 * (TAU * 2_000.0 * t).sin();
            let composite = mono + side * (TAU * 38_000.0 * t).cos() + 0.09 * (TAU * 19_000.0 * t).sin();
            phase += TAU * 75_000.0 * composite / fs;
            [(phase.cos() * 0.1) as f32, (phase.sin() * 0.1) as f32]
        }).collect();
        receiver.process_into(&input, &mut pcm);
        let side = receiver.take_side();
        if start as f64 > fs * 0.6 {
            assert!(receiver.stereo_detected());
            for (&m, &s) in pcm.iter().zip(&side) { rendered.extend([m + s, m - s]); }
        }
    }
    let peak = rendered.iter().map(|v| v.abs()).fold(0.0_f32, f32::max);
    let level = rms(&rendered);
    assert!(peak <= 0.951, "stereo peak exceeds the linked AGC ceiling: {peak}");
    assert!((0.085..0.115).contains(&level), "stereo RMS misses the listening target: {level}");
}

#[test]
fn native_ssb_cold_start_and_reset_do_not_amplify_fir_startup() {
    let input = iq(6782, 0);
    let mut receiver = demod(48000.0);
    let mut scratch = Vec::new();
    let mut first = Vec::new();
    for chunk in input.chunks(240 * 2) {
        receiver.process_into(chunk, &mut scratch);
        first.extend_from_slice(&scratch);
    }
    let startup = rms(&first[4800..9600]);
    assert!((0.09..0.11).contains(&startup), "100-200 ms RMS {startup}");
    assert!(first[..2725 + 960].iter().all(|v| *v == 0.0));
    receiver.reset();
    let mut reset = Vec::new();
    for chunk in input.chunks(254 * 2) {
        receiver.process_into(chunk, &mut scratch);
        reset.extend_from_slice(&scratch);
    }
    assert_eq!(reset.len(), first.len());
    let worst = reset
        .iter()
        .zip(&first)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst < 1e-5,
        "reset changed PCM across packet sizes: {worst}"
    );
}

#[test]
fn native_ssb_gain_and_pcm_cadence_survive_iq_partitioning() {
    let input = iq(13564, 0);
    for rate in [44100.0, 48000.0, 96000.0] {
        let mut reference = demod(rate);
        let mut expected = Vec::new();
        reference.process_into(&input, &mut expected);
        for size in [1, 17, 239, 240, 254, 960, 8192] {
            let mut receiver = demod(rate);
            let mut actual = Vec::new();
            let mut scratch = Vec::new();
            for chunk in input.chunks(size * 2) {
                receiver.process_into(chunk, &mut scratch);
                actual.extend_from_slice(&scratch);
            }
            assert_eq!(
                actual.len(),
                expected.len(),
                "rate {rate}, IQ packet size {size}"
            );
            let worst = actual
                .iter()
                .zip(&expected)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                worst < 2e-5,
                "rate {rate}, IQ packet size {size}: PCM changed by {worst}"
            );
        }
    }
}
