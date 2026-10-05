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

use crate::digital::drm::cellmap::CellMap;
use crate::digital::drm::dsp::Cplx;
use crate::digital::drm::tables;

/// One super frame's worth of demodulated cells, in symbol order from the start of a super
/// frame.
pub struct FrameSync {
    mode: crate::digital::drm::params::RobustnessMode,
    /// Absolute carrier index -> cell index, resolved once for the pilot table.
    kmin: i32,
    num_carriers: usize,
}

/// The time pilots resolved to cell indices with their reference phases (radians), dropping
/// the ones outside the occupied band (an occupancy narrower than the mode's full band can cut
/// a time pilot off).
fn pilot_table(mode: crate::digital::drm::params::RobustnessMode, kmin: i32, num_carriers: usize) -> Vec<(usize, f64)> {
    tables::time_pilots(mode)
        .iter()
        .filter_map(|&(k, phase)| {
            let c = i32::from(k) - kmin;
            (c >= 0 && (c as usize) < num_carriers).then(|| {
                (c as usize, 2.0 * core::f64::consts::PI * f64::from(phase) / 1024.0)
            })
        })
        .collect()
}

/// Streaming frame-phase acquisition: the time-pilot correlation is accumulated per candidate
/// phase as rows arrive, so the phase can be read at any point without re-scanning the capture.
///
/// This is the block-fed counterpart of [`FrameSync::search`]: the whole-buffer search re-reads
/// every row each time, while the accumulator adds one row per call. The two agree — the
/// accumulator over rows `0..n` is exactly `search(&rows[0..n])` — but the accumulator lets the
/// receiver keep the phase-0 reference while the capture streams in, and lets it *wait* until
/// the correct phase separates from the noise instead of committing on the first few frames.
pub struct FramePhaseAcquisition {
    pilots: Vec<(usize, f64)>,
    spf: usize,
    /// Coherent pilot sum per candidate phase (indexed by the phase of row 0).
    acc: Vec<Cplx>,
    /// Frame starts counted per candidate phase.
    starts: Vec<f64>,
    rows: usize,
}

impl FramePhaseAcquisition {
    pub fn new(map: &CellMap) -> Self {
        Self::new_from_parts(pilot_table(map.mode(), map.kmin, map.num_carriers), map.mode().symbols_per_frame())
    }

    fn new_from_parts(pilots: Vec<(usize, f64)>, spf: usize) -> Self {
        Self { pilots, spf, acc: vec![Cplx::zero(); spf], starts: vec![0.0; spf], rows: 0 }
    }

    /// Add one demodulated symbol (in capture order) to the accumulator.
    pub fn push(&mut self, row: &[Cplx]) {
        let r = (self.spf - self.rows % self.spf) % self.spf;
        let mut a = Cplx::zero();
        for &(c, angle) in &self.pilots {
            if let Some(cell) = row.get(c) {
                a += *cell * Cplx::from_polar(1.0, -angle);
            }
        }
        self.acc[r] += a;
        self.starts[r] += 1.0;
        self.rows += 1;
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The score of every candidate phase, as [`FrameSync::search`] computes it.
    pub fn scores(&self) -> Vec<f64> {
        let denom = self.pilots.len().max(1) as f64;
        self.acc
            .iter()
            .zip(&self.starts)
            .map(|(a, s)| a.norm() / s.max(1.0) / denom)
            .collect()
    }

    /// `(phase, best_score, second_best_score)` over the rows accumulated so far.
    pub fn best(&self) -> (usize, f64, f64) {
        let scores = self.scores();
        let mut best = (0usize, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (r, &s) in scores.iter().enumerate() {
            if s > best.1 {
                best = (r, s, best.1);
            } else if s > best.2 {
                best.2 = s;
            }
        }
        best
    }
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
        let mut acq = FramePhaseAcquisition::new_from_parts(
            pilot_table(self.mode, self.kmin, self.num_carriers),
            self.mode.symbols_per_frame(),
        );
        for row in rows {
            acq.push(row);
        }
        let (phase, score, _) = acq.best();
        SyncResult { phase, score }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm::ofdm::OfdmDemod;
    use crate::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
    use crate::digital::drm::sync::timesync::TimeSync;

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
