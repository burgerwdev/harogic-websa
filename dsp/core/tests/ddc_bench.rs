//! Per-block cost of the DDC chain.
//!
//! The number that matters is the *wasm* one, and this measures natively — so it is a
//! regression tripwire, not the performance claim: it catches an accidental O(n^2) (a quadratic
//! convolution, a per-sample allocation) in the kernel before anyone looks at the browser. The
//! real budget checks run against the compiled artifact (the audio block deadline) as the audio
//! and demodulator stages land.
//!
//! The assertion is release-only: a debug build is unoptimized and several times slower, so
//! gating on it would fail for the wrong reason. Run the numbers with:
//!
//!     cargo test --release --test ddc_bench -- --nocapture
use std::time::Instant;

// The recorded number is the BEST block: the whole test suite runs in parallel, so a mean over a few
// hundred blocks measures the scheduler as much as the kernel, and the budget is a statement about
// the kernel.

use websa_dsp::ddc::{Ddc, DdcConfig};

/// One IQS packet is a few thousand complex samples; 4096 is the shape the worker sees.
const BLOCK_SAMPLES: usize = 4096;
/// Budget per block in release: generous (the chain is ~0.2 ms for this block on the bench
/// host), but far below anything that would sound like a dropout.
#[cfg(not(debug_assertions))]
const RELEASE_BUDGET_US: f64 = 2_000.0;

fn block() -> Vec<i16> {
    // A tone plus noise, so the filter and the AGC do real work.
    let mut iq = Vec::with_capacity(BLOCK_SAMPLES * 2);
    let mut seed = 12_345_u32;
    for k in 0..BLOCK_SAMPLES {
        let ph = 2.0 * std::f64::consts::PI * 80_078.125 / 1_000_000.0 * k as f64;
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = ((seed >> 8) as f64 / 16_777_216.0 - 0.5) * 0.002;
        iq.push(((0.4 * ph.cos() + noise) * 32768.0) as i16);
        iq.push(((0.4 * ph.sin() + noise) * 32768.0) as i16);
    }
    iq
}

fn measure(iterations: usize) -> f64 {
    let iq = block();
    let mut ddc = Ddc::new(DdcConfig::for_channel(1_000_000.0, 80_078.125, 48_000.0, 129));
    let mut out = Vec::new();
    // Warm up: the first block pays for the tap design and the allocations.
    ddc.process_i16_into(&iq, &mut out);
    let mut best = f64::MAX;
    for _ in 0..iterations {
        let start = Instant::now();
        ddc.process_i16_into(&iq, &mut out);
        best = best.min(start.elapsed().as_secs_f64());
    }
    best
}

#[test]
fn records_the_per_block_cost_of_the_ddc_chain() {
    let iterations = 200;
    let seconds = measure(iterations);
    let us = seconds * 1.0e6;
    let ns_per_complex_sample = seconds * 1.0e9 / BLOCK_SAMPLES as f64;
    let realtime_ratio = seconds / (BLOCK_SAMPLES as f64 / 1_000_000.0);
    println!(
        "ddc chain: {us:.1} us per {BLOCK_SAMPLES}-sample block \
         ({ns_per_complex_sample:.1} ns/complex sample, {:.1}x real time at 1 MSps)",
        realtime_ratio
    );
    assert!(seconds > 0.0, "the measurement must actually run");

    #[cfg(not(debug_assertions))]
    assert!(
        us < RELEASE_BUDGET_US,
        "the DDC chain takes {us:.1} us per block (budget {RELEASE_BUDGET_US} us): \
         something became quadratic or started allocating per sample"
    );
    #[cfg(debug_assertions)]
    println!("(debug build: the budget assertion is release-only)");
}

#[test]
fn the_chain_does_not_grow_its_memory_per_block() {
    // A per-block allocation in the hot path is invisible in a single measurement but shows up
    // as steady growth here. The first blocks are warm-up: a resampler carries its fractional
    // phase across blocks, so the produced length alternates between two values and the buffer
    // legitimately grows to the larger one before it settles.
    let iq = block();
    let mut ddc = Ddc::new(DdcConfig::for_channel(1_000_000.0, 80_078.125, 48_000.0, 129));
    let mut out = Vec::new();
    for _ in 0..5 {
        ddc.process_i16_into(&iq, &mut out);
    }
    let capacity = out.capacity();
    for _ in 0..50 {
        ddc.process_i16_into(&iq, &mut out);
    }
    assert_eq!(out.capacity(), capacity, "the output buffer must be reused, not reallocated");
}
