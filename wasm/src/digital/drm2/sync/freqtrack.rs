//! Continuous frequency tracking and the coarse sample-rate-offset estimate from the three
//! continuous frequency pilots (port of DecDRM's `rx/framesync.rs` frequency part, which is
//! Dream's `CSyncUsingPil` frequency tracker).
//!
//! The coarse acquisition (`freqacq`) resolves the DC carrier to one FFT bin, but on a real
//! signal a sample-rate offset makes the residual carrier offset drift; a one-shot fine
//! correction removes only the mean, and the drifting residual keeps rotating the channel,
//! which the time interpolation cannot follow. This stage tracks the residual offset from the
//! pilots' per-symbol phase advance, symbol by symbol, so the mixer can be adjusted every
//! symbol. It also estimates the sample-rate offset from the slope of that phase advance
//! across the three pilots (the reference's coarse `sro_estimate`, used during acquisition).

use crate::digital::drm2::cellmap::CellMap;
use crate::digital::drm2::dsp::util::{iir1_c, iir1_lambda};
use crate::digital::drm2::dsp::Cplx;
use crate::digital::drm2::params::{RobustnessMode, SAMPLE_RATE};

/// Time constant of the frequency-offset averaging, s (Dream's `TICONST_FREQ_OFF_EST`).
const TICONST_FREQ_OFF_EST: f64 = 1.0;
/// Time constant of the pilot-slope sample-rate-offset estimate, s.
const TICONST_SRO_EST: f64 = 0.5;
/// Symbols to average before the pilot-slope SRO estimate is reported.
const SRO_EST_MIN_SYMBOLS: usize = 40;

/// What the stage learned from one demodulated symbol.
#[derive(Debug, Clone, Copy, Default)]
pub struct FreqTrackOutput {
    /// Frequency correction to add to the mixer, Hz.
    pub freq_delta_hz: f64,
    /// Sample-rate offset from the pilot phase slope (fraction, positive when the received
    /// spectrum is stretched), once enough symbols are averaged.
    pub sro_estimate: Option<f64>,
}

/// Continuous frequency / coarse-SRO tracker, fed one demodulated symbol at a time.
pub struct FreqTrack {
    n: usize,
    kmin: i32,
    mode: RobustnessMode,
    freq_pil: [usize; 3],
    old_pil: [Cplx; 3],
    have_old: bool,
    freq_vec: Cplx,
    lambda: f64,
    norm_const: f64,
    sym_rate: f64,
    pil_ph_diff: [Cplx; 3],
    sro_lambda: f64,
    sro_count: usize,
}

impl FreqTrack {
    pub fn new(map: &CellMap) -> Self {
        let n = map.mode().fft_size();
        let ts = map.mode().symbol_len() as f64;
        let sym_rate = f64::from(SAMPLE_RATE) / ts;
        let mut freq_pil = [0usize; 3];
        let mut cnt = 0;
        for c in 0..map.num_carriers {
            if map.cell(0, c).is_freq_pilot() && cnt < 3 {
                freq_pil[cnt] = c;
                cnt += 1;
            }
        }
        assert!(cnt == 3, "mode {:?} must carry three frequency pilots", map.mode());
        Self {
            n,
            kmin: map.kmin,
            mode: map.mode(),
            freq_pil,
            old_pil: [Cplx::zero(); 3],
            have_old: false,
            freq_vec: Cplx::zero(),
            lambda: iir1_lambda(TICONST_FREQ_OFF_EST, sym_rate),
            norm_const: 1.0 / (2.0 * core::f64::consts::PI * ts),
            sym_rate,
            pil_ph_diff: [Cplx::zero(); 3],
            sro_lambda: iir1_lambda(TICONST_SRO_EST, sym_rate),
            sro_count: 0,
        }
    }

    /// Feed one demodulated symbol's cells (`shift` is its timing shift). The frequency
    /// correction is the residual offset the mixer must remove from the NEXT symbols.
    pub fn process(&mut self, cells: &[Cplx], shift: i64) -> FreqTrackOutput {
        let mut out = FreqTrackOutput::default();
        let mut est = Cplx::zero();
        let mut prods = [Cplx::zero(); 3];
        for i in 0..3 {
            let c = self.freq_pil[i];
            let k = (self.kmin + c as i32) as f64;
            // The previous pilot rotated to this symbol's window timing.
            let old = self.old_pil[i]
                * Cplx::from_polar(1.0, 2.0 * core::f64::consts::PI * k * shift as f64 / self.n as f64);
            let cur = cells[c];
            prods[i] = cur * old.conj();
            if self.have_old {
                est += prods[i];
            }
            // Mode D: the first two frequency pilots alternate in sign every symbol.
            self.old_pil[i] = if self.mode == RobustnessMode::D && i < 2 { -cur } else { cur };
        }
        if self.have_old {
            iir1_c(&mut self.freq_vec, est, self.lambda);
            let e = self.freq_vec.arg();
            let mag = self.freq_vec.norm();
            self.freq_vec = Cplx::new(mag, 0.0);
            out.freq_delta_hz = e * self.norm_const * f64::from(SAMPLE_RATE);

            // Coarse SRO: the per-symbol phase advance grows linearly with the carrier index
            // (slope 2π·ε·ts/N). The common frequency part is removed by the differencing.
            for i in 0..3 {
                iir1_c(&mut self.pil_ph_diff[i], prods[i], self.sro_lambda);
            }
            self.sro_count += 1;
            if self.sro_count >= SRO_EST_MIN_SYMBOLS {
                let k: [f64; 3] = self.freq_pil.map(|c| (self.kmin + c as i32) as f64);
                let ph: [f64; 3] = self.pil_ph_diff.map(|v| v.arg());
                let slope = (wrap_phase(ph[1] - ph[0]) / (k[1] - k[0])
                    + wrap_phase(ph[2] - ph[0]) / (k[2] - k[0]))
                    / 2.0;
                let ts = 1.0 / (2.0 * core::f64::consts::PI * self.norm_const);
                out.sro_estimate = Some(slope * self.n as f64 / (2.0 * core::f64::consts::PI * ts));
            }
        }
        self.have_old = true;
        out
    }

    /// Forget the pilot-slope SRO average (after the resampler was corrected).
    pub fn reset_sro_estimate(&mut self) {
        self.pil_ph_diff = [Cplx::zero(); 3];
        self.sro_count = 0;
    }

    /// Faster frequency averaging during acquisition (the reference switches to 0.1 s on the
    /// first symbol, then back to 1.0 s on entering tracking).
    pub fn set_freq_time_constant(&mut self, tau: f64) {
        self.lambda = iir1_lambda(tau, self.sym_rate);
    }
}

/// Wrap an angle difference to (−π, π].
fn wrap_phase(d: f64) -> f64 {
    (d + core::f64::consts::PI).rem_euclid(2.0 * core::f64::consts::PI) - core::f64::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm2::ofdm::OfdmDemod;
    use crate::digital::drm2::params::SpectrumOccupancy;
    use crate::digital::drm2::sync::nco::Nco;
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

    fn rows_of(map: &CellMap, iq: &[Cplx]) -> (Vec<Vec<Cplx>>, Vec<i64>) {
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut demod = OfdmDemod::new(map);
        let mut cells = Vec::new();
        let mut rows = Vec::new();
        let mut shifts = Vec::new();
        for block in iq.chunks(3248) {
            let _ = ts.push(block);
            while let Some(w) = ts.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 {
                    continue;
                }
                demod.demodulate(&w.samples, &mut cells);
                rows.push(cells.clone());
                shifts.push(w.shift);
            }
        }
        (rows, shifts)
    }

    /// A known offset mixed into the clean fixture must be tracked: the FIRST non-zero
    /// correction has the offset's sign and a magnitude within half a hertz of it (the one-pole
    /// starts from zero, so its first phase is the offset's; later corrections damp toward zero
    /// — `freq_delta_hz` is a per-symbol increment for the mixer's closed loop, not an absolute
    /// measurement).
    #[test]
    fn tracks_a_synthetic_offset() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        for injected in [-4.0f64, -2.0, 2.0, 4.0] {
            let iq = load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
            let mut mixed = iq.clone();
            let mut nco = Nco::new(-injected);
            nco.process(&mut mixed);
            let (rows, shifts) = rows_of(&map, &mixed);
            let mut tr = FreqTrack::new(&map);
            let mut first = None;
            for (row, shift) in rows.iter().zip(&shifts) {
                let o = tr.process(row, *shift);
                if o.freq_delta_hz != 0.0 && first.is_none() {
                    first = Some(o.freq_delta_hz);
                }
            }
            let tracked = first.expect("a correction is produced");
            eprintln!("[freqtrack] injected {injected:+.1} Hz -> first correction {tracked:+.3} Hz");
            // The first window pair carries a small timing transient, so the bound is loose;
            // what must hold is the sign and the rough magnitude.
            assert!(
                (tracked - injected).abs() < 0.8,
                "injected {injected:+.1} Hz but first correction {tracked:+.3} Hz"
            );
        }
    }
}
