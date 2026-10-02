//! QAM soft demapping.
//!
//! DRM uses square QAM (4/16/64) with the axis levels of `tables::QAM*`. Each axis
//! carries `log2(levels)` bits; the level index is the bit label with the most
//! significant bit first (e.g. 16-QAM index `0b10` → bits `1,0`). A max-log LLR is
//! returned per bit (unscaled by noise variance, which the Viterbi decoder does not
//! need): **positive LLR → bit 1**, negative → bit 0.

/// Soft bits (max-log LLRs) for one QAM symbol. Returns 2·log2(levels.len()) values:
/// all in-phase-axis bits first (MSB first), then the quadrature-axis bits.
pub fn soft_bits(re: f64, im: f64, levels: &[f64]) -> Vec<f64> {
    let bits_per_axis = levels.len().trailing_zeros() as usize;
    let mut out = Vec::with_capacity(2 * bits_per_axis);
    for b in 0..bits_per_axis {
        out.push(axis_llr(re, levels, b));
    }
    for b in 0..bits_per_axis {
        out.push(axis_llr(im, levels, b));
    }
    out
}

/// Hard decision: the nearest level index for a single axis value.
pub fn hard_axis(v: f64, levels: &[f64]) -> usize {
    levels
        .iter()
        .enumerate()
        .min_by(|a, b| (v - a.1).abs().total_cmp(&(v - b.1).abs()))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Max-log LLR for one axis bit: `min_{bit=0} d² − min_{bit=1} d²` (positive → bit 1).
fn axis_llr(v: f64, levels: &[f64], bit: usize) -> f64 {
    let bits = levels.len().trailing_zeros() as usize;
    let mask = 1usize << (bits - 1 - bit);
    let mut d0 = f64::INFINITY;
    let mut d1 = f64::INFINITY;
    for (idx, &lvl) in levels.iter().enumerate() {
        let d = (v - lvl) * (v - lvl);
        if idx & mask == 0 {
            d0 = d0.min(d);
        } else {
            d1 = d1.min(d);
        }
    }
    d0 - d1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm::tables::{QAM16, QAM4, QAM64_SM};

    #[test]
    fn qam4_sign_and_magnitude() {
        let l = soft_bits(QAM4[0], 0.0, &QAM4); // near +1/√2 → I bit 0
        assert!(l[0] < 0.0, "near level 0 → bit 0 (negative LLR)");
        let l = soft_bits(QAM4[1], 0.0, &QAM4); // near −1/√2 → I bit 1
        assert!(l[0] > 0.0, "near level 1 → bit 1 (positive LLR)");
        assert_eq!(soft_bits(0.0, 0.0, &QAM4).len(), 2);
        assert_eq!(soft_bits(0.0, 0.0, &QAM16).len(), 4);
        assert_eq!(soft_bits(0.0, 0.0, &QAM64_SM).len(), 6);
    }

    #[test]
    fn hard_axis_picks_nearest() {
        assert_eq!(hard_axis(QAM4[1], &QAM4), 1);
        assert_eq!(hard_axis(QAM16[2], &QAM16), 2);
    }
}
