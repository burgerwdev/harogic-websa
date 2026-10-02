//! OFDM transform and demodulation.
//!
//! Carrier `k` maps to FFT bin `k mod N` (true baseband: the DRM DC carrier sits at
//! 0 Hz). Mode B uses a 1024-point radix-2 transform; modes A (1152), C (704) and
//! D (448) use a Bluestein DFT, since their sizes are not powers of two.

use crate::digital::drm::cellmap::CellMap;
use crate::digital::drm::Cplx;
use crate::fft::{Bluestein, Fft};

enum Backend {
    Pow2(Fft),
    Blue(Bluestein),
}

impl Backend {
    fn new(n: usize) -> Self {
        if n.is_power_of_two() { Backend::Pow2(Fft::new(n)) } else { Backend::Blue(Bluestein::new(n)) }
    }

    fn forward(&mut self, re: &mut [f64], im: &mut [f64]) {
        match self {
            Backend::Pow2(f) => f.transform(re, im, false),
            Backend::Blue(b) => b.forward(re, im),
        }
    }
}

/// A full forward DFT of one OFDM symbol, for any of the DRM FFT sizes.
pub struct FullFft {
    backend: Backend,
    n: usize,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl FullFft {
    pub fn new(n: usize) -> Self {
        Self { backend: Backend::new(n), n, re: vec![0.0; n], im: vec![0.0; n] }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    /// Forward DFT of `window` (length `n`), scaled by 1/n, into `out` (length `n`,
    /// bin order 0..n).
    pub fn forward(&mut self, window: &[Cplx], out: &mut [Cplx]) {
        assert_eq!(window.len(), self.n);
        assert_eq!(out.len(), self.n);
        for (i, v) in window.iter().enumerate() {
            self.re[i] = v.re;
            self.im[i] = v.im;
        }
        self.backend.forward(&mut self.re, &mut self.im);
        let scale = 1.0 / self.n as f64;
        for (i, v) in out.iter_mut().enumerate() {
            *v = Cplx::new(self.re[i] * scale, self.im[i] * scale);
        }
    }
}

/// OFDM demodulator for one mode/occupancy: FFT and extraction of carriers Kmin..=Kmax.
pub struct OfdmDemod {
    fft: FullFft,
    kmin: i32,
    num_carriers: usize,
    all: Vec<Cplx>,
}

impl OfdmDemod {
    pub fn new(map: &CellMap) -> Self {
        let n = map.mode().fft_size();
        Self {
            fft: FullFft::new(n),
            kmin: map.kmin,
            num_carriers: map.num_carriers,
            all: vec![Cplx::new(0.0, 0.0); n],
        }
    }

    /// Demodulate one window of `N` samples into `num_carriers` cells (index c =
    /// k − Kmin).
    pub fn demodulate(&mut self, window: &[Cplx], out: &mut Vec<Cplx>) {
        self.fft.forward(window, &mut self.all);
        out.clear();
        let n = self.fft.len() as i32;
        for c in 0..self.num_carriers {
            let k = self.kmin + c as i32;
            out.push(self.all[k.rem_euclid(n) as usize]);
        }
    }
}

/// Carrier value at absolute carrier index `k` from a full FFT row.
pub fn carrier_at(row: &[Cplx], n: usize, k: i32) -> Cplx {
    row[k.rem_euclid(n as i32) as usize]
}
