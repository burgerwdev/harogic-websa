use super::*;
use super::tests::amplitude_at;

const RATE: f64 = 48_000.0;

fn demod(sync: bool) -> AnalogDemod {
    let mut demod = AnalogDemod::new_prechannelized(spec_for("am").unwrap(),
        DemodConfig::new(RATE, 12_000.0), RATE);
    demod.set_agc_enabled(false);
    demod.set_am_sync(sync);
    demod
}

fn tone(start: usize, count: usize, offset: f64, phase: f64, depth: f64, carrier: f64) -> Vec<f32> {
    (start..start + count).flat_map(|n| {
        let time = n as f64 / RATE;
        let envelope = carrier * (1.0 + depth * (TAU * 1_000.0 * time).cos());
        let angle = TAU * offset * time + phase;
        [(envelope * angle.cos()) as f32, (envelope * angle.sin()) as f32]
    }).collect()
}

#[test]
fn am_sync_recovers_signed_offsets_initial_phase_and_weak_carriers() {
    for (offset, phase, carrier) in [(317.0, 1.57, 0.2), (-733.0, -2.1, 0.2),
        (23.0, 3.0, 6.0 / 32_768.0), (2_100.0, 0.8, 0.2)] {
        let mut demod = demod(true);
        let mut audio = Vec::new();
        let mut out = Vec::new();
        for block in 0..100 {
            demod.process_into(&tone(block * 960, 960, offset, phase, 0.5, carrier), &mut out);
            if block >= 75 { audio.extend_from_slice(&out); }
        }
        let status = demod.am_status().unwrap();
        assert!(status.sync_locked, "{offset} Hz at amplitude {carrier}: {status:?}");
        assert!((status.carrier_offset_hz.unwrap() - offset).abs() < 1.0, "{status:?}");
        assert!((status.modulation_pct.unwrap() - 50.0).abs() < 1.0, "{status:?}");
        let expected_db = 10.0 * (carrier * carrier * 1.125).log10();
        assert!((status.input_dbfs.unwrap() - expected_db).abs() < 0.1, "{status:?}");
        let fundamental = amplitude_at(&audio, RATE, 1_000.0);
        assert!(fundamental > carrier * 0.22 && fundamental < carrier * 0.27,
            "{offset}: tone amplitude {fundamental}");
        assert!(amplitude_at(&audio, RATE, 900.0) < fundamental * 0.01);
        assert!(amplitude_at(&audio, RATE, 1_100.0) < fundamental * 0.01);
        assert!(audio.iter().all(|s| s.is_finite()));
    }
}

#[test]
fn am_sync_product_preserves_overmodulation_without_envelope_rectification() {
    for audio_hz in [50.0, 1_000.0] {
        let mut sync = demod(true);
        let mut envelope = demod(false);
        let mut product_audio = Vec::new();
        let mut envelope_audio = Vec::new();
        let mut out = Vec::new();
        for block in 0..150 {
            let iq: Vec<f32> = (block * 960..(block + 1) * 960).flat_map(|n| {
                let time = n as f64 / RATE;
                let amplitude = 0.2 * (1.0 + 1.5 * (TAU * audio_hz * time).cos());
                let phase = -TAU * 277.0 * time + 2.2;
                [(amplitude * phase.cos()) as f32, (amplitude * phase.sin()) as f32]
            }).collect();
            sync.process_into(&iq, &mut out);
            if block >= 100 { product_audio.extend_from_slice(&out); }
            envelope.process_into(&iq, &mut out);
            if block >= 100 { envelope_audio.extend_from_slice(&out); }
        }
        assert!(sync.am_status().unwrap().sync_locked, "{audio_hz} Hz");
        let fundamental = amplitude_at(&product_audio, RATE, audio_hz);
        let second = amplitude_at(&product_audio, RATE, 2.0 * audio_hz);
        let third = amplitude_at(&product_audio, RATE, 3.0 * audio_hz);
        let envelope_second = amplitude_at(&envelope_audio, RATE, 2.0 * audio_hz);
        assert!(fundamental > 0.14, "{audio_hz} Hz fundamental {fundamental}");
        assert!(second < fundamental * 0.015 && third < fundamental * 0.015,
            "{audio_hz} Hz product harmonics {second}, {third}/{fundamental}");
        assert!(envelope_second > fundamental * 0.08,
            "{audio_hz} Hz envelope harmonic {envelope_second}");
    }
}

#[test]
fn am_carrier_evidence_rejects_fades_noise_dsbsc_and_out_of_pull_range() {
    let mut demod = demod(true);
    let mut out = Vec::new();
    demod.process_into(&tone(0, 48_000, 127.0, 0.5, 0.5, 0.2), &mut out);
    assert!(demod.am_status().unwrap().sync_locked);
    // A controlled 80 dB fade has nonzero IQ and must not retain a valid estimate.
    demod.process_into(&tone(48_000, 24_000, 127.0, 0.5, 0.5, 0.2e-4), &mut out);
    let status = demod.am_status().unwrap();
    assert!(!status.sync_locked && status.carrier_offset_hz.is_none() && status.modulation_pct.is_none());
    demod.process_into(&tone(72_000, 48_000, -183.0, -1.3, 0.5, 0.2), &mut out);
    assert!(demod.am_status().unwrap().sync_locked);
    for kind in 0..4 {
        demod.reset();
        let mut seed = 7_u32;
        let iq: Vec<f32> = (0..96_000).flat_map(|n| {
            let time = n as f64 / RATE;
            match kind {
                0 => [0.0, 0.0],
                1 => {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let i = seed as i32 as f32 / i32::MAX as f32 * 0.2;
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    [i, seed as i32 as f32 / i32::MAX as f32 * 0.2]
                },
                2 => {
                    let amplitude = 0.2 * (TAU * 1_000.0 * time).cos();
                    [(amplitude * (TAU * 127.0 * time).cos()) as f32,
                        (amplitude * (TAU * 127.0 * time).sin()) as f32]
                },
                _ => [(0.2 * (TAU * 3_300.0 * time).cos()) as f32,
                    (0.2 * (TAU * 3_300.0 * time).sin()) as f32],
            }
        }).collect();
        demod.process_into(&iq, &mut out);
        let status = demod.am_status().unwrap();
        assert!(!status.sync_locked && status.carrier_offset_hz.is_none()
            && status.modulation_pct.is_none(), "kind {kind}: {status:?}");
        assert!(out.iter().all(|sample| sample.is_finite()));
    }
}

#[test]
fn am_live_toggle_is_sample_partition_invariant_and_preserves_frame_count() {
    fn run(partition: usize) -> Vec<f32> {
        let mut demod = demod(false);
        let iq = tone(0, 96_000, 317.0, 1.3, 0.5, 0.2);
        let mut audio = Vec::new();
        let mut out = Vec::new();
        for (start, end, enabled) in [(0, 24_000, false), (24_000, 72_000, true), (72_000, 96_000, false)] {
            demod.set_am_sync(enabled);
            for chunk in iq[2 * start..2 * end].chunks(2 * partition) {
                // Repeated controls must not restart either PLL or the sample-clock fade.
                demod.set_am_sync(enabled);
                demod.process_into(chunk, &mut out);
                audio.extend_from_slice(&out);
            }
        }
        audio
    }
    let audio = run(960);
    assert_eq!(audio, run(137));
    assert!((95_999..=96_000).contains(&audio.len()));
    for at in [24_000, 72_000] {
        let jump = (audio[at] - audio[at - 1]).abs();
        assert!(jump < 0.02, "toggle at {at}: {jump}");
    }
}

#[test]
fn am_live_toggle_preserves_audio_through_either_carrier_phase_polarity() {
    for phase in [0.0, 3.0] {
        let mut selected = demod(false);
        let mut envelope = demod(false);
        let mut actual = Vec::new();
        let mut reference = Vec::new();
        for block in 0..100 {
            if block == 50 { selected.set_am_sync(true); }
            if block == 75 { selected.set_am_sync(false); }
            let iq = tone(block * 960, 960, 23.0, phase, 0.5, 0.02);
            selected.process_into(&iq, &mut actual);
            envelope.process_into(&iq, &mut reference);
            if block >= 50 {
                // Clean AM should retain the same waveform during both transitions.
                // A modulo-pi lock used to cancel it at the middle of the crossfade.
                let error: f64 = actual.iter().zip(&reference)
                    .map(|(&a, &b)| (a as f64 - b as f64).powi(2)).sum();
                let power: f64 = reference.iter().map(|&s| (s as f64).powi(2)).sum();
                assert!(error < power * 1e-4,
                    "phase {phase}, block {block}: relative error {}", error / power);
            }
        }
    }
}

#[test]
fn am_retune_clears_carrier_evidence_without_restarting_pcm_clock() {
    let mut demod = demod(true);
    let mut out = Vec::new();
    demod.process_into(&tone(0, 48_000, 317.0, 1.3, 0.5, 0.1), &mut out);
    assert!(demod.am_status().unwrap().sync_locked);
    let before_count = out.len();
    demod.retune();
    let status = demod.am_status().unwrap();
    assert!(status.sync_enabled && !status.sync_locked && status.carrier_offset_hz.is_none()
        && status.modulation_pct.is_none() && status.input_dbfs.is_none());
    demod.process_into(&tone(48_000, 48_000, -477.0, -2.4, 0.5, 0.1), &mut out);
    assert_eq!(before_count,out.len());
    assert!(out.iter().all(|sample|sample.is_finite() && sample.abs()<0.2));
    assert!(demod.am_status().unwrap().sync_locked);
    demod.reset();
    assert!(!demod.am_status().unwrap().sync_locked);
    demod.process_into(&vec![0.0;960*2], &mut out);
    assert!(out.iter().all(|sample|*sample==0.0));
}

#[test]
fn am_broadcast_tracks_drift_colored_noise_and_brief_selective_fades() {
    let mut receiver = demod(true);
    let mut out = Vec::new();
    let mut seed = 73_u32;
    let mut noise_i = 0.0;
    let mut noise_q = 0.0;
    let mut phase = 2.2;
    let mut locked = 0;
    let mut input_levels = Vec::new();
    let mut depths = Vec::new();
    let mut offsets = Vec::new();
    for block in 0..400 {
        let iq: Vec<f32> = (block * 960..(block + 1) * 960).flat_map(|n| {
            let t = n as f64 / RATE;
            let offset = 317.0 + 0.6 * t;
            phase += TAU * offset / RATE;
            // Speech-like multiple tones, slow propagation fading, and 30 ms nulls.
            let fade = if n % 48_000 < 1440 { 0.04 } else { 1.0 };
            let carrier = 0.02 * (0.8 + 0.2 * (TAU * 0.3 * t).cos()) * fade;
            let envelope = carrier * (1.0 + 0.35 * (TAU * 173.0 * t).cos()
                + 0.25 * (TAU * 701.0 * t).cos() + 0.15 * (TAU * 1_311.0 * t).cos());
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise_i += 0.22 * (seed as i32 as f64 / i32::MAX as f64 * 0.012 - noise_i);
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise_q += 0.22 * (seed as i32 as f64 / i32::MAX as f64 * 0.012 - noise_q);
            // Unequal sidebands make differential phase a biased frequency estimate.
            let sideband = 0.0015;
            [(envelope * phase.cos() + sideband * (phase + TAU * 701.0 * t).cos() + noise_i) as f32,
                (envelope * phase.sin() + sideband * (phase + TAU * 701.0 * t).sin() + noise_q) as f32]
        }).collect();
        receiver.process_into(&iq, &mut out);
        assert!(out.iter().all(|sample| sample.is_finite()));
        if block >= 150 {
            let status = receiver.am_status().unwrap();
            if status.sync_locked { locked += 1; }
            if let Some(offset) = status.carrier_offset_hz { offsets.push(offset); }
            if let Some(level) = status.input_dbfs { input_levels.push(level); }
            if let Some(depth) = status.modulation_pct { depths.push(depth); }
        }
    }
    assert!(locked >= 240, "only {locked}/250 broadcast blocks locked");
    assert!(offsets.iter().all(|offset| (317.0..325.0).contains(offset)), "{offsets:?}");
    // The deliberately inserted null should not produce an abrupt meter jump.
    assert!(input_levels.windows(2).all(|v| (v[1] - v[0]).abs() < 1.0));
    assert!(depths.windows(2).all(|v| (v[1] - v[0]).abs() < 6.0));
}

#[test]
fn default_am_fixture_pcm_bytes_match_legacy_envelope_detector() {
    let fixture = include_bytes!("../../../tests/fixtures/dsp/demod_am_iq.bin");
    let iq: Vec<f32> = fixture.chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect();
    let mut current = build("am", DemodConfig::new(96_000.0, 12_000.0), RATE).unwrap();
    // DSB retains the exact original envelope/DC-block implementation.
    let mut legacy = build("dsb", DemodConfig::new(96_000.0, 12_000.0), RATE).unwrap();
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for chunk in iq.chunks(2 * 977) {
        current.process_into(chunk, &mut actual);
        legacy.process_into(chunk, &mut expected);
        assert_eq!(actual.iter().map(|s|s.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|s|s.to_bits()).collect::<Vec<_>>());
    }
}
