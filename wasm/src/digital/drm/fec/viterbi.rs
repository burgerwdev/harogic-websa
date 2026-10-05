//! Soft-input Viterbi decoder for the punctured DRM convolutional code.
//!
//! The encoder starts and ends in the all-zero state (six zero tail bits), so the
//! trellis is initialised in state 0 and traced back from state 0.

use super::conv::mother_output;
use super::BitMetric;
use crate::digital::drm::tables::{CONSTRAINT_LENGTH, NUM_STATES, PunctureMask};

/// Reusable decoder; keeps its decision memory between calls.
#[derive(Debug, Default)]
pub struct ViterbiDecoder {
    decisions: Vec<u64>,
}

/// Mother-code output bits packed into one byte (4 for DRM30, 6 for DRM+ 1/6).
fn output_table<const N: usize>() -> [[u8; 2]; NUM_STATES] {
    let mut t = [[0u8; 2]; NUM_STATES];
    for (s, entry) in t.iter_mut().enumerate() {
        for c in 0..2 {
            let pred = (s >> 1) | (c << 5);
            let reg = (((pred << 1) | (s & 1)) & 0x7F) as u8;
            let mut w = 0u8;
            for j in 0..N {
                w |= mother_output(reg, j) << j;
            }
            entry[c] = w;
        }
    }
    t
}

impl ViterbiDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode. `metrics` holds one entry per transmitted coded bit, in encoder output
    /// order; `masks` is the puncturing table (one per input bit incl. tail). Returns
    /// the decoded information bits (tail removed) and the final path metric
    /// normalised by the number of coded bits (lower is better).
    pub fn decode(&mut self, metrics: &[BitMetric], masks: &[PunctureMask], out: &mut Vec<u8>) -> f64 {
        if masks.iter().any(|&m| m & 0b11_0000 != 0) {
            self.decode_outputs::<6, 64>(metrics, masks, out)
        } else {
            self.decode_outputs::<4, 16>(metrics, masks, out)
        }
    }

    fn decode_outputs<const N: usize, const WORDS: usize>(
        &mut self, metrics: &[BitMetric], masks: &[PunctureMask], out: &mut Vec<u8>,
    ) -> f64 {
        let steps = masks.len();
        let num_bits = steps.saturating_sub(CONSTRAINT_LENGTH - 1);
        let outputs = output_table::<N>();

        self.decisions.clear();
        self.decisions.resize(steps, 0);

        const INF: f64 = 1e30;
        let mut old = [INF; NUM_STATES];
        old[0] = 0.0;
        let mut new = [0.0f64; NUM_STATES];
        let mut pos = 0usize;
        let mut word_metric = [0.0f64; WORDS];

        for (step, &mask) in masks.iter().enumerate() {
            let mut m = [BitMetric::ERASURE; N];
            for (j, mj) in m.iter_mut().enumerate() {
                if mask & (1 << j) != 0 {
                    *mj = metrics.get(pos).copied().unwrap_or(BitMetric::ERASURE);
                    pos += 1;
                }
            }
            for (w, wm) in word_metric.iter_mut().enumerate() {
                let mut acc = 0.0;
                for (j, mj) in m.iter().enumerate() {
                    acc += if (w >> j) & 1 == 0 { mj.to0 } else { mj.to1 };
                }
                *wm = acc;
            }

            let mut dec = 0u64;
            for s in 0..NUM_STATES {
                let p0 = s >> 1;
                let p1 = p0 | 32;
                let m0 = old[p0] + word_metric[outputs[s][0] as usize];
                let m1 = old[p1] + word_metric[outputs[s][1] as usize];
                if m1 < m0 {
                    new[s] = m1;
                    dec |= 1u64 << s;
                } else {
                    new[s] = m0;
                }
            }
            self.decisions[step] = dec;
            core::mem::swap(&mut old, &mut new);
        }

        out.clear();
        out.resize(num_bits, 0);
        let mut state = 0usize;
        for i in 0..num_bits {
            let step = steps - 1 - i;
            let bit = ((self.decisions[step] >> state) & 1) as usize;
            state = (state >> 1) | (bit << 5);
            out[num_bits - 1 - i] = bit as u8;
        }
        old[0] / pos.max(1) as f64
    }
}
