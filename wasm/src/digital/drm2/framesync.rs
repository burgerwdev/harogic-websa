//! Frame synchronisation: the frame phase from the time pilots. Port of Dream's `CFrameSync`
//! (`src/sync/FrameSync.cpp`).
//!
//! The time pilots sit at fixed carriers and their reference phases follow a fixed sequence
//! within the frame, so correlating the received pilot cells against that reference sequence for
//! every candidate frame phase identifies where the frame starts: the correct phase makes all
//! the pilots add coherently, every other phase lets their phases average out. The score is the
//! magnitude of that coherent sum per frame start, so it is comparable across captures.
//!
//! This runs on the demodulated cells (`cells[c]` with `c = k - Kmin`), which is why it belongs
//! with the OFDM demodulation rather than with the sample-level sync stages.

use crate::digital::drm2::cellmap::CellMap;
use crate::digital::drm2::dsp::Cplx;
use crate::digital::drm2::tables;

/// One super frame's worth of demodulated cells, in symbol order from the start of a super
/// frame.
pub struct FrameSync {
    mode: crate::digital::drm2::params::RobustnessMode,
    /// Absolute carrier index -> cell index, resolved once for the pilot table.
    kmin: i32,
    num_carriers: usize,
}

/// The result of a frame-sync search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncResult {
    /// The frame phase: the index (within the frame) of the first row of `rows`.
    pub phase: usize,
    /// The score of that phase (coherent pilot magnitude per frame start).
    pub score: f64,
}

impl FrameSync {
    pub fn new(map: &CellMap) -> Self {
        Self { mode: map.mode(), kmin: map.kmin, num_carriers: map.num_carriers }
    }

    /// Search the frame phase over `rows` (demodulated symbols in capture order). A row count of
    /// at least a few frames is needed for the score to separate the phases.
    pub fn search(&self, rows: &[Vec<Cplx>]) -> SyncResult {
        let tp = tables::time_pilots(self.mode);
        let spf = self.mode.symbols_per_frame();
        // The pilot table is in absolute carrier indices; pre-resolve them to cell indices and
        // drop the ones outside the occupied band (an occupancy narrower than the mode's full
        // band can cut a time pilot off).
        let pilots: Vec<(usize, f64)> = tp
            .iter()
            .filter_map(|&(k, phase)| {
                let c = i32::from(k) - self.kmin;
                (c >= 0 && (c as usize) < self.num_carriers).then(|| {
                    (c as usize, 2.0 * core::f64::consts::PI * f64::from(phase) / 1024.0)
                })
            })
            .collect();
        let mut best = SyncResult { phase: 0, score: -1.0 };
        for r in 0..spf {
            let mut acc = Cplx::zero();
            let mut starts = 0.0f64;
            for (i, row) in rows.iter().enumerate() {
                if (i + r) % spf != 0 {
                    continue;
                }
                starts += 1.0;
                for &(c, angle) in &pilots {
                    if let Some(cell) = row.get(c) {
                        acc += *cell * Cplx::from_polar(1.0, -angle);
                    }
                }
            }
            let score = acc.norm() / starts.max(1.0) / pilots.len().max(1) as f64;
            if score > best.score {
                best = SyncResult { phase: r, score };
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm2::ofdm::OfdmDemod;
    use crate::digital::drm2::params::{RobustnessMode, SpectrumOccupancy};
    use crate::digital::drm2::sync::timesync::TimeSync;

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

    /// Demodulate a capture into cell rows, one per symbol window the timing stage emits.
    fn cell_rows(iq: &[Cplx]) -> Vec<Vec<Cplx>> {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut demod = OfdmDemod::new(&map);
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        for block in iq.chunks(3248) {
            let _ = ts.push(block);
            while let Some(w) = ts.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 {
                    continue; // do not let a mistimed window into the frame search
                }
                demod.demodulate(&w.samples, &mut cells);
                rows.push(cells.clone());
            }
        }
        rows
    }

    #[test]
    fn finds_the_frame_phase_on_the_clean_fixture() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let fs = FrameSync::new(&map);
        let rows = cell_rows(&load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32"));
        assert!(rows.len() > 3 * RobustnessMode::B.symbols_per_frame());
        let found = fs.search(&rows);
        eprintln!("[framesync] phase={} score={:.4}", found.phase, found.score);
        assert!(found.score > 0.01, "the correct phase must stand out: {}", found.score);
        // The phase is a property of the capture, not of where we start looking: searching a
        // window shifted by whole frames must agree modulo the frame length.
        let spf = RobustnessMode::B.symbols_per_frame();
        let shifted = fs.search(&rows[spf..]);
        assert_eq!(
            (found.phase + spf - shifted.phase) % spf,
            0,
            "phase {} vs {} after shifting by one frame",
            found.phase,
            shifted.phase
        );
    }

    #[test]
    fn finds_the_frame_phase_on_the_committed_live_fixture() {
        use crate::ddc::resampler::ComplexResampler;
        let raw = std::fs::read("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32").expect("live");
        let iq: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut rs = ComplexResampler::new(48_828.125, 48_000.0);
        let mut converted: Vec<f32> = Vec::new();
        rs.process_f32_into(&iq, &mut converted);
        let iq: Vec<Cplx> = converted
            .chunks_exact(2)
            .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
            .collect();
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let rows = cell_rows(&iq);
        assert!(rows.len() > 2 * RobustnessMode::B.symbols_per_frame());
        let found = FrameSync::new(&map).search(&rows);
        eprintln!("[framesync/live] phase={} score={:.4}", found.phase, found.score);
        assert!(found.score > 0.01, "the bench capture must sync: {}", found.score);
    }
}
