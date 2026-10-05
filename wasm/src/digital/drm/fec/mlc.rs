//! Multilevel coding (ES 201 980 §7.3): partitioning of the bit stream over the QAM
//! levels, per-level punctured convolutional coding and bit interleaving, and the
//! multistage decoder.

use super::conv;
use super::dispersal;
use super::interleaver::BitInterleaver;
use super::puncture::{mask_table, TailRule};
use super::qam::{EqCell, Mapping};
use super::viterbi::ViterbiDecoder;
use super::BitMetric;
use crate::digital::drm::tables::{
    BIT_INTERLEAVER_T0, CODE_RATES, FAC_RATE, MSC16_SM, MSC64_HMMIX, MSC64_HMSYM, MSC64_SM,
    NUM_FAC_CELLS, SDC4_RATE, SDC16_RATES, PunctureMask,
};

/// Number of FAC information bits per frame (incl. CRC).
pub const FAC_BITS: usize = 72;

/// Protection levels of the MSC (from the SDC multiplex description).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MscProtection {
    pub part_a: usize,
    pub part_b: usize,
    pub hierarchical: usize,
}

/// Coding parameters of one MLC level.
#[derive(Debug, Clone)]
pub struct LevelParams {
    pub bits_a: usize,
    pub bits_b: usize,
    pub rate_a: usize,
    pub rate_b: usize,
    pub interleaver: Option<usize>,
}

/// Full MLC configuration of one logical channel (FAC, SDC or MSC frame).
#[derive(Debug, Clone)]
pub struct MlcParams {
    pub mapping: Mapping,
    pub cells: usize,
    pub n1: usize,
    pub n2: usize,
    pub levels: Vec<LevelParams>,
    pub bits_hpp: usize,
    pub bits_lpp: usize,
    pub bits_vspp: usize,
    tail_rule: TailRule,
    is_fac: bool,
}

fn rate_of(idx: usize) -> f64 {
    CODE_RATES[idx].rate()
}

fn floor_bits(idx: usize, coded: i64) -> usize {
    let r = &CODE_RATES[idx];
    if coded <= 0 {
        return 0;
    }
    r.rx * (coded as usize / r.ry)
}

impl MlcParams {
    /// FAC: 4-QAM, R = 0.6, 72 bits in 65 cells.
    pub fn fac() -> Self {
        Self::fac_cells(NUM_FAC_CELLS)
    }

    /// FAC for a robustness mode: the same 4-QAM/coding, over the mode's cell count (mode E,
    /// DRM+, carries 244 cells, ES 201 980 §8.5.2 table 66).
    pub fn fac_for(mode: crate::digital::drm::params::RobustnessMode) -> Self {
        Self::fac_cells(crate::digital::drm::tables::fac_cell_count(mode))
    }

    fn fac_cells(cells: usize) -> Self {
        Self {
            mapping: Mapping::Qam4,
            cells,
            n1: 0,
            n2: cells,
            levels: vec![LevelParams {
                bits_a: 0,
                bits_b: FAC_BITS,
                rate_a: 0,
                rate_b: FAC_RATE,
                interleaver: Some(1),
            }],
            bits_hpp: 0,
            bits_lpp: FAC_BITS,
            bits_vspp: 0,
            tail_rule: TailRule::Standard,
            is_fac: true,
        }
    }

    /// SDC with 4-QAM (R = 0.5) or 16-QAM (R = 0.5) over `n_sdc` cells.
    pub fn sdc(mapping: Mapping, n_sdc: usize) -> Self {
        let coded = 2 * n_sdc as i64 - 12;
        let levels = match mapping {
            Mapping::Qam4 => vec![LevelParams {
                bits_a: 0,
                bits_b: floor_bits(SDC4_RATE, coded),
                rate_a: 0,
                rate_b: SDC4_RATE,
                interleaver: Some(1),
            }],
            Mapping::Qam16 => SDC16_RATES
                .iter()
                .enumerate()
                .map(|(i, &r)| LevelParams {
                    bits_a: 0,
                    bits_b: floor_bits(r, coded),
                    rate_a: 0,
                    rate_b: r,
                    interleaver: Some(i),
                })
                .collect(),
            other => panic!("SDC cannot use {other:?}"),
        };
        let bits_lpp = levels.iter().map(|l| l.bits_b).sum();
        Self {
            mapping,
            cells: n_sdc,
            n1: 0,
            n2: n_sdc,
            levels,
            bits_hpp: 0,
            bits_lpp,
            bits_vspp: 0,
            tail_rule: TailRule::Standard,
            is_fac: false,
        }
    }

    /// MSC over `n_mux` cells. `part_a_bytes` is the total length of the higher
    /// protected parts of all streams (bytes per multiplex frame).
    pub fn msc(mapping: Mapping, n_mux: usize, prot: MscProtection, part_a_bytes: usize) -> Self {
        let x = 8.0 * part_a_bytes as f64;
        let n1_for = |rates: &[usize], rylcm: usize, k: f64| -> usize {
            let sum: f64 = rates.iter().map(|&r| rate_of(r)).sum();
            let n1 = (x / (k * rylcm as f64 * sum)).ceil() as usize * rylcm;
            if n1 > n_mux { 0 } else { n1 }
        };
        let il = |j: usize| -> Option<usize> {
            match mapping {
                Mapping::Qam16 => Some(j),
                Mapping::Qam64Sm | Mapping::Qam64HmSym => [None, Some(0), Some(1)][j],
                Mapping::Qam64HmMix => [None, None, Some(0), Some(0), Some(1), Some(1)][j],
                Mapping::Qam4 => Some(1),
            }
        };

        let (n1, levels, tail_rule, vspp) = match mapping {
            Mapping::Qam16 | Mapping::Qam64Sm => {
                let (ra, rb, rylcm): (Vec<usize>, Vec<usize>, usize) = if mapping == Mapping::Qam16 {
                    let a = MSC16_SM[prot.part_a.min(1)];
                    let b = MSC16_SM[prot.part_b.min(1)];
                    (a.0.to_vec(), b.0.to_vec(), a.1)
                } else {
                    let a = MSC64_SM[prot.part_a.min(3)];
                    let b = MSC64_SM[prot.part_b.min(3)];
                    (a.0.to_vec(), b.0.to_vec(), a.1)
                };
                let n1 = n1_for(&ra, rylcm, 2.0);
                let n2 = n_mux - n1;
                let levels = (0..ra.len())
                    .map(|j| LevelParams {
                        bits_a: (2.0 * n1 as f64 * rate_of(ra[j])) as usize,
                        bits_b: floor_bits(rb[j], 2 * n2 as i64 - 12),
                        rate_a: ra[j],
                        rate_b: rb[j],
                        interleaver: il(j),
                    })
                    .collect();
                (n1, levels, TailRule::Standard, 0)
            }
            Mapping::Qam64HmSym => {
                let a = MSC64_HMSYM[prot.part_a.min(3)];
                let b = MSC64_HMSYM[prot.part_b.min(3)];
                let h = MSC64_HMSYM[prot.hierarchical.min(3)].0[0];
                let n1 = n1_for(&a.0[1..], a.1, 2.0);
                let n2 = n_mux - n1;
                let mut levels = vec![LevelParams {
                    bits_a: 0,
                    bits_b: floor_bits(h, 2 * (n1 + n2) as i64 - 12),
                    rate_a: 0,
                    rate_b: h,
                    interleaver: il(0),
                }];
                for j in 1..3 {
                    levels.push(LevelParams {
                        bits_a: (2.0 * n1 as f64 * rate_of(a.0[j])) as usize,
                        bits_b: floor_bits(b.0[j], 2 * n2 as i64 - 12),
                        rate_a: a.0[j],
                        rate_b: b.0[j],
                        interleaver: il(j),
                    });
                }
                let vspp = levels[0].bits_b;
                (n1, levels, TailRule::HmSym, vspp)
            }
            Mapping::Qam64HmMix => {
                let a = MSC64_HMMIX[prot.part_a.min(3)];
                let b = MSC64_HMMIX[prot.part_b.min(3)];
                let h = MSC64_HMMIX[prot.hierarchical.min(3)].0[0];
                let n1 = n1_for(&a.0[1..], a.1, 1.0);
                let n2 = n_mux - n1;
                let mut levels = vec![LevelParams {
                    bits_a: 0,
                    bits_b: floor_bits(h, (n1 + n2) as i64 - 12),
                    rate_a: 0,
                    rate_b: h,
                    interleaver: il(0),
                }];
                for j in 1..6 {
                    levels.push(LevelParams {
                        bits_a: (n1 as f64 * rate_of(a.0[j])) as usize,
                        bits_b: floor_bits(b.0[j], n2 as i64 - 12),
                        rate_a: a.0[j],
                        rate_b: b.0[j],
                        interleaver: il(j),
                    });
                }
                let vspp = levels[0].bits_b;
                (n1, levels, TailRule::HmMix, vspp)
            }
            Mapping::Qam4 => panic!("DRM30 MSC does not use 4-QAM"),
        };

        let first = if vspp > 0 { 1 } else { 0 };
        let bits_hpp = levels[first..].iter().map(|l| l.bits_a).sum();
        let bits_lpp = levels[first..].iter().map(|l| l.bits_b).sum();
        Self {
            mapping,
            cells: n_mux,
            n1,
            n2: n_mux - n1,
            levels,
            bits_hpp,
            bits_lpp,
            bits_vspp: vspp,
            tail_rule,
            is_fac: false,
        }
    }

    /// Total information bits per block.
    pub fn total_bits(&self) -> usize {
        self.bits_hpp + self.bits_lpp + self.bits_vspp
    }

    fn masks(&self) -> Vec<Vec<PunctureMask>> {
        self.levels
            .iter()
            .enumerate()
            .map(|(j, l)| {
                mask_table(&super::puncture::PunctureSpec {
                    n1: self.n1,
                    n2: self.n2,
                    bits_a: l.bits_a,
                    bits_b: l.bits_b,
                    rate_a: l.rate_a,
                    rate_b: l.rate_b,
                    level: j,
                    tail_rule: self.tail_rule,
                    is_fac: self.is_fac,
                })
            })
            .collect()
    }

    fn interleavers(&self) -> [BitInterleaver; 2] {
        let (a, b) = match self.mapping {
            Mapping::Qam64HmMix => (self.n1, self.n2),
            _ => (2 * self.n1, 2 * self.n2),
        };
        [
            BitInterleaver::new(a, b, BIT_INTERLEAVER_T0[0]),
            BitInterleaver::new(a, b, BIT_INTERLEAVER_T0[1]),
        ]
    }

    /// Assemble the block bit stream from the per-level information bits (the inverse
    /// of the transmitter's partition).
    fn departition(&self, levels: &[Vec<u8>], out: &mut Vec<u8>) {
        out.clear();
        let first = if self.bits_vspp > 0 {
            out.extend_from_slice(&levels[0][..self.levels[0].bits_b]);
            1
        } else {
            0
        };
        for j in first..self.levels.len() {
            out.extend_from_slice(&levels[j][..self.levels[j].bits_a]);
        }
        for j in first..self.levels.len() {
            let a = self.levels[j].bits_a;
            out.extend_from_slice(&levels[j][a..a + self.levels[j].bits_b]);
        }
    }
}

/// Multistage decoder (receiver side).
#[derive(Debug)]
pub struct MlcDecoder {
    params: MlcParams,
    masks: Vec<Vec<PunctureMask>>,
    interleavers: [BitInterleaver; 2],
    viterbi: ViterbiDecoder,
    /// Number of additional decoding passes (0 = plain multistage).
    pub iterations: usize,
    decided: Vec<Vec<u8>>,
    metrics: Vec<BitMetric>,
    info: Vec<Vec<u8>>,
}

impl MlcDecoder {
    pub fn new(params: MlcParams, iterations: usize) -> Self {
        let masks = params.masks();
        let interleavers = params.interleavers();
        let n = params.levels.len();
        let iterations = if params.mapping == Mapping::Qam4 { 0 } else { iterations };
        Self {
            params,
            masks,
            interleavers,
            viterbi: ViterbiDecoder::new(),
            iterations,
            decided: vec![Vec::new(); n],
            metrics: Vec::new(),
            info: vec![Vec::new(); n],
        }
    }

    pub fn params(&self) -> &MlcParams {
        &self.params
    }

    /// Decode `params.cells` equalised cells into `params.total_bits()` bits (energy
    /// dispersal removed). Returns false when the cell count does not match, which
    /// happens while a capture holds only part of a multiplex frame and at the end of an
    /// interleaver's fill-in delay. A panic here would trap the wasm module.
    pub fn decode(&mut self, cells: &[EqCell], out: &mut Vec<u8>) -> bool {
        let p = &self.params;
        if cells.len() != p.cells {
            return false;
        }
        let nl = p.levels.len();
        let last_branch = if p.mapping == Mapping::Qam64HmMix { nl - 2 } else { nl - 1 };
        for d in &mut self.decided {
            d.clear();
        }

        for pass in 0..=self.iterations {
            for j in 0..nl {
                p.mapping.metrics(cells, j, &self.decided, pass > 0, &mut self.metrics);
                let il = p.levels[j].interleaver;
                if let Some(t) = il {
                    self.interleavers[t].deinterleave(&mut self.metrics);
                }
                self.viterbi.decode(&self.metrics, &self.masks[j], &mut self.info[j]);

                if pass < self.iterations || j < last_branch {
                    let mut c = core::mem::take(&mut self.decided[j]);
                    conv::encode(&self.info[j], &self.masks[j], &mut c);
                    if let Some(t) = il {
                        self.interleavers[t].interleave(&mut c);
                    }
                    self.decided[j] = c;
                }
            }
        }

        p.departition(&self.info, out);
        dispersal::apply(out, p.bits_vspp);
        true
    }
}
