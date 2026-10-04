//! QAM mapping (ES 201 980 §7.4) and per-level soft metrics for the multilevel
//! decoder. DRM maps each MLC level's coded bits onto one bit of the per-axis PAM
//! index (most significant first); HMmix splits in-phase and quadrature into six
//! single-bit levels.
//!
//! The metric is the squared Euclidean distance weighted by the channel power |H|²
//! (the Gaussian log-likelihood); this is the metric used throughout.

use super::BitMetric;
use crate::digital::drm2::tables;
use crate::digital::drm2::dsp::Cplx;

/// Constellation / mapping scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    Qam4,
    Qam16,
    Qam64Sm,
    Qam64HmSym,
    Qam64HmMix,
}

/// An equalised cell with its channel state information.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EqCell {
    /// Received cell divided by the channel estimate.
    pub sig: Cplx,
    /// Channel power |H|² at this cell (the reliability weight).
    pub chan: f64,
}

impl Mapping {
    pub const fn levels(self) -> usize {
        match self {
            Self::Qam4 => 1,
            Self::Qam16 => 2,
            Self::Qam64Sm | Self::Qam64HmSym => 3,
            Self::Qam64HmMix => 6,
        }
    }

    pub const fn axis_bits(self) -> usize {
        match self {
            Self::Qam4 => 1,
            Self::Qam16 => 2,
            _ => 3,
        }
    }

    /// Number of coded bits one level carries in `cells` cells.
    pub const fn coded_bits_per_level(self, cells: usize) -> usize {
        match self {
            Self::Qam64HmMix => cells,
            _ => 2 * cells,
        }
    }

    fn pam(self, axis: usize) -> &'static [f64] {
        match self {
            Self::Qam4 => &tables::QAM4,
            Self::Qam16 => &tables::QAM16,
            Self::Qam64Sm => &tables::QAM64_SM,
            Self::Qam64HmSym => &tables::QAM64_HMSYM,
            Self::Qam64HmMix => {
                if axis == 0 { &tables::QAM64_HMMIX_RE } else { &tables::QAM64_HMMIX_IM }
            }
        }
    }

    /// The PAM index bit a level controls (0 = LSB).
    fn level_bit(self, level: usize) -> usize {
        match self {
            Self::Qam64HmMix => 2 - level / 2,
            _ => self.axis_bits() - 1 - level,
        }
    }

    /// For HMmix, the axis a level lives on.
    fn level_axis(self, level: usize) -> Option<usize> {
        match self {
            Self::Qam64HmMix => Some(level % 2),
            _ => None,
        }
    }

    /// Map the coded bit streams of all levels onto cells. `levels[j]` holds
    /// [`Self::coded_bits_per_level`] bits.
    pub fn map(self, levels: &[Vec<u8>], out: &mut [Cplx]) {
        let nl = self.levels();
        for (i, cell) in out.iter_mut().enumerate() {
            let mut idx = [0usize; 2];
            for (j, bits) in levels.iter().enumerate().take(nl) {
                let b = self.level_bit(j);
                match self.level_axis(j) {
                    Some(axis) => idx[axis] |= usize::from(bits[i] & 1) << b,
                    None => {
                        idx[0] |= usize::from(bits[2 * i] & 1) << b;
                        idx[1] |= usize::from(bits[2 * i + 1] & 1) << b;
                    }
                }
            }
            *cell = Cplx::new(self.pam(0)[idx[0]], self.pam(1)[idx[1]]);
        }
    }

    /// Soft metrics (squared distance) for one level. `decided[j]` holds the
    /// re-encoded coded bits of level `j` from the current pass (`j < level`) or, when
    /// `iteration` is true, from the previous pass (`j > level`). Levels above `level`
    /// are left free on the first pass.
    pub fn metrics(
        self,
        cells: &[EqCell],
        level: usize,
        decided: &[Vec<u8>],
        iteration: bool,
        out: &mut Vec<BitMetric>,
    ) {
        let nl = self.levels();
        let target_bit = self.level_bit(level);
        out.clear();
        out.reserve(self.coded_bits_per_level(cells.len()));

        // PAM index bits already known from other levels at coded-bit position k.
        let known = |k: usize| -> (usize, usize) {
            let mut mask = 0usize;
            let mut val = 0usize;
            for j in 0..nl {
                if j == level || (j > level && !iteration) {
                    continue;
                }
                if let (Some(a), Some(aj)) = (self.level_axis(level), self.level_axis(j)) {
                    if a != aj {
                        continue;
                    }
                }
                let Some(bits) = decided.get(j) else { continue };
                let Some(&bit) = bits.get(k) else { continue };
                let b = self.level_bit(j);
                mask |= 1 << b;
                val |= usize::from(bit & 1) << b;
            }
            (mask, val)
        };

        let mut push = |a: f64, w: f64, table: &[f64], k: usize| {
            let (mask, val) = known(k);
            let mut best = [f64::INFINITY; 2];
            for (idx, &s) in table.iter().enumerate() {
                if idx & mask != val {
                    continue;
                }
                let d = (a - s) * (a - s) * w;
                let bit = (idx >> target_bit) & 1;
                if d < best[bit] {
                    best[bit] = d;
                }
            }
            out.push(BitMetric { to0: best[0], to1: best[1] });
        };

        match self.level_axis(level) {
            Some(axis) => {
                let table = self.pam(axis);
                for (i, c) in cells.iter().enumerate() {
                    let a = if axis == 0 { c.sig.re } else { c.sig.im };
                    push(a, c.chan, table, i);
                }
            }
            None => {
                let (t_re, t_im) = (self.pam(0), self.pam(1));
                for (i, c) in cells.iter().enumerate() {
                    push(c.sig.re, c.chan, t_re, 2 * i);
                    push(c.sig.im, c.chan, t_im, 2 * i + 1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_round_trips_through_metrics() {
        for m in [Mapping::Qam4, Mapping::Qam16, Mapping::Qam64Sm] {
            let cells = 64;
            let nb = m.coded_bits_per_level(cells);
            let levels: Vec<Vec<u8>> = (0..m.levels())
                .map(|j| (0..nb).map(|k| ((k * 31 + j * 7) % 5 % 2) as u8).collect())
                .collect();
            let mut sym = vec![Cplx::new(0.0, 0.0); cells];
            m.map(&levels, &mut sym);
            let eq: Vec<EqCell> = sym.iter().map(|&s| EqCell { sig: s, chan: 1.0 }).collect();
            for lvl in 0..m.levels() {
                let mut out = Vec::new();
                m.metrics(&eq, lvl, &levels, false, &mut out);
                assert_eq!(out.len(), nb);
                for (k, bm) in out.iter().enumerate() {
                    let hard = u8::from(bm.to1 < bm.to0);
                    assert_eq!(hard, levels[lvl][k], "{m:?} level {lvl} bit {k}");
                }
            }
        }
    }
}
