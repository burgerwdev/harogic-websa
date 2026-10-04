//! MSC cell interleaving (ES 201 980 §7.6): a pseudo-random permutation within each
//! multiplex frame (t₀ = 5), optionally spread over D = 5 frames (long interleaving),
//! where cell i is delayed by i mod D frames at the transmitter.

use crate::digital::drm2::fec::interleaver::permutation;
use crate::digital::drm2::fec::qam::EqCell;

/// t₀ of the MSC cell interleaver.
pub const CELL_INTERLEAVER_T0: usize = 5;

/// Receiver-side cell deinterleaver. Positions not yet received after a (re)start are
/// erasures (zero channel weight), so decoding can start before the long interleaver
/// has filled.
#[derive(Debug, Clone)]
pub struct CellDeinterleaver {
    d: usize,
    table: Vec<usize>,
    mem: Vec<Vec<EqCell>>,
    cur: Vec<usize>,
}

impl CellDeinterleaver {
    /// `n_mux` cells per multiplex frame, depth `d` (1 = short, 5 = long).
    pub fn new(n_mux: usize, d: usize) -> Self {
        let d = d.max(1);
        Self {
            d,
            table: permutation(n_mux, CELL_INTERLEAVER_T0),
            mem: vec![vec![EqCell::default(); n_mux]; d],
            cur: (0..d).collect(),
        }
    }

    pub fn depth(&self) -> usize {
        self.d
    }

    /// Push one received multiplex frame; returns the deinterleaved frame whose cells
    /// are now complete (erasures where nothing was received yet).
    pub fn push(&mut self, frame: &[EqCell]) -> Option<Vec<EqCell>> {
        let n = self.table.len();
        if frame.len() != n {
            return None;
        }
        for (i, cell) in frame.iter().enumerate() {
            let b = self.cur[i % self.d];
            self.mem[b][self.table[i]] = *cell;
        }
        let out_b = self.cur[self.d - 1];
        let out = core::mem::replace(&mut self.mem[out_b], vec![EqCell::default(); n]);
        for c in &mut self.cur {
            *c = if *c == 0 { self.d - 1 } else { *c - 1 };
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::drm2::dsp::Cplx;

    #[test]
    fn long_interleaving_roundtrip() {
        // Mirror the transmitter's `CellInterleaver::push` inline (this crate only
        // ships the receiver side).
        let n = 100usize;
        let d = 5usize;
        let table = permutation(n, CELL_INTERLEAVER_T0);
        let mut tx_mem = vec![vec![Cplx::new(0.0, 0.0); n]; d];
        let mut tx_cur: Vec<usize> = (0..d).collect();
        let mut de = CellDeinterleaver::new(n, d);

        let frames: Vec<Vec<Cplx>> =
            (0..12).map(|f| (0..n).map(|i| Cplx::new(f as f64, i as f64)).collect()).collect();
        let mut outs = Vec::new();
        for f in &frames {
            let newest = tx_cur[0];
            tx_mem[newest][..n].copy_from_slice(&f[..n]);
            let tx: Vec<Cplx> = (0..n).map(|i| tx_mem[tx_cur[i % d]][table[i]]).collect();
            for c in &mut tx_cur {
                *c = if *c == 0 { d - 1 } else { *c - 1 };
            }
            let rx: Vec<EqCell> = tx.iter().map(|&s| EqCell { sig: s, chan: 1.0 }).collect();
            outs.push(de.push(&rx).unwrap());
        }
        // Total delay is d − 1 frames.
        for f in (d - 1)..frames.len() {
            let got: Vec<Cplx> = outs[f].iter().map(|c| c.sig).collect();
            assert_eq!(got, frames[f + 1 - d], "depth {d} frame {f}");
        }
    }
}
