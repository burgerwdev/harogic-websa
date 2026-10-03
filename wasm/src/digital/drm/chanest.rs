//! Channel estimation and equalisation (simplified first implementation).
//!
//! Per OFDM symbol, the gain-reference (scattered) pilots are divided by their known
//! transmitted value to obtain the channel `H` at the pilot carriers, then `H` is
//! linearly interpolated across the carrier axis and every data cell is divided by
//! it. This is exact for the clean synthesised fixture and correct for flat/slowly
//! varying channels; the full Wiener (time+frequency) estimator Dream/DecDRM use is a
//! later refinement, not required for the feasibility proof.

use crate::digital::drm::cellmap::CellMap;
use crate::digital::drm::Cplx;

/// One equalised OFDM symbol.
pub struct EqSymbol {
    /// Equalised cells, indexed by carrier offset `c = k - kmin`.
    pub cells: Vec<Cplx>,
    /// Channel estimate per carrier offset.
    pub chan: Vec<Cplx>,
}

/// Equalise one symbol's cells using its gain-reference pilots.
pub fn equalize_symbol(map: &CellMap, sym: usize, cells: &[Cplx]) -> EqSymbol {
    let n = map.num_carriers;
    // Gather the channel at every gain-reference (scattered) pilot.
    let mut pilot_c: Vec<usize> = Vec::new();
    let mut pilot_h: Vec<Cplx> = Vec::new();
    for c in 0..n {
        let ty = map.cell(sym, c);
        if ty.is_scattered() && !ty.is_dc() {
            let r = map.pilot(sym, c);
            if r.norm_sqr() > 0.0 {
                pilot_c.push(c);
                pilot_h.push(cells[c] / r);
            }
        }
    }

    // Linearly interpolate H across the carrier axis.
    let mut chan = vec![Cplx::new(0.0, 0.0); n];
    for c in 0..n {
        chan[c] = interpolate(&pilot_c, &pilot_h, c);
    }

    // Smooth the channel estimate across carriers. A real channel is smooth in frequency,
    // so this reduces the pilot noise without changing the channel shape. The end carriers
    // keep their raw estimate because the filter needs neighbours on both sides.
    let mut smooth = chan.clone();
    for c in 1..n - 1 {
        let lo = chan[c - 1];
        let mid = chan[c];
        let hi = chan[c + 1];
        smooth[c] = Cplx::new(
            (lo.re + mid.re * 2.0 + hi.re) / 4.0,
            (lo.im + mid.im * 2.0 + hi.im) / 4.0,
        );
    }

    // Equalise.
    let mut eq = vec![Cplx::new(0.0, 0.0); n];
    for c in 0..n {
        if map.cell(sym, c).is_dc() {
            eq[c] = Cplx::new(0.0, 0.0);
        } else if smooth[c].norm_sqr() > 0.0 {
            eq[c] = cells[c] / smooth[c];
        }
    }
    EqSymbol { cells: eq, chan: smooth }
}

/// Complex linear interpolation of H at carrier `c` from the sorted pilot positions.
fn interpolate(pc: &[usize], ph: &[Cplx], c: usize) -> Cplx {
    if pc.is_empty() {
        return Cplx::new(0.0, 0.0);
    }
    if c <= pc[0] {
        return ph[0];
    }
    if c >= *pc.last().unwrap() {
        return *ph.last().unwrap();
    }
    // Find the bracketing pilots.
    let mut hi = 0usize;
    while hi < pc.len() && pc[hi] < c {
        hi += 1;
    }
    let lo = hi - 1;
    let span = (pc[hi] - pc[lo]) as f64;
    let t = (c - pc[lo]) as f64 / span;
    Cplx::new(
        ph[lo].re + (ph[hi].re - ph[lo].re) * t,
        ph[lo].im + (ph[hi].im - ph[lo].im) * t,
    )
}
