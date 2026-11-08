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
