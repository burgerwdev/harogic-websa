//! Shared scenario harness for the FT8 decoder tests: deterministic noise, frequency shifts, and
//! the slot builder that places a transmission at a chosen in-band SNR. No dependencies, fixed
//! seeds — the same code produces the same samples everywhere, which is what makes the committed
//! sensitivity baselines a contract instead of a hope.
//!
//! SNR here is **in-band**: signal mean power against noise power in the 100-3000 Hz band the
//! waterfall reads (white noise at 48 kHz scaled so its in-band share is exact), within ~0.6 dB of
//! WSJT-X's 2500 Hz reference convention.
#![allow(dead_code)]

pub const RATE: f64 = 48_000.0;
/// One FT8 slot: the buffer a decode runs over.
pub const SLOT_SAMPLES: usize = 720_000;
/// The waterfall's analysis band: what "in-band" noise means.
pub const BAND_HZ: f64 = 3_000.0 - 100.0;

/// Deterministic PRNG (xorshift64*): no dependencies, same sequence everywhere.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Standard normal, Box-Muller (both values of the pair, cached).
    pub fn normal(&mut self) -> f64 {
        fn unit(rng: &mut Rng) -> f64 {
            // 53 random bits -> uniform in (0, 1]: the open lower bound keeps log() finite.
            ((rng.next_u64() >> 11) as f64 + 0.5) * (1.0 / 9_007_199_254_740_992.0)
        }
        let r = (-2.0 * unit(self).ln()).sqrt();
        let theta = core::f64::consts::TAU * unit(self);
        r * theta.cos()
    }
}

/// Interleaved complex f32 — the decoder's input format — from a committed fixture file.
pub fn load_iq(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// White complex Gaussian noise, `samples` complex values, mean power exactly `power`. The decoder
/// only reads the 100-3000 Hz bins, so what it *sees* of this buffer is
/// `power * BAND_HZ / RATE` — callers pass that pre-scaled total.
pub fn noise(samples: usize, power: f64, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut iq = vec![0.0_f32; samples * 2];
    let sigma = (power / 2.0).sqrt(); // split between I and Q: E[|z|^2] = power
    for chunk in iq.chunks_exact_mut(2) {
        chunk[0] = (rng.normal() * sigma) as f32;
        chunk[1] = (rng.normal() * sigma) as f32;
    }
    // Normalize the sample mean power to exactly `power`: the seed decides the waveform, not the
    // achieved level, so every trial at one SNR is the same SNR.
    let measured = power_of(&iq);
    let scale = (power / measured).sqrt();
    for value in iq.iter_mut() {
        *value = (*value as f64 * scale) as f32;
    }
    iq
}

/// Mean power of an interleaved complex buffer.
pub fn power_of(iq: &[f32]) -> f64 {
    let n = iq.len() / 2;
    let mut p = 0.0_f64;
    for chunk in iq.chunks_exact(2) {
        p += chunk[0] as f64 * chunk[0] as f64 + chunk[1] as f64 * chunk[1] as f64;
    }
    p / n as f64
}

/// Frequency-shift `iq` by `hz` (exact per-sample rotation).
pub fn shift(iq: &[f32], hz: f64) -> Vec<f32> {
    let step = core::f64::consts::TAU * hz / RATE;
    let mut out = vec![0.0_f32; iq.len()];
    for (n, chunk) in iq.chunks_exact(2).enumerate() {
        let (s, c) = (step * n as f64).sin_cos();
        let re = chunk[0] as f64;
        let im = chunk[1] as f64;
        out[n * 2] = (re * c - im * s) as f32;
        out[n * 2 + 1] = (re * s + im * c) as f32;
    }
    out
}

/// Scale `iq` so its mean power becomes `amp_db` decibels relative to its current level.
pub fn at_amp_db(iq: &[f32], amp_db: f64) -> Vec<f32> {
    let scale = 10.0_f64.powf(amp_db / 20.0);
    iq.iter().map(|v| (*v as f64 * scale) as f32).collect()
}

/// One scenario buffer: noise at `noise_power` (total) + `signal` inserted at `snr_db` in-band,
/// `insert_s` into the slot, shifted by `hz`. For pre-scaled signals (already at their level) use
/// `snr_db = 0.0` and fold the level into `signal` with [`at_amp_db`].
pub fn scenario(
    noise_iq: &[f32],
    noise_power: f64,
    signal: &[f32],
    snr_db: f64,
    insert_s: f64,
    hz: f64,
) -> Vec<f32> {
    let sig_power = power_of(signal);
    let amp = (sig_power.recip() * noise_power * 10.0_f64.powf(snr_db / 10.0)).sqrt();
    let mut slot = noise_iq.to_vec();
    let shifted = shift(signal, hz);
    let start = (insert_s * RATE) as usize * 2;
    for (i, value) in shifted.iter().enumerate() {
        slot[start + i] += (*value as f64 * amp) as f32;
    }
    slot
}

/// Add an already-scaled `signal` into `slot` at `insert_s`, shifted by `hz`. No scaling: the
/// first signal of a scenario goes through [`scenario`] (which scales to an SNR), and every
/// further one is scaled with [`at_amp_db`] and inserted here.
pub fn insert(slot: &mut [f32], signal: &[f32], insert_s: f64, hz: f64) {
    let shifted = shift(signal, hz);
    let start = (insert_s * RATE) as usize * 2;
    for (i, value) in shifted.iter().enumerate() {
        slot[start + i] += *value;
    }
}

/// Noise power (total, pre-scaling) that puts a unit-power signal at `snr_db` in-band.
pub fn noise_power_for(snr_db: f64) -> f64 {
    10.0_f64.powf(-snr_db / 10.0) * RATE / BAND_HZ
}
