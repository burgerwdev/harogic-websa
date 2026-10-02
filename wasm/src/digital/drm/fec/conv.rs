//! Punctured rate-1/4 convolutional code (ES 201 980 §7.3.1).

use crate::digital::drm::tables::{CONSTRAINT_LENGTH, GENERATORS, PunctureMask};

/// Parity of the tap-masked 7-bit register for mother-code output `j`.
#[inline]
pub fn mother_output(reg: u8, j: usize) -> u8 {
    ((reg & GENERATORS[j]).count_ones() & 1) as u8
}

/// Encode `bits` (0/1) followed by six zero tail bits, keeping the outputs selected by
/// `masks` (one mask per input bit incl. tail). Outputs of one input bit are emitted
/// in order b₀, b₁, b₂, b₃.
pub fn encode(bits: &[u8], masks: &[PunctureMask], out: &mut Vec<u8>) {
    debug_assert_eq!(masks.len(), bits.len() + CONSTRAINT_LENGTH - 1);
    out.clear();
    let mut reg: u8 = 0;
    for (i, &mask) in masks.iter().enumerate() {
        let input = bits.get(i).copied().unwrap_or(0) & 1;
        reg = ((reg << 1) | input) & 0x7F;
        for j in 0..4 {
            if mask & (1 << j) != 0 {
                out.push(mother_output(reg, j));
            }
        }
    }
}
