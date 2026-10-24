//! Direct Rust PFB baseline against the same packet bytes as the Python/ctypes replay.
use std::{env, fs, time::Instant};
use websa_channelizer::{pfb_close, pfb_geometry, pfb_open_os, pfb_process};

const FS: f64 = 31_250_000.0;
const PACKET: usize = 16_240;

fn input() -> Vec<i16> {
    if let Ok(path) = env::var("WEBSA_PFB_IQ") {
        let bytes = fs::read(path).expect("recorded IQ fixture");
        assert!(!bytes.is_empty() && bytes.len() % (PACKET * 4) == 0);
        return bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
    }
    // CI fallback: deterministic complex IQ, not claimed equivalent to a hardware recording.
    let mut seed = 20261019u32;
    (0..120 * PACKET * 2)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as i16
        })
        .collect()
}

fn quantile(sorted: &[f64], percentile: f64) -> f64 {
    let at = ((sorted.len() - 1) as f64 * percentile).round() as usize;
    sorted[at]
}

#[test]
fn direct_rust_pfb_packet_baseline() {
    let raw = input();
    let blocks: Vec<_> = raw.chunks_exact(PACKET * 2).collect();
    let repeats = if cfg!(debug_assertions) { 1 } else { 12 };
    for oversample in [2u32, 4] {
        let handle = pfb_open_os(FS, oversample);
        assert!(!handle.is_null());
        let (mut bins, mut hop) = (0u32, 0u32);
        let (mut rate, mut delay) = (0f64, 0f64);
        assert_eq!(
            unsafe { pfb_geometry(handle, &mut bins, &mut hop, &mut rate, &mut delay) },
            0
        );
        let selected = [0u32];
        let mut output = vec![0f32; 2 * (PACKET / hop as usize + 2)];
        for block in blocks.iter().cycle().take(8) {
            assert!(
                unsafe {
                    pfb_process(
                        handle,
                        block.as_ptr(),
                        PACKET as i32,
                        selected.as_ptr(),
                        1,
                        output.as_mut_ptr(),
                        output.len() as i32 / 2,
                    )
                } > 0
            );
        }
        let mut samples_ms = Vec::new();
        let mut groups = 0usize;
        let started = Instant::now();
        for _ in 0..repeats {
            for block in &blocks {
                let before = Instant::now();
                let got = unsafe {
                    pfb_process(
                        handle,
                        block.as_ptr(),
                        PACKET as i32,
                        selected.as_ptr(),
                        1,
                        output.as_mut_ptr(),
                        output.len() as i32 / 2,
                    )
                };
                samples_ms.push(before.elapsed().as_secs_f64() * 1000.0);
                assert!(got >= 0);
                groups += got as usize;
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        assert_eq!(groups, repeats * blocks.len() * PACKET / hop as usize);
        samples_ms.sort_by(f64::total_cmp);
        println!("Rust direct PFB {oversample}x: p50={:.4} p95={:.4} p99={:.4} max={:.4} ms, throughput={:.2} MSps, packets={}",
                 quantile(&samples_ms, 0.50), quantile(&samples_ms, 0.95),
                 quantile(&samples_ms, 0.99), *samples_ms.last().unwrap(),
                 (repeats * blocks.len() * PACKET) as f64 / elapsed / 1e6, samples_ms.len());
        unsafe {
            pfb_close(handle);
        }
    }
}
