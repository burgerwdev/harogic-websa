//! Levinson-Durbin recursion (the Haykin form Dream's `MatlibSigProToolbox.cpp` uses) for a
//! symmetric Toeplitz system, which is how the Wiener filters below turn correlation functions
//! into filter taps.
//!
//! It solves `R x = b` with `R[i][j] = rx[|i-j|]` (symmetric Toeplitz) and `b = rhp`, both
//! real-valued as in Dream's channel estimation; the caller applies each tap's phase.

/// Solves the Toeplitz system; `rx` must be at least as long as `rhp` and `rx[0] != 0`.
pub fn levinson(rx: &[f64], rhp: &[f64]) -> Vec<f64> {
    let n = rhp.len().min(rx.len());
    assert!(n > 0, "the recursion needs at least one tap");
    assert!(rx[0].abs() > 0.0, "the zero lag must be positive");
    let mut x = vec![0.0f64; n];
    let mut a = vec![0.0f64; n];
    let mut ap = vec![0.0f64; n];
    a[0] = 1.0;
    ap[0] = 1.0;
    x[0] = rhp[0] / rx[0];
    let mut e = rx[0];
    for j in 0..n.saturating_sub(1) {
        let next = j + 1;
        let mut gamma = rx[next];
        for i in 1..next {
            gamma += a[i] * rx[next - i];
        }
        let gamma_cap = -gamma / e;
        a[next] = gamma_cap;
        for i in 1..next {
            ap[i] = a[i] + gamma_cap * a[next - i];
        }
        for i in 1..next {
            a[i] = ap[i];
        }
        e *= 1.0 - gamma_cap * gamma_cap;
        if e.abs() < 1e-18 * rx[0].abs() {
            // The recursion has converged (a singular or noise-free system): the remaining
            // taps stay zero, which is the least-squares answer.
            break;
        }
        let mut delta = 0.0;
        for i in 0..next {
            delta += x[i] * rx[next - i];
        }
        let q = (rhp[next] - delta) / e;
        x[next] = q;
        for i in 0..next {
            x[i] += q * a[next - i];
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A diagonal system returns the right-hand side divided by the zero lag.
    #[test]
    fn solves_a_diagonal_system() {
        let rx = [2.0, 0.0, 0.0];
        let b = [1.0, -2.0, 3.0];
        let x = levinson(&rx, &b);
        for i in 0..3 {
            assert!((x[i] - b[i] / 2.0).abs() < 1e-12, "{x:?}");
        }
    }

    /// A small symmetric system whose solution is known: R = [[1, 0.5], [0.5, 1]], b = [1, 0].
    #[test]
    fn solves_a_two_by_two_system() {
        let rx = [1.0, 0.5];
        let b = [1.0, 0.0];
        let x = levinson(&rx, &b);
        // x = R^-1 b = [4/3, -2/3].
        assert!((x[0] - 4.0 / 3.0).abs() < 1e-12, "{x:?}");
        assert!((x[1] + 2.0 / 3.0).abs() < 1e-12, "{x:?}");
    }
}
