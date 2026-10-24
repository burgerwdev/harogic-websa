use super::FineBank;
use crate::{BasebandFramer, PfbStream};
use std::f64::consts::PI;
use std::time::Duration;
use websa_dsp::pipeline_abi::{
    websa_dsp_demod_free, websa_dsp_demod_new, websa_dsp_demod_process, websa_dsp_demod_retune,
};

fn feed_frame(
    frame: &[u8],
    handle: u32,
    audio: &mut Vec<f32>,
    seqs: &mut Vec<u32>,
    samples: &mut usize,
) {
    assert_eq!(&frame[..4], b"IQBF");
    let seq = u32::from_le_bytes(frame[8..12].try_into().unwrap());
    let count = u32::from_le_bytes(frame[12..16].try_into().unwrap()) as usize;
    assert_eq!(frame.len(), 56 + 8 * count);
    seqs.push(seq);
    if seq == 0 {
        assert_eq!(count, 0);
        assert_eq!(websa_dsp_demod_retune(handle), 1);
        return;
    }
    *samples += count;
    let iq: Vec<f32> = frame[56..]
        .chunks_exact(4)
        .map(|part| f32::from_le_bytes(part.try_into().unwrap()))
        .collect();
    let mut pcm = vec![0.0; count * 2 + 4096];
    let written = unsafe {
        websa_dsp_demod_process(
            handle,
            iq.as_ptr(),
            count as u32,
            pcm.as_mut_ptr(),
            pcm.len() as u32,
            0,
        )
    };
    audio.extend_from_slice(&pcm[..written as usize]);
}

fn tone_level(pcm: &[f32], hz: f64) -> f64 {
    let n = pcm.len() as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (at, &sample) in pcm.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * at as f64 / n).cos();
        let phase = 2.0 * PI * hz * at as f64 / 48_000.0;
        re += sample as f64 * w * phase.cos();
        im -= sample as f64 * w * phase.sin();
    }
    (re * re + im * im).sqrt() / n
}

#[test]
fn fullband_am_pfb_fine_wire_wasm_recovers_audio_without_sample_holes() {
    const FS: f64 = 62_500_000.0;
    const N: usize = 16240;
    let mut stream = PfbStream::new(FS, N, vec![0], 55).unwrap();
    let rate = stream.sample_rate / 12.0;
    let mut bank = FineBank::new(
        &stream,
        50_000_000.0,
        N / 64 + 2,
        1,
        rate,
        Duration::from_secs(1),
    )
    .unwrap();
    bank.subscribe(1, 0, 0.0, 12).unwrap();
    let mut framer = BasebandFramer::new(1, 55, 100_000_000.0, rate).unwrap();
    let mode = b"am";
    let handle = unsafe {
        websa_dsp_demod_new(
            rate,
            48_000.0,
            mode.as_ptr(),
            mode.len() as u32,
            12_000.0,
            700.0,
        )
    };
    assert_ne!(handle, 0, "browser AM pipeline unavailable");
    let envelope: Vec<i16> = (0..62500)
        .map(|at| (10_000.0 * (1.0 + 0.4 * (2.0 * PI * at as f64 / 62500.0).sin())) as i16)
        .collect();
    let (mut audio, mut seqs, mut framed_samples, mut fine_samples) =
        (Vec::new(), Vec::new(), 0, 0);
    let mut next_seq = 0;
    let mut fine_clock = 0;
    let mut drain = |stream: &mut PfbStream| {
        while let Some(packet) = stream.take() {
            assert_eq!(packet.seq, next_seq);
            next_seq += 1;
            bank.deliver(&packet, |out| {
                assert_eq!(out.output_index, fine_clock);
                fine_clock += (out.iq.len() / 2) as u64;
                fine_samples += out.iq.len() / 2;
                for frame in framer.push(out).unwrap() {
                    feed_frame(&frame, handle, &mut audio, &mut seqs, &mut framed_samples);
                }
                Ok(())
            })
            .unwrap();
        }
    };
    let packets = (FS * 0.12 / N as f64).ceil() as u64;
    for seq in 0..packets {
        let mut iq = Vec::with_capacity(2 * N);
        for sample in 0..N {
            iq.push(envelope[((seq as usize * N + sample) % 62500) as usize]);
            iq.push(0);
        }
        loop {
            match stream.submit(seq, 1000 + seq * (2 * N) as u64, &iq) {
                Ok(()) => break,
                Err(-2) => drain(&mut stream),
                err => panic!("PFB submit: {err:?}"),
            }
        }
        drain(&mut stream);
    }
    stream.finish().unwrap();
    drain(&mut stream);
    drop(drain);
    if let Some(tail) = framer.finish().unwrap() {
        feed_frame(&tail, handle, &mut audio, &mut seqs, &mut framed_samples);
    }
    assert_eq!(websa_dsp_demod_free(handle), 1);
    assert_eq!(next_seq, packets);
    assert_eq!(framed_samples, fine_samples);
    assert_eq!(seqs[0], 0);
    assert_eq!(seqs[1..], (1..seqs.len() as u32).collect::<Vec<_>>());
    assert!(audio.len() > 4_000, "PCM too short: {}", audio.len());
    let stable = &audio[audio.len() / 3..];
    let peak = (950..=1050)
        .step_by(5)
        .max_by(|&a, &b| tone_level(stable, a as f64).total_cmp(&tone_level(stable, b as f64)))
        .unwrap();
    let signal = tone_level(stable, peak as f64);
    let unwanted = tone_level(stable, 1700.0);
    assert!((peak as i32 - 1000).abs() <= 20, "AM audio peak={peak} Hz");
    assert!(
        signal > unwanted * 5.0,
        "AM tone={signal}, unwanted={unwanted}"
    );
    assert!(audio.iter().all(|v| v.is_finite()));
}
