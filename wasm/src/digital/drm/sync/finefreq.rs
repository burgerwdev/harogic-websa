//! Fine carrier-offset estimation from the continuous pilots' phase rotation.
//!
//! The coarse acquisition (`freqacq`) resolves the DC carrier to one FFT bin (7.8 Hz here), and
//! what remains — a hertz or two — is enough to rotate every symbol's cells against the next
//! (1 Hz over three mode-B symbols is 86 degrees), which no channel estimate can follow. The
//! three continuous frequency pilots give the residual directly: their reference values are
//! fixed, so the phase of `cell / reference` rotates by `2*pi*f*Tsym` from one symbol to the
//! next, and averaging that rotation over the pilots and over many symbol pairs gives `f` with
//! sign. Dream tracks the same quantity with its frequency tracker.

use crate::digital::drm::cellmap::CellMap;
use crate::digital::drm::dsp::Cplx;

/// Estimates the residual carrier offset, Hz, from demodulated cell rows in symbol order.
/// `syms` gives each row's symbol index within the super frame (the map needs it for the
/// reference values). Returns `None` when no consecutive pair carries a frequency pilot.
pub fn estimate_residual_hz(map: &CellMap, rows: &[Vec<Cplx>], syms: &[usize]) -> Option<f64> {
    if rows.len() < 2 || rows.len() != syms.len() {
        return None;
    }
    let carriers: Vec<usize> = (0..map.num_carriers)
        .filter(|&c| map.cell(syms[0], c).is_freq_pilot())
        .collect();
    if carriers.is_empty() {
        return None;
    }
    let mut acc = Cplx::zero();
    let mut pairs = 0usize;
    for i in 1..rows.len() {
        // The channel may differ between the two symbols, but not by much over one symbol; the
        // pilots' own references are constant, so their ratio's phase is the offset's rotation.
        for &c in &carriers {
            let r_prev = map.pilot(syms[i - 1], c);
            let r_now = map.pilot(syms[i], c);
            if r_prev.norm_sqr() == 0.0 || r_now.norm_sqr() == 0.0 {
                continue;
            }
            let h_prev = rows[i - 1][c] / r_prev;
            let h_now = rows[i][c] / r_now;
            if h_prev.norm_sqr() == 0.0 || h_now.norm_sqr() == 0.0 {
                continue;
            }
            // The phase advance of H from one symbol to the next.
            acc += (h_now / h_prev).conj().conj();
            pairs += 1;
        }
    }
    if pairs == 0 {
        return None;
    }
    let mean = acc / pairs as f64;
    let tsym = map.mode().symbol_len() as f64 / f64::from(map.mode().sample_rate());
    // A positive offset advances the phase; solve for f from the wrapped mean rotation.
    let phase = mean.arg();
    Some(phase / (2.0 * core::f64::consts::PI * tsym))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm::ofdm::OfdmDemod;
    use crate::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
    use crate::digital::drm::sync::nco::Nco;
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

    /// Demodulate the fixture into rows and their symbol indices.
    fn rows_of(map: &CellMap, iq: &[Cplx]) -> (Vec<Vec<Cplx>>, Vec<usize>) {
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut demod = OfdmDemod::new(map);
        let mut cells = Vec::new();
        let mut rows = Vec::new();
        for block in iq.chunks(3248) {
            let _ = ts.push(block);
            while let Some(w) = ts.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 {
                    continue;
                }
                demod.demodulate(&w.samples, &mut cells);
                rows.push(cells.clone());
            }
        }
        let phase = crate::digital::drm::framesync::FrameSync::new(map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        let syms = (0..rows.len()).map(|i| (i % spf + phase) % spf).collect();
        (rows, syms)
    }

    /// A known offset applied to the fixture must be measured back.
    #[test]
    fn measures_a_synthetic_offset() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        for injected in [-2.0f64, -0.5, 0.5, 2.0] {
            let iq = load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
            let mut mixed = iq.clone();
            let mut nco = Nco::new(-injected, 48_000.0); // mixing by -injected leaves +injected in the signal
            nco.process(&mut mixed);
            let (rows, syms) = rows_of(&map, &mixed);
            let est = estimate_residual_hz(&map, &rows, &syms).expect("an estimate");
            eprintln!("[finefreq] injected {injected:+.1} Hz -> estimated {est:+.3} Hz");
            assert!(
                (est - injected).abs() < 0.2,
                "injected {injected:+.1} Hz but measured {est:+.3} Hz"
            );
        }
    }
}
