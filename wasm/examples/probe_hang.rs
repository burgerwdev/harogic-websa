// Stream baseband from stdin (raw interleaved f32) through the receiver in
// worker-sized blocks and print progress, to catch a hang on the live path.
//
//   python3 - <<pump | probe_hang 48828.125
use std::io::{Read, Write};

use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::DrmReceiver;

fn main() {
    let in_rate: f64 = std::env::args()
        .nth(1)
        .and_then(|r| r.parse().ok())
        .unwrap_or(48828.125);
    let mut stdin = std::io::stdin();
    let mut rx = DrmReceiver::new();
    let mut fed = 0usize;
    let mut resampler = ComplexResampler::new(in_rate, 48_000.0);
    let mut buf = vec![0u8; 3248 * 2 * 4];
    'outer: loop {
        let mut got = 0usize;
        while got < buf.len() {
            let n = match stdin.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break 'outer,
                Ok(n) => n,
            };
            got += n;
        }
        let iq: Vec<f32> = buf
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut converted = Vec::new();
        resampler.process_f32_into(&iq, &mut converted);
        for block in converted.chunks(3248 * 2) {
            let t0 = std::time::Instant::now();
            rx.push(block);
            rx.run();
            fed += block.len() / 2;
            let dt = t0.elapsed();
            if dt.as_millis() > 500 || fed % (3248 * 40) == 0 {
                println!(
                    "fed={:.2}s push+run took {:?} locked={} au={} facs={} snr={:?}",
                    fed as f64 / 48000.0,
                    dt,
                    rx.locked(),
                    rx.audio_access_units.len(),
                    rx.facs.len(),
                    rx.snr_db
                );
                let _ = std::io::stdout().flush();
            }
        }
    }
    println!(
        "DONE fed={:.2}s au={} facs={}",
        fed as f64 / 48000.0,
        rx.audio_access_units.len(),
        rx.facs.len()
    );
}
