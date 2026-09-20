//! One-pole DC blocker — the first stage of the analog audio chain.
//!
//! A DC offset is what makes AM audio sit off-centre and what wastes headroom in every later
//! stage, so it is removed first. The recursion is the reference's
//! (`web_sa/demod/demod.py`, the AM branch): `y[n] = a*y[n-1] + a*(x[n] - x[n-1])` with
//! `a = 0.9995` at audio rate, run in f64 so a long block cannot accumulate drift.

use crate::plugin::AudioStage;

/// Coefficient of the reference implementation at 48 kHz (~3.3 Hz corner).
pub const DEFAULT_A: f64 = 0.9995;

pub struct DcBlock {
    a: f64,
    y: f64,
    prev: Option<f32>,
}

impl DcBlock {
    pub fn new(a: f64) -> Self {
        Self { a, y: 0.0, prev: None }
    }

    pub fn reference() -> Self {
        Self::new(DEFAULT_A)
    }
}

impl AudioStage for DcBlock {
    fn id(&self) -> &'static str {
        "dc_block"
    }

    fn process_into(&mut self, input: &[f32], _hold: bool, out: &mut Vec<f32>) {
        out.clear();
        if input.is_empty() {
            return;
        }
        out.reserve(input.len());
        let mut prev = self.prev.unwrap_or(input[0]);
        for sample in input {
            let delta = (*sample - prev) * self.a as f32;
            // The recursion itself is f64 (the reference does the same) so a slow fade cannot
            // lose precision sample by sample.
            self.y = self.a * self.y + delta as f64;
            prev = *sample;
            out.push(self.y as f32);
        }
        self.prev = Some(prev);
    }

    fn reset(&mut self) {
        self.y = 0.0;
        self.prev = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dc_offset_is_removed_while_a_tone_survives() {
        // 1000 Hz tone riding on a +0.4 offset: after the transient the mean must be ~0 and the
        // tone must still be there.
        let mut stage = DcBlock::reference();
        let mut out = Vec::new();
        let n = 4800;
        let input: Vec<f32> = (0..n)
            .map(|k| 0.4 + 0.2 * (2.0 * core::f32::consts::PI * 1000.0 * k as f32 / 48_000.0).sin())
            .collect();
        stage.process_into(&input, false, &mut out);
        let tail = &out[n / 2..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(mean.abs() < 1e-3, "DC must be gone, mean {mean}");
        let peak = tail.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.15, "the tone must survive, peak {peak}");
    }

    #[test]
    fn the_block_boundary_is_seamless() {
        // Same input, one block vs two: the recursion state must carry over.
        let input: Vec<f32> = (0..2048).map(|k| (k as f32 * 0.01).sin() + 0.3).collect();
        let mut whole = DcBlock::reference();
        let mut one = Vec::new();
        whole.process_into(&input, false, &mut one);

        let mut split = DcBlock::reference();
        let mut a = Vec::new();
        let mut b = Vec::new();
        split.process_into(&input[..1024], false, &mut a);
        split.process_into(&input[1024..], false, &mut b);
        a.extend_from_slice(&b);
        for (index, (u, v)) in one.iter().zip(a.iter()).enumerate() {
            assert!((u - v).abs() < 1e-6, "sample {index}: {u} vs {v}");
        }
    }

    #[test]
    fn reset_clears_the_state() {
        let mut stage = DcBlock::reference();
        let mut out = Vec::new();
        stage.process_into(&[1.0, 1.0, 1.0], false, &mut out);
        stage.reset();
        let mut again = Vec::new();
        stage.process_into(&[1.0, 1.0, 1.0], false, &mut again);
        assert_eq!(out, again);
    }
}
