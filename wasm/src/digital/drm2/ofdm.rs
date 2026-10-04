//! OFDM demodulation: one symbol window in, the map's cells out.
//!
//! Carrier `k` maps to FFT bin `k mod N` (true baseband: the DRM DC carrier sits at 0 Hz), and
//! the demodulator hands out the cells for `Kmin..=Kmax` in map order (`c = k - Kmin`), so the
//! cell map's classification (scattered pilot, FAC, SDC, MSC) applies directly.

use crate::digital::drm2::cellmap::CellMap;
use crate::digital::drm2::dsp::{Cplx, FullFft};

/// Value of carrier `k` in a full FFT row.
pub fn carrier_at(row: &[Cplx], n: usize, k: i32) -> Cplx {
    row[k.rem_euclid(n as i32) as usize]
}

/// The OFDM demodulator for one mode/occupancy: an FFT and the carrier extraction.
pub struct OfdmDemod {
    fft: FullFft,
    n: usize,
    kmin: i32,
    num_carriers: usize,
    all: Vec<Cplx>,
}

impl OfdmDemod {
    pub fn new(map: &CellMap) -> Self {
        let n = map.mode().fft_size();
        Self {
            fft: FullFft::new(n),
            n,
            kmin: map.kmin,
            num_carriers: map.num_carriers,
            all: vec![Cplx::zero(); n],
        }
    }

    /// Demodulate one window of `N` complex samples into `num_carriers` cells (index
    /// `c = k - Kmin`).
    pub fn demodulate(&mut self, window: &[Cplx], out: &mut Vec<Cplx>) {
        self.fft.forward(window, &mut self.all);
        out.clear();
        out.reserve(self.num_carriers);
        for c in 0..self.num_carriers as i32 {
            out.push(carrier_at(&self.all, self.n, self.kmin + c));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm2::params::{RobustnessMode, SpectrumOccupancy};
    use crate::digital::drm2::sync::timesync::{SymbolWindow, TimeSync};

    fn load_iq(path: &str) -> Vec<Cplx> {
        let raw = std::fs::read(path).expect("capture file");
        raw.chunks_exact(8)
            .map(|c| {
                Cplx::new(
                    f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                    f64::from(f32::from_le_bytes([c[4], c[5], c[6], c[7]])),
                )
            })
            .collect()
    }

    fn windows(iq: &[Cplx], limit: usize) -> Vec<SymbolWindow> {
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut out = Vec::new();
        for block in iq.chunks(3248) {
            let _ = ts.push(block);
            while let Some(w) = ts.next_window() {
                out.push(w);
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// The demodulator must agree with the chain it was taken from, sample for sample, on the
    /// real fixture's windows.
    #[test]
    fn matches_the_previous_chains_demodulator() {
        use crate::digital::drm::cellmap::CellMap as OldMap;
        use crate::digital::drm::ofdm::OfdmDemod as OldDemod;
        use crate::digital::drm::params::{
            RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy,
        };

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("bench layout");
        let old_map = OldMap::new(
            OldMode::from_index(RobustnessMode::B.index()).unwrap(),
            OldOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("bench layout");
        let mut demod = OfdmDemod::new(&map);
        let mut old_demod = OldDemod::new(&old_map);

        let iq = load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let windows = windows(&iq, 20);
        assert!(windows.len() >= 10, "the fixture must yield windows");
        let mut new_cells = Vec::new();
        let mut old_cells = Vec::new();
        for w in &windows {
            demod.demodulate(&w.samples, &mut new_cells);
            let old_window: Vec<crate::digital::drm::Cplx> = w
                .samples
                .iter()
                .map(|c| crate::digital::drm::Cplx::new(c.re, c.im))
                .collect();
            old_demod.demodulate(&old_window, &mut old_cells);
            assert_eq!(new_cells.len(), old_cells.len());
            for (a, b) in new_cells.iter().zip(&old_cells) {
                assert!(
                    (a.re - b.re).abs() < 1e-9 && (a.im - b.im).abs() < 1e-9,
                    "cell mismatch: {a:?} vs {b:?}"
                );
            }
        }
    }

    /// The demodulated pilots must carry the map's reference values scaled by the channel:
    /// on the clean fixture the scattered pilots' ratio `cell / reference` has a tangible
    /// magnitude and a bounded spread (a wrong carrier mapping, an off-by-one window or a
    /// broken FFT scaling shows up here immediately).
    #[test]
    fn scattered_pilot_ratios_are_consistent_on_the_clean_fixture() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let mut demod = OfdmDemod::new(&map);
        let iq = load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let windows = windows(&iq, 40);
        assert!(windows.len() >= 20, "the fixture must yield windows");
        let spsf = RobustnessMode::B.symbols_per_superframe();
        let mut magnitudes = Vec::new();
        let mut cells = Vec::new();
        let first_start = windows[0].start;
        let ts = RobustnessMode::B.symbol_len() as i64;
        for w in &windows {
            let sym = (((w.start - first_start) / ts) as usize) % spsf;
            demod.demodulate(&w.samples, &mut cells);
            for c in 0..map.num_carriers {
                if map.cell(sym, c).is_scattered() {
                    let r = map.pilot(sym, c);
                    if r.norm_sqr() > 0.0 {
                        magnitudes.push((cells[c] / r).norm());
                    }
                }
            }
        }
        assert!(!magnitudes.is_empty(), "the layout must have scattered pilots");
        let mean = magnitudes.iter().sum::<f64>() / magnitudes.len() as f64;
        let var =
            magnitudes.iter().map(|m| (m - mean).powi(2)).sum::<f64>() / magnitudes.len() as f64;
        let spread = var.sqrt() / mean;
        eprintln!("[ofdm] pilots={} mean|H|={mean:.5} spread={spread:.4}", magnitudes.len());
        // The absolute scale is the FFT's 1/N times the capture level, so only "power is
        // present" is asserted here; the meaningful check is that the ratios agree across the
        // frame (a wrong carrier mapping or window offset destroys that agreement).
        assert!(mean > 0.001, "pilots must carry power, mean |H| = {mean:.5}");
        assert!(spread < 0.4, "pilot ratios must be consistent, spread {spread:.3}");
    }

    /// Mode E's 216-point demodulator: a synthesised symbol with known values on a few
    /// carriers spanning the negative side, DC and the positive side comes back on the
    /// map's carriers, which is the "mode E trial signal demodulates" check of this task.
    #[test]
    fn mode_e_demodulates_a_synthesised_symbol() {
        let map = CellMap::new(RobustnessMode::E, SpectrumOccupancy::SO_0).unwrap();
        let mut demod = OfdmDemod::new(&map);
        let n = map.mode().fft_size();
        assert_eq!(n, 216);
        let carriers = [-106i32, -54, 0, 1, 53, 106];
        let mut window = vec![Cplx::zero(); n];
        for &k in &carriers {
            let x = Cplx::new(f64::from(k) + 200.0, f64::from(k) - 3.0);
            for (t, s) in window.iter_mut().enumerate() {
                let ph = 2.0 * core::f64::consts::PI * k as f64 * t as f64 / n as f64;
                *s += x * Cplx::from_polar(1.0, ph);
            }
        }
        let mut out = Vec::new();
        demod.demodulate(&window, &mut out);
        assert_eq!(out.len(), map.num_carriers);
        for &k in &carriers {
            let c = map.carrier_offset(k).unwrap();
            let want = Cplx::new(f64::from(k) + 200.0, f64::from(k) - 3.0);
            let got = out[c];
            assert!(
                (got.re - want.re).abs() < 1e-9 && (got.im - want.im).abs() < 1e-9,
                "carrier {k}: {got:?} vs {want:?}"
            );
        }
    }
}
