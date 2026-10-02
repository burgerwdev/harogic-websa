//! Per-bit puncturing masks for one MLC level (ES 201 980 §7.3.1).

use crate::digital::drm::tables::{CODE_RATES, CONSTRAINT_LENGTH, TAIL_PATTERNS, PunctureMask};

/// How the tail-bit pattern index is derived for a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailRule {
    /// Standard (SM) and 4/16-QAM: uses 2·N₂ for every level.
    Standard,
    /// HMsym: level 0 uses 2·(N₁+N₂), other levels 2·N₂.
    HmSym,
    /// HMmix: level 0 uses N₁+N₂, other levels N₂.
    HmMix,
}

/// Parameters needed to build one level's puncturing mask table.
#[derive(Debug, Clone, Copy)]
pub struct PunctureSpec {
    pub n1: usize,
    pub n2: usize,
    pub bits_a: usize,
    pub bits_b: usize,
    pub rate_a: usize,
    pub rate_b: usize,
    pub level: usize,
    pub tail_rule: TailRule,
    pub is_fac: bool,
}

/// One puncturing mask per encoder input bit, including the six tail bits.
pub fn mask_table(spec: &PunctureSpec) -> Vec<PunctureMask> {
    let num_bits = spec.bits_a + spec.bits_b;
    let with_tail = num_bits + CONSTRAINT_LENGTH - 1;

    let tail_param = match (spec.tail_rule, spec.level) {
        (TailRule::HmMix, 0) => spec.n1 + spec.n2,
        (TailRule::HmMix, _) => spec.n2,
        (TailRule::HmSym, 0) => 2 * (spec.n1 + spec.n2),
        _ => 2 * spec.n2,
    };
    let ry_b = CODE_RATES[spec.rate_b].ry;
    let base = tail_param as i64 - 12;
    let tail_index = (base - ry_b as i64 * (base / ry_b as i64)) as usize;

    let pat_a = CODE_RATES[spec.rate_a].pattern;
    let pat_b = CODE_RATES[spec.rate_b].pattern;
    let tail = TAIL_PATTERNS.get(tail_index).copied().unwrap_or(TAIL_PATTERNS[0]);

    let mut out = Vec::with_capacity(with_tail);
    for i in 0..with_tail {
        if i < spec.bits_a {
            out.push(pat_a[i % pat_a.len()]);
        } else if i < num_bits || spec.is_fac {
            out.push(pat_b[(i - spec.bits_a) % pat_b.len()]);
        } else {
            out.push(tail[i - num_bits]);
        }
    }
    out
}

/// Number of coded bits produced by a mask table.
pub fn coded_len(masks: &[PunctureMask]) -> usize {
    masks.iter().map(|m| m.count_ones() as usize).sum()
}
