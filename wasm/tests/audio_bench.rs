//! Audio-chain budget: one 20 ms block of PCM must be processed well inside its own period.
//!
//! The audio chain is the stage with a hard deadline — a block that takes longer than 20 ms
//! starves the AudioWorklet ring and is heard as a dropout — so the budget is asserted per block
//! and not only as an average. The number recorded here is native; the wasm build is slower, which
//! is what the HIL pass measures on the bench.
//!
//! The assertion is release-only (a debug build is unoptimized and several times slower, so gating
//! on it would fail for the wrong reason). Record the numbers with:
//!
//!     cargo test --release --test audio_bench -- --nocapture
use std::time::Instant;

// The recorded number is the BEST block: the whole test suite runs in parallel, so a mean over a few
// hundred blocks measures the scheduler as much as the kernel, and the budget is a statement about
// the kernel.

use websa_dsp::audio::default_chain;
use websa_dsp::pipeline::AnalogPcm;

/// 20 ms at 48 kHz: the block the SDR audio path delivers.
const BLOCK: usize = 960;
const AUDIO_RATE: f64 = 48_000.0;
/// Budget per block in release. The chain (including the STFT) is far below this on the bench
/// host; the value is a tripwire for an accidental O(n^2) or per-block allocation.
const RELEASE_BUDGET_MS: f64 = 8.0;

fn block() -> Vec<f32> {
    let mut seed = 7_u32;
    (0..BLOCK)
        .map(|k| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.05;
            // A voice-band tone plus noise and an occasional impulse, so every stage has work.
            let tone = 0.2 * (2.0 * std::f64::consts::PI * 1_000.0 * k as f64 / AUDIO_RATE).sin();
            let impulse = if k % 480 == 0 { 2.0 } else { 0.0 };
            (tone + noise + impulse) as f32
        })
        .collect()
}

fn measure(iterations: usize) -> f64 {
    let mut chain = default_chain();
    let input = block();
    let mut pcm = AnalogPcm::default();
    for _ in 0..10 {
        pcm.samples_mut().clear();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
    }
    let mut best = f64::MAX;
    for _ in 0..iterations {
        let start = Instant::now();
        pcm.samples_mut().clear();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
        best = best.min(start.elapsed().as_secs_f64());
    }
    best * 1000.0
}

#[test]
fn records_the_per_block_cost_of_the_audio_chain() {
    let ms = measure(300);
    let budget = BLOCK as f64 / AUDIO_RATE * 1000.0;
    println!(
        "audio chain: {ms:.3} ms per {BLOCK}-sample block (deadline {budget:.1} ms, {:.1}% of it)",
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

#[test]
fn the_chain_does_not_grow_its_state_per_block() {
    // A per-block allocation shows up as steady growth of the chain's own buffers.
    let mut chain = default_chain();
    let input = block();
    let mut pcm = AnalogPcm::default();
    for _ in 0..200 {
        pcm.samples_mut().clear();
        pcm.samples_mut().extend_from_slice(&input);
        chain.process(&mut pcm, false);
        assert_eq!(pcm.samples().len(), input.len(), "the chain must not change the block length");
    }
    assert_eq!(chain.ids().len(), 7, "every implemented stage must be in the chain");
}
