use super::{ChannelAction, ChannelCommand, FineBank};
use crate::pfb_pool::{PfbOutput, PfbStream, MAX_IN_FLIGHT};
use crate::wire::BasebandFramer;
use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::time::Duration;

#[test]
fn multichannel_fine_quality_and_admission_matrix() {
    let fs = 62_500_000.0;
    let bins = [
        0, 1, 5, 15, 20, 31, 40, 45, 80, 85, 90, 95, 100, 110, 120, 127,
    ];
    let packet_lengths = [777, 16240, 941, 16240, 16240];
    let total: usize = packet_lengths.iter().sum();
    let tone_hz = 1_000.0;
    let spacing = fs / 128.0;
    let iq: Vec<i16> = (0..total)
        .flat_map(|n| {
            let t = n as f64 / fs;
            let (re, im) = bins.iter().fold((0.0, 0.0), |(re, im), &bin| {
                let signed = if bin >= 64 {
                    bin as i32 - 128
                } else {
                    bin as i32
                };
                let phase = 2.0 * PI * (signed as f64 * spacing + tone_hz) * t;
                (re + 600.0 * phase.cos(), im + 600.0 * phase.sin())
            });
            [re as i16, im as i16]
        })
        .collect();

    for count in [1, 4, 8, 16] {
        for decimate in [4, 12] {
            let selected = bins[..count].to_vec();
            let mut stream = PfbStream::new(fs, 16240, selected.clone(), 60).unwrap();
            let rate = stream.sample_rate / decimate as f64;
            let mut bank = FineBank::new(
                &stream,
                50_000_000.0,
                255,
                count,
                count as f64 * rate,
                Duration::from_secs(1),
            )
            .unwrap();
            for (i, &bin) in selected.iter().enumerate() {
                bank.subscribe(i as u64 + 1, bin, 0.0, decimate).unwrap();
            }
            assert_eq!(
                bank.subscribe(count as u64 + 1, selected[0], 0.0, decimate),
                Err("fine channel budget exceeded")
            );
            let mut samples = vec![Vec::<(u64, f32, f32)>::new(); count];
            let mut next_index = vec![0u64; count];
            let mut expected_seq = 0;
            let mut deliver = |packet: PfbOutput| {
                assert_eq!((packet.seq, packet.epoch), (expected_seq, 60));
                bank.deliver(&packet, |out| {
                    let index = (out.channel_id - 1) as usize;
                    assert_eq!(
                        (out.bin, out.output_index),
                        (selected[index], next_index[index])
                    );
                    assert_eq!(out.input_index, (next_index[index] + 1) * decimate as u64 * 64 - 1);
                    assert!((out.output_rate - rate).abs() < 1e-8);
                    assert!(out.delay_samples > 0.0);
                    for pair in out.iq.chunks_exact(2) {
                        samples[index].push((next_index[index], pair[0], pair[1]));
                        next_index[index] += 1;
                    }
                    Ok(())
                })
                .unwrap();
                expected_seq += 1;
            };
            let mut at = 0;
            for (seq, &n) in packet_lengths.iter().enumerate() {
                loop {
                    match stream.submit(seq as u64, 1000 + 2 * at as u64, &iq[2 * at..2 * (at + n)])
                    {
                        Ok(()) => break,
                        Err(-2) => deliver(stream.take().unwrap()),
                        other => panic!("PFB submit: {other:?}"),
                    }
                }
                at += n;
                while let Some(packet) = stream.take() {
                    deliver(packet);
                }
            }
            stream.finish().unwrap();
            while let Some(packet) = stream.take() {
                deliver(packet);
            }
            drop(deliver);
            assert_eq!(expected_seq, packet_lengths.len() as u64);
            assert_eq!(stream.stats().task_capacity, MAX_IN_FLIGHT as u32);
            assert_eq!(stream.stats().result_capacity, MAX_IN_FLIGHT as u32);
            for (i, channel) in samples.iter().enumerate() {
                // Skip FIR startup, then fit one complex gain to the known 1 kHz output.
                let steady = &channel[channel.len() / 2..];
                let (mut gain_re, mut gain_im) = (0.0f64, 0.0f64);
                for &(index, re, im) in steady {
                    let phase = 2.0 * PI * tone_hz * index as f64 / rate;
                    gain_re += re as f64 * phase.cos() + im as f64 * phase.sin();
                    gain_im += im as f64 * phase.cos() - re as f64 * phase.sin();
                }
                gain_re /= steady.len() as f64;
                gain_im /= steady.len() as f64;
                let amplitude = gain_re.hypot(gain_im);
                let error = (steady
                    .iter()
                    .map(|&(index, re, im)| {
                        let phase = 2.0 * PI * tone_hz * index as f64 / rate;
                        let want_re = gain_re * phase.cos() - gain_im * phase.sin();
                        let want_im = gain_re * phase.sin() + gain_im * phase.cos();
                        (re as f64 - want_re).powi(2) + (im as f64 - want_im).powi(2)
                    })
                    .sum::<f64>()
                    / steady.len() as f64)
                    .sqrt()
                    / amplitude;
                assert!(amplitude > 300.0 && error < 0.05,
                    "count={count} d={decimate} bin={} amplitude={amplitude:.1} relative_rms={error:.4}", selected[i]);
            }
        }
    }
}

#[test]
fn fine_rejects_adjacent_and_decimation_aliases_at_positive_and_negative_bins() {
    let fs = 62_500_000.0;
    let spacing = fs / 128.0;
    let response = |bin: u32, decimate: i32, offset: f64| {
        let mut stream = PfbStream::new(fs, 16240, vec![bin], 61).unwrap();
        let mut bank = FineBank::new(
            &stream,
            50_000_000.0,
            255,
            1,
            stream.sample_rate / decimate as f64,
            Duration::from_secs(1),
        )
        .unwrap();
        bank.subscribe(1, bin, 0.0, decimate).unwrap();
        let signed = if bin >= 64 {
            bin as i32 - 128
        } else {
            bin as i32
        };
        let frequency = signed as f64 * spacing + offset;
        let mut power = 0.0;
        let mut count = 0;
        let mut deliver = |packet: PfbOutput| {
            bank.deliver(&packet, |out| {
                for (index, z) in out.iq.chunks_exact(2).enumerate() {
                    if out.output_index + index as u64 >= 50 {
                        power += (z[0] * z[0] + z[1] * z[1]) as f64;
                        count += 1;
                    }
                }
                Ok(())
            })
            .unwrap();
        };
        for seq in 0..6u64 {
            let iq: Vec<i16> = (0..16240)
                .flat_map(|j| {
                    let phase = 2.0 * PI * frequency * (seq * 16240 + j) as f64 / fs;
                    [
                        (12000.0 * phase.cos()) as i16,
                        (12000.0 * phase.sin()) as i16,
                    ]
                })
                .collect();
            loop {
                match stream.submit(seq, 1000 + seq * 32480, &iq) {
                    Ok(()) => break,
                    Err(-2) => deliver(stream.take().unwrap()),
                    other => panic!("PFB submit: {other:?}"),
                }
            }
            while let Some(packet) = stream.take() {
                deliver(packet);
            }
        }
        stream.finish().unwrap();
        while let Some(packet) = stream.take() {
            deliver(packet);
        }
        drop(deliver);
        assert!(count > 50);
        (power / count as f64).sqrt()
    };
    for bin in [5, 45, 80, 127] {
        for decimate in [4, 12] {
            let rate = fs / 64.0 / decimate as f64;
            let pass = response(bin, decimate, 1000.0);
            let alias = response(bin, decimate, rate + 1000.0);
            let neighbor = response(bin, decimate, 1.8 * spacing + 1000.0);
            let alias_db = 20.0 * (alias / pass).log10();
            let neighbor_db = 20.0 * (neighbor / pass).log10();
            assert!(pass > 100.0 && alias_db < -40.0 && neighbor_db < -40.0,
                "bin={bin} d={decimate} pass={pass:.1} alias={alias_db:.1} dB neighbor={neighbor_db:.1} dB");
        }
    }
}

#[test]
fn sixteen_bin_fixture_subscription_transitions_keep_clocks_and_failure_evidence() {
    let bins = [
        0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 80, 85, 90, 95, 100, 127,
    ];
    let mut stream = PfbStream::new(62_500_000.0, 16240, bins.to_vec(), 31).unwrap();
    let mut bank = FineBank::new(
        &stream,
        50_000_000.0,
        255,
        16,
        stream.sample_rate * 16.0 / 12.0,
        Duration::from_secs(1),
    )
    .unwrap();
    let mut delivered = 0;
    let mut completed = 0;
    let mut fine_outputs = 0;
    let mut clocks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut handle = |mut item: PfbOutput| {
        assert_eq!(item.seq, delivered);
        assert_eq!((item.epoch, item.input_index), (31, delivered * 16240));
        assert_eq!(item.stamp, 1000 + delivered * 32480);
        for (at, start, end) in [(0, 0, 1), (4, 1, 4), (8, 4, 8), (12, 8, 16)] {
            if item.seq == at {
                for id in start..end {
                    bank.subscribe(id as u64 + 1, bins[id], 0.0, 12).unwrap();
                }
            }
        }
        if item.seq == 12 {
            assert_eq!(
                bank.subscribe(17, 5, 0.0, 12),
                Err("fine channel budget exceeded")
            );
        }
        if item.seq == 16 {
            for id in 1..=8 {
                assert!(bank.unsubscribe(id));
                clocks.remove(&id);
            }
            bank.subscribe(17, 5, -10_000.0, 12).unwrap();
        }
        if item.seq == 10 {
            item.epoch = 30;
            assert_eq!(
                bank.deliver(&item, |_| Ok(())),
                Err("fine bank epoch/sequence mismatch")
            );
            item.epoch = 31;
        }
        let result = bank.deliver(&item, |out| {
            assert_eq!((out.seq, out.epoch, out.stamp), (item.seq, 31, item.stamp));
            let next = clocks.entry(out.channel_id).or_insert(0);
            assert_eq!(out.output_index, *next);
            *next += (out.iq.len() / 2) as u64;
            fine_outputs += 1;
            if item.seq == 23 {
                Err("injected sink error")
            } else {
                Ok(())
            }
        });
        if item.seq == 23 {
            assert_eq!(result, Err("injected sink error"));
            assert_eq!(bank.deliver(&item, |_| Ok(())), Err("fine bank failed"));
        } else {
            result.unwrap();
            completed += 1;
        }
        delivered += 1;
    };
    let mut seed = 20261020u32;
    for seq in 0..24u64 {
        let iq: Vec<i16> = (0..2 * 16240)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as i16
            })
            .collect();
        loop {
            match stream.submit(seq, 1000 + seq * 32480, &iq) {
                Ok(()) => break,
                Err(-2) => handle(stream.take().expect("drain bounded output")),
                error => panic!("PFB submit: {error:?}"),
            }
        }
        while let Some(item) = stream.take() {
            handle(item);
        }
    }
    stream.finish().unwrap();
    while let Some(item) = stream.take() {
        handle(item);
    }
    drop(handle);
    assert_eq!((delivered, completed), (24, 23));
    assert!(fine_outputs > 100);
    let stats = stream.stats();
    assert_eq!(
        (
            stats.submitted,
            stats.completed,
            stats.task_capacity,
            stats.result_capacity
        ),
        (24, 24, MAX_IN_FLIGHT as u32, MAX_IN_FLIGHT as u32)
    );
    assert!(stats.max_pending <= MAX_IN_FLIGHT as u32);
    assert!(stats.max_ready <= MAX_IN_FLIGHT as u32);
}

#[test]
fn admission_rejects_channels_outside_reported_usable_bandwidth() {
    let stream = PfbStream::new(62_500_000.0, 16240, vec![51, 52, 64, 76, 77], 32).unwrap();
    assert!(FineBank::new(
        &stream,
        62_500_001.0,
        255,
        3,
        stream.sample_rate * 3.0 / 12.0,
        Duration::from_secs(1)
    )
    .is_none());
    assert!(FineBank::new(
        &stream,
        f64::NAN,
        255,
        3,
        stream.sample_rate * 3.0 / 12.0,
        Duration::from_secs(1)
    )
    .is_none());
    let mut bank = FineBank::new(
        &stream,
        50_000_000.0,
        255,
        3,
        stream.sample_rate * 3.0 / 12.0,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        bank.subscribe(1, 52, 0.0, 12),
        Err("fine channel outside usable IQ bandwidth")
    );
    assert_eq!(
        bank.subscribe(1, 64, 0.0, 12),
        Err("fine channel outside usable IQ bandwidth")
    );
    assert_eq!(
        bank.subscribe(1, 76, 0.0, 12),
        Err("fine channel outside usable IQ bandwidth")
    );
    assert_eq!(
        bank.subscribe(1, 51, -100_000.0, 12),
        Err("fine channel outside usable IQ bandwidth")
    );
    bank.subscribe(1, 51, 0.0, 12).unwrap();
    bank.subscribe(2, 77, 0.0, 12).unwrap();
    assert_eq!(
        bank.subscribe(3, 51, 0.0, 0),
        Err("fine channel configuration rejected")
    );
    bank.subscribe(3, 51, -50_000.0, 12).unwrap();
    assert_eq!(
        bank.subscribe(4, 77, 0.0, 12),
        Err("fine channel budget exceeded")
    );
}

#[test]
fn output_rate_budget_and_atomic_retune_preserve_existing_clock() {
    let stream = PfbStream::new(62_500_000.0, 16240, vec![5, 10], 41).unwrap();
    let rate = stream.sample_rate / 12.0;
    assert!(FineBank::new(
        &stream,
        50_000_000.0,
        255,
        3,
        f64::NAN,
        Duration::from_secs(1)
    )
    .is_none());
    let mut bank = FineBank::new(
        &stream,
        50_000_000.0,
        255,
        3,
        2.0 * rate,
        Duration::from_secs(1),
    )
    .unwrap();
    let command = |epoch, action| ChannelCommand { epoch, action };
    bank.apply(command(
        41,
        ChannelAction::Subscribe {
            id: 1,
            bin: 5,
            offset_hz: 0.0,
            decimate: 12,
        },
    ))
    .unwrap();
    bank.apply(command(
        41,
        ChannelAction::Subscribe {
            id: 2,
            bin: 10,
            offset_hz: 0.0,
            decimate: 12,
        },
    ))
    .unwrap();
    assert_eq!(
        bank.apply(command(
            41,
            ChannelAction::Subscribe {
                id: 3,
                bin: 5,
                offset_hz: 0.0,
                decimate: 4
            }
        )),
        Err("fine output rate budget exceeded")
    );
    assert_eq!(
        bank.apply(command(40, ChannelAction::Unsubscribe { id: 1 })),
        Err("fine bank command epoch mismatch")
    );
    assert_eq!(
        bank.apply(command(
            41,
            ChannelAction::Retune {
                old_id: 1,
                id: 3,
                bin: 5,
                offset_hz: 0.0,
                decimate: 4
            }
        )),
        Err("fine output rate budget exceeded")
    );
    assert_eq!(
        bank.apply(command(
            41,
            ChannelAction::Retune {
                old_id: 1,
                id: 3,
                bin: 5,
                offset_hz: 300_000.0,
                decimate: 12
            }
        )),
        Err("coarse bin not provisioned")
    );
    let packet = |seq| PfbOutput {
        seq,
        stamp: 1000 + seq * 32480,
        input_index: seq * 16240,
        epoch: 41,
        groups: 192,
        process_ns: 0,
        iq: vec![0.0; 2 * 192 * 2],
    };
    let mut seen = Vec::new();
    bank.deliver(&packet(0), |out| {
        assert_eq!(
            out.center_offset_hz,
            Some(out.bin as f64 * 62_500_000.0 / 128.0)
        );
        seen.push((out.channel_id, out.output_index));
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [(1, 0), (2, 0)]);
    for (bin, offset, decimate, error) in [
        (5, 300_000.0, 12, "coarse bin not provisioned"),
        (stream.bin_count() as u32, 0.0, 12, "coarse bin not provisioned"),
        (5, -62_500_000.0, 12, "fine channel outside usable IQ bandwidth"),
        (5, 0.0, 25, "fine channel configuration rejected"),
        (5, 0.0, 4, "fine output rate budget exceeded"),
    ] {
        assert_eq!(bank.retune(1, 3, bin, offset, decimate), Err(error));
    }
    bank.deliver(&packet(1), |out| {
        assert_eq!(out.output_index, 16);
        assert!(out.channel_id == 1 || out.channel_id == 2);
        Ok(())
    }).unwrap();
    assert_eq!(bank.output_rate_used, 2.0 * rate);
    bank.apply(command(
        41,
        ChannelAction::Retune {
            old_id: 1,
            id: 3,
            bin: 10,
            offset_hz: -10_000.0,
            decimate: 12,
        },
    ))
    .unwrap();
    seen.clear();
    bank.deliver(&packet(2), |out| {
        if out.channel_id == 3 {
            assert_eq!(
                out.center_offset_hz,
                Some(10.0 * 62_500_000.0 / 128.0 + 10_000.0)
            );
        }
        seen.push((out.channel_id, out.output_index));
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [(3, 0), (2, 32)]);
    assert_eq!(
        bank.apply(command(41, ChannelAction::Unsubscribe { id: 1 })),
        Err("fine channel not active")
    );
    bank.apply(command(41, ChannelAction::Unsubscribe { id: 2 }))
        .unwrap();
    bank.apply(command(
        41,
        ChannelAction::Subscribe {
            id: 4,
            bin: 5,
            offset_hz: 0.0,
            decimate: 12,
        },
    ))
    .unwrap();
    seen.clear();
    bank.deliver(&packet(3), |out| {
        seen.push((out.channel_id, out.output_index));
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [(3, 16), (4, 0)]);
}

#[test]
fn ordered_pfb_fine_iq_matches_baseband_frame_clock() {
    let fs = 62_500_000.0;
    let mut stream = PfbStream::new(fs, 16240, vec![5], 53).unwrap();
    let mut bank = FineBank::new(
        &stream,
        50_000_000.0,
        255,
        1,
        stream.sample_rate / 12.0,
        Duration::from_secs(1),
    )
    .unwrap();
    bank.subscribe(1, 5, 0.0, 12).unwrap();
    let metadata = bank.channel_metadata(1).unwrap();
    let mut framer =
        BasebandFramer::new(1, 53, 100e6 + 5.0 * fs / 128.0, stream.sample_rate / 12.0).unwrap();
    for seq in 0..3 {
        let input = vec![0i16; 2 * 16240];
        loop {
            match stream.submit(seq, 1000 + seq * 32480, &input) {
                Ok(()) => break,
                Err(-2) => panic!("unexpected ordered result backpressure"),
                other => panic!("PFB submit: {other:?}"),
            }
        }
    }
    stream.finish().unwrap();
    let mut clock = 0u64;
    for seq in 0..3 {
        let packet = stream.take().unwrap();
        assert_eq!(packet.seq, seq);
        bank.deliver(&packet, |out| {
            assert_eq!(out.output_index, clock);
            clock += (out.iq.len() / 2) as u64;
            let frames = framer.push(out).unwrap();
            if seq == 0 {
                assert_eq!(frames.len(), 1);
                assert_eq!(u32::from_le_bytes(frames[0][8..12].try_into().unwrap()), 0);
                assert_eq!(u32::from_le_bytes(frames[0][32..36].try_into().unwrap()), 1);
                assert_eq!(u32::from_le_bytes(frames[0][36..40].try_into().unwrap()), 53);
                assert_eq!(u64::from_le_bytes(frames[0][40..48].try_into().unwrap()), 767);
                assert_eq!(f64::from_le_bytes(frames[0][48..56].try_into().unwrap()), metadata.1);
            } else {
                assert!(frames.is_empty());
            }
            Ok(())
        })
        .unwrap();
    }
    assert!(stream.take().is_none());
    let tail = framer.finish().unwrap().unwrap();
    assert_eq!(u32::from_le_bytes(tail[8..12].try_into().unwrap()), 1);
    assert_eq!(
        u32::from_le_bytes(tail[12..16].try_into().unwrap()),
        clock as u32
    );
    assert_eq!(tail.len(), 56 + clock as usize * 8);
    assert_eq!(u64::from_le_bytes(tail[40..48].try_into().unwrap()), 767);
    assert_eq!(f64::from_le_bytes(tail[48..56].try_into().unwrap()), metadata.1);
    assert!(clock > 50);
}

#[test]
fn cross_bin_tuning_preserves_rf_and_equivalent_representations() {
    for fs in [62_500_000.0, 4_000_000.0] {
        for taps in [8, 16] {
            let probe = PfbStream::with_taps(fs, 16240, vec![0], 71, taps).unwrap();
            let m = probe.bin_count() as u32;
            drop(probe);
            for fraction in [-0.703125f64, -0.515625, -0.5, 0.5, 0.515625, 0.703125] {
                let mut stream = PfbStream::with_taps(
                    fs, 16240, vec![0, 1, m - 1], 71, taps,
                ).unwrap();
                let spacing = stream.sample_rate / 2.0;
                let tuned = fraction * spacing;
                let neighbor = if fraction < 0.0 { m - 1 } else { 1 };
                let center = fraction.signum() * spacing;
                let mut bank = FineBank::new(
                    &stream, fs, 16240 / stream.input_hop() + 1, 2,
                    stream.sample_rate, Duration::from_secs(1),
                ).unwrap();
                bank.subscribe(1, 0, -tuned, 4).unwrap();
                bank.subscribe(2, neighbor, center - tuned, 4).unwrap();
                let mut packets = Vec::new();
                for seq in 0..4u64 {
                    let iq: Vec<i16> = (0..16240).flat_map(|n| {
                        let phase = 2.0 * PI * tuned * (seq * 16240 + n) as f64 / fs;
                        [(12000.0 * phase.cos()) as i16, (12000.0 * phase.sin()) as i16]
                    }).collect();
                    loop {
                        match stream.submit(seq, seq * 32480, &iq) {
                            Ok(()) => break,
                            Err(-2) => while let Some(packet) = stream.take() {
                                packets.push(packet);
                            },
                            Err(status) => panic!("submit status {status}"),
                        }
                    }
                }
                stream.finish().unwrap();
                while let Some(packet) = stream.take() {
                    packets.push(packet);
                }
                let mut late = Vec::new();
                for packet in packets {
                    let mut first = None;
                    bank.deliver(&packet, |out| {
                        assert_eq!(out.bin, neighbor);
                        assert_eq!(out.center_offset_hz, Some(tuned));
                        assert_eq!(out.output_rate, stream.sample_rate / 4.0);
                        if out.channel_id == 1 {
                            first = Some((out.output_index, out.iq.to_vec()));
                            if packet.seq >= 2 { late.extend_from_slice(out.iq); }
                        } else {
                            let (clock, iq) = first.as_ref().unwrap();
                            assert_eq!(out.output_index, *clock);
                            assert_eq!(out.iq, iq.as_slice());
                        }
                        Ok(())
                    }).unwrap();
                }
                let power: f64 = late.chunks_exact(2)
                    .map(|z| (z[0] as f64).powi(2) + (z[1] as f64).powi(2)).sum();
                let variation: f64 = late.windows(4).step_by(2)
                    .map(|z| ((z[2] - z[0]) as f64).powi(2) + ((z[3] - z[1]) as f64).powi(2)).sum();
                assert!(power / (late.len() / 2) as f64 > 100.0f64.powi(2));
                assert!(variation / power < 0.001, "fs={fs}, taps={taps}, offset={fraction}");
            }
        }
    }
}

#[test]
fn tap_dependent_passband_rejects_wide_edge_without_changing_channel() {
    for fs in [62_500_000.0, 4_000_000.0] {
        let mut stream = PfbStream::with_taps(fs, 16240, vec![0, 1], 72, 4).unwrap();
        let mut bank = FineBank::new(
            &stream, fs, 16240 / stream.input_hop() + 1, 1,
            stream.sample_rate, Duration::from_secs(1),
        ).unwrap();
        bank.subscribe(1, 0, 0.0, 4).unwrap();
        let iq = vec![1000i16; 2 * 16240];
        for seq in 0..2 { stream.submit(seq, seq * 32480, &iq).unwrap(); }
        stream.finish().unwrap();
        let mut clock = 0;
        bank.deliver(&stream.take().unwrap(), |out| {
            clock += (out.iq.len() / 2) as u64;
            Ok(())
        }).unwrap();
        assert_eq!(bank.retune(1, 2, 0, -stream.sample_rate / 4.0, 4),
            Err("fine channel outside coarse passband"));
        bank.deliver(&stream.take().unwrap(), |out| {
            assert_eq!(out.channel_id, 1);
            assert_eq!(out.output_index, clock);
            assert_eq!(out.center_offset_hz, Some(0.0));
            Ok(())
        }).unwrap();
    }
}

#[test]
fn nyquist_coarse_bin_accepts_both_capture_edges_without_wrapping_rf() {
    for fs in [1_000_000.0, 2_000_000.0] {
        for edge_distance in [0.5, 0.25] {
            let mut stream = PfbStream::new(fs, 16240, vec![0, 1], 73).unwrap();
            let m = stream.bin_count() as u32;
            if m == 4 {
                stream = PfbStream::new(fs, 16240, vec![0, 2], 73).unwrap();
            }
            let tuned = fs / 2.0 - edge_distance * stream.sample_rate / 2.0;
            let mut bank = FineBank::new(
                &stream, fs, 16240 / stream.input_hop() + 1, 2,
                stream.sample_rate, Duration::from_secs(1),
            ).unwrap();
            bank.subscribe(1, 0, -tuned, 4).unwrap();
            bank.subscribe(2, 0, tuned, 4).unwrap();
            let mut clocks = [0u64; 2];
            let mut late = [Vec::<f32>::new(), Vec::<f32>::new()];
            let mut deliver = |packet: PfbOutput| {
                for sign in [-1.0, 1.0] {
                    assert_eq!(bank.retune(1, 3, 0, sign * fs / 2.0, 4),
                        Err("fine channel outside usable IQ bandwidth"));
                    assert_eq!(bank.retune(1, 3, 0, sign * (fs / 2.0 + 1.0), 4),
                        Err("fine channel outside usable IQ bandwidth"));
                }
                bank.deliver(&packet, |out| {
                    let at = (out.channel_id - 1) as usize;
                    assert_eq!(out.bin, m / 2);
                    assert_eq!(out.center_offset_hz, Some(if at == 0 { tuned } else { -tuned }));
                    assert_eq!(out.output_index, clocks[at]);
                    clocks[at] += (out.iq.len() / 2) as u64;
                    if packet.seq >= 2 { late[at].extend_from_slice(out.iq); }
                    Ok(())
                }).unwrap();
            };
            for seq in 0..4u64 {
                let iq: Vec<i16> = (0..16240).flat_map(|n| {
                    let phase = 2.0 * PI * tuned * (seq * 16240 + n) as f64 / fs;
                    [(12000.0 * phase.cos()) as i16, 0]
                }).collect();
                loop {
                    match stream.submit(seq, seq * 32480, &iq) {
                        Ok(()) => break,
                        Err(-2) => while let Some(packet) = stream.take() { deliver(packet); },
                        Err(error) => panic!("PFB submit failed: {error}"),
                    }
                }
            }
            stream.finish().unwrap();
            while let Some(packet) = stream.take() { deliver(packet); }
            drop(deliver);
            for iq in late {
                let power: f64 = iq.chunks_exact(2)
                    .map(|z| (z[0] as f64).powi(2) + (z[1] as f64).powi(2)).sum();
                let variation: f64 = iq.windows(4).step_by(2)
                    .map(|z| ((z[2] - z[0]) as f64).powi(2) + ((z[3] - z[1]) as f64).powi(2)).sum();
                assert!(power / (iq.len() / 2) as f64 > 100.0f64.powi(2));
                assert!(variation / power < 0.001, "fs={fs}, edge_distance={edge_distance}");
            }
        }
    }
}
