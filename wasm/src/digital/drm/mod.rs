//! DRM30 (Digital Radio Mondiale, ES 201 980) demodulator — the digital-path plugin
//! for SW-broadcast DRM (robustness modes A–D, spectrum occupancies 0–5).
//!
//! This is an independent reimplementation of the DRM physical layer; the open-source
//! DecDRM receiver was consulted as a reference and cross-check oracle, but no code is
//! copied. The module is on the **digital path**: it reads RAW complex baseband and
//! never touches the audio chain.
//!
//! Pipeline: guard-interval time/robustness-mode sync → OFDM demodulation (FFT) →
//! occupancy detection from the power spectrum → frame sync on the time-reference
//! pilots → gain-pilot channel estimation + equalisation → QAM soft-bit demapping.
//! The output is a constellation, an SNR estimate and soft bits for the FAC/SDC/MSC
//! channels (the FAC/SDC decoders in later steps turn those bits into metadata).

pub mod cellmap;
pub mod chanest;
pub mod ofdm;
pub mod params;
pub mod qam;
pub mod tables;
pub mod timesync;

use cellmap::CellMap;
use ofdm::{carrier_at, FullFft};
use params::{RobustnessMode, SpectrumOccupancy};
use tables::{QAM16, QAM4, QAM64_SM};

/// A complex sample/value. Internal precision is f64.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cplx {
    pub re: f64,
    pub im: f64,
}

impl Cplx {
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub fn from_polar(r: f64, theta: f64) -> Self {
        Self { re: r * theta.cos(), im: r * theta.sin() }
    }

    pub fn conj(self) -> Self {
        Self { re: self.re, im: -self.im }
    }

    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    pub fn norm(self) -> f64 {
        self.norm_sqr().sqrt()
    }
}

impl core::ops::Add for Cplx {
    type Output = Cplx;
    fn add(self, o: Cplx) -> Cplx {
        Cplx::new(self.re + o.re, self.im + o.im)
    }
}

impl core::ops::Sub for Cplx {
    type Output = Cplx;
    fn sub(self, o: Cplx) -> Cplx {
        Cplx::new(self.re - o.re, self.im - o.im)
    }
}

impl core::ops::Mul for Cplx {
    type Output = Cplx;
    fn mul(self, o: Cplx) -> Cplx {
        Cplx::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

impl core::ops::Div for Cplx {
    type Output = Cplx;
    fn div(self, o: Cplx) -> Cplx {
        let d = o.norm_sqr();
        Cplx::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}

impl core::ops::Neg for Cplx {
    type Output = Cplx;
    fn neg(self) -> Cplx {
        Cplx::new(-self.re, -self.im)
    }
}

impl core::ops::AddAssign for Cplx {
    fn add_assign(&mut self, o: Cplx) {
        self.re += o.re;
        self.im += o.im;
    }
}

/// Complex value with amplitude `amp` and phase `2π·phase/1024`.
fn polar_1024(amp: f64, phase: i32) -> Cplx {
    Cplx::from_polar(amp, 2.0 * core::f64::consts::PI * f64::from(phase) / 1024.0)
}

/// Map FFT bin to absolute carrier index (bin 0 = DC, upper half = negative carriers).
fn bin_to_k(bin: usize, n: usize) -> i32 {
    let b = bin as i32;
    let half = (n as i32 - 1) / 2;
    if b <= half { b } else { b - n as i32 }
}

/// The DRM receiver: buffers baseband and, once enough is available, runs the whole
/// physical-layer acquisition + demodulation chain once.
pub struct DrmReceiver {
    buf: Vec<Cplx>,
    locked: bool,
    pub mode: Option<RobustnessMode>,
    pub occupancy: Option<SpectrumOccupancy>,
    /// Frame offset found by the time-pilot sync (symbol `(i + offset) % symbols_per_frame`
    /// is symbol 0 of a frame).
    pub frame_offset: usize,
    /// Time-pilot correlation score at the chosen frame offset (a frame-lock metric).
    pub frame_sync_score: f64,
    /// FAC constellation SNR (from 4-QAM decisions), dB.
    pub snr_db: Option<f64>,
    /// Equalised FAC cells (I, Q) in mapping order.
    pub fac_constellation: Vec<(f64, f64)>,
    /// Soft bits (LLRs, positive → bit 1) per channel, in cell-mapping order.
    pub fac_soft_bits: Vec<f64>,
    pub sdc_soft_bits: Vec<f64>,
    pub msc_soft_bits: Vec<f64>,
    pub symbols_demodulated: usize,
}

impl Default for DrmReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl DrmReceiver {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            locked: false,
            mode: None,
            occupancy: None,
            frame_offset: 0,
            frame_sync_score: 0.0,
            snr_db: None,
            fac_constellation: Vec::new(),
            fac_soft_bits: Vec::new(),
            sdc_soft_bits: Vec::new(),
            msc_soft_bits: Vec::new(),
            symbols_demodulated: 0,
        }
    }

    /// Feed interleaved complex baseband (f32 I,Q pairs) at 48 kHz.
    pub fn push(&mut self, iq: &[f32]) {
        for c in iq.chunks_exact(2) {
            self.buf.push(Cplx::new(f64::from(c[0]), f64::from(c[1])));
        }
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Run acquisition + demodulation once, if enough samples are buffered. Idempotent
    /// after the first successful lock.
    pub fn run(&mut self) {
        if self.locked {
            return;
        }
        // Two seconds is enough for mode detection (the guard correlations need a few
        // symbol periods) plus at least one frame.
        if self.buf.len() < 2 * params::SAMPLE_RATE as usize {
            return;
        }
        let Some(acq) = timesync::acquire(&self.buf) else { return };
        let mode = acq.mode;
        let n = mode.fft_size();
        let ts = mode.symbol_len();

        // Demodulate every complete symbol, keeping the full FFT rows and a power
        // spectrum for occupancy detection.
        let mut fft = FullFft::new(n);
        let mut power = vec![0.0f64; n];
        let mut rows: Vec<Vec<Cplx>> = Vec::new();
        let mut start = acq.guard_start + mode.guard_len();
        while start + mode.fft_size() <= self.buf.len() {
            let mut row = vec![Cplx::new(0.0, 0.0); n];
            fft.forward(&self.buf[start..start + mode.fft_size()], &mut row);
            for (bin, v) in row.iter().enumerate() {
                power[bin] += v.norm_sqr();
            }
            rows.push(row);
            start += ts;
        }
        if rows.len() < mode.symbols_per_frame() {
            return;
        }
        let nrows = rows.len() as f64;
        for p in power.iter_mut() {
            *p /= nrows;
        }

        let Some(so) = detect_occupancy(mode, &power, n) else { return };
        let Some(map) = CellMap::new(mode, so) else { return };
        let (r, score) = frame_sync(mode, &rows, n);

        self.mode = Some(mode);
        self.occupancy = Some(so);
        self.frame_offset = r;
        self.frame_sync_score = score;
        self.symbols_demodulated = rows.len();
        self.decode(&map, &rows, r);
        self.locked = true;
    }

    /// Channel estimation + equalisation + QAM demapping for every symbol.
    fn decode(&mut self, map: &CellMap, rows: &[Vec<Cplx>], r: usize) {
        let n = map.mode().fft_size();
        let spsf = map.symbols_per_superframe;
        let sdc_syms = map.mode().sdc_symbols();

        let mut sig = 0.0f64;
        let mut noise = 0.0f64;
        let mut fac_n = 0.0f64;

        for (i, row) in rows.iter().enumerate() {
            // Super-frame symbol index. Assumes the first demodulated symbol is frame 0
            // of a super frame (true for the synthesised fixture; real signals get the
            // frame number from the FAC, which the FAC decoder supplies).
            let sym = (i + r) % spsf;
            let cells: Vec<Cplx> = (0..map.num_carriers)
                .map(|c| carrier_at(row, n, map.kmin + c as i32))
                .collect();
            let eq = chanest::equalize_symbol(map, sym, &cells);

            // FAC (always 4-QAM).
            for &c in &map.fac_carriers[sym] {
                let v = eq.cells[c as usize];
                self.fac_constellation.push((v.re, v.im));
                for llr in qam::soft_bits(v.re, v.im, &QAM4) {
                    self.fac_soft_bits.push(llr);
                }
                let ir = qam::hard_axis(v.re, &QAM4);
                let ii = qam::hard_axis(v.im, &QAM4);
                let dec = Cplx::new(QAM4[ir], QAM4[ii]);
                sig += dec.norm_sqr();
                noise += (v - dec).norm_sqr();
                fac_n += 1.0;
            }

            // SDC (16-QAM in this fixture; 4-QAM also exists — resolved from the FAC).
            if sym < sdc_syms {
                for &c in &map.sdc_carriers[sym] {
                    let v = eq.cells[c as usize];
                    for llr in qam::soft_bits(v.re, v.im, &QAM16) {
                        self.sdc_soft_bits.push(llr);
                    }
                }
            }

            // MSC (64-QAM SM in this fixture; resolved from the FAC).
            for &c in &map.msc_carriers[sym] {
                let v = eq.cells[c as usize];
                for llr in qam::soft_bits(v.re, v.im, &QAM64_SM) {
                    self.msc_soft_bits.push(llr);
                }
            }
        }

        self.snr_db = if noise > 0.0 && fac_n > 0.0 {
            Some(10.0 * (sig / noise).log10())
        } else {
            None
        };
    }
}

/// Detect the spectrum occupancy from the per-carrier power: the occupied carriers
/// are exactly [Kmin, Kmax] of one of the standard layouts.
fn detect_occupancy(mode: RobustnessMode, power: &[f64], n: usize) -> Option<SpectrumOccupancy> {
    let maxp = power.iter().cloned().fold(0.0f64, f64::max);
    if maxp <= 0.0 {
        return None;
    }
    let thr = maxp * 0.05;
    let (mut kmin, mut kmax) = (i32::MAX, i32::MIN);
    for (bin, &p) in power.iter().enumerate() {
        if p > thr {
            let k = bin_to_k(bin, n);
            kmin = kmin.min(k);
            kmax = kmax.max(k);
        }
    }
    for so in SpectrumOccupancy::ALL {
        if let Some((a, b)) = params::carrier_range(mode, so) {
            if a == kmin && b == kmax {
                return Some(so);
            }
        }
    }
    None
}

/// Frame synchronisation on the time-reference pilots (present only in symbol 0 of
/// each frame). Returns `(offset, score)` where `(i + offset) % symbols_per_frame` is
/// symbol 0, and `score` is the normalised time-pilot correlation.
fn frame_sync(mode: RobustnessMode, rows: &[Vec<Cplx>], n: usize) -> (usize, f64) {
    let tp = tables::time_pilots(mode);
    let spf = mode.symbols_per_frame();
    let mut best_r = 0usize;
    let mut best = -1.0f64;
    for r in 0..spf {
        let mut acc = Cplx::new(0.0, 0.0);
        let mut cnt = 0.0f64;
        for (i, row) in rows.iter().enumerate() {
            if (i + r) % spf == 0 {
                for &(k, phase) in tp {
                    let cell = carrier_at(row, n, i32::from(k));
                    let rf = polar_1024(1.0, i32::from(phase));
                    acc += cell * rf.conj();
                }
                cnt += 1.0;
            }
        }
        let score = acc.norm() / cnt.max(1.0);
        if score > best {
            best = score;
            best_r = r;
        }
    }
    (best_r, best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cplx_arithmetic() {
        let a = Cplx::new(3.0, 4.0);
        let b = Cplx::new(1.0, -2.0);
        assert_eq!(a.norm(), 5.0);
        assert_eq!((a * b), Cplx::new(11.0, -2.0));
        assert!(((a / a) - Cplx::new(1.0, 0.0)).norm() < 1e-12);
        assert_eq!(a.conj(), Cplx::new(3.0, -4.0));
    }

    #[test]
    fn bin_to_carrier_maps_dc_and_negative_half() {
        assert_eq!(bin_to_k(0, 1024), 0);
        assert_eq!(bin_to_k(103, 1024), 103);
        assert_eq!(bin_to_k(1023, 1024), -1);
        assert_eq!(bin_to_k(1024 - 103, 1024), -103);
    }
}
