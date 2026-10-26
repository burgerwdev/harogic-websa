// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

use super::fax::{
    FaxDecodeEvent, FaxDecodeEventRef, FaxDecoder, FaxDecoderConfig, FaxEncodeOptions, FaxEncoder,
    FaxIoc, FaxLpm, FaxModulation, FaxPolarity, FaxSpec,
};
use super::GrayImage;

fn two_line_image(ioc: FaxIoc) -> GrayImage {
    let width = ioc.width();
    let mut pixels = vec![0_u8; width as usize * 2];
    for x in 0..width as usize {
        pixels[width as usize + x] = (x * 255 / width as usize) as u8;
    }
    GrayImage::new(width, 2, pixels).unwrap()
}

fn process_encoder(
    encoder: &mut FaxEncoder,
    decoder: &mut FaxDecoder,
    events: &mut Vec<FaxDecodeEvent>,
) {
    let mut pcm = [0.0_f32; 333];
    while !encoder.is_finished() {
        let count = encoder.read_samples(&mut pcm);
        decoder.process_into(&pcm[..count], events).unwrap();
    }
}

fn append_tone(pcm: &mut Vec<f32>, frequency_hz: f64, samples: usize, phase: &mut f64) {
    let step = std::f64::consts::TAU * frequency_hz / 12_000.0;
    for _ in 0..samples {
        pcm.push((phase.sin() * 0.5) as f32);
        *phase = (*phase + step) % std::f64::consts::TAU;
    }
}

fn brightest_window_start(pixels: &[u8], window: usize) -> usize {
    pixels
        .windows(window)
        .enumerate()
        .max_by_key(|(_, values)| values.iter().map(|value| u64::from(*value)).sum::<u64>())
        .map_or(0, |(index, _)| index)
}

#[test]
fn apt_and_phasing_lead_to_streaming_lines() {
    let ioc = FaxIoc::Ioc288;
    let lpm = FaxLpm::LPM_240;
    let image = two_line_image(ioc);
    let spec = FaxSpec::standard(ioc, lpm);
    let mut encoder = FaxEncoder::new(image, spec, 12_000, FaxEncodeOptions::default()).unwrap();
    let mut decoder = FaxDecoder::new(12_000, FaxDecoderConfig::default()).unwrap();
    let mut events = Vec::new();
    process_encoder(&mut encoder, &mut decoder, &mut events);
    let mut sink = |event: FaxDecodeEventRef<'_>| events.push(event.to_owned());
    decoder.finish(&mut sink).unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        FaxDecodeEvent::AptDetected { ioc: detected } if *detected == ioc
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        FaxDecodeEvent::PhasingLocked { lpm: detected, .. } if *detected == lpm
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
            .count(),
        2,
        "events: {events:#?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        FaxDecodeEvent::PageCompleted {
            lines: 2,
            partial: false,
            ..
        }
    )));

    let decoded_gradient = events.iter().find_map(|event| match event {
        FaxDecodeEvent::LineReady {
            line_index: 1,
            pixels,
            ..
        } => Some(pixels),
        _ => None,
    });
    let decoded_gradient = decoded_gradient.expect("second image line");
    let image_width = ioc.width() as usize;
    let mean_absolute_error = decoded_gradient[..image_width]
        .iter()
        .enumerate()
        .map(|(x, actual)| {
            let expected = (x * 255 / ioc.width() as usize) as u8;
            actual.abs_diff(expected) as f64
        })
        .sum::<f64>()
        / image_width as f64;
    assert!(
        mean_absolute_error < 18.0,
        "clean FM gradient MAE was {mean_absolute_error}"
    );
}

#[test]
fn short_apt_does_not_trigger_reception() {
    let ioc = FaxIoc::Ioc288;
    let mut spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
    spec.start_seconds = 1.0;
    let mut encoder = FaxEncoder::new(
        two_line_image(ioc),
        spec,
        12_000,
        FaxEncodeOptions::default(),
    )
    .unwrap();
    let mut decoder = FaxDecoder::new(12_000, FaxDecoderConfig::default()).unwrap();
    let mut events = Vec::new();
    let mut pcm = [0.0_f32; 257];
    let mut emitted = 0;
    while emitted < 24_000 {
        let count = encoder.read_samples(&mut pcm);
        decoder.process_into(&pcm[..count], &mut events).unwrap();
        emitted += count;
    }
    assert!(!events.iter().any(|event| matches!(
        event,
        FaxDecodeEvent::AptDetected { .. } | FaxDecodeEvent::PageStarted { .. }
    )));
}

#[test]
fn forced_ioc_does_not_turn_silence_into_apt() {
    let config = FaxDecoderConfig {
        ioc: Some(FaxIoc::Ioc576),
        ..FaxDecoderConfig::default()
    };
    let mut decoder = FaxDecoder::new(12_000, config).unwrap();
    let mut events = Vec::new();
    for chunk in vec![0.0_f32; 72_000].chunks(311) {
        decoder.process_into(chunk, &mut events).unwrap();
    }
    assert!(events.is_empty());
}

#[test]
fn acquisition_timeout_rejects_never_ending_apt() {
    let ioc = FaxIoc::Ioc288;
    let spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
    let mut encoder = FaxEncoder::new(
        two_line_image(ioc),
        spec,
        12_000,
        FaxEncodeOptions::default(),
    )
    .unwrap();
    let config = FaxDecoderConfig {
        apt_confirm_seconds: 0.2,
        acquisition_timeout_seconds: 1.0,
        ..FaxDecoderConfig::default()
    };
    let mut decoder = FaxDecoder::new(12_000, config).unwrap();
    let mut events = Vec::new();
    let mut pcm = [0.0_f32; 293];
    let mut emitted = 0;
    while emitted < 24_000 {
        let count = encoder.read_samples(&mut pcm);
        decoder.process_into(&pcm[..count], &mut events).unwrap();
        emitted += count;
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, FaxDecodeEvent::AptDetected { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        FaxDecodeEvent::SignalRejected {
            reason: "apt-end-timeout"
        }
    )));
}

#[test]
fn signal_loss_never_generates_frozen_fax_rows() {
    let ioc = FaxIoc::Ioc288;
    let spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
    let pixels = vec![96; ioc.width() as usize * 40];
    let image = GrayImage::new(ioc.width(), 40, pixels).unwrap();
    let mut encoder = FaxEncoder::new(image, spec, 12_000, FaxEncodeOptions::default()).unwrap();
    let mut decoder = FaxDecoder::new(12_000, FaxDecoderConfig::default()).unwrap();
    let mut events = Vec::new();
    let mut pcm = [0.0_f32; 333];
    while !encoder.is_finished() {
        let count = encoder.read_samples(&mut pcm);
        decoder.process_into(&pcm[..count], &mut events).unwrap();
        if events
            .iter()
            .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
            .count()
            >= 1
        {
            break;
        }
    }
    let rows_before_loss = events
        .iter()
        .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
        .count();
    for chunk in vec![0.0_f32; 36_000].chunks(333) {
        decoder.process_into(chunk, &mut events).unwrap();
    }
    let rows_after_loss = events
        .iter()
        .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
        .count();
    assert_eq!(rows_after_loss, rows_before_loss);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, FaxDecodeEvent::PageCompleted { partial: true, .. }))
    );
}

#[test]
fn carrier_loss_to_wideband_noise_does_not_generate_fax_rows() {
    let ioc = FaxIoc::Ioc288;
    let spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
    let pixels = vec![96; ioc.width() as usize * 40];
    let image = GrayImage::new(ioc.width(), 40, pixels).unwrap();
    let mut encoder = FaxEncoder::new(image, spec, 12_000, FaxEncodeOptions::default()).unwrap();
    let mut decoder = FaxDecoder::new(12_000, FaxDecoderConfig::default()).unwrap();
    let mut events = Vec::new();
    let mut pcm = [0.0_f32; 333];
    while !encoder.is_finished() {
        let count = encoder.read_samples(&mut pcm);
        decoder.process_into(&pcm[..count], &mut events).unwrap();
        if events
            .iter()
            .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
            .count()
            >= 1
        {
            break;
        }
    }
    let rows_before_loss = events
        .iter()
        .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
        .count();
    let mut state = 7_u64;
    let mut noise = vec![0.0_f32; 36_000];
    for sample in &mut noise {
        let mut gaussianish = 0.0_f32;
        for _ in 0..12 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            gaussianish += ((state >> 40) as f32) / ((1_u32 << 24) as f32);
        }
        *sample = (gaussianish - 6.0) * 0.12;
    }
    for chunk in noise.chunks(333) {
        decoder.process_into(chunk, &mut events).unwrap();
    }
    let rows_after_loss = events
        .iter()
        .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
        .count();
    assert_eq!(rows_after_loss, rows_before_loss);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, FaxDecodeEvent::PageCompleted { partial: true, .. }))
    );
}

#[test]
fn custom_fm_and_am_round_trip_and_preserve_page_spec() {
    let modulations = [
        FaxModulation::FmSubcarrier {
            center_hz: 2_000.0,
            deviation_hz: 300.0,
            polarity: FaxPolarity::Inverted,
        },
        FaxModulation::AmSubcarrier {
            carrier_hz: 1_800.0,
            black_level: 1.0,
            white_level: 0.04,
        },
    ];
    for modulation in modulations {
        let ioc = FaxIoc::Ioc288;
        let mut spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
        spec.modulation = modulation;
        let mut encoder = FaxEncoder::new(
            two_line_image(ioc),
            spec,
            12_000,
            FaxEncodeOptions::default(),
        )
        .unwrap();
        let config = FaxDecoderConfig {
            modulation,
            max_lines: Some(2),
            ..FaxDecoderConfig::default()
        };
        let mut decoder = FaxDecoder::new(12_000, config).unwrap();
        let mut events = Vec::new();
        process_encoder(&mut encoder, &mut decoder, &mut events);
        assert!(
            events.iter().any(|event| matches!(
                event,
                FaxDecodeEvent::PageStarted { spec, .. } if spec.modulation == modulation
            )),
            "missing PageStarted for {modulation:?}: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
                .count(),
            2,
            "round-trip failed for {modulation:?}: {events:?}"
        );
    }
}

#[test]
fn phasing_only_starts_when_ioc_is_preconfigured() {
    let ioc = FaxIoc::Ioc288;
    let spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
    let options = FaxEncodeOptions {
        include_apt: false,
        include_phasing: true,
        ..FaxEncodeOptions::default()
    };
    let mut encoder = FaxEncoder::new(two_line_image(ioc), spec, 12_000, options).unwrap();
    let config = FaxDecoderConfig {
        ioc: Some(ioc),
        max_lines: Some(2),
        ..FaxDecoderConfig::default()
    };
    let mut decoder = FaxDecoder::new(12_000, config).unwrap();
    let mut events = Vec::new();
    process_encoder(&mut encoder, &mut decoder, &mut events);
    let mut sink = |event: FaxDecodeEventRef<'_>| events.push(event.to_owned());
    decoder.finish(&mut sink).unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, FaxDecodeEvent::AptDetected { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, FaxDecodeEvent::PhasingLocked { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
            .count(),
        2,
        "events: {events:?}"
    );
}

#[test]
fn symmetric_phasing_is_accepted_with_preconfigured_ioc() {
    let ioc = FaxIoc::Ioc288;
    let lpm = FaxLpm::LPM_240;
    let mut pcm = Vec::new();
    let mut phase = 0.0;
    let half_line = (lpm.line_seconds() * 12_000.0 * 0.5).round() as usize;
    for _ in 0..20 {
        append_tone(&mut pcm, 1_500.0, half_line, &mut phase);
        append_tone(&mut pcm, 2_300.0, half_line, &mut phase);
    }
    let mut image_pixels = vec![24; ioc.width() as usize];
    image_pixels[200..240].fill(224);
    let image = GrayImage::new(ioc.width(), 1, image_pixels).unwrap();
    let options = FaxEncodeOptions {
        include_apt: false,
        include_phasing: false,
        ..FaxEncodeOptions::default()
    };
    let mut image_encoder =
        FaxEncoder::new(image, FaxSpec::standard(ioc, lpm), 12_000, options).unwrap();
    let mut chunk = [0.0_f32; 257];
    while !image_encoder.is_finished() {
        let count = image_encoder.read_samples(&mut chunk);
        pcm.extend_from_slice(&chunk[..count]);
    }

    let config = FaxDecoderConfig {
        ioc: Some(ioc),
        lpm: Some(lpm),
        expected_phasing_seconds: Some(5.0),
        max_lines: Some(1),
        ..FaxDecoderConfig::default()
    };
    let mut decoder = FaxDecoder::new(12_000, config).unwrap();
    let mut events = Vec::new();
    for chunk in pcm.chunks(281) {
        decoder.process_into(chunk, &mut events).unwrap();
    }
    let mut sink = |event: FaxDecodeEventRef<'_>| events.push(event.to_owned());
    decoder.finish(&mut sink).unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, FaxDecodeEvent::PhasingLocked { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
            .count(),
        1,
        "events: {events:?}"
    );
    let line = events.iter().find_map(|event| match event {
        FaxDecodeEvent::LineReady { pixels, .. } => Some(pixels),
        _ => None,
    });
    let marker = brightest_window_start(&line.unwrap()[..ioc.width() as usize], 40);
    assert!(
        marker.abs_diff(200) <= 4,
        "symmetric phasing shifted the marker to x={marker}"
    );
}

#[test]
fn fractional_custom_lpm_uses_measured_cumulative_timing() {
    let ioc = FaxIoc::Ioc288;
    let lpm = FaxLpm::new(110).unwrap();
    let mut spec = FaxSpec::standard(ioc, lpm);
    spec.phasing_seconds = 6.0;
    let mut image_pixels = vec![24; ioc.width() as usize * 12];
    for row in image_pixels.chunks_exact_mut(ioc.width() as usize) {
        row[300..340].fill(224);
    }
    let image = GrayImage::new(ioc.width(), 12, image_pixels).unwrap();
    let mut encoder = FaxEncoder::new(image, spec, 12_000, FaxEncodeOptions::default()).unwrap();
    let config = FaxDecoderConfig {
        lpm: Some(lpm),
        expected_phasing_seconds: Some(6.0),
        max_lines: Some(12),
        ..FaxDecoderConfig::default()
    };
    let mut decoder = FaxDecoder::new(12_000, config).unwrap();
    let mut events = Vec::new();
    process_encoder(&mut encoder, &mut decoder, &mut events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, FaxDecodeEvent::LineReady { .. }))
            .count(),
        12,
        "events: {events:?}"
    );
    let markers: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            FaxDecodeEvent::LineReady { pixels, .. } => {
                Some(brightest_window_start(&pixels[..ioc.width() as usize], 40))
            }
            _ => None,
        })
        .collect();
    assert!(
        markers[0].abs_diff(*markers.last().unwrap()) <= 2,
        "marker drift: {markers:?}"
    );
}
