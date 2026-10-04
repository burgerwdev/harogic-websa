//! OFDM cell map: which cell of the transmission super frame carries what, and the
//! complex values of all reference (pilot) cells (ES 201 980 §8.4, §7.7).
//!
//! Independent reimplementation of the standard's mapping; DecDRM's `cellmap.rs` was
//! used as a cross-check reference. The map covers one whole super frame (3 frames)
//! because SDC cells and the MSC dummy cells only occur once per super frame.

use crate::digital::drm2::params::{ChannelLayout, RobustnessMode, SpectrumOccupancy};
use crate::digital::drm2::tables::{self, AFS_PILOT_POWER, BOOSTED_PILOT_POWER, DATA_CELL_POWER, PILOT_POWER};
use crate::digital::drm2::dsp::Cplx;

/// Classification of one OFDM cell (a small bit set: several pilot flags may be set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellType(u16);

impl CellType {
    pub const DC: Self = Self(1);
    pub const MSC: Self = Self(2);
    pub const SDC: Self = Self(4);
    pub const FAC: Self = Self(8);
    pub const TIME_PILOT: Self = Self(16);
    pub const FREQ_PILOT: Self = Self(32);
    pub const SCAT_PILOT: Self = Self(64);
    pub const BOOSTED: Self = Self(128);
    pub const AFS_PILOT: Self = Self(256);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
    pub const fn is_dc(self) -> bool {
        self.contains(Self::DC)
    }
    pub const fn is_msc(self) -> bool {
        self.contains(Self::MSC)
    }
    pub const fn is_sdc(self) -> bool {
        self.contains(Self::SDC)
    }
    pub const fn is_fac(self) -> bool {
        self.contains(Self::FAC)
    }
    pub const fn is_data(self) -> bool {
        self.0 & (Self::MSC.0 | Self::SDC.0 | Self::FAC.0) != 0
    }
    pub const fn is_pilot(self) -> bool {
        self.0 & (Self::TIME_PILOT.0 | Self::FREQ_PILOT.0 | Self::SCAT_PILOT.0 | Self::AFS_PILOT.0) != 0
    }
    pub const fn is_scattered(self) -> bool {
        self.contains(Self::SCAT_PILOT)
    }
    pub const fn is_time_pilot(self) -> bool {
        self.contains(Self::TIME_PILOT)
    }
    pub const fn is_freq_pilot(self) -> bool {
        self.contains(Self::FREQ_PILOT)
    }
    pub const fn is_afs(self) -> bool {
        self.contains(Self::AFS_PILOT)
    }
    pub const fn is_boosted(self) -> bool {
        self.contains(Self::BOOSTED)
    }
}

impl core::ops::BitOr for CellType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for CellType {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Complete cell map of one transmission super frame for a mode/occupancy pair.
#[derive(Debug, Clone)]
pub struct CellMap {
    pub layout: ChannelLayout,
    pub kmin: i32,
    pub kmax: i32,
    pub num_carriers: usize,
    pub symbols_per_frame: usize,
    pub symbols_per_superframe: usize,
    pub scattered: tables::ScatteredPilotParams,
    cells: Vec<CellType>,
    pilots: Vec<Cplx>,
    pub msc_carriers: Vec<Vec<u16>>,
    pub fac_carriers: Vec<Vec<u16>>,
    pub sdc_carriers: Vec<Vec<u16>>,
    pub msc_cells_per_frame: usize,
    pub msc_dummy_cells: usize,
    pub sdc_cells_per_superframe: usize,
    pub avg_power_per_symbol: f64,
    pub avg_scattered_pilot_power: f64,
}

impl CellMap {
    pub fn new(mode: RobustnessMode, occupancy: SpectrumOccupancy) -> Option<Self> {
        let layout = ChannelLayout::new(mode, occupancy)?;
        let (kmin, kmax) = layout.carrier_range();
        let num_carriers = layout.num_carriers();
        let symbols_per_frame = mode.symbols_per_frame();
        let symbols_per_superframe = mode.symbols_per_superframe();
        let sp = tables::scattered_pilots(mode);
        let boosted = tables::boosted_pilots(mode, occupancy.index());
        let fac = tables::fac_positions(mode);
        let time_pilots = tables::time_pilots(mode);
        let freq_pilots = tables::freq_pilots(mode);
        let sdc_symbols = mode.sdc_symbols();

        let n = symbols_per_superframe * num_carriers;
        let mut cells = vec![CellType::default(); n];
        let mut pilots = vec![Cplx::new(0.0, 0.0); n];

        let x = sp.freq_int as i32;
        let y = sp.time_int as i32;
        for sym in 0..symbols_per_superframe {
            let s = (sym % symbols_per_frame) as i32;
            for k in kmin..=kmax {
                let idx = sym * num_carriers + (k - kmin) as usize;

                // Default MSC, overridden by SDC in the first symbols, then FAC.
                let mut ty = if sym < sdc_symbols { CellType::SDC } else { CellType::MSC };
                if fac.iter().any(|&(fs, fk)| i32::from(fs) == s && i32::from(fk) == k) {
                    ty = CellType::FAC;
                }

                // Gain references.
                if (k - sp.k0 - x * (s % y)).rem_euclid(x * y) == 0 {
                    ty = CellType::SCAT_PILOT;
                    let nn = (s % y) as usize;
                    let m = (s / y) as usize;
                    let p = (k - sp.k0 - x * (s % y)) / (x * y);
                    let z = sp.z[nn * sp.wz_cols + m];
                    let phase = if mode == RobustnessMode::E {
                        let r = sp.r[nn * sp.wz_cols + m];
                        let q = sp.q_mat[nn * sp.wz_cols + m];
                        (p * p * r + p * z + q).rem_euclid(1024)
                    } else {
                        let w = sp.w[nn * sp.wz_cols + m];
                        (4 * z + p * w + p * p * (1 + s) * sp.q).rem_euclid(1024)
                    };
                    let is_boosted = boosted.iter().any(|&b| i32::from(b) == k);
                    let power = if is_boosted {
                        ty |= CellType::BOOSTED;
                        BOOSTED_PILOT_POWER
                    } else {
                        PILOT_POWER
                    };
                    pilots[idx] = polar_1024(power.sqrt(), phase);
                }

                // Time references in the first symbol of every frame.
                if s == 0 {
                    if let Some(&(_, phase)) = time_pilots.iter().find(|&&(tk, _)| i32::from(tk) == k) {
                        ty = if ty.is_scattered() { ty | CellType::TIME_PILOT } else { CellType::TIME_PILOT };
                        pilots[idx] = polar_1024(PILOT_POWER.sqrt(), i32::from(phase));
                    }
                }

                // Frequency references in every symbol.
                if let Some(pos) = freq_pilots.iter().position(|&(fk, _)| i32::from(fk) == k) {
                    ty = if ty.is_time_pilot() || ty.is_scattered() { ty | CellType::FREQ_PILOT } else { CellType::FREQ_PILOT };
                    let mut phase = i32::from(freq_pilots[pos].1);
                    if mode == RobustnessMode::D && pos != 2 && s % 2 == 1 {
                        phase = (phase + 512) % 1024;
                    }
                    pilots[idx] = polar_1024(PILOT_POWER.sqrt(), phase);
                }

                // AFS references (mode E only): the fifth OFDM symbol (s = 4) of the first
                // frame and the fortieth (s = 39) of the fourth frame, on every fourth
                // carrier (§8.4.5). Cells that are also gain references keep the gain
                // reference's phase and amplitude set above; the rest carry amplitude 1.0.
                if mode == RobustnessMode::E && (sym == 4 || sym == symbols_per_superframe - 1) {
                    let rel = k + 106;
                    if rel >= 0 && rel <= 212 && rel % 4 == 0 {
                        let i = (rel / 4) as usize;
                        let phase = if sym == 4 { tables::AFS_PHASE_S4[i] } else { tables::AFS_PHASE_S39[i] };
                        ty |= CellType::AFS_PILOT;
                        if !ty.is_scattered() {
                            pilots[idx] = polar_1024(AFS_PILOT_POWER.sqrt(), i32::from(phase));
                        }
                    }
                }

                // Unused carriers (table 50): mode E uses every carrier, including DC.
                if mode != RobustnessMode::E && (k == 0 || (mode == RobustnessMode::A && (k == -1 || k == 1))) {
                    ty = CellType::DC;
                    pilots[idx] = Cplx::new(0.0, 0.0);
                }

                cells[idx] = ty;
            }
        }

        let mut msc_carriers = vec![Vec::new(); symbols_per_superframe];
        let mut fac_carriers = vec![Vec::new(); symbols_per_superframe];
        let mut sdc_carriers = vec![Vec::new(); symbols_per_superframe];
        let mut total_msc = 0usize;
        let mut sdc_total = 0usize;
        let mut power_sum = 0.0;
        let mut scat_power_sum = 0.0;
        let mut scat_count = 0usize;
        for sym in 0..symbols_per_superframe {
            for c in 0..num_carriers {
                let ty = cells[sym * num_carriers + c];
                if ty.is_msc() {
                    msc_carriers[sym].push(c as u16);
                    total_msc += 1;
                }
                if ty.is_fac() {
                    fac_carriers[sym].push(c as u16);
                }
                if ty.is_sdc() {
                    sdc_carriers[sym].push(c as u16);
                    sdc_total += 1;
                }
                if ty.is_dc() {
                    continue;
                }
                if ty.is_data() {
                    power_sum += DATA_CELL_POWER;
                } else if ty.is_boosted() {
                    power_sum += BOOSTED_PILOT_POWER;
                    if ty.is_scattered() {
                        scat_power_sum += BOOSTED_PILOT_POWER;
                        scat_count += 1;
                    }
                } else if ty.is_afs() && !ty.is_scattered() {
                    // AFS-only reference cell: amplitude 1.0 (§8.4.5.2).
                    power_sum += AFS_PILOT_POWER;
                } else {
                    power_sum += PILOT_POWER;
                    if ty.is_scattered() {
                        scat_power_sum += PILOT_POWER;
                        scat_count += 1;
                    }
                }
            }
        }
        let frames = mode.frames_per_superframe();
        let msc_cells_per_frame = total_msc / frames;
        let msc_dummy_cells = total_msc - msc_cells_per_frame * frames;

        Some(Self {
            layout,
            kmin,
            kmax,
            num_carriers,
            symbols_per_frame,
            symbols_per_superframe,
            scattered: sp,
            cells,
            pilots,
            msc_carriers,
            fac_carriers,
            sdc_carriers,
            msc_cells_per_frame,
            msc_dummy_cells,
            sdc_cells_per_superframe: sdc_total,
            avg_power_per_symbol: power_sum / symbols_per_superframe as f64,
            avg_scattered_pilot_power: scat_power_sum / scat_count.max(1) as f64,
        })
    }

    pub fn mode(&self) -> RobustnessMode {
        self.layout.mode
    }

    pub fn occupancy(&self) -> SpectrumOccupancy {
        self.layout.occupancy
    }

    /// Cell type at super-frame symbol `sym`, carrier offset `c = k - kmin`.
    pub fn cell(&self, sym: usize, c: usize) -> CellType {
        self.cells[sym * self.num_carriers + c]
    }

    /// Reference value at super-frame symbol `sym`, carrier offset `c` (zero for
    /// non-pilot cells).
    pub fn pilot(&self, sym: usize, c: usize) -> Cplx {
        self.pilots[sym * self.num_carriers + c]
    }

    /// Carrier index k of carrier offset c.
    pub fn carrier_index(&self, c: usize) -> i32 {
        self.kmin + c as i32
    }

    /// Carrier offset c of carrier index k, if within range.
    pub fn carrier_offset(&self, k: i32) -> Option<usize> {
        if k < self.kmin || k > self.kmax {
            None
        } else {
            Some((k - self.kmin) as usize)
        }
    }
}

/// Complex value with amplitude `amp` and phase `2π·phase/1024`.
fn polar_1024(amp: f64, phase: i32) -> Cplx {
    Cplx::from_polar(amp, 2.0 * core::f64::consts::PI * f64::from(phase) / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm2::tables::fac_cell_count;

    #[test]
    fn mode_b_so3_cell_counts_match_spec() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        assert_eq!(map.num_carriers, 207);
        assert_eq!(map.msc_cells_per_frame, 2337);
        assert_eq!(map.sdc_cells_per_superframe, 322);
        assert_eq!(map.msc_dummy_cells, 2);
    }

    #[test]
    fn every_frame_has_the_specs_fac_cell_count() {
        for m in RobustnessMode::ALL {
            for so in SpectrumOccupancy::ALL {
                let Some(map) = CellMap::new(m, so) else { continue };
                for f in 0..m.frames_per_superframe() {
                    let n: usize = (0..map.symbols_per_frame)
                        .map(|s| map.fac_carriers[f * map.symbols_per_frame + s].len())
                        .sum();
                    assert_eq!(n, fac_cell_count(m), "{m:?} {so:?}");
                }
            }
        }
    }

    #[test]
    fn pilots_have_expected_power() {
        for m in RobustnessMode::ALL {
            for so in SpectrumOccupancy::ALL {
                let Some(map) = CellMap::new(m, so) else { continue };
                for sym in 0..map.symbols_per_superframe {
                    for c in 0..map.num_carriers {
                        let ty = map.cell(sym, c);
                        let p = map.pilot(sym, c).norm_sqr();
                        if ty.is_pilot() && !ty.is_dc() {
                            let want = if ty.is_afs() && !ty.is_scattered() {
                                1.0
                            } else if ty.is_boosted() && !ty.is_freq_pilot() && !ty.is_time_pilot() {
                                4.0
                            } else {
                                2.0
                            };
                            assert!((p - want).abs() < 1e-9, "{m:?} {so:?} sym {sym} c {c}");
                        } else {
                            assert_eq!(p, 0.0);
                        }
                    }
                }
            }
        }
    }

    /// Mode E's cell layout, pinned to the spec's own numbers (§8.4.3 table 57, §8.4.5
    /// table 61, §8.5.2 table 66, §8.5.3.1): 244 FAC cells per frame, five SDC symbols, 21
    /// time pilots, no frequency pilots, the DC carrier used, and AFS cells only in
    /// symbols 4 and 39.
    #[test]
    fn mode_e_layout_matches_the_spec() {
        let map = CellMap::new(RobustnessMode::E, SpectrumOccupancy::SO_0).unwrap();
        assert_eq!(map.num_carriers, 213);
        assert_eq!(map.kmin, -106);
        assert_eq!(map.kmax, 106);
        assert_eq!(map.symbols_per_frame, 40);
        assert_eq!(map.symbols_per_superframe, 160);

        // 244 FAC cells in every frame (§8.5.2, table 66).
        for f in 0..4 {
            let n: usize = (0..40).map(|s| map.fac_carriers[f * 40 + s].len()).sum();
            assert_eq!(n, 244, "FAC cells frame {f}");
        }
        // FAC cells span symbols 5..=26 of each frame.
        for sym in 0..160 {
            let s = sym % 40;
            assert_eq!(
                !map.fac_carriers[sym].is_empty(),
                (5..=26).contains(&s),
                "FAC presence at sym {sym}"
            );
        }

        // The DC carrier is used (table 50: mode E has no unused carriers).
        for sym in 0..160 {
            assert!(!map.cell(sym, map.carrier_offset(0).unwrap()).is_dc(), "DC at sym {sym}");
        }

        // No frequency reference cells (table 51).
        for sym in 0..160 {
            for c in 0..map.num_carriers {
                assert!(!map.cell(sym, c).is_freq_pilot(), "freq pilot at sym {sym} c {c}");
            }
        }

        // 21 time pilots in symbol 0 of every frame, nowhere else (table 57).
        for f in 0..4 {
            let n = (0..map.num_carriers).filter(|&c| map.cell(f * 40, c).is_time_pilot()).count();
            assert_eq!(n, 21, "time pilots frame {f}");
        }
        for sym in 0..160 {
            if sym % 40 != 0 {
                for c in 0..map.num_carriers {
                    assert!(!map.cell(sym, c).is_time_pilot(), "time pilot at sym {sym}");
                }
            }
        }

        // SDC occupies the first five symbols of the super frame (§8.5.3.1).
        for sym in 0..160 {
            let is_sdc_sym = sym < 5;
            let any_sdc = map.sdc_carriers[sym]
                .iter()
                .any(|&c| map.cell(sym, c as usize).is_sdc());
            assert_eq!(any_sdc, is_sdc_sym, "SDC presence at sym {sym}");
        }

        // AFS cells: 54 in symbol 4 and 54 in symbol 39, nowhere else (table 61).
        let afs4 = (0..map.num_carriers).filter(|&c| map.cell(4, c).is_afs()).count();
        let afs39 = (0..map.num_carriers).filter(|&c| map.cell(159, c).is_afs()).count();
        assert_eq!(afs4, 54);
        assert_eq!(afs39, 54);
        for sym in 0..160 {
            if sym != 4 && sym != 159 {
                for c in 0..map.num_carriers {
                    assert!(!map.cell(sym, c).is_afs(), "AFS at sym {sym}");
                }
            }
        }
    }

    /// AFS cells that coincide with a gain reference carry the gain reference's phase and
    /// amplitude; the AFS-only cells carry amplitude 1.0. Table 61's phases must match
    /// `map.pilot` at every AFS carrier — this cross-checks the scattered-pilot
    /// transcription too.
    #[test]
    fn mode_e_afs_phases_match_table_61() {
        use crate::digital::drm2::tables::{afs_carrier, AFS_PHASE_S39, AFS_PHASE_S4};
        let map = CellMap::new(RobustnessMode::E, SpectrumOccupancy::SO_0).unwrap();
        for (sym, table) in [(4usize, &AFS_PHASE_S4[..]), (159usize, &AFS_PHASE_S39[..])] {
            for i in 0..54 {
                let k = afs_carrier(i);
                let c = map.carrier_offset(i32::from(k)).unwrap();
                let ty = map.cell(sym, c);
                assert!(ty.is_afs(), "carrier {k} sym {sym}");
                let amp = if ty.is_scattered() { PILOT_POWER.sqrt() } else { AFS_PILOT_POWER.sqrt() };
                let want = polar_1024(amp, table[i] as i32);
                let got = map.pilot(sym, c);
                assert!(
                    (got.re - want.re).abs() < 1e-12 && (got.im - want.im).abs() < 1e-12,
                    "carrier {k} sym {sym}: {got:?} vs {want:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;
    use crate::digital::drm::cellmap::CellMap as PreviousMap;

    /// The ported map must agree with the chain it was taken from, cell by cell, for every
    /// legal mode/occupancy pair — counts, per-symbol carrier lists, cell classification and
    /// the complex pilot values. That chain locked and decoded the bench signal with this
    /// map, so agreement is the strongest evidence available until the new chain runs.
    #[test]
    fn matches_the_previous_chains_map_everywhere() {
        use crate::digital::drm::params::{
            RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy,
        };
        for mode in RobustnessMode::ALL {
            if mode == RobustnessMode::E {
                // The previous chain has no mode E; its geometry is pinned by the
                // spec-anchor tests below instead.
                continue;
            }
            for so in SpectrumOccupancy::ALL {
                let old_mode = OldMode::from_index(mode.index()).expect("mode index");
                let old_so = OldOccupancy::new(so.value()).expect("occupancy code");
                let new = CellMap::new(mode, so);
                let old = PreviousMap::new(old_mode, old_so);
                match (new, old) {
                    (None, None) => continue,
                    (Some(new), Some(old)) => {
                        let tag = format!("{mode:?}/{so:?}");
                        assert_eq!(new.num_carriers, old.num_carriers, "{tag} carriers");
                        assert_eq!(new.kmin, old.kmin, "{tag} kmin");
                        assert_eq!(new.msc_cells_per_frame, old.msc_cells_per_frame, "{tag} MSC/frame");
                        assert_eq!(
                            new.sdc_cells_per_superframe, old.sdc_cells_per_superframe,
                            "{tag} SDC cells"
                        );
                        assert_eq!(new.msc_dummy_cells, old.msc_dummy_cells, "{tag} dummy cells");
                        for sym in 0..mode.symbols_per_superframe() {
                            assert_eq!(new.msc_carriers[sym], old.msc_carriers[sym], "{tag} sym {sym}");
                            assert_eq!(new.fac_carriers[sym], old.fac_carriers[sym], "{tag} sym {sym}");
                            assert_eq!(new.sdc_carriers[sym], old.sdc_carriers[sym], "{tag} sym {sym}");
                            for c in 0..new.num_carriers {
                                let (a, b) = (new.cell(sym, c), old.cell(sym, c));
                                let same = a.is_dc() == b.is_dc()
                                    && a.is_msc() == b.is_msc()
                                    && a.is_sdc() == b.is_sdc()
                                    && a.is_fac() == b.is_fac()
                                    && a.is_data() == b.is_data()
                                    && a.is_pilot() == b.is_pilot()
                                    && a.is_scattered() == b.is_scattered()
                                    && a.is_freq_pilot() == b.is_freq_pilot()
                                    && a.is_time_pilot() == b.is_time_pilot();
                                assert!(same, "{tag} sym {sym} carrier {c}: {a:?} vs {b:?}");
                                let (pa, pb) = (new.pilot(sym, c), old.pilot(sym, c));
                                assert!(
                                    (pa.re - pb.re).abs() < 1e-12 && (pa.im - pb.im).abs() < 1e-12,
                                    "{tag} sym {sym} carrier {c} pilot value"
                                );
                            }
                        }
                    }
                    (a, b) => panic!("{mode:?}/{so:?}: new {a:?} old {b:?}"),
                }
            }
        }
    }

    /// Anchors that do not depend on the previous chain: the spec's FAC cells per frame, the
    /// three continuous frequency pilots, and the bench signal's MSC cell count (mode B, 10 kHz,
    /// the value the previous chain's MSC decoder was fed with).
    #[test]
    fn spec_anchors_hold() {
        assert_eq!(crate::digital::drm2::tables::NUM_FAC_CELLS, 65);
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("bench layout");
        assert_eq!(map.msc_cells_per_frame, 2337, "mode B / 10 kHz MSC cells per frame");
        let symbols = RobustnessMode::B.symbols_per_superframe();
        let freq_pilots = (0..symbols)
            .flat_map(|sym| (0..map.num_carriers).map(move |c| (sym, c)))
            .filter(|&(sym, c)| map.cell(sym, c).is_freq_pilot())
            .count();
        // Three continuous pilots in every symbol of the super frame.
        assert_eq!(freq_pilots, 3 * symbols, "three continuous pilots per symbol");
    }
}
