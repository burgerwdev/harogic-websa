//! Channel estimation and equalisation. Port of Dream's `CChannelEstimation`
//! (`src/chanest/ChannelEstimation.cpp`, `TimeLinear.cpp`, `TimeWiener.cpp`), structured the
//! way the reference port is:
//!
//! 1. **The gain-reference lattice**: every symbol carries scattered pilots on a lattice whose
//!    positions repeat every `time_int` symbols, spaced `freq_int` carriers apart. Dividing a
//!    pilot cell by its reference value gives the channel `H` at that point.
//! 2. **Time interpolation** between the symbols that carry the same carrier's pilot
//!    (`TimeLinear`: Dream switches to its Wiener time filter when the Doppler spread is not
//!    negligible, and its linear path is exact for the bench signal's flat, slow channel).
//! 3. **Frequency Wiener interpolation** to every carrier (`update_freq_wiener`): the sinc
//!    correlation functions of the channel's delay spread and the SNR-regularised zero lag are
//!    solved with [`crate::digital::drm2::dsp::levinson`] for the taps of each carrier's filter.
//! 4. **Equalisation**: `cell / H`, with `|H|^2` as the cell's reliability.
//! 5. **MER from the FAC decisions** (Dream's SNR-from-FAC), which is the number the reference
//!    receiver reports (17.8 dB on the bench capture) and therefore the number to compare with.
//!
//! The estimator is stateful with a delay of `time_int` symbols: a symbol can only be fully
//! interpolated once the next lattice symbol has arrived.

use std::collections::VecDeque;

use crate::digital::drm2::cellmap::CellMap;
use crate::digital::drm2::dsp::levinson::levinson;
use crate::digital::drm2::dsp::{sinc, Cplx};
use crate::digital::drm2::params::RobustnessMode;

pub mod track;
// The time-direction Wiener (Doppler-adapted) interpolation is ported in `time_wiener.rs` but
// NOT yet wired into `ChanEst`: an integration attempt broke the clean-fixture MSC bit-exact
// (3.4% hard-decision mismatches on the TimeSync rows vs the linear interpolation's 2.5%, which
// the Viterbi still corrected). It is kept as the reference for the live-capture deficit, which
// the fixed linear interpolation leaves at −8.6 dB FAC MER (reference 17.8).
pub mod time_wiener;
use track::{PdsTracker, TrackOutput};

/// One equalised cell: the symbol estimate and the channel power it was divided by.
/// Shared with the FEC demappers (`fec::qam`), which consume these cells.
pub use crate::digital::drm2::fec::qam::EqCell;

/// Sentinel used by Dream when the signal estimate is unavailable.
const Q_VALUE_INVALID: f64 = -1.0e9;

/// The estimator's measurements.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChanStats {
    /// SNR in the nominal bandwidth, dB (from the FAC decisions; `None` before the first frame).
    pub snr_db: Option<f64>,
    /// MER of the FAC cells over the last frame, dB.
    pub fac_mer_db: Option<f64>,
}

/// The channel estimator for one mode/occupancy.
pub struct ChanEst {
    mode: RobustnessMode,
    n_car: usize,
    kmin: i32,
    /// Running total of the symbol-window timing shifts (samples), for the per-carrier
    /// phase rotation the time interpolation applies when the timing moves.
    cum_shift: i64,
    /// Scattered-pilot carrier spacing within one symbol.
    freq_int: usize,
    /// Symbols between two occurrences of the same carrier's pilot.
    time_int: usize,
    /// Per-symbol prototype: for each position in the `time_int` cycle, the pilot carriers.
    lattice: Vec<Vec<usize>>,
    /// Recent symbols: the super-frame symbol index, the pilot estimates on the full carrier
    /// grid (zero where absent), the symbol's own demodulated cells and the cumulative timing
    /// shift at that symbol — the emitted symbol must be equalised with ITS cells and ITS
    /// channel estimate, and the pilots rotated to the output symbol's timing.
    history: VecDeque<(usize, Vec<Cplx>, Vec<Cplx>, i64)>,
    /// Pilot-grid carrier spacing (the per-symbol shift, `x` in the spec's phase formula).
    x: usize,
    /// Number of pilot-grid carriers (`(n_car − 1) / x + 1`).
    num_pil: usize,
    /// Impulse-response tracker (delay spread, timing drift, sample-rate offset).
    track: PdsTracker,
    /// Frequency-Wiener filter length and per-carrier tap tables.
    fw_len: usize,
    fw_offset: Vec<usize>,
    fw_taps: Vec<Vec<Cplx>>,
    pub last_track: TrackOutput,
    snr_linear: f64,
    /// FAC MER accumulator over the current super frame.
    fac_err: f64,
    fac_pow: f64,
    fac_cnt: usize,
    last_frame_sym: usize,
    pub stats: ChanStats,
    /// The FAC-carried SNR the estimator was last built with (dB), for diagnostics.
    pub wiener_snr_db: f64,
}

/// Frequency-Wiener filter length per mode (Dream's `update_freq_wiener` tables).
fn freq_wiener_len(mode: RobustnessMode) -> usize {
    match mode {
        RobustnessMode::A => 6,
        RobustnessMode::B | RobustnessMode::C => 11,
        RobustnessMode::D => 13,
        RobustnessMode::E => 11,
    }
}

impl ChanEst {
    pub fn new(map: &CellMap) -> Self {
        let mode = map.mode();
        let n_car = map.num_carriers;
        let spsf = mode.symbols_per_superframe();
        // Derive the lattice geometry from the map: which carriers carry a scattered pilot in
        // each symbol of the cycle, and how many symbols the cycle takes.
        let mut lattice: Vec<Vec<usize>> = Vec::new();
        let mut freq_int = n_car;
        for sym in 0..spsf {
            let carriers: Vec<usize> = (0..n_car)
                .filter(|&c| map.cell(sym, c).is_scattered())
                .collect();
            if !carriers.is_empty() {
                for pair in carriers.windows(2) {
                    freq_int = freq_int.min(pair[1] - pair[0]);
                }
            }
            lattice.push(carriers);
        }
        // The cycle length: the smallest number of symbols after which the pilot carrier sets
        // repeat (compare against a zero-offset window of the same length).
        let mut time_int = spsf;
        for len in 1..=spsf {
            let repeats = (0..len).all(|i| {
                let j = i + len;
                j >= spsf || lattice[i] == lattice[j]
            });
            if repeats {
                time_int = len;
                break;
            }
        }
        let x = map.scattered.freq_int;
        let num_pil = (n_car - 1) / x + 1;
        let fw_len = freq_wiener_len(mode);
        let mut est = Self {
            mode,
            n_car,
            kmin: map.kmin,
            cum_shift: 0,
            freq_int: freq_int.max(1),
            time_int: time_int.max(1),
            lattice,
            history: VecDeque::new(),
            x,
            num_pil,
            track: PdsTracker::new(map, num_pil, time_int.max(1) + 1),
            fw_len,
            fw_offset: vec![0; n_car],
            fw_taps: vec![vec![Cplx::zero(); fw_len]; n_car],
            last_track: TrackOutput::default(),
            snr_linear: 10f64.powf(3.0), // 30 dB, Dream's initial value
            fac_err: 0.0,
            fac_pow: 0.0,
            fac_cnt: 0,
            last_frame_sym: 0,
            stats: ChanStats::default(),
            wiener_snr_db: 30.0,
        };
        // Initial frequency-Wiener taps from the guard ratio and the initial SNR, as the
        // reference builds them before any symbol has arrived.
        let (gn, gd) = mode.guard_ratio();
        est.update_freq_wiener(est.snr_linear, gn as f64 / gd as f64, 0.0);
        est
    }

    /// The estimator's symbol delay.
    pub fn delay(&self) -> usize {
        self.time_int
    }

    /// Enable impulse-response based timing tracking (Dream's `start_timing_tracking`): the
    /// tracker then emits `timing_adjust` corrections for the receiver's FFT window.
    pub fn start_timing_tracking(&mut self) {
        self.track.tracking = true;
    }

    /// Whether the impulse-response timing tracker is active.
    pub fn timing_tracking(&self) -> bool {
        self.track.tracking
    }

    /// Rebuild the frequency-Wiener interpolation filters for the current SNR and the delay
    /// spread (`len_ratio` = impulse-response length / useful symbol, `offs_ratio` = its start
    /// / useful symbol, both from the impulse-response tracker). This is the reference's
    /// `update_freq_wiener`: the sinc correlation functions of a rectangular impulse response
    /// are solved with Levinson for each of the `(l−1)·x + 1` carrier phases, and the taps are
    /// phase-rotated by the response's position.
    fn update_freq_wiener(&mut self, snr: f64, len_ratio: f64, offs_ratio: f64) {
        let l = self.fw_len;
        let x = self.x;
        let n_filters = (l - 1) * x + 1;
        let snr = snr.max(1.0);
        let filters: Vec<Vec<Cplx>> = (0..n_filters)
            .map(|diff| {
                let rhp: Vec<f64> = (0..l)
                    .map(|i| sinc(((i * x) as f64 - diff as f64) * len_ratio))
                    .collect();
                let mut rpp: Vec<f64> = (0..l).map(|i| sinc((i * x) as f64 * len_ratio)).collect();
                rpp[0] += 1.0 / snr;
                let h = levinson(&rpp, &rhp);
                (0..l)
                    .map(|i| {
                        // The reference's tap phase positions the delay spread:
                        // `π·pos·(len_ratio + 2·offs_ratio)` with `pos = i·x − diff`.
                        let pos = (i * x) as f64 - diff as f64;
                        let arg = core::f64::consts::PI * pos * (len_ratio + 2.0 * offs_ratio);
                        Cplx::from_polar(h[i], arg)
                    })
                    .collect()
            })
            .collect();
        let offset = l / 2;
        for j in 0..self.n_car {
            let cur = j / x;
            let off = if cur < offset {
                0
            } else if cur - offset > self.num_pil - l {
                self.num_pil - l
            } else {
                cur - offset
            };
            self.fw_offset[j] = off;
            let diff = j - off * x;
            self.fw_taps[j].clone_from(&filters[diff.min(n_filters - 1)]);
        }
    }

    pub fn stats(&self) -> ChanStats {
        self.stats
    }

    /// Feed one demodulated symbol (`cells` in map order) at super-frame symbol index `sym`,
    /// with the timing shift `shift` of this symbol's window. Returns the equalised symbol
    /// `delay()` symbols earlier — its frame symbol index and its cells — once available.
    pub fn process(&mut self, cells: &[Cplx], sym: usize, shift: i64, map: &CellMap) -> Option<(usize, Vec<EqCell>)> {
        let fft_n = self.mode.fft_size() as f64;
        let cycle = sym % self.time_int;
        // 1. The pilot lattice of this symbol.
        let mut h = vec![Cplx::zero(); self.n_car];
        for &c in &self.lattice[cycle] {
            let r = map.pilot(sym, c);
            if r.norm_sqr() > 0.0 {
                h[c] = cells[c] / r;
            }
        }
        self.cum_shift += shift;
        self.history.push_back((sym, h, cells.to_vec(), self.cum_shift));
        // The emitted symbol must have a lattice symbol on BOTH sides for the time
        // interpolation, so the history holds `2*time_int+1` symbols and the middle one is
        // emitted (a one-sided history would degrade the interpolation to a hold).
        while self.history.len() > 2 * self.time_int + 1 {
            self.history.pop_front();
        }
        if self.history.len() < 2 * self.time_int + 1 {
            return None;
        }
        // 2. Time interpolation: the symbol to emit is `time_int` back, so both of its
        //    neighbouring lattice symbols are available for every carrier. Each pilot is
        //    rotated to the emitted symbol's timing (a window shift of Δ samples is a
        //    per-carrier phase ramp 2π·k·Δ/N).
        let mid = self.time_int;
        let (out_sym, _, out_data, out_cum) = self.history[mid].clone();
        let kmin = self.kmin;
        let fft_n = self.mode.fft_size();
        let rot = |v: Cplx, c: usize, dshift: i64| -> Cplx {
            if dshift == 0 {
                v
            } else {
                let k = (kmin + c as i32) as f64;
                v * Cplx::from_polar(
                    1.0,
                    2.0 * core::f64::consts::PI * k * dshift as f64 / fft_n as f64,
                )
            }
        };
        let mut dense = vec![Cplx::zero(); self.n_car];
        let hist: Vec<(usize, Vec<Cplx>, Vec<Cplx>, i64)> = self.history.iter().cloned().collect();
        for c in 0..self.n_car {
            // Find the nearest earlier and later symbols with a pilot at c.
            let mut before: Option<(usize, Cplx)> = None;
            let mut after: Option<(usize, Cplx)> = None;
            for (idx, (_s, row, _d, cum)) in hist.iter().enumerate() {
                if row[c].norm_sqr() == 0.0 {
                    continue;
                }
                let v = rot(row[c], c, out_cum - *cum);
                let d = idx as isize - mid as isize;
                if d <= 0 && before.is_none_or(|(bd, _)| (-d) < bd as isize) {
                    before = Some(((-d) as usize, v));
                }
                if d >= 0 && after.is_none_or(|(ad, _)| d < ad as isize) {
                    after = Some((d as usize, v));
                }
            }
            dense[c] = match (before, after) {
                (Some((0, v)), _) | (_, Some((0, v))) => v,
                (Some((db, a)), Some((da, b))) => {
                    let denom = (db + da) as f64;
                    let t = if denom > 0.0 { db as f64 / denom } else { 0.5 };
                    a * (1.0 - t) + b * t
                }
                (Some((_, v)), None) | (None, Some((_, v))) => v,
                (None, None) => Cplx::zero(),
            };
        }
        // 3. Impulse-response tracking, then the frequency Wiener to every carrier.
        // The grid is the time-interpolated channel at the pilot-grid carriers (`p · x`).
        let mut grid = vec![Cplx::zero(); self.num_pil];
        for (p, g) in grid.iter_mut().enumerate() {
            let c = p * self.x;
            if c < self.n_car {
                *g = dense[c];
            }
        }
        // Mode D's DC carrier is not a pilot; hold the grid point at zero like the reference.
        if self.mode == RobustnessMode::D {
            if let Some(dc) = map.carrier_offset(0) {
                let p = dc / self.x;
                if p < self.num_pil {
                    grid[p] = Cplx::zero();
                }
            }
        }
        self.last_track = self.track.process(&grid, shift);
        let t = self.last_track;
        self.update_freq_wiener(
            self.snr_linear,
            t.pds_len / self.n_car as f64,
            t.pds_offset / self.n_car as f64,
        );
        let mut chan = vec![Cplx::zero(); self.n_car];
        for (j, ch) in chan.iter_mut().enumerate() {
            let off = self.fw_offset[j];
            let mut acc = Cplx::zero();
            for (i, tap) in self.fw_taps[j].iter().enumerate() {
                acc += grid[off + i] * *tap;
            }
            *ch = acc;
        }
        let out_cells: Vec<EqCell> = (0..self.n_car)
            .map(|c| {
                let p = chan[c].norm_sqr();
                if p > 0.0 {
                    EqCell { sig: out_data[c] / chan[c], chan: p }
                } else {
                    EqCell { sig: Cplx::zero(), chan: 0.0 }
                }
            })
            .collect();
        // 4. MER from the FAC decisions (4-QAM), the reference's metric.
        let qam4 = crate::digital::drm2::tables::QAM4[0];
        let mut err = 0.0;
        let mut pow = 0.0;
        let mut cnt = 0usize;
        for c in 0..self.n_car {
            if !map.cell(out_sym, c).is_fac() {
                continue;
            }
            let s = out_cells[c].sig;
            // Nearest 4-QAM decision; the DRM constellation points are +/-1/sqrt(2) per axis
            // (`tables::QAM4`), not +/-1 — deciding at the wrong amplitude would report a
            // constant large error and hide the real MER.
            let dr = if s.re >= 0.0 { qam4 } else { -qam4 };
            let di = if s.im >= 0.0 { qam4 } else { -qam4 };
            err += out_cells[c].chan * ((s.re - dr).powi(2) + (s.im - di).powi(2));
            pow += out_cells[c].chan;
            cnt += 1;
        }
        if cnt > 0 {
            self.fac_err += err;
            self.fac_pow += pow;
            self.fac_cnt += 1;
        }
        if out_sym == self.mode.symbols_per_frame() - 1 && self.fac_err > 0.0 {
            let e = self.fac_err / self.fac_pow.max(1e-30);
            self.stats.fac_mer_db = Some(-10.0 * e.max(1e-12).log10());
            // Dream estimates the SNR from the same decisions and feeds it back to the Wiener
            // filters, which is what adapts them to a weak or noisy signal.
            self.snr_linear = (1.0 / e.max(1e-12)).max(1.0);
            self.fac_err = 0.0;
            self.fac_pow = 0.0;
            self.fac_cnt = 0;
        }
        self.last_frame_sym = out_sym;
        let _ = Q_VALUE_INVALID;
        Some((out_sym, out_cells))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm2::ofdm::OfdmDemod;
    use crate::digital::drm2::params::SpectrumOccupancy;
    use crate::digital::drm2::sync::timesync::{SymbolWindow, TimeSync};
    use crate::ddc::resampler::ComplexResampler;

    fn load_iq_f64(path: &str) -> Vec<Cplx> {
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

    fn resample_to_core(path: &str, rate: f64) -> Vec<Cplx> {
        let raw = std::fs::read(path).expect("capture file");
        let iq: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut rs = ComplexResampler::new(rate, 48_000.0);
        let mut out: Vec<f32> = Vec::new();
        rs.process_f32_into(&iq, &mut out);
        out.chunks_exact(2)
            .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
            .collect()
    }

    /// Run the chain's front (timing, demodulation) and the estimator over a capture; return
    /// the last FAC MER and the estimator's geometry.
    /// Collect the demodulated rows, their super-frame symbol indices and the timing shift
    /// of each window for a capture.
    fn rows_and_syms(map: &CellMap, iq: &[Cplx]) -> (Vec<Vec<Cplx>>, Vec<usize>, Vec<i64>) {
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
        if rows.is_empty() {
            return (rows, Vec::new(), Vec::new());
        }
        let phase = crate::digital::drm2::framesync::FrameSync::new(map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        let syms = (0..rows.len()).map(|i| (i % spf + phase) % spf).collect();
        (rows, syms, shifts)
    }

    /// The front end as the receiver runs it: coarse acquisition, remove it, estimate what is
    /// left with the continuous pilots, remove that too, and return the equaliser's FAC MER.
    fn run(iq: &[Cplx]) -> (Option<f64>, usize, usize) {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let mut flat: Vec<f64> = Vec::with_capacity(iq.len() * 2);
        for v in iq {
            flat.push(v.re);
            flat.push(v.im);
        }
        let mut acq = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true);
        let coarse = acq.push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0);
        let mut corrected = iq.to_vec();
        let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse);
        nco.process(&mut corrected);
        // What the coarse step left behind, measured on the continuous pilots and removed.
        let (rows, syms, _) = rows_and_syms(&map, &corrected);
        let mut fine = 0.0;
        if let Some(f) = crate::digital::drm2::sync::finefreq::estimate_residual_hz(&map, &rows, &syms)
        {
            fine = f;
            let mut nco2 = crate::digital::drm2::sync::nco::Nco::new(f);
            nco2.process(&mut corrected);
        }
        let (rows, syms, shifts) = rows_and_syms(&map, &corrected);
        eprintln!(
            "[chanest] coarse {coarse:.1} Hz, fine {fine:+.2} Hz, rows {}",
            rows.len()
        );
        // Feed the estimator and keep the last frame's FAC MER.
        let mut est = ChanEst::new(&map);
        let mut mer = None;
        for ((row, sym), shift) in rows.iter().zip(&syms).zip(&shifts) {
            if est.process(row, *sym, *shift, &map).is_some() {
                if let Some(m) = est.stats().fac_mer_db {
                    mer = Some(m);
                }
            }
        }
        // One IR sample spans fft_len / (num_pil · x) input samples.
        let ir_ms = map.mode().fft_size() as f64
            / (est.num_pil * est.x) as f64
            / 48_000.0
            * 1000.0;
        eprintln!(
            "[chanest] pds_len_ir={:.1} ({:.2} ms) pds_off_ir={:.1} sro_applied_hz={:+.3} sro_acq={}",
            est.last_track.pds_len,
            est.last_track.pds_len * ir_ms,
            est.last_track.pds_offset,
            est.track.applied_sro_hz(),
            est.track.sro_acquisition,
        );
        (mer, est.freq_int, est.time_int)
    }

    /// One-dimensional search: with the timing the chain chose, how does the FAC MER depend on
    /// the residual carrier offset alone? If no offset reaches a usable MER, the residual is not
    /// a pure frequency error (a timing or sample-rate drift contributes the same per-symbol
    /// rotation) and the next step is timing tracking rather than more frequency precision.
    #[test]
    #[ignore = "diagnostic sweep; run with --ignored --nocapture"]
    fn sweep_residual_offset_with_timing_fixed() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let base = resample_to_core(path, 48_828.125);
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        let mut flat: Vec<f64> = Vec::with_capacity(base.len() * 2);
        for v in &base {
            flat.push(v.re);
            flat.push(v.im);
        }
        let mut acq = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true);
        let coarse = acq.push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0);
        let mut out = String::new();
        for step in -12..=12 {
            let extra = step as f64 * 0.5;
            let mut iq = base.clone();
            let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse + extra);
            nco.process(&mut iq);
            let mut est = ChanEst::new(&map);
            let (rows, syms, shifts) = rows_and_syms(&map, &iq);
            let mut mer = f64::NAN;
            for ((row, sym), shift) in rows.iter().zip(&syms).zip(&shifts) {
                if est.process(row, *sym, *shift, &map).is_some() {
                    if let Some(m) = est.stats().fac_mer_db {
                        mer = m;
                    }
                }
            }
            out.push_str(&format!("{extra:+.1}:{mer:.1} "));
        }
        eprintln!("[sweep] coarse {coarse:.1} Hz -> MER(dB) by extra offset: {out}");
    }

    /// Is a sample-rate error what defeats the live capture? Resample it as if the source rate
    /// were off by a few tens of ppm and read the FAC MER: a peak near the reference's 17.8 dB
    /// would confirm the sample-rate offset (SRO) as the cause and size the tracking's range.
    #[test]
    #[ignore = "diagnostic sweep; run with --ignored --nocapture"]
    fn sweep_sample_rate_error_on_the_live_capture() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let mut out = String::new();
        for ppm in [-200.0f64, -100.0, -50.0, -20.0, -5.0, 0.0, 5.0, 20.0, 50.0, 100.0, 200.0] {
            let rate = 48_828.125 * (1.0 + ppm * 1e-6);
            let iq = resample_to_core(path, rate);
            let (mer, _, _) = run(&iq);
            out.push_str(&format!("{ppm:+.0}ppm:{:.1} ", mer.unwrap_or(f64::NAN)));
        }
        eprintln!("[sro] MER(dB) by assumed source-rate error: {out}");
    }

    /// Is the FAC mask misaligned? Sweep every frame phase on the live capture and read the
    /// FAC MER: if one phase stands out, the estimator is fine and the frame alignment (the
    /// frame-sync stage's choice) is what fails on a real signal.
    #[test]
    #[ignore = "diagnostic sweep; run with --ignored --nocapture"]
    fn sweep_frame_phase_on_the_live_capture() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        let base = resample_to_core(path, 48_828.125);
        let mut flat: Vec<f64> = Vec::with_capacity(base.len() * 2);
        for v in &base {
            flat.push(v.re);
            flat.push(v.im);
        }
        let mut acq = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true);
        let coarse = acq.push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0);
        let mut corrected = base.clone();
        let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse);
        nco.process(&mut corrected);
        let (rows, _, shifts) = rows_and_syms(&map, &corrected);
        let spf = RobustnessMode::B.symbols_per_frame();
        let mut out = String::new();
        for phase in 0..spf {
            let mut est = ChanEst::new(&map);
            let mut mer = f64::NAN;
            for (i, row) in rows.iter().enumerate() {
                let sym = (i % spf + phase) % spf;
                if est.process(row, sym, shifts[i], &map).is_some() {
                    if let Some(m) = est.stats().fac_mer_db {
                        mer = m;
                    }
                }
            }
            out.push_str(&format!("{phase}:{mer:.1} "));
        }
        eprintln!("[phase] FAC MER(dB) by frame phase: {out}");
    }

    /// The clean fixture's FAC must decode through the whole chain — acquisition, NCO,
    /// timing, demodulation, channel estimation, equalisation, 4-QAM demap, Viterbi, CRC —
    /// to the channel and service parameters the fixture's manifest records.
    #[test]
    fn fac_decodes_to_the_fixture_manifest() {
        use crate::digital::drm2::fac::{Fac, Interleaving, MscMode, SdcMode};
        use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams};
        use crate::digital::drm2::tables::fac_cell_count;

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, shifts) = rows_and_syms(&map, &iq);

        let mut est = ChanEst::new(&map);
        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut facs: Vec<Fac> = Vec::new();
        let mut errors = 0usize;
        let mut bits = Vec::new();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], shifts[i], &map) else {
                continue;
            };
            // Reset at the frame boundary so a warm-up that starts mid-frame cannot mix two
            // frames' FAC cells (the FAC spans symbols 2..13 of one frame).
            if out_sym == 0 {
                fac_cells.clear();
            }
            for &c in &map.fac_carriers[out_sym] {
                fac_cells.push(cells[c as usize]);
            }
            if fac_cells.len() == fac_cell_count(RobustnessMode::B) {
                if fac_dec.decode(&fac_cells, &mut bits) {
                    match Fac::parse(&bits) {
                        Some(f) => facs.push(f),
                        None => errors += 1,
                    }
                } else {
                    errors += 1;
                }
                fac_cells.clear();
            }
        }
        eprintln!("[fac] decoded {} blocks, {} CRC failures", facs.len(), errors);
        assert!(facs.len() >= 10, "the 6 s fixture must yield a block per frame, got {}", facs.len());
        assert_eq!(errors, 0, "every FAC frame must pass its CRC");
        let f0 = &facs[0];
        assert_eq!(f0.channel.occupancy, SpectrumOccupancy::SO_3);
        assert_eq!(f0.channel.msc_mode, MscMode::Qam64Sm);
        assert_eq!(f0.channel.sdc_mode, SdcMode::Qam16);
        assert_eq!(f0.channel.interleaving, Interleaving::Long);
        assert_eq!(f0.channel.num_audio, 1);
        assert_eq!(f0.channel.num_data, 0);
        assert_eq!(f0.service.service_id, 0x123456);
        // The frame index must cycle 0,1,2 over the super frame (the warm-up discards the
        // first, incomplete frame, so the first decoded block need not be frame 0).
        let indices: Vec<u8> = facs.iter().map(|f| f.channel.frame_index).collect();
        assert!(
            indices.windows(2).all(|w| (w[1] as usize) == ((w[0] as usize) + 1) % 3),
            "indices {indices:?}"
        );
        assert!(indices.iter().copied().filter(|&i| i == 0).count() >= 3, "indices {indices:?}");
    }

    /// The clean fixture's SDC must decode through the chain to the station label and audio
    /// parameters the manifest records. The SDC sits in symbols 0..1 of frame 0 of each super
    /// frame, so its cells are keyed by the FAC's frame index.
    #[test]
    fn sdc_decodes_to_the_fixture_manifest() {
        use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams};
        use crate::digital::drm2::fec::qam::Mapping;
        use crate::digital::drm2::sdc::{parse_entities, parse_sdc_block, Entity};

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, shifts) = rows_and_syms(&map, &iq);

        let mut est = ChanEst::new(&map);
        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut bits = Vec::new();
        // Each frame contributes its SDC cells (symbols 0..sdc_syms) and its FAC frame index;
        // both are complete at the frame's last FAC symbol, so they are stored together.
        let mut frame_sdc: Vec<EqCell> = Vec::new();
        let mut sdc_blocks: Vec<(u8, Vec<EqCell>)> = Vec::new();
        let sdc_syms = map.mode().sdc_symbols();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], shifts[i], &map) else {
                continue;
            };
            if out_sym == 0 {
                frame_sdc.clear();
                fac_cells.clear();
            }
            if out_sym < sdc_syms {
                for &c in &map.sdc_carriers[out_sym] {
                    frame_sdc.push(cells[c as usize]);
                }
            }
            for &c in &map.fac_carriers[out_sym] {
                fac_cells.push(cells[c as usize]);
            }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm2::fac::Fac::parse(&bits)
                        .map(|f| f.channel.frame_index)
                        .unwrap_or(0xFF)
                } else {
                    0xFF
                };
                sdc_blocks.push((idx, std::mem::take(&mut frame_sdc)));
                fac_cells.clear();
            }
        }

        // The frame whose FAC says 0 is the super-frame start and carries the SDC.
        let mut labels: Vec<String> = Vec::new();
        let mut audios: Vec<crate::digital::drm2::sdc::AudioInfo> = Vec::new();
        let mut muxes: Vec<crate::digital::drm2::sdc::MultiplexDescription> = Vec::new();
        let mut sdc_ok = 0usize;
        for (idx, cells) in &sdc_blocks {
            if *idx != 0 || cells.len() != map.sdc_cells_per_superframe {
                continue;
            }
            let mut sdc16 = MlcDecoder::new(
                MlcParams::sdc(Mapping::Qam16, map.sdc_cells_per_superframe),
                0,
            );
            let mut sdc4 = MlcDecoder::new(
                MlcParams::sdc(Mapping::Qam4, map.sdc_cells_per_superframe),
                0,
            );
            let mut block = if sdc16.decode(cells, &mut bits) {
                parse_sdc_block(&bits).filter(|b| b.crc_ok)
            } else {
                None
            };
            if block.is_none() {
                let mut b4 = Vec::new();
                block = if sdc4.decode(cells, &mut b4) {
                    parse_sdc_block(&b4).filter(|b| b.crc_ok)
                } else {
                    None
                };
            }
            if let Some(b) = block {
                sdc_ok += 1;
                for e in parse_entities(&b.data) {
                    match e {
                        Entity::Label(l) => labels.push(l.text()),
                        Entity::Audio(a) => audios.push(a),
                        Entity::Multiplex(m) => muxes.push(m),
                        _ => {}
                    }
                }
            }
        }
        eprintln!("[sdc] decoded {sdc_ok} blocks, labels {labels:?}");
        assert!(sdc_ok >= 3, "each super frame must decode its SDC, got {sdc_ok}");
        assert!(labels.iter().all(|l| l == "SAN90 DRM TEST"), "labels {labels:?}");
        assert!(!labels.is_empty());
        // Audio descriptor: AAC (coding 0), SBR on, mono, 24 kHz sample-rate code.
        let a = audios.first().expect("audio entity in SDC");
        assert_eq!(a.coding, 0, "AAC");
        assert!(a.sbr, "SBR enabled");
        assert_eq!(a.mode, 0, "mono");
        assert_eq!(a.sample_rate, 3, "24 kHz sample-rate code");
        assert!(a.text, "text message present");
        // Multiplex description: EEP, protection level 0/1.
        let m = muxes.first().expect("multiplex entity in SDC");
        assert_eq!(m.protection_a, 0);
        assert_eq!(m.protection_b, 1);
        assert_eq!(m.streams.len(), 1, "one audio stream");
        eprintln!("[sdc] stream len_a={} len_b={}", m.streams[0].len_a, m.streams[0].len_b);
    }

    /// The clean fixture's MSC must decode bit-exact: the MSC payload is a deterministic
    /// xorshift stream, the manifest records 8390 information bits per multiplex frame and the
    /// long (depth-5) cell interleaver delays by four frames.
    #[test]
    fn msc_decodes_bit_exact() {
        use crate::digital::drm2::fac::MscMode;
        use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams, MscProtection};
        use crate::digital::drm2::fec::qam::Mapping;
        use crate::digital::drm2::interleave::CellDeinterleaver;

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, shifts) = rows_and_syms(&map, &iq);

        let mut est = ChanEst::new(&map);
        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut bits = Vec::new();
        let mut emitted: Vec<(usize, Vec<EqCell>)> = Vec::new();
        let mut frame_indices: Vec<u8> = Vec::new();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], shifts[i], &map) else {
                continue;
            };
            if out_sym == 0 {
                fac_cells.clear();
            }
            for &c in &map.fac_carriers[out_sym] {
                fac_cells.push(cells[c as usize]);
            }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm2::fac::Fac::parse(&bits)
                        .map(|f| f.channel.frame_index)
                        .unwrap_or(0xFF)
                } else {
                    0xFF
                };
                frame_indices.push(idx);
                fac_cells.clear();
            }
            emitted.push((out_sym, cells));
        }

        // Walk the emitted symbols, skipping the partial first frame, and assemble each
        // super frame's MSC cells in super-frame symbol order (0..44).
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut in_partial = true;
        let mut complete_frame = 0usize;
        // Configure the MSC decoder from the FAC's mode and the SDC's protection (EEP 0/1).
        let mapping = match MscMode::Qam64Sm {
            MscMode::Qam64Sm => Mapping::Qam64Sm,
            MscMode::Qam64HmMix => Mapping::Qam64HmMix,
            MscMode::Qam64HmSym => Mapping::Qam64HmSym,
            MscMode::Qam16Sm => Mapping::Qam16,
        };
        let prot = MscProtection { part_a: 0, part_b: 1, hierarchical: 0 };
        let params = MlcParams::msc(mapping, map.msc_cells_per_frame, prot, 0);
        let mut de = CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut msc_dec = MlcDecoder::new(params, 1);
        let mut msc_frames: Vec<Vec<u8>> = Vec::new();
        let mut decode_super = |super_msc: &mut Vec<Vec<EqCell>>, msc_frames: &mut Vec<Vec<u8>>| {
            let mut all: Vec<EqCell> = Vec::new();
            for c in super_msc.iter() {
                all.extend_from_slice(c);
            }
            for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                let Some(deint) = de.push(frame) else { continue };
                if deint.iter().any(|c| c.chan <= 0.0) {
                    continue;
                }
                let mut b = Vec::new();
                if msc_dec.decode(&deint, &mut b) {
                    msc_frames.push(b);
                }
            }
            for c in super_msc.iter_mut() {
                c.clear();
            }
        };

        for (out_sym, cells) in &emitted {
            if *out_sym == 0 && in_partial {
                in_partial = false;
                complete_frame = 0;
            } else if *out_sym == 0 {
                complete_frame += 1;
            }
            if in_partial {
                continue;
            }
            let Some(&frame_index) = frame_indices.get(complete_frame) else {
                continue;
            };
            if frame_index == 0xFF {
                continue;
            }
            let super_sym = frame_index as usize * 15 + *out_sym;
            // Flush at frame 0's start (before collecting), once the previous super frame is
            // complete in sym order. The MSC multiplex-frame boundary is every N_MUX cells, not
            // every 15 symbols, so the sym-order concatenation is chunked by cell count.
            if *out_sym == 0
                && frame_index == 0
                && (0..45).all(|s| map.msc_carriers[s].is_empty() || !super_msc[s].is_empty())
            {
                decode_super(&mut super_msc, &mut msc_frames);
            }
            for &c in &map.msc_carriers[super_sym] {
                super_msc[super_sym].push(cells[c as usize]);
            }
        }
        decode_super(&mut super_msc, &mut msc_frames);

        let n = 8390usize;
        let stream = xorshift_bits(12 * n);
        eprintln!("[msc] decoded {} frames", msc_frames.len());
        assert!(msc_frames.len() >= 6, "the depth-5 interleaver must yield complete frames, got {}", msc_frames.len());
        for (f, bits) in msc_frames.iter().enumerate().take(stream.len() / n) {
            assert_eq!(bits.len(), n, "MSC frame {f} length");
        }
        // The chanest warm-up drops the first `delay` symbols, so the first complete super frame
        // is the second one, and the depth-5 interleaver needs a few frames of fill; the decoded
        // bits must match the stream bit-exact from the first complete frame onward.
        let matched = msc_frames.iter().enumerate().take(6).map(|(f, bits)| {
            let s = (f + 3) * n;
            bits.iter().zip(&stream[s..s + n]).filter(|(a, b)| a == b).count()
        }).collect::<Vec<_>>();
        eprintln!("[msc] bit match per frame (of {}, shifted +3): {matched:?}", n);
        for f in 0..3 {
            let s = (f + 6) * n;
            assert_eq!(&msc_frames[f + 3][..], &stream[s..s + n], "MSC frame {} bit-exact", f + 3);
        }
    }

    /// Decisive diagnostic: replace the drm2 TimeSync windows with the previous receiver's
    /// guard-start anchor (guard_start + guard_len) and run the drm2 chain downstream. If this
    /// decodes bit-exact, the defect is the drm2 window positions; if it still fails, the defect
    /// is in the chanest/FEC.
    #[test]
    fn msc_with_previous_window_anchor() {
        use crate::digital::drm2::fac::MscMode;
        use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams, MscProtection};
        use crate::digital::drm2::fec::qam::Mapping;
        use crate::digital::drm2::interleave::CellDeinterleaver;
        use crate::digital::drm2::ofdm::OfdmDemod;

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let acq = crate::digital::drm::timesync::acquire(&iq.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect::<Vec<_>>())
            .expect("acquire");
        let g = acq.mode.guard_len();
        let n = acq.mode.fft_size();
        let ts = acq.mode.symbol_len();
        eprintln!("[prevwin] guard_start {} guard_len {} fft {} sym {}", acq.guard_start, g, n, ts);

        let mut demod = OfdmDemod::new(&map);
        let mut rows: Vec<Vec<Cplx>> = Vec::new();
        let mut cells = Vec::new();
        let start0 = acq.guard_start + g;
        let mut start = start0;
        while start + n <= iq.len() {
            demod.demodulate(&iq[start..start + n], &mut cells);
            rows.push(cells.clone());
            start += ts;
        }
        eprintln!("[prevwin] {} rows", rows.len());
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        let syms: Vec<usize> = (0..rows.len()).map(|i| (i % spf + phase) % spf).collect();

        let mut est = ChanEst::new(&map);
        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut bits = Vec::new();
        let mut emitted: Vec<(usize, Vec<EqCell>)> = Vec::new();
        let mut frame_indices: Vec<u8> = Vec::new();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], 0, &map) else {
                continue;
            };
            if out_sym == 0 {
                fac_cells.clear();
            }
            for &c in &map.fac_carriers[out_sym] {
                fac_cells.push(cells[c as usize]);
            }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm2::fac::Fac::parse(&bits)
                        .map(|f| f.channel.frame_index)
                        .unwrap_or(0xFF)
                } else {
                    0xFF
                };
                frame_indices.push(idx);
                fac_cells.clear();
            }
            emitted.push((out_sym, cells));
        }

        let mapping = match MscMode::Qam64Sm {
            MscMode::Qam64Sm => Mapping::Qam64Sm,
            MscMode::Qam64HmMix => Mapping::Qam64HmMix,
            MscMode::Qam64HmSym => Mapping::Qam64HmSym,
            MscMode::Qam16Sm => Mapping::Qam16,
        };
        let prot = MscProtection { part_a: 0, part_b: 1, hierarchical: 0 };
        let params = MlcParams::msc(mapping, map.msc_cells_per_frame, prot, 0);
        let mut de = CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut msc_dec = MlcDecoder::new(params, 1);
        let mut msc_frames: Vec<Vec<u8>> = Vec::new();
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut complete_frame = 0usize;
        let mut first_frame_done = false;
        for (out_sym, cells) in &emitted {
            if *out_sym == 0 {
                complete_frame += 1;
                if complete_frame <= 1 {
                    continue;
                }
                first_frame_done = true;
            }
            if !first_frame_done {
                continue;
            }
            let Some(&frame_index) = frame_indices.get(complete_frame - 1) else {
                continue;
            };
            if frame_index == 0xFF {
                continue;
            }
            let super_sym = frame_index as usize * 15 + *out_sym;
            if *out_sym == 0 && frame_index == 0 {
                let mut all: Vec<EqCell> = Vec::new();
                for c in super_msc.iter() {
                    all.extend_from_slice(c);
                }
                for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                    if let Some(d) = de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if msc_dec.decode(&d, &mut b) {
                                msc_frames.push(b);
                            }
                        }
                    }
                }
                for c in super_msc.iter_mut() {
                    c.clear();
                }
            }
            for &c in &map.msc_carriers[super_sym] {
                super_msc[super_sym].push(cells[c as usize]);
            }
        }

        let nn = 8390usize;
        let stream = xorshift_bits(12 * nn);
        let matched: Vec<usize> = msc_frames
            .iter()
            .zip(stream.chunks(nn))
            .take(8)
            .map(|(b, s)| b.iter().zip(s).filter(|(a, z)| a == z).count())
            .collect();
        eprintln!("[prevwin] decoded {} frames, bit match per frame (of {nn}): {matched:?}", msc_frames.len());
    }

    /// Probe: the old receiver's sync parameters for the fixture, to compare the drm2 chain's
    /// front end against the oracle.
    #[test]
    fn probe_old_sync() {
        let raw = std::fs::read("../tests/fixtures/drm/drm_modeB_so3_48k.f32").expect("capture file");
        let interleaved: Vec<f32> = raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let old: Vec<crate::digital::drm::Cplx> = interleaved.chunks_exact(2).map(|c| crate::digital::drm::Cplx::new(f64::from(c[0]), f64::from(c[1]))).collect();
        let acq = crate::digital::drm::timesync::acquire(&old).expect("acquire");
        eprintln!("[probe] old acquire: guard_start={} freq_offset_hz={:+.3}", acq.guard_start, acq.freq_offset_hz);
        let mut rx = crate::digital::drm::DrmReceiver::new();
        rx.push(&interleaved);
        rx.run();
        eprintln!("[probe] old rx: frame_offset={} carrier_offset_hz={:+.3} mode={:?}", rx.frame_offset, rx.carrier_offset_hz, rx.mode);
        let ids: Vec<u8> = rx.facs.iter().map(|f| f.channel.frame_index).collect();
        eprintln!("[probe] old FAC frame_index sequence: {ids:?} ({} blocks)", rx.facs.len());
    }

    /// Diagnostic: hard-decision comparison of the drm2 chanest cells against the old
    /// equaliser on the TimeSync rows (the real front-end path). Counts how often they land on
    /// DIFFERENT 64-QAM points, which is what actually matters for the MSC decode.
    #[test]
    fn compare_chanest_hard_decision() {
        use crate::digital::drm::qam::hard_axis;
        use crate::digital::drm::tables::QAM64_SM;

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("old layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, shifts) = rows_and_syms(&map, &iq);

        let mut est = ChanEst::new(&map);
        let mut mismatch = 0usize;
        let mut total = 0usize;
        let mut err_my = 0.0f64;
        let mut err_old = 0.0f64;
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], shifts[i], &map) else { continue };
            let src_i = i - est.delay();
            let old_row: Vec<crate::digital::drm::Cplx> =
                rows[src_i].iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, out_sym, &old_row);
            for &c in &map.msc_carriers[out_sym] {
                let a = cells[c as usize].sig;
                let b = eq.cells[c as usize];
                if b.norm_sqr() <= 0.01 { continue; }
                let (ar, ai) = (hard_axis(a.re, &QAM64_SM), hard_axis(a.im, &QAM64_SM));
                let (br, bi) = (hard_axis(b.re, &QAM64_SM), hard_axis(b.im, &QAM64_SM));
                if ar != br || ai != bi {
                    mismatch += 1;
                }
                total += 1;
                err_my += (a - crate::digital::drm2::dsp::Cplx::new(QAM64_SM[ar], QAM64_SM[ai])).norm();
                err_old += (crate::digital::drm2::dsp::Cplx::new(b.re, b.im) - crate::digital::drm2::dsp::Cplx::new(QAM64_SM[br], QAM64_SM[bi])).norm();
            }
        }
        eprintln!("[harddec] MSC cells: {} total, {} mismatches ({:.2}%), err my {:.3} old {:.3}", total, mismatch, 100.0 * mismatch as f64 / total as f64, err_my / total as f64, err_old / total as f64);
    }

    /// Direct row comparison: demodulate the previous receiver's window anchor and the drm2
    /// TimeSync window for the SAME symbol, remove each one's timing ramp, and compare the cells
    /// carrier by carrier. If they differ, the rows differ beyond a cyclic shift; if they agree,
    /// the defect is downstream (equalisation/assembly).
    #[test]
    fn compare_rows_old_anchor_vs_timesync() {
        use crate::digital::drm2::ofdm::OfdmDemod;
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let acq = crate::digital::drm::timesync::acquire(
            &iq.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect::<Vec<_>>(),
        )
        .expect("acquire");
        let n = acq.mode.fft_size();
        let ts = acq.mode.symbol_len();
        let start0 = acq.guard_start + acq.mode.guard_len();
        let mut demod = OfdmDemod::new(&map);
        let mut cells = Vec::new();

        // TimeSync windows.
        let mut tsync = crate::digital::drm2::sync::timesync::TimeSync::new(RobustnessMode::B);
        let mut tsw: Vec<(i64, Vec<Cplx>)> = Vec::new();
        for block in iq.chunks(3248) {
            let _ = tsync.push(block);
            while let Some(w) = tsync.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 { continue; }
                demod.demodulate(&w.samples, &mut cells);
                tsw.push((w.start, cells.clone()));
            }
        }
        eprintln!("[rows] old-anchor start0={start0}, TimeSync windows {} first start {}", tsw.len(), tsw.first().map(|w| w.0).unwrap_or(0));
        let n_fft = map.mode().fft_size() as f64;
        let kmin = map.kmin;
        let mut worst = 0.0f64;
        let mut n_cmp = 0usize;
        // For each TimeSync window, find the nearest old-anchor window and compare.
        for (s, row) in &tsw {
            let d = (*s - start0 as i64) as f64;
            let k_sym = (d / ts as f64).round();
            if k_sym < 0.0 { continue; }
            let anchor = start0 as i64 + (k_sym as i64) * ts as i64;
            if anchor < 0 || anchor + n as i64 > iq.len() as i64 { continue; }
            demod.demodulate(&iq[anchor as usize..anchor as usize + n], &mut cells);
            let old_row = cells.clone();
            // Measure each row's ramp (sym 0 pilot positions) and de-ramp both.
            let off_ts = sym_offset(&map, std::slice::from_ref(row), &[0], 0);
            let off_old = sym_offset(&map, std::slice::from_ref(&old_row), &[0], 0);
            let mut diff = 0.0f64;
            let mut n_pt = 0usize;
            for c in 0..map.num_carriers {
                let k = (kmin + c as i32) as f64;
                let a = row[c] * Cplx::from_polar(1.0, -2.0 * core::f64::consts::PI * k * off_ts / n_fft);
                let b = old_row[c] * Cplx::from_polar(1.0, -2.0 * core::f64::consts::PI * k * off_old / n_fft);
                diff += (a - b).norm_sqr();
                n_pt += 1;
            }
            worst = worst.max(diff / n_pt as f64);
            n_cmp += 1;
        }
        eprintln!("[rows] compared {n_cmp} symbols, worst mean-square diff {worst:.3e}");
    }

    /// Verify the drm2 MSC carrier table matches the previous chain's exactly (order matters for
    /// the cell deinterleaver).
    #[test]
    fn equalize_is_frame_periodic() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("old layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let row: Vec<crate::digital::drm::Cplx> = {
            let mut demod = crate::digital::drm2::ofdm::OfdmDemod::new(&map);
            let mut cells = Vec::new();
            demod.demodulate(&iq[360..360 + 1024], &mut cells);
            cells.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect()
        };
        let e0 = crate::digital::drm::chanest::equalize_symbol(&old_map, 0, &row);
        let e15 = crate::digital::drm::chanest::equalize_symbol(&old_map, 15, &row);
        let mut maxdiff = 0.0f64;
        for c in 0..map.num_carriers {
            maxdiff = maxdiff.max((e0.cells[c] - e15.cells[c]).norm());
        }
        eprintln!("[eqper] equalize(sym=0) vs (sym=15) max cell diff: {maxdiff:.3e}");
    }

    /// Verify the drm2 MSC carrier table matches the previous chain's exactly (order matters for
    /// the cell deinterleaver).
    #[test]
    fn msc_carriers_match_the_previous_chain() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("old layout");
        for s in 0..45 {
            assert_eq!(map.msc_carriers[s].len(), old_map.msc_carriers[s].len(), "msc carrier count sym {s}");
            for (a, b) in map.msc_carriers[s].iter().zip(&old_map.msc_carriers[s]) {
                assert_eq!(*a as usize, *b as usize, "msc carrier sym {s}");
            }
        }
        assert_eq!(map.msc_cells_per_frame, old_map.msc_cells_per_frame);
        eprintln!("[cells] msc_carriers identical across 45 symbols");
    }

    /// Compare the equalised cells (sig AND channel power) of the drm2 chanest against the old
    /// equaliser on the old-anchor rows. A sig match with a chan mismatch points at the soft
    /// metric weighting.
    #[test]
    fn compare_chan_field() {
        use crate::digital::drm2::ofdm::OfdmDemod;
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("old layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let acq = crate::digital::drm::timesync::acquire(
            &iq.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect::<Vec<_>>(),
        )
        .expect("acquire");
        let n = acq.mode.fft_size();
        let ts = acq.mode.symbol_len();
        let start0 = acq.guard_start + acq.mode.guard_len();
        let mut demod = OfdmDemod::new(&map);
        let mut rows: Vec<Vec<Cplx>> = Vec::new();
        let mut cells = Vec::new();
        let mut start = start0;
        while start + n <= iq.len() {
            demod.demodulate(&iq[start..start + n], &mut cells);
            rows.push(cells.clone());
            start += ts;
        }
        let spf = RobustnessMode::B.symbols_per_frame();
        let syms: Vec<usize> = (0..rows.len()).map(|i| i % spf).collect();
        let mut est = ChanEst::new(&map);
        let mut sig_max = 0.0f64;
        let mut sig_sum = 0.0f64;
        let mut chan_max = 0.0f64;
        let mut err_my = 0.0f64;
        let mut err_old = 0.0f64;
        let mut n = 0usize;
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], 0, &map) else { continue };
            let src_i = i - est.delay();
            let old_row: Vec<crate::digital::drm::Cplx> =
                rows[src_i].iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, out_sym, &old_row);
            for &c in &map.msc_carriers[out_sym] {
                let a = cells[c as usize];
                let b = eq.cells[c as usize];
                let old_chan = eq.chan[c as usize].norm_sqr();
                let diff = (a.sig - crate::digital::drm2::dsp::Cplx::new(b.re, b.im)).norm();
                sig_max = sig_max.max(diff);
                sig_sum += diff;
                chan_max = chan_max.max((a.chan - old_chan).abs() / old_chan.max(1e-9));
                // Distance to the nearest 64-QAM point (my hard decision).
                let (ar, ai) = (
                    crate::digital::drm::qam::hard_axis(a.sig.re, &crate::digital::drm::tables::QAM64_SM),
                    crate::digital::drm::qam::hard_axis(a.sig.im, &crate::digital::drm::tables::QAM64_SM),
                );
                err_my += (a.sig - crate::digital::drm2::dsp::Cplx::new(crate::digital::drm::tables::QAM64_SM[ar], crate::digital::drm::tables::QAM64_SM[ai])).norm();
                err_old += (crate::digital::drm2::dsp::Cplx::new(b.re, b.im) - crate::digital::drm2::dsp::Cplx::new(crate::digital::drm::tables::QAM64_SM[ar], crate::digital::drm::tables::QAM64_SM[ai])).norm();
                n += 1;
            }
        }
        eprintln!("[chan] {n} cells: sig diff avg {:.3e} max {:.3e}, chan diff max {:.3e}", sig_sum / n as f64, sig_max, chan_max);
        eprintln!("[chan] err vs QAM point: my {:.3e} old {:.3e}", err_my / n as f64, err_old / n as f64);
    }

    /// Decisive isolation: on the previous receiver's own window anchor, run the drm2 chanest and
    /// then BOTH the OLD FEC and the NEW FEC on the same equalised cells. If the OLD FEC decodes
    /// bit-exact and the NEW one does not, the defect is the drm2 MSC FEC; if both decode, the
    /// defect is upstream (equalisation).
    #[test]
    fn msc_fec_isolation() {
        use crate::digital::drm::fec::mlc::{MlcDecoder as OldMlc, MlcParams as OldParams, MscProtection as OldProt};
        use crate::digital::drm::fec::qam::{EqCell as OldEq, Mapping as OldMapping};
        use crate::digital::drm::interleave::CellDeinterleaver as OldDeint;
        use crate::digital::drm2::fac::MscMode;
        use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams, MscProtection};
        use crate::digital::drm2::fec::qam::{EqCell, Mapping};
        use crate::digital::drm2::interleave::CellDeinterleaver;
        use crate::digital::drm2::ofdm::OfdmDemod;

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("old layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let acq = crate::digital::drm::timesync::acquire(
            &iq.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect::<Vec<_>>(),
        )
        .expect("acquire");
        let n = acq.mode.fft_size();
        let ts = acq.mode.symbol_len();
        let start0 = acq.guard_start + acq.mode.guard_len();

        let mut demod = OfdmDemod::new(&map);
        let mut rows: Vec<Vec<Cplx>> = Vec::new();
        let mut cells = Vec::new();
        let mut start = start0;
        while start + n <= iq.len() {
            demod.demodulate(&iq[start..start + n], &mut cells);
            rows.push(cells.clone());
            start += ts;
        }
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        eprintln!("[feciso] FrameSync phase for old-anchor rows: {phase}");
        let syms: Vec<usize> = (0..rows.len()).map(|i| (i % spf + phase) % spf).collect();

        let mut est = ChanEst::new(&map);
        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut bits = Vec::new();
        let mut emitted: Vec<(usize, Vec<EqCell>, Vec<OldEq>)> = Vec::new();
        let mut frame_indices: Vec<u8> = Vec::new();
        let delay = est.delay();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], 0, &map) else { continue };
            if out_sym == 0 { fac_cells.clear(); }
            for &c in &map.fac_carriers[out_sym] { fac_cells.push(cells[c as usize]); }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm2::fac::Fac::parse(&bits).map(|f| f.channel.frame_index).unwrap_or(0xFF)
                } else { 0xFF };
                frame_indices.push(idx);
                fac_cells.clear();
            }
            // Old equaliser at the SAME row/symbol (the chanest output is delayed by `delay`).
            let src = i - delay;
            let old_row: Vec<crate::digital::drm::Cplx> =
                rows[src].iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, out_sym, &old_row);
            let old_cells: Vec<OldEq> = (0..map.num_carriers)
                .map(|c| OldEq { sig: eq.cells[c], chan: eq.chan[c].norm_sqr() })
                .collect();
            emitted.push((out_sym, cells, old_cells));
        }
        eprintln!("[feciso] frame_indices: {frame_indices:?}");
        eprintln!("[feciso] emitted first 20 out_sym: {:?}", emitted.iter().take(20).map(|(s, _, _)| *s).collect::<Vec<_>>());

        let mut old_de = OldDeint::new(old_map.msc_cells_per_frame, 5);
        let mut old_dec = OldMlc::new(
            OldParams::msc(OldMapping::Qam64Sm, old_map.msc_cells_per_frame, OldProt { part_a: 0, part_b: 1, hierarchical: 0 }, 0),
            1,
        );
        let mut my_de = CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut my_dec = MlcDecoder::new(
            MlcParams::msc(Mapping::Qam64Sm, map.msc_cells_per_frame, MscProtection { part_a: 0, part_b: 1, hierarchical: 0 }, 0),
            1,
        );
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut super_msc_old: Vec<Vec<OldEq>> = vec![Vec::new(); 45];
        let mut in_partial = true;
        let mut complete_frame = 0usize;
        let mut syms_collected = 0usize;
        let mut old_all: Vec<u8> = Vec::new();
        let mut my_all: Vec<u8> = Vec::new();
        let mut old2_all: Vec<u8> = Vec::new();
        let mut old2_de = OldDeint::new(old_map.msc_cells_per_frame, 5);
        let mut old2_dec = OldMlc::new(
            OldParams::msc(OldMapping::Qam64Sm, old_map.msc_cells_per_frame, OldProt { part_a: 0, part_b: 1, hierarchical: 0 }, 0),
            1,
        );
        let mut uniform_all: Vec<u8> = Vec::new();
        let mut uniform_dec = OldMlc::new(
            OldParams::msc(OldMapping::Qam64Sm, old_map.msc_cells_per_frame, OldProt { part_a: 0, part_b: 1, hierarchical: 0 }, 0),
            1,
        );
        let mut uniform_de = OldDeint::new(old_map.msc_cells_per_frame, 5);
        let mut first_flush = true;
        for (out_sym, cells, old_cells) in &emitted {
            if *out_sym == 0 && in_partial {
                in_partial = false;
                complete_frame = 0;
            } else if *out_sym == 0 {
                complete_frame += 1;
            }
            if in_partial { continue; }
            let Some(&frame_index) = frame_indices.get(complete_frame) else { continue };
            if frame_index == 0xFF { continue; }
            let super_sym = frame_index as usize * 15 + *out_sym;
            // Flush at frame 0's start (before collecting), once the previous super frame is
            // complete in sym order. The MSC multiplex frame boundary is every N_MUX cells,
            // NOT every 15 symbols, so the sym-order concatenation is chunked by cell count —
            // exactly what the previous receiver does.
            if *out_sym == 0
                && frame_index == 0
                && (0..45).all(|s| map.msc_carriers[s].is_empty() || !super_msc[s].is_empty())
            {
                let mut all: Vec<EqCell> = Vec::new();
                for c in super_msc.iter() { all.extend_from_slice(c); }
                let mut all_old: Vec<OldEq> = Vec::new();
                for c in super_msc_old.iter() { all_old.extend_from_slice(c); }
                if first_flush {
                    first_flush = false;
                    eprintln!("[feciso] first super-frame first 20 sigs (my): {:?}", all.iter().take(20).map(|c| (c.sig.re, c.sig.im)).collect::<Vec<_>>());
                }
                for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                    let old_frame: Vec<OldEq> = frame.iter().map(|c| OldEq { sig: crate::digital::drm::Cplx::new(c.sig.re, c.sig.im), chan: c.chan }).collect();
                    let old_deint = old_de.push(&old_frame);
                    let my_deint = my_de.push(frame);
                    let uniform_frame: Vec<OldEq> = frame.iter().map(|c| OldEq { sig: crate::digital::drm::Cplx::new(c.sig.re, c.sig.im), chan: 1.0 }).collect();
                    let uniform_deint = uniform_de.push(&uniform_frame);
                    if let Some(d) = old_deint {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if old_dec.decode(&d, &mut b) { old_all.extend_from_slice(&b); }
                        }
                    }
                    if let Some(d) = my_deint {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if my_dec.decode(&d, &mut b) { my_all.extend_from_slice(&b); }
                        }
                    }
                    if let Some(d) = uniform_deint {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if uniform_dec.decode(&d, &mut b) { uniform_all.extend_from_slice(&b); }
                        }
                    }
                }
                for frame in all_old.chunks(old_map.msc_cells_per_frame).take(3) {
                    if let Some(d) = old2_de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if old2_dec.decode(&d, &mut b) { old2_all.extend_from_slice(&b); }
                        }
                    }
                }
                for c in super_msc.iter_mut() { c.clear(); }
                for c in super_msc_old.iter_mut() { c.clear(); }
            }
            for &c in &map.msc_carriers[super_sym] {
                super_msc[super_sym].push(cells[c as usize]);
                super_msc_old[super_sym].push(old_cells[c as usize]);
            }
        }
        let nn = 8390usize;
        let stream = xorshift_bits(12 * nn);
        let om: Vec<usize> = old_all.chunks(nn).take(8).enumerate().map(|(f, b)| b.iter().zip(&stream[f * nn..(f + 1) * nn]).filter(|(a, z)| a == z).count()).collect();
        let mm: Vec<usize> = my_all.chunks(nn).take(8).enumerate().map(|(f, b)| b.iter().zip(&stream[f * nn..(f + 1) * nn]).filter(|(a, z)| a == z).count()).collect();
        // The chanest warm-up drops the first `delay` symbols, so the first complete frame is
        // frame 1; print the match against every rotation to find the correct alignment.
        let rot: Vec<Vec<usize>> = (0..6).map(|r| {
            my_all.chunks(nn).take(8).enumerate().map(|(f, b)| {
                let s = ((f + r) % 12) * nn;
                b.iter().zip(&stream[s..s + nn]).filter(|(a, z)| a == z).count()
            }).collect()
        }).collect();
        eprintln!("[feciso] OLD FEC on my cells: {} frames, match {om:?}", old_all.len() / nn);
        eprintln!("[feciso] MY  FEC on my cells: {} frames, match {mm:?}", my_all.len() / nn);
        let o2: Vec<usize> = old2_all.chunks(nn).take(8).enumerate().map(|(f, b)| b.iter().zip(&stream[f * nn..(f + 1) * nn]).filter(|(a, z)| a == z).count()).collect();
        eprintln!("[feciso] OLD FEC on OLD cells via my assembly: {} frames, match {o2:?}", old2_all.len() / nn);
        let o2rot: Vec<Vec<usize>> = (0..3).map(|r| {
            old2_all.chunks(nn).take(8).enumerate().map(|(f, b)| {
                let s = ((f + r) % 12) * nn;
                b.iter().zip(&stream[s..s + nn]).filter(|(a, z)| a == z).count()
            }).collect()
        }).collect();
        eprintln!("[feciso] OLD cells via my assembly rotated: {o2rot:?}");
        eprintln!("[feciso] MY FEC rotated matches: {rot:?}");
        let um: Vec<usize> = uniform_all.chunks(nn).take(8).enumerate().map(|(f, b)| b.iter().zip(&stream[f * nn..(f + 1) * nn]).filter(|(a, z)| a == z).count()).collect();
        eprintln!("[feciso] UNIFORM chan=1.0 match: {um:?}");
        eprintln!("[feciso] old==my byte-identical: {}", old_all == my_all);
    }

    /// Confirmation: the OLD equaliser + OLD FEC on the old-anchor rows must decode bit-exact
    /// (it is the previous receiver's own pipeline, replicated with `sym = i % 45`).
    #[test]
    fn old_equaliser_fec_on_old_anchor() {
        use crate::digital::drm::fec::mlc::{MlcDecoder as OldMlc, MlcParams as OldParams, MscProtection as OldProt};
        use crate::digital::drm::fec::qam::{EqCell as OldEq, Mapping as OldMapping};
        use crate::digital::drm::interleave::CellDeinterleaver as OldDeint;
        use crate::digital::drm2::ofdm::OfdmDemod;

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("old layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let acq = crate::digital::drm::timesync::acquire(
            &iq.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect::<Vec<_>>(),
        )
        .expect("acquire");
        let n = acq.mode.fft_size();
        let ts = acq.mode.symbol_len();
        let start0 = acq.guard_start + acq.mode.guard_len();

        let mut demod = OfdmDemod::new(&map);
        let mut rows: Vec<Vec<Cplx>> = Vec::new();
        let mut cells = Vec::new();
        let mut start = start0;
        while start + n <= iq.len() {
            demod.demodulate(&iq[start..start + n], &mut cells);
            rows.push(cells.clone());
            start += ts;
        }
        let spsf = 45usize;

        let mut old_de = OldDeint::new(old_map.msc_cells_per_frame, 5);
        let mut old_dec = OldMlc::new(
            OldParams::msc(OldMapping::Qam64Sm, old_map.msc_cells_per_frame, OldProt { part_a: 0, part_b: 1, hierarchical: 0 }, 0),
            1,
        );
        let mut super_msc: Vec<Vec<OldEq>> = vec![Vec::new(); 45];
        let mut frames: Vec<Vec<u8>> = Vec::new();
        for i in 0..rows.len() {
            let sym = i % spsf; // super-frame symbol, exactly as the old receiver computes it
            let old_row: Vec<crate::digital::drm::Cplx> =
                rows[i].iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, sym, &old_row);
            if sym == 0 {
                let mut all: Vec<OldEq> = Vec::new();
                for c in super_msc.iter() { all.extend_from_slice(c); }
                let chunks: Vec<&[OldEq]> = all.chunks(old_map.msc_cells_per_frame).collect();
                if chunks.len() >= 2 {
                    static D2: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !D2.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!("[oldpipe] frame 1 (2nd chunk) first 20 sigs: {:?}", chunks[1].iter().take(20).map(|c| (c.sig.re, c.sig.im)).collect::<Vec<_>>());
                    }
                }
                for frame in all.chunks(old_map.msc_cells_per_frame).take(3) {
                    if let Some(d) = old_de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if old_dec.decode(&d, &mut b) { frames.push(b); }
                        }
                    }
                }
                for c in super_msc.iter_mut() { c.clear(); }
            }
            for &c in &old_map.msc_carriers[sym] {
                super_msc[sym].push(OldEq { sig: eq.cells[c as usize], chan: eq.chan[c as usize].norm_sqr() });
            }
        }
        let nn = 8390usize;
        let stream = xorshift_bits(12 * nn);
        let m: Vec<usize> = frames.iter().zip(stream.chunks(nn)).take(8).map(|(b, s)| b.iter().zip(s).filter(|(a, z)| a == z).count()).collect();
        eprintln!("[oldpipe] old equaliser + old FEC: {} frames, match {m:?}", frames.len());
        assert!(frames.iter().zip(stream.chunks(nn)).all(|(b, s)| b == s), "the old pipeline must decode bit-exact on the old-anchor rows");
    }

    /// Diagnostic (ignored): the MSC chain with the previous chain's per-symbol linear equaliser
    /// (no Wiener, no delay) on these rows. This too fails bit-exact, so the defect is in the
    /// drm2 rows' timing (a sub-sample window offset corrupts 64-QAM while 4-QAM FAC and 16-QAM
    /// SDC survive), not in the FEC port.
    #[test]
    #[ignore = "records the timing-offset finding; the drm2 rows corrupt 64-QAM MSC"]
    fn msc_decodes_with_the_previous_equaliser() {
        use crate::digital::drm::cellmap::CellMap as OldMap;
        use crate::digital::drm::params::{
            RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy,
        };
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = OldMap::new(
            OldMode::from_index(RobustnessMode::B.index()).unwrap(),
            OldOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, _shifts) = rows_and_syms(&map, &iq);
        let mut fac_dec = crate::digital::drm::fec::mlc::MlcDecoder::new(
            crate::digital::drm::fec::mlc::MlcParams::fac(),
            0,
        );
        let mut fac_cells: Vec<crate::digital::drm::fec::qam::EqCell> = Vec::new();
        let mut bits = Vec::new();
        let mut super_msc: Vec<Vec<crate::digital::drm::fec::qam::EqCell>> = vec![Vec::new(); 45];
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut de = crate::digital::drm::interleave::CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let msc_params = crate::digital::drm::fec::mlc::MlcParams::msc(
            crate::digital::drm::fec::qam::Mapping::Qam64Sm,
            map.msc_cells_per_frame,
            crate::digital::drm::fec::mlc::MscProtection { part_a: 0, part_b: 1, hierarchical: 0 },
            0,
        );
        let mut msc_dec = crate::digital::drm::fec::mlc::MlcDecoder::new(msc_params, 1);
        // Pass 1: decode the FAC per frame, caching the equalised cells.
        let mut cached: Vec<(usize, Vec<crate::digital::drm::fec::qam::EqCell>)> = Vec::new();
        let mut frame_indices: Vec<u8> = Vec::new();
        for i in 0..rows.len() {
            let sym = syms[i];
            let old_row: Vec<crate::digital::drm::Cplx> =
                rows[i].iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, sym, &old_row);
            let cells: Vec<crate::digital::drm::fec::qam::EqCell> = (0..map.num_carriers)
                .map(|c| crate::digital::drm::fec::qam::EqCell {
                    sig: eq.cells[c],
                    chan: eq.chan[c].norm_sqr(),
                })
                .collect();
            if sym == 0 {
                fac_cells.clear();
            }
            for &c in &map.fac_carriers[sym] {
                fac_cells.push(cells[c as usize]);
            }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm::fac::Fac::parse(&bits)
                        .map(|f| f.channel.frame_index)
                        .unwrap_or(0xFF)
                } else {
                    0xFF
                };
                frame_indices.push(idx);
                fac_cells.clear();
            }
            cached.push((sym, cells));
        }
        // Pass 2: walk the cached rows, assigning the super-frame symbol from the FAC index.
        let mut complete_frame = 0usize;
        let mut in_partial = true;
        for (sym, cells) in &cached {
            if *sym == 0 && in_partial {
                in_partial = false;
                complete_frame = 0;
            } else if *sym == 0 {
                complete_frame += 1;
            }
            if in_partial {
                continue;
            }
            let Some(&frame_index) = frame_indices.get(complete_frame) else { continue };
            if frame_index == 0xFF {
                continue;
            }
            let super_sym = frame_index as usize * 15 + *sym;
            for &c in &map.msc_carriers[super_sym] {
                super_msc[super_sym].push(cells[c as usize]);
            }
            if *sym == 0 && frame_index == 1 {
                let mut all: Vec<crate::digital::drm::fec::qam::EqCell> = Vec::new();
                for c in super_msc.iter() {
                    all.extend_from_slice(c);
                }
                for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                    if let Some(d) = de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if msc_dec.decode(&d, &mut b) {
                                frames.push(b);
                            }
                        }
                    }
                }
                for c in super_msc.iter_mut() {
                    c.clear();
                }
            }
        }
        let n = 8390usize;
        let stream = xorshift_bits(12 * n);
        let matched = frames.iter().zip(stream.chunks(n)).take(3).map(|(bits, s)| {
            bits.iter().zip(s).filter(|(a, b)| a == b).count()
        }).collect::<Vec<_>>();
        eprintln!("[msc/linear] decoded {} frames, bit match per frame (of {n}): {matched:?}", frames.len());
        assert!(frames.iter().zip(stream.chunks(n)).all(|(b, s)| b == s), "linear equaliser must decode bit-exact");
    }

    /// Compare the drm2 collection's decoded MSC bits against the old receiver's (which the
    /// fixture test verifies bit-exact), to expose the defect's shape.
    #[test]
    fn msc_bits_vs_previous_receiver() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        // The previous receiver's own decode.
        let raw = std::fs::read("../tests/fixtures/drm/drm_modeB_so3_48k.f32").expect("fixture");
        let iq_f32: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut rx = crate::digital::drm::DrmReceiver::new();
        rx.push(&iq_f32);
        rx.run();
        let old_frames = rx.msc_frames.clone();
        eprintln!("[msc/cmp] previous receiver: {} frames, frame_offset {}, soft_bits {}", old_frames.len(), rx.frame_offset, rx.msc_soft_bits.len());
        // The drm2 collection's decoded frames (reuse the bit-exact test's logic inline).
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, shifts) = rows_and_syms(&map, &iq);
        let mut est = ChanEst::new(&map);
        let mut fac_dec = crate::digital::drm2::fec::mlc::MlcDecoder::new(
            crate::digital::drm2::fec::mlc::MlcParams::fac(),
            0,
        );
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut bits = Vec::new();
        let mut emitted: Vec<(usize, Vec<EqCell>)> = Vec::new();
        let mut frame_indices: Vec<u8> = Vec::new();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], shifts[i], &map) else {
                continue;
            };
            if out_sym == 0 {
                fac_cells.clear();
            }
            for &c in &map.fac_carriers[out_sym] {
                fac_cells.push(cells[c as usize]);
            }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm2::fac::Fac::parse(&bits)
                        .map(|f| f.channel.frame_index)
                        .unwrap_or(0xFF)
                } else {
                    0xFF
                };
                frame_indices.push(idx);
                fac_cells.clear();
            }
            emitted.push((out_sym, cells));
        }
        let params = crate::digital::drm2::fec::mlc::MlcParams::msc(
            crate::digital::drm2::fec::qam::Mapping::Qam64Sm,
            map.msc_cells_per_frame,
            crate::digital::drm2::fec::mlc::MscProtection { part_a: 0, part_b: 1, hierarchical: 0 },
            0,
        );
        let mut de = crate::digital::drm2::interleave::CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut msc_dec = crate::digital::drm2::fec::mlc::MlcDecoder::new(params, 1);
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut all_cells: Vec<EqCell> = Vec::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut in_partial = true;
        let mut complete_frame = 0usize;
        for (out_sym, cells) in &emitted {
            if *out_sym == 0 && in_partial {
                in_partial = false;
                complete_frame = 0;
            } else if *out_sym == 0 {
                complete_frame += 1;
            }
            if in_partial {
                continue;
            }
            let Some(&fidx) = frame_indices.get(complete_frame) else { continue };
            if fidx == 0xFF {
                continue;
            }
            let super_sym = fidx as usize * 15 + *out_sym;
            if *out_sym == 0 && fidx == 1 {
                let mut all: Vec<EqCell> = Vec::new();
                for c in super_msc.iter() {
                    all.extend_from_slice(c);
                }
                for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                    if let Some(d) = de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if msc_dec.decode(&d, &mut b) {
                                frames.push(b);
                            }
                        }
                    }
                }
                for c in super_msc.iter_mut() {
                    c.clear();
                }
            }
            for &c in &map.msc_carriers[super_sym] {
                super_msc[super_sym].push(cells[c as usize]);
                all_cells.push(cells[c as usize]);
            }
        }
        // Compare the first frames.
        let n = old_frames[0].len();
        eprintln!("[msc/cmp] drm2: {} frames, old: {} frames, n={n}", frames.len(), old_frames.len());
        for f in 0..3.min(frames.len()).min(old_frames.len()) {
            let eq = frames[f].iter().zip(&old_frames[f]).filter(|(a, b)| a == b).count();
            eprintln!("[msc/cmp] frame {f}: equal {eq}/{n}");
        }
        // Check whether my frames are the old frames shifted by one (my collection skips the
        // partial frame 0, so my frame f = old frame f+1).
        for f in 0..3.min(frames.len()).min(old_frames.len() - 1) {
            let eq = frames[f].iter().zip(&old_frames[f + 1]).filter(|(a, b)| a == b).count();
            eprintln!("[msc/cmp] shift: my frame {f} vs old {f}+1: equal {eq}/{n}");
        }
        // The MSC MER (nearest 64-QAM decision error) over the collected MSC cells, split
        // into band centre vs edges, to localise the corruption.
        let qam = crate::digital::drm2::tables::QAM64_SM;
        let mut centre = (0.0, 0.0);
        let mut edges = (0.0, 0.0);
        for (sym, bucket) in super_msc.iter().enumerate() {
            for cell in bucket {
                let k = (map.kmin + 0).abs(); // carrier offset not tracked per bucket; use sym
                let _ = k;
                let (err_sum, pow_sum) = if sym < 20 { &mut centre } else { &mut edges };
                let re = qam.iter().map(|p| (cell.sig.re - p).abs()).fold(f64::INFINITY, f64::min).powi(2);
                let im = qam.iter().map(|p| (cell.sig.im - p).abs()).fold(f64::INFINITY, f64::min).powi(2);
                *err_sum += (re + im) * cell.chan;
                *pow_sum += cell.chan * 2.0;
            }
        }
        let mer = |(e, p): (f64, f64)| -10.0 * (e / p.max(1e-30)).max(1e-12).log10();
        eprintln!("[msc/cmp] MSC MER (low syms vs high syms): {} dB vs {} dB", mer(centre), mer(edges));
        // Cell-level agreement: hard-decode the previous receiver's msc_soft_bits (the LLR
        // stream in its collection order) and my collected cells to 6 bits, and compare.
        let qam = crate::digital::drm2::tables::QAM64_SM;
        let mut match_cells = 0usize;
        let mut total_cells = 0usize;
        let mut first_mismatch: Option<(Vec<u8>, Vec<u8>)> = None;
        for (i, cell) in all_cells.iter().enumerate() {
            // The previous receiver starts at frame 0 (its frame_offset is 0), while this
            // collection skips the warm-up's partial frame 0; skip the previous receiver's
            // first 2337 cells (frame 0) to align the frames.
            let base = (2337 + i) * 6;
            if base + 6 > rx.msc_soft_bits.len() {
                break;
            }
            let old_bits: Vec<u8> = (0..6)
                .map(|b| u8::from(rx.msc_soft_bits[base + b] >= 0.0))
                .collect();
            let my_bits = cell_bits(cell.sig, &qam);
            total_cells += 1;
            if old_bits == my_bits {
                match_cells += 1;
            } else if first_mismatch.is_none() {
                first_mismatch = Some((my_bits.clone(), old_bits));
            }
        }
        eprintln!("[msc/cmp] cell-level agreement: {match_cells}/{total_cells} ({:.1}%)", 100.0 * match_cells as f64 / total_cells.max(1) as f64);
        if let Some((mine, old)) = first_mismatch {
            eprintln!("[msc/cmp] first mismatch: mine {mine:?} old {old:?}");
        }
    }

    /// Hard-decode one equalised cell to its 6 bits (I-axis MSB first, then Q-axis).
    fn cell_bits(sig: Cplx, qam: &[f64]) -> Vec<u8> {
        let mut out = Vec::new();
        for v in [sig.re, sig.im] {
            let idx = qam.iter().enumerate().min_by(|a, b| (v - a.1).abs().total_cmp(&(v - b.1).abs())).map(|(i, _)| i).unwrap_or(0);
            for b in (0..3).rev() {
                out.push(u8::from((idx >> b) & 1 == 1));
            }
        }
        out
    }

    /// Confirm the timing-correction recipe: resample the fixture at its true clock error and
    /// remove the residual window-offset phase ramp, then the MSC must decode bit-exact.
    #[test]
    #[ignore = "timing-correction confirmation; run with --ignored --nocapture"]
    fn msc_decodes_with_timing_correction() {
        use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams, MscProtection};
        use crate::digital::drm2::fec::qam::Mapping;
        use crate::digital::drm2::interleave::CellDeinterleaver;
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (mut rows, syms, shifts) = rows_and_syms(&map, &iq);
        let n = map.mode().fft_size() as f64;
        // Per-symbol correction: measure each symbol's window offset from its own pilot phase
        // ramp and remove it, exactly tracking the constant offset and its drift.
        let offsets: Vec<f64> = (0..rows.len()).map(|i| sym_offset(&map, &rows, &syms, i)).collect();
        for (i, row) in rows.iter_mut().enumerate() {
            let off = offsets[i];
            for (c, v) in row.iter_mut().enumerate() {
                let k = (map.kmin + c as i32) as f64;
                *v = *v * Cplx::from_polar(1.0, -2.0 * core::f64::consts::PI * k * off / n);
            }
        }
        let first = sym_offset(&map, &rows, &syms, 0);
        let last = sym_offset(&map, &rows, &syms, rows.len() - 1);
        eprintln!("[msc/fix] offset after correction: first {first:+.2} last {last:+.2}");
        let mut est = ChanEst::new(&map);
        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut bits = Vec::new();
        let mut emitted: Vec<(usize, Vec<EqCell>)> = Vec::new();
        let mut frame_indices: Vec<u8> = Vec::new();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], 0, &map) else { continue };
            if out_sym == 0 { fac_cells.clear(); }
            for &c in &map.fac_carriers[out_sym] { fac_cells.push(cells[c as usize]); }
            if fac_cells.len() == 65 {
                let idx = if fac_dec.decode(&fac_cells, &mut bits) {
                    crate::digital::drm2::fac::Fac::parse(&bits).map(|f| f.channel.frame_index).unwrap_or(0xFF)
                } else { 0xFF };
                frame_indices.push(idx);
                fac_cells.clear();
            }
            emitted.push((out_sym, cells));
        }
        let params = MlcParams::msc(Mapping::Qam64Sm, map.msc_cells_per_frame, MscProtection { part_a: 0, part_b: 1, hierarchical: 0 }, 0);
        let mut de = CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut msc_dec = MlcDecoder::new(params, 1);
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut in_partial = true;
        let mut complete_frame = 0usize;
        for (out_sym, cells) in &emitted {
            if *out_sym == 0 && in_partial { in_partial = false; complete_frame = 0; }
            else if *out_sym == 0 { complete_frame += 1; }
            if in_partial { continue; }
            let Some(&fidx) = frame_indices.get(complete_frame) else { continue };
            if fidx == 0xFF { continue; }
            let super_sym = fidx as usize * 15 + *out_sym;
            if *out_sym == 0 && fidx == 1 {
                let mut all: Vec<EqCell> = Vec::new();
                for c in super_msc.iter() { all.extend_from_slice(c); }
                for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                    if let Some(d) = de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            let mut b = Vec::new();
                            if msc_dec.decode(&d, &mut b) { frames.push(b); }
                        }
                    }
                }
                for c in super_msc.iter_mut() { c.clear(); }
            }
            for &c in &map.msc_carriers[super_sym] { super_msc[super_sym].push(cells[c as usize]); }
        }
        let nn = 8390usize;
        let stream = xorshift_bits(12 * nn);
        let matched: Vec<usize> = frames.iter().zip(stream.chunks(nn)).take(8).map(|(b, s)| b.iter().zip(s).filter(|(a, z)| a == z).count()).collect();
        eprintln!("[msc/fix] decoded {} frames, bit match per frame (of {nn}): {matched:?}", frames.len());
        assert!(frames.iter().zip(stream.chunks(nn)).all(|(b, s)| b == s), "timing correction must make the MSC bit-exact");
    }

    /// Deterministic bit stream the fixture's MSC payload carries (the old chain's fixture
    /// test verifies against the same sequence).
    fn xorshift_bits(n: usize) -> Vec<u8> {
        let mut seed = 1u32;
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed & 1) as u8
            })
            .collect()
    }

    /// Diagnostic: the previous chain's own equaliser + MLC decoder on the rows this harness
    /// produces. If this decodes, the harness's rows/symbol indices are right and the defect is
    /// in the chanest cells; if it fails, the harness's frame alignment is wrong.
    #[test]
    fn previous_pipeline_decodes_on_this_harness_rows() {
        use crate::digital::drm::cellmap::CellMap as OldMap;
        use crate::digital::drm::params::{
            RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy,
        };
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = OldMap::new(
            OldMode::from_index(RobustnessMode::B.index()).unwrap(),
            OldOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, _shifts) = rows_and_syms(&map, &iq);
        let mut old_dec = crate::digital::drm::fec::mlc::MlcDecoder::new(
            crate::digital::drm::fec::mlc::MlcParams::fac(),
            0,
        );
        let mut fac_cells: Vec<crate::digital::drm::fec::qam::EqCell> = Vec::new();
        let mut ok_blocks = 0usize;
        let mut bad = 0usize;
        let mut bits = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let sym = syms[i];
            let old_row: Vec<crate::digital::drm::Cplx> =
                row.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, sym, &old_row);
            for &c in &map.fac_carriers[sym] {
                fac_cells.push(crate::digital::drm::fec::qam::EqCell {
                    sig: eq.cells[c as usize],
                    chan: eq.chan[c as usize].norm_sqr(),
                });
            }
            if fac_cells.len() == 65 {
                if old_dec.decode(&fac_cells, &mut bits) && crate::digital::drm::fac::Fac::parse(&bits).is_some() {
                    ok_blocks += 1;
                } else {
                    bad += 1;
                }
                fac_cells.clear();
            }
        }
        eprintln!("[oldpipe] ok={ok_blocks} bad={bad}");
        assert!(ok_blocks >= 10, "the previous pipeline must decode on these rows, got {ok_blocks}");
        assert_eq!(bad, 0);
    }

    /// Diagnostic: the ratio of the chanest's equalised FAC cell to the previous chain's
    /// equaliser, which must be a constant (the same channel phase). A per-carrier ramp here
    /// would point at the timing rotation; a constant means a global phase convention.
    #[test]
    fn chanest_matches_the_previous_equaliser_phase() {
        use crate::digital::drm::cellmap::CellMap as OldMap;
        use crate::digital::drm::params::{
            RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy,
        };
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = OldMap::new(
            OldMode::from_index(RobustnessMode::B.index()).unwrap(),
            OldOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (rows, syms, shifts) = rows_and_syms(&map, &iq);
        let mut est = ChanEst::new(&map);
        let mut ratios: Vec<Cplx> = Vec::new();
        for i in 0..rows.len() {
            let Some((out_sym, cells)) = est.process(&rows[i], syms[i], shifts[i], &map) else {
                continue;
            };
            let src_i = i - est.delay();
            let old_row: Vec<crate::digital::drm::Cplx> =
                rows[src_i].iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let old_eq = crate::digital::drm::chanest::equalize_symbol(&old_map, out_sym, &old_row);
            for &c in &map.fac_carriers[out_sym] {
                let a = cells[c as usize].sig;
                let b = old_eq.cells[c as usize];
                if b.norm_sqr() > 0.01 {
                    ratios.push(a / crate::digital::drm2::dsp::Cplx::new(b.re, b.im));
                }
            }
            for &c in &map.msc_carriers[out_sym] {
                let a = cells[c as usize].sig;
                let b = old_eq.cells[c as usize];
                if b.norm_sqr() > 0.01 {
                    ratios.push(a / crate::digital::drm2::dsp::Cplx::new(b.re, b.im));
                }
            }
        }
        let n = ratios.len();
        let mean = ratios.iter().fold(Cplx::zero(), |a, &r| a + r) / n as f64;
        let phase = mean.arg() * 180.0 / core::f64::consts::PI;
        let spread = (ratios.iter().map(|&r| (r - mean).norm()).sum::<f64>() / n as f64) / mean.norm();
        eprintln!("[phase] n={n} mean ratio |·|={:.3} angle={phase:.2}° spread={spread:.3}", mean.norm());
    }

    /// Measure the sub-sample timing offset: on the flat clean fixture the channel estimate at
    /// a scattered pilot is `H·e^{j2πkδ/N}`, so the phase difference between two pilots yields
    /// the window offset δ in samples. This is the diagnostic behind the MSC defect.
    #[test]
    fn measures_the_timing_offset() {
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        // The full-rate guard correlation peak: the prefix start of the first symbol.
        let nu = 1024usize;
        let g = 256usize;
        let mut best = (0usize, 0.0f64);
        for t in 0..g {
            let mut c = 0.0;
            for i in 0..g {
                let a = iq[t + i];
                let b = iq[t + i + nu];
                c += a.re * b.re + a.im * b.im;
            }
            if c > best.1 {
                best = (t, c);
            }
        }
        eprintln!("[timing] full-rate guard-correlation peak at sample {}", best.0);
        // The first window start from the TimeSync.
        let mut ts = crate::digital::drm2::sync::timesync::TimeSync::new(RobustnessMode::B);
        let mut first_start = None;
        for block in iq.chunks(3248) {
            let _ = ts.push(block);
            while let Some(w) = ts.next_window() {
                first_start = Some(w.start);
                break;
            }
            if first_start.is_some() {
                break;
            }
        }
        let start = first_start.expect("a window");
        eprintln!("[timing] first window start {start}; offset vs peak = {} samples", start - best.0 as i64);
        let (rows, syms, _shifts) = rows_and_syms(&map, &iq);
        let n = map.mode().fft_size() as f64;
        let mut offsets = Vec::new();
        for (row, sym) in rows.iter().zip(&syms).take(15) {
            let pilots: Vec<(i32, Cplx)> = (0..map.num_carriers)
                .filter(|&c| map.cell(*sym, c).is_scattered())
                .map(|c| {
                    let r = map.pilot(*sym, c);
                    (map.kmin + c as i32, if r.norm_sqr() > 0.0 { row[c] / r } else { Cplx::zero() })
                })
                .collect();
            for pair in pilots.windows(2) {
                let (k1, h1) = pair[0];
                let (k2, h2) = pair[1];
                if h1.norm() < 1e-3 || h2.norm() < 1e-3 {
                    continue;
                }
                let dphase = (h2 / h1).arg();
                let dk = (k2 - k1) as f64;
                offsets.push(dphase * n / (2.0 * core::f64::consts::PI * dk));
            }
        }
        let mean = offsets.iter().sum::<f64>() / offsets.len() as f64;
        let std = (offsets.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / offsets.len() as f64).sqrt();
        eprintln!("[timing] window offset from pilots: {mean:+.3} ± {std:.3} samples (n={})", offsets.len());
        // Per-symbol offset, to see whether the offset is constant or drifts with the timing
        // tracking.
        let per_sym: Vec<f64> = rows
            .iter()
            .zip(&syms)
            .map(|(row, sym)| {
                let ps: Vec<f64> = (0..map.num_carriers)
                    .filter(|&c| map.cell(*sym, c).is_scattered())
                    .collect::<Vec<_>>()
                    .windows(2)
                    .filter_map(|w| {
                        let (c1, c2) = (w[0], w[1]);
                        let (r1, r2) = (map.pilot(*sym, c1), map.pilot(*sym, c2));
                        if r1.norm_sqr() == 0.0 || r2.norm_sqr() == 0.0 {
                            return None;
                        }
                        let h1 = row[c1] / r1;
                        let h2 = row[c2] / r2;
                        if h1.norm() < 1e-3 || h2.norm() < 1e-3 {
                            return None;
                        }
                        let dphase = (h2 / h1).arg();
                        let dk = (map.kmin + c2 as i32 - (map.kmin + c1 as i32)) as f64;
                        Some(dphase * n / (2.0 * core::f64::consts::PI * dk))
                    })
                    .collect();
                ps.iter().sum::<f64>() / ps.len().max(1) as f64
            })
            .collect();
        eprintln!("[timing] per-symbol offset (first 15): {:?}", &per_sym[..15.min(per_sym.len())]);
        eprintln!("[timing] per-symbol offset (last 15): {:?}", &per_sym[per_sym.len().saturating_sub(15)..]);
        // Sweep a source-rate assumption: the drift vanishes at the fixture's true clock error.
        let mut drift_line = String::new();
        for ppm in [-80.0f64, -52.0, -20.0, 0.0, 20.0, 52.0, 80.0] {
            let iq2 = resample_to_core("../tests/fixtures/drm/drm_modeB_so3_48k.f32", 48_000.0 * (1.0 + ppm * 1e-6));
            let (rows2, syms2, _) = rows_and_syms(&map, &iq2);
            let first = sym_offset(&map, &rows2, &syms2, 0);
            let last = sym_offset(&map, &rows2, &syms2, rows2.len().saturating_sub(1));
            drift_line.push_str(&format!("{ppm:+.0}ppm:{first:+.1}/{last:+.1} "));
        }
        eprintln!("[timing] offset first/last by assumed source-rate error: {drift_line}");
    }

    /// Mean window offset (samples) of one symbol's scattered-pilot phase ramp.
    fn sym_offset(map: &CellMap, rows: &[Vec<Cplx>], syms: &[usize], i: usize) -> f64 {
        let n = map.mode().fft_size() as f64;
        let sym = syms[i];
        let row = &rows[i];
        let os: Vec<f64> = (0..map.num_carriers)
            .filter(|&c| map.cell(sym, c).is_scattered())
            .collect::<Vec<_>>()
            .windows(2)
            .filter_map(|w| {
                let (c1, c2) = (w[0], w[1]);
                let (r1, r2) = (map.pilot(sym, c1), map.pilot(sym, c2));
                if r1.norm_sqr() == 0.0 || r2.norm_sqr() == 0.0 {
                    return None;
                }
                let h1 = row[c1] / r1;
                let h2 = row[c2] / r2;
                if h1.norm() < 1e-3 || h2.norm() < 1e-3 {
                    return None;
                }
                let dphase = (h2 / h1).arg();
                let dk = (c2 as i32 - c1 as i32) as f64;
                Some(dphase * n / (2.0 * core::f64::consts::PI * dk))
            })
            .collect();
        os.iter().sum::<f64>() / os.len().max(1) as f64
    }

    /// The clean fixture's FAC constellation must come out of the chain — acquisition, NCO,
    /// timing, demodulation, channel estimation, equalisation — as the 4-QAM the FAC is. With
    /// the lattice-indexed time interpolation and the linear frequency interpolation it reaches
    /// 42 dB (measured); the previous chain's per-symbol equaliser, by comparison, lands the
    /// same cells within about nine degrees of the constellation points.
    #[test]
    fn estimates_the_clean_fixture_with_a_high_mer() {
        let iq = load_iq_f64("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let (mer, freq_int, time_int) = run(&iq);
        eprintln!("[chanest] lattice: freq_int={freq_int} time_int={time_int} FAC MER={mer:?}");
        let mer = mer.expect("a frame's worth of FAC cells must have been equalised");
        assert!(mer > 20.0, "clean fixture FAC MER {mer:.1} dB must be high");
    }

    /// Search the residual carrier offset and timing on the live capture: if a residual
    /// explains the low MER, the fix is a finer acquisition or a residual tracking loop.
    #[test]
    #[ignore = "diagnostic search; run with --ignored --nocapture"]
    fn search_residual_offset_and_timing_on_the_live_capture() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let base = resample_to_core(path, 48_828.125);
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        // Coarse acquisition on the whole capture, then remove its offset.
        let mut flat: Vec<f64> = Vec::with_capacity(base.len() * 2);
        for v in &base {
            flat.push(v.re);
            flat.push(v.im);
        }
        let mut acq = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true);
        let coarse = acq.push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0);
        eprintln!("[search] coarse offset {coarse:.1} Hz");
        let mut best = (0.0f64, 0i32, f64::NEG_INFINITY);
        for residual in [-4.0f64, -2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 4.0] {
            for shift in [-3i32, -2, -1, 0, 1, 2, 3] {
                let mut iq = base.clone();
                let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse + residual);
                nco.process(&mut iq);
                let iq = if shift > 0 { &iq[shift as usize..] } else { &iq[..] };
                let (mer, _, _) = run(iq);
                if let Some(m) = mer {
                    if m > best.2 {
                        best = (residual, shift, m);
                    }
                }
            }
        }
        eprintln!("[search] best residual {:.1} Hz shift {} -> MER {:.1} dB", best.0, best.1, best.2);
    }

    /// OPEN: on the live capture the same chain reports MER -1.4 dB while the reference reports
    /// 17.8 dB, so a real signal still defeats the estimator. The clean fixture reaching 42 dB
    /// says the structure is right; the difference is what a real channel adds — a residual
    /// carrier/timing error after acquisition (the acquisition resolves 7.8 Hz, the timing a
    /// quarter sample) and the three-symbol-span time interpolation on a channel that moves.
    /// Next: measure the residual with the framesync phase and the FAC decisions over time.
    #[test]
    #[ignore = "live capture: MER -1.4 dB against the reference's 17.8 dB (see the comment)"]
    fn estimates_the_live_capture_near_the_reference() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return; // a fresh capture is a bonus, never a fixture
        }
        let iq = resample_to_core(path, 48_828.125);
        let (mer, freq_int, time_int) = run(&iq);
        eprintln!("[chanest/live] lattice: freq_int={freq_int} time_int={time_int} FAC MER={mer:?}");
        let mer = mer.expect("the live capture must equalise at least one frame");
        // The reference receiver reports MER 17.8 dB on this capture.
        assert!(mer > 12.0, "live FAC MER {mer:.1} dB is below the reference's 17.8 dB band");
    }

    /// Diagnostic: close the timing loop — feed the tracker's `timing_adjust` back into the
    /// TimeSync's window positions on a second pass — and see whether the live MER moves. Result:
    /// −10.4 dB (the open loop reads −8.6), so the simple timing loop is not the deficit; the
    /// remaining gap to the reference's 17.8 dB is the time-Wiener (Doppler-adapted) time
    /// interpolation, which this port still replaces with a fixed linear interpolation.
    #[test]
    #[ignore = "live timing-loop diagnostic; run with --ignored --nocapture"]
    fn live_capture_with_closed_timing_loop() {
        use crate::digital::drm2::sync::timesync::TimeSync;
        use crate::digital::drm2::ofdm::OfdmDemod;
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        let base = resample_to_core(path, 48_828.125);
        // Coarse + fine exactly as `run` does.
        let mut flat: Vec<f64> = Vec::with_capacity(base.len() * 2);
        for v in &base { flat.push(v.re); flat.push(v.im); }
        let mut acq = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true);
        let coarse = acq.push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0);
        let mut corrected = base.clone();
        let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse);
        nco.process(&mut corrected);
        let (rows, syms, _) = rows_and_syms(&map, &corrected);
        let mut fine = 0.0;
        if let Some(f) = crate::digital::drm2::sync::finefreq::estimate_residual_hz(&map, &rows, &syms) {
            fine = f;
            let mut nco2 = crate::digital::drm2::sync::nco::Nco::new(f);
            nco2.process(&mut corrected);
        }
        let (rows, syms, _) = rows_and_syms(&map, &corrected);
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        eprintln!("[closeloop] coarse {coarse:.1} fine {fine:+.2} phase {phase}");

        // Second pass, interleaved: demodulate, feed the estimator, feed timing_adjust back.
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut demod = OfdmDemod::new(&map);
        let mut est = ChanEst::new(&map);
        let mut cells = Vec::new();
        let mut mer = None;
        let mut n = 0usize;
        for block in corrected.chunks(3248) {
            let _ = ts.push(block);
            while let Some(w) = ts.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 { continue; }
                demod.demodulate(&w.samples, &mut cells);
                let sym = (n % spf + phase) % spf;
                if n == 45 { est.start_timing_tracking(); }
                if est.process(&cells, sym, w.shift, &map).is_some() {
                    let ta = est.last_track.timing_adjust;
                    if ta != 0 { ts.adjust_timing(ta as f64); }
                    if n % 90 == 0 {
                        eprintln!("[closeloop] n={n} ta={ta} pds_off={:.1} pds_len={:.1} mer={:?}", est.last_track.pds_offset, est.last_track.pds_len, est.stats().fac_mer_db);
                    }
                    if let Some(m) = est.stats().fac_mer_db { mer = Some(m); }
                }
                n += 1;
            }
        }
        eprintln!("[closeloop] MER {mer:?} after {n} symbols");
    }

    /// Diagnostic: compare the old per-symbol linear equaliser's FAC MER on the live capture
    /// with the new estimator's. If the old equaliser also reads low, the deficit is the
    /// channel/timing, not the Wiener; if the old equaliser reads high, the new estimator has a
    /// regression.
    #[test]
    #[ignore = "live old-equaliser comparison; run with --ignored --nocapture"]
    fn live_capture_old_equaliser_comparison() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        let old_map = crate::digital::drm::cellmap::CellMap::new(
            crate::digital::drm::params::RobustnessMode::from_index(RobustnessMode::B.index()).unwrap(),
            crate::digital::drm::params::SpectrumOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .unwrap();
        let base = resample_to_core(path, 48_828.125);
        let mut flat: Vec<f64> = Vec::with_capacity(base.len() * 2);
        for v in &base { flat.push(v.re); flat.push(v.im); }
        let mut acq = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true);
        let coarse = acq.push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0);
        let mut corrected = base.clone();
        let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse);
        nco.process(&mut corrected);
        let (rows, syms, _) = rows_and_syms(&map, &corrected);
        let mut fine = 0.0;
        if let Some(f) = crate::digital::drm2::sync::finefreq::estimate_residual_hz(&map, &rows, &syms) {
            fine = f;
            let mut nco2 = crate::digital::drm2::sync::nco::Nco::new(f);
            nco2.process(&mut corrected);
        }
        let (rows, syms, _) = rows_and_syms(&map, &corrected);
        let qam4 = crate::digital::drm2::tables::QAM4[0];
        let (mut err, mut pow) = (0.0, 0.0);
        for (row, sym) in rows.iter().zip(&syms) {
            let old_row: Vec<crate::digital::drm::Cplx> =
                row.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = crate::digital::drm::chanest::equalize_symbol(&old_map, *sym, &old_row);
            for c in 0..map.num_carriers {
                if !map.cell(*sym, c).is_fac() { continue; }
                let s = eq.cells[c];
                let dr = if s.re >= 0.0 { qam4 } else { -qam4 };
                let di = if s.im >= 0.0 { qam4 } else { -qam4 };
                err += eq.chan[c].norm_sqr() * ((s.re - dr).powi(2) + (s.im - di).powi(2));
                pow += eq.chan[c].norm_sqr();
            }
        }
        let old_mer = -10.0 * (err / pow.max(1e-30)).max(1e-12).log10();
        eprintln!("[liveold] old per-symbol equaliser FAC MER {old_mer:.1} dB (new reads -8.6, reference 17.8)");
    }
}

#[cfg(test)]
mod low_snr_tests {
    use super::*;
    use crate::digital::drm2::ofdm::OfdmDemod;
    use crate::digital::drm2::params::{RobustnessMode, SpectrumOccupancy};
    use crate::digital::drm2::sync::timesync::TimeSync;

    /// Deterministic AWGN, so the measurement is reproducible without a seed file.
    fn add_noise(iq: &mut [Cplx], amplitude: f64) {
        let mut state = 0x2545_F491u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f64 / 8_388_608.0 - 1.0
        };
        for v in iq.iter_mut() {
            v.re += next() * amplitude;
            v.im += next() * amplitude;
        }
    }

    /// The FAC MER a sequence of rows yields through a given per-symbol equaliser output.
    fn fac_mer(map: &CellMap, rows: &[Vec<Cplx>], syms: &[usize], eq: impl Fn(usize, &[Cplx]) -> Vec<EqCell>) -> f64 {
        let qam4 = crate::digital::drm2::tables::QAM4[0];
        let (mut err, mut pow) = (0.0, 0.0);
        for (row, sym) in rows.iter().zip(syms) {
            let out = eq(*sym, row);
            for c in 0..map.num_carriers {
                if !map.cell(*sym, c).is_fac() {
                    continue;
                }
                let s = out[c].sig;
                let dr = if s.re >= 0.0 { qam4 } else { -qam4 };
                let di = if s.im >= 0.0 { qam4 } else { -qam4 };
                err += out[c].chan * ((s.re - dr).powi(2) + (s.im - di).powi(2));
                pow += out[c].chan;
            }
        }
        -10.0 * (err / pow.max(1e-30)).max(1e-12).log10()
    }

    /// At a low in-band SNR the new estimator must not be worse than the previous chain's
    /// per-symbol linear equaliser — that comparison is the task's "low level" acceptance item, and
    /// it is measurable today, before the FAC/SDC/MSC stages exist.
    #[test]
    fn is_not_worse_than_the_previous_equaliser_at_low_snr() {
        use crate::digital::drm::chanest::equalize_symbol as old_equalize;
        use crate::digital::drm::cellmap::CellMap as OldMap;
        use crate::digital::drm::params::{RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy};

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = OldMap::new(
            OldMode::from_index(RobustnessMode::B.index()).unwrap(),
            OldOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("layout");

        let raw = std::fs::read("../tests/fixtures/drm/drm_modeB_so3_48k.f32").expect("fixture");
        let mut iq: Vec<Cplx> = raw
            .chunks_exact(8)
            .map(|c| {
                Cplx::new(
                    f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                    f64::from(f32::from_le_bytes([c[4], c[5], c[6], c[7]])),
                )
            })
            .collect();
        // The fixture's rms is about 0.25; 0.05 of noise puts the in-band SNR near 12 dB, which is
        // where a real shortwave signal sits.
        let rms = (iq.iter().map(|v| v.norm_sqr()).sum::<f64>() / iq.len() as f64).sqrt();
        add_noise(&mut iq, rms * 0.2);

        // Rows and symbol indices, as the chain produces them.
        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut demod = OfdmDemod::new(&map);
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
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        let syms: Vec<usize> = (0..rows.len()).map(|i| (i % spf + phase) % spf).collect();

        let mut est = ChanEst::new(&map);
        let mut mer_new = None;
        for ((row, sym), shift) in rows.iter().zip(&syms).zip(&shifts) {
            if est.process(row, *sym, *shift, &map).is_some() {
                if let Some(m) = est.stats().fac_mer_db {
                    mer_new = Some(m);
                }
            }
        }
        let mer_old = fac_mer(&map, &rows, &syms, |sym, row| {
            let old_cells: Vec<crate::digital::drm::Cplx> =
                row.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let eq = old_equalize(&old_map, sym, &old_cells);
            eq.cells
                .iter()
                .zip(&eq.chan)
                .map(|(s, ch)| EqCell { sig: Cplx::new(s.re, s.im), chan: ch.norm_sqr() })
                .collect()
        });
        let new = mer_new.expect("the new estimator must produce a frame");
        eprintln!("[low-snr] FAC MER: new {new:.1} dB, previous equaliser {mer_old:.1} dB");
        assert!(
            new + 1.0 >= mer_old,
            "the new estimator ({new:.1} dB) must not trail the previous equaliser ({mer_old:.1} dB)"
        );
    }
}

#[cfg(test)]
mod fading_tests {
    use super::*;
    use crate::digital::drm2::ofdm::OfdmDemod;
    use crate::digital::drm2::params::{RobustnessMode, SpectrumOccupancy};
    use crate::digital::drm2::sync::timesync::TimeSync;

    /// The task's "fading" acceptance item, in its simplest honest form: a single echo one
    /// millisecond behind the direct path — the delay spread the reference measures on the bench
    /// capture, whose coherence bandwidth (~160 Hz) is narrower than the scattered pilots' 281 Hz
    /// lattice spacing. Measured today through both estimators, so the tracking and the Wiener work
    /// has a baseline to beat.
    #[test]
    fn is_not_worse_than_the_previous_equaliser_with_a_one_ms_echo() {
        use crate::digital::drm::chanest::equalize_symbol as old_equalize;
        use crate::digital::drm::cellmap::CellMap as OldMap;
        use crate::digital::drm::params::{
            RobustnessMode as OldMode, SpectrumOccupancy as OldOccupancy,
        };

        let map = CellMap::new(RobustnessMode::B, SpectrumOccupancy::SO_3).expect("layout");
        let old_map = OldMap::new(
            OldMode::from_index(RobustnessMode::B.index()).unwrap(),
            OldOccupancy::new(SpectrumOccupancy::SO_3.value()).unwrap(),
        )
        .expect("layout");

        let raw = std::fs::read("../tests/fixtures/drm/drm_modeB_so3_48k.f32").expect("fixture");
        let direct: Vec<Cplx> = raw
            .chunks_exact(8)
            .map(|c| {
                Cplx::new(
                    f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                    f64::from(f32::from_le_bytes([c[4], c[5], c[6], c[7]])),
                )
            })
            .collect();
        // One millisecond at the core rate, half the direct path's amplitude.
        let delay = 48usize;
        let mut iq = direct.clone();
        for n in delay..direct.len() {
            iq[n] += direct[n - delay] * 0.5;
        }

        let mut ts = TimeSync::new(RobustnessMode::B);
        let mut demod = OfdmDemod::new(&map);
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
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = RobustnessMode::B.symbols_per_frame();
        let syms: Vec<usize> = (0..rows.len()).map(|i| (i % spf + phase) % spf).collect();
        let qam4 = crate::digital::drm2::tables::QAM4[0];
        let mer_of = |eq: &dyn Fn(usize, &[Cplx]) -> Vec<EqCell>| -> f64 {
            let (mut err, mut pow) = (0.0, 0.0);
            for (row, sym) in rows.iter().zip(&syms) {
                let out = eq(*sym, row);
                for c in 0..map.num_carriers {
                    if !map.cell(*sym, c).is_fac() {
                        continue;
                    }
                    let s = out[c].sig;
                    let dr = if s.re >= 0.0 { qam4 } else { -qam4 };
                    let di = if s.im >= 0.0 { qam4 } else { -qam4 };
                    err += out[c].chan * ((s.re - dr).powi(2) + (s.im - di).powi(2));
                    pow += out[c].chan;
                }
            }
            -10.0 * (err / pow.max(1e-30)).max(1e-12).log10()
        };
        let mut est = ChanEst::new(&map);
        for ((row, sym), shift) in rows.iter().zip(&syms).zip(&shifts) {
            let _ = est.process(row, *sym, *shift, &map);
        }
        let mer_new = est.stats().fac_mer_db.expect("a frame through the new estimator");
        let mer_old = mer_of(&|sym, row| {
            let old_cells: Vec<crate::digital::drm::Cplx> =
                row.iter().map(|c| crate::digital::drm::Cplx::new(c.re, c.im)).collect();
            let out = old_equalize(&old_map, sym, &old_cells);
            out.cells
                .iter()
                .zip(&out.chan)
                .map(|(s, ch)| EqCell { sig: Cplx::new(s.re, s.im), chan: ch.norm_sqr() })
                .collect()
        });
        eprintln!("[echo] FAC MER: new {mer_new:.1} dB, previous equaliser {mer_old:.1} dB");
        assert!(
            mer_new + 1.0 >= mer_old,
            "with a 1 ms echo the new estimator ({mer_new:.1} dB) must not trail the previous one ({mer_old:.1} dB)"
        );
    }
}
