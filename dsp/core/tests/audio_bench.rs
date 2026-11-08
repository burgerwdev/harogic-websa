//! Native listening-NR kernel budget for a 20 ms PCM block.
//!
//! The minimum observed block time limits scheduler interference in the kernel regression.
//! It does not prove that every live block meets its deadline, or establish browser/WASM cost.
//!
//! The assertion is release-only (a debug build is unoptimized and several times slower, so gating
//! on it would fail for the wrong reason). Record the numbers with:
//!
//!     cargo test --release -j 1 --test audio_bench -- --test-threads=1 --nocapture
use std::time::Instant;

// The recorded number is the BEST block to reduce interference from other host work.
// This kernel regression does not measure deadline misses or sustained throughput.

use websa_dsp::audio::{chain_for_mode, AudioPolicy, SQUELCH_OPEN_DBM};
use websa_dsp::pipeline::AnalogPcm;

/// 20 ms at 48 kHz: the block the SDR audio path delivers.
const BLOCK: usize = 960;
const AUDIO_RATE: f64 = 48_000.0;
/// Budget per block in release. The chain (including the STFT) is far below this on the bench
/// host; the value is a tripwire for an accidental O(n^2) or per-block allocation.
#[cfg(not(debug_assertions))]
const RELEASE_BUDGET_MS: f64 = 8.0;

fn block() -> Vec<f32> {
    let mut seed = 7_u32;
    (0..BLOCK)
        .map(|k| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.05;
            // A voice-band tone plus noise and an occasional impulse for Wiener and blanker.
            let tone = 0.2 * (2.0 * std::f64::consts::PI * 1_000.0 * k as f64 / AUDIO_RATE).sin();
            let impulse = if k % 480 == 0 { 2.0 } else { 0.0 };
            (tone + noise + impulse) as f32
        })
        .collect()
}

fn measure(iterations: usize) -> f64 {
    let mut chain = chain_for_mode("am", AUDIO_RATE,
        AudioPolicy { nr: true, nr_strength: 1.0, squelch_dbm: SQUELCH_OPEN_DBM });
    assert_eq!(chain.active_ids(), vec!["wiener", "blanker"]);
    let input = block();
    let mut pcm = AnalogPcm::default();
    for _ in 0..10 {
        pcm.samples_mut().clear();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
    }
    assert_ne!(pcm.samples(), input.as_slice(), "the benchmark must actually process audio");
    let mut best = f64::MAX;
    for _ in 0..iterations {
        let start = Instant::now();
        pcm.samples_mut().clear();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
        best = best.min(start.elapsed().as_secs_f64());
        assert_eq!(pcm.samples().len(), input.len(), "repeated processing must preserve the sample count");
    }
    best * 1000.0
}

#[test]
fn records_the_per_block_cost_of_the_audio_chain() {
    let ms = measure(300);
    let budget = BLOCK as f64 / AUDIO_RATE * 1000.0;
    println!(
        "audio chain: best {ms:.3} ms per {BLOCK}-sample block (period {budget:.1} ms, {:.1}% of it)",
        ms / budget * 100.0
    );
    assert!(ms > 0.0);
    #[cfg(not(debug_assertions))]
    assert!(
        ms < RELEASE_BUDGET_MS,
        "the audio chain takes {ms:.2} ms per {BLOCK}-sample block (budget {RELEASE_BUDGET_MS} ms)"
    );
    #[cfg(debug_assertions)]
    println!("(debug build: the budget assertion is release-only)");
}
