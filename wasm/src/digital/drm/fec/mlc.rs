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
    BIT_INTERLEAVER_T0, CODE_RATES, FAC_RATE, MSC16_E, MSC16_SM, MSC64_HMMIX, MSC64_HMSYM, MSC64_SM,
    NUM_FAC_CELLS, SDC4_RATE, SDC16_RATES, PunctureMask,
};

/// Number of FAC information bits per frame (incl. CRC).
pub const FAC_BITS: usize = 72;
/// Mode E FAC has two service descriptors, per ES 201 980 §6.3.5.
pub const FAC_BITS_E: usize = 116;

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
        Self::fac_cells(NUM_FAC_CELLS, FAC_BITS, FAC_RATE)
    }

    /// Mode E uses 244 cells, 116 input bits and a 1/4 code (table 40).
    pub fn fac_for(mode: crate::digital::drm::params::RobustnessMode) -> Self {
        let cells = crate::digital::drm::tables::fac_cell_count(mode);
        if mode == crate::digital::drm::params::RobustnessMode::E {
            Self::fac_cells(cells, FAC_BITS_E, 0)
        } else {
            Self::fac_cells(cells, FAC_BITS, FAC_RATE)
        }
    }

    fn fac_cells(cells: usize, bits: usize, rate: usize) -> Self {
        Self {
            mapping: Mapping::Qam4,
            cells,
            n1: 0,
            n2: cells,
            levels: vec![LevelParams {
                bits_a: 0,
                bits_b: bits,
                rate_a: 0,
                rate_b: rate,
                interleaver: Some(1),
            }],
            bits_hpp: 0,
            bits_lpp: bits,
            bits_vspp: 0,
            tail_rule: TailRule::Standard,
            is_fac: true,
        }
    }

    /// SDC with 4-QAM (R = 0.5) or 16-QAM (R = 0.5) over `n_sdc` cells.
    pub fn sdc(mapping: Mapping, n_sdc: usize) -> Self {
        Self::sdc_with_rate(mapping, n_sdc, SDC4_RATE)
    }

    /// Mode E 4-QAM SDC: protection flag 0 = rate 1/2, 1 = rate 1/4 (table 38).
    pub fn sdc_e(n_sdc: usize, protection: u8) -> Self {
        Self::sdc_with_rate(Mapping::Qam4, n_sdc, if protection == 0 { SDC4_RATE } else { 0 })
    }

    fn sdc_with_rate(mapping: Mapping, n_sdc: usize, sdc4_rate: usize) -> Self {
        let coded = 2 * n_sdc as i64 - 12;
        let levels = match mapping {
            Mapping::Qam4 => vec![LevelParams {
                bits_a: 0,
                bits_b: floor_bits(sdc4_rate, coded),
                rate_a: 0,
                rate_b: sdc4_rate,
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
        Self::msc_with_rates(mapping, n_mux, prot, part_a_bytes, false)
    }

    /// Mode E uses different 16-QAM rates (table 31) and admits 4-QAM (table 29).
    pub fn msc_e(mapping: Mapping, n_mux: usize, prot: MscProtection, part_a_bytes: usize) -> Self {
        Self::msc_with_rates(mapping, n_mux, prot, part_a_bytes, true)
    }

    fn msc_with_rates(mapping: Mapping, n_mux: usize, prot: MscProtection, part_a_bytes: usize, drm_plus: bool) -> Self {
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
                    let rates = if drm_plus { &MSC16_E[..] } else { &MSC16_SM[..] };
                    let a = rates[prot.part_a.min(rates.len() - 1)];
                    let b = rates[prot.part_b.min(rates.len() - 1)];
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
            Mapping::Qam4 => {
                let ra = crate::digital::drm::tables::MSC4_E[prot.part_a.min(3)];
                let rb = crate::digital::drm::tables::MSC4_E[prot.part_b.min(3)];
                let n1 = n1_for(&[ra], CODE_RATES[ra].ry, 2.0);
                let n2 = n_mux - n1;
                (n1, vec![LevelParams {
                    bits_a: (2.0 * n1 as f64 * rate_of(ra)) as usize,
                    bits_b: floor_bits(rb, 2 * n2 as i64 - 12),
                    rate_a: ra,
                    rate_b: rb,
                    interleaver: Some(1),
                }], TailRule::Standard, 0)
            },
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
    #[cfg(test)]
    pub(crate) fn encode_test_single_level(&self, bits: &[u8]) -> Vec<crate::digital::drm::dsp::Cplx> {
        assert!(self.levels.len() == 1 && bits.len() == self.bits_lpp);
        let mut dispersed = bits.to_vec();
        dispersal::apply(&mut dispersed, 0);
        let mut coded = Vec::new();
        conv::encode(&dispersed, &self.masks()[0], &mut coded);
        self.interleavers()[1].interleave(&mut coded);
        let mut symbols = vec![crate::digital::drm::dsp::Cplx::zero(); self.cells];
        Mapping::Qam4.map(&[coded], &mut symbols);
        symbols
    }

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

#[cfg(test)]
mod mode_e_tests {
    use super::*;
    use crate::digital::drm::dsp::Cplx;
    use crate::digital::drm::params::RobustnessMode;
    use crate::digital::drm::fec::puncture::coded_len;

    #[test]
    fn mode_e_fac_codes_116_bits_into_244_cells_and_decodes_them() {
        let p = MlcParams::fac_for(RobustnessMode::E);
        assert_eq!(p.total_bits(), 116);
        assert_eq!(p.cells, 244);
        assert_eq!(p.levels[0].rate_b, 0); // 1/4
        let bits: Vec<u8> = (0..116).map(|i| ((i * 37 + i / 7) % 2) as u8).collect();
        let mut dispersed = bits.clone();
        dispersal::apply(&mut dispersed, 0);
        let masks = p.masks();
        let mut coded = Vec::new();
        conv::encode(&dispersed, &masks[0], &mut coded);
        assert_eq!(coded.len(), 488);
        p.interleavers()[1].interleave(&mut coded);
        let mut symbols = vec![Cplx::zero(); p.cells];
        Mapping::Qam4.map(&[coded], &mut symbols);
        let cells: Vec<EqCell> = symbols.into_iter().map(|sig| EqCell { sig, chan: 1.0 }).collect();
        let mut receiver = MlcDecoder::new(p, 0);
        let mut recovered = Vec::new();
        assert!(receiver.decode(&cells, &mut recovered));
        assert_eq!(recovered, bits);
    }

    #[test]
    fn mode_e_16qam_uses_six_mother_bits_and_roundtrips_soft_decoding() {
        let p = MlcParams::msc_e(Mapping::Qam16, 7460,
            MscProtection { part_a: 0, part_b: 0, hierarchical: 0 }, 0);
        assert_eq!(p.total_bits(), 9938);
        let masks = p.masks();
        assert!(masks[0].iter().any(|&m| m & 0b11_0000 != 0));
        let mut source = Vec::new();
        let mut coded_levels = Vec::new();
        let interleavers = p.interleavers();
        for (j, level) in p.levels.iter().enumerate() {
            let bits: Vec<u8> = (0..level.bits_b)
                .map(|i| ((i * 31 + i / 3 + j * 7) % 2) as u8).collect();
            source.extend_from_slice(&bits);
            let mut coded = Vec::new();
            conv::encode(&bits, &masks[j], &mut coded);
            assert_eq!(coded.len(), 2 * p.cells);
            if let Some(t) = level.interleaver { interleavers[t].interleave(&mut coded); }
            coded_levels.push(coded);
        }
        let mut symbols = vec![Cplx::zero(); p.cells];
        Mapping::Qam16.map(&coded_levels, &mut symbols);
        let cells: Vec<EqCell> = symbols.into_iter().map(|sig| EqCell { sig, chan: 1.0 }).collect();
        let mut decoded = Vec::new();
        assert!(MlcDecoder::new(p, 1).decode(&cells, &mut decoded));
        dispersal::apply(&mut source, 0);
        assert_eq!(decoded, source);
    }

    #[test]
    fn mode_e_sdc_and_msc_code_sizes_match_standard_tables() {
        for (protection, bits) in [(0, 930), (1, 465)] {
            let sdc = MlcParams::sdc_e(936, protection);
            assert_eq!(sdc.total_bits(), bits);
            assert_eq!(coded_len(&sdc.masks()[0]), 1872);
        }
        for (mapping, totals) in [
            (Mapping::Qam4, [3727, 4969, 5962, 7454]),
            (Mapping::Qam16, [9938, 12243, 14907, 18635]),
        ] {
            for (level, expected) in totals.into_iter().enumerate() {
                let p = MlcParams::msc_e(mapping, 7460,
                    MscProtection { part_a: 0, part_b: level, hierarchical: 0 }, 0);
                assert_eq!(p.total_bits(), expected, "{mapping:?} protection {level}");
                assert!(p.masks().iter().all(|m| coded_len(m) == 14920), "{mapping:?} protection {level}");
            }
        }
    }
}
