//! Pseudo-random block interleavers (ES 201 980 §7.3.3).

/// Permutation Π of 0..`size`: `s = next_pow2(size)`, `q = s/4 − 1`, `Π(0) = 0`,
/// `Π(i) = (t₀·Π(i−1) + q) mod s`, re-applied while `Π(i) ≥ size`.
pub fn permutation(size: usize, t0: usize) -> Vec<usize> {
    if size == 0 {
        return Vec::new();
    }
    let s = size.next_power_of_two();
    let q = (s / 4).saturating_sub(1);
    let mut table = Vec::with_capacity(size);
    table.push(0);
    for i in 1..size {
        let mut v = (t0 * table[i - 1] + q) % s;
        while v >= size {
            v = (t0 * v + q) % s;
        }
        table.push(v);
    }
    table
}

/// Bit interleaver of one MLC level: part A (`n1` bits) and part B (`n2` bits) are
/// permuted independently.
#[derive(Debug, Clone)]
pub struct BitInterleaver {
    table_a: Vec<usize>,
    table_b: Vec<usize>,
}

impl BitInterleaver {
    pub fn new(n1: usize, n2: usize, t0: usize) -> Self {
        Self { table_a: permutation(n1, t0), table_b: permutation(n2, t0) }
    }

    pub fn len(&self) -> usize {
        self.table_a.len() + self.table_b.len()
    }

    /// Transmit direction: `out[i] = in[Π(i)]` within each block.
    pub fn interleave<T: Copy>(&self, data: &mut [T]) {
        let (a, b) = data.split_at_mut(self.table_a.len());
        permute_forward(a, &self.table_a);
        permute_forward(&mut b[..self.table_b.len()], &self.table_b);
    }

    /// Receive direction: `out[Π(i)] = in[i]` within each block.
    pub fn deinterleave<T: Copy>(&self, data: &mut [T]) {
        let (a, b) = data.split_at_mut(self.table_a.len());
        permute_inverse(a, &self.table_a);
        permute_inverse(&mut b[..self.table_b.len()], &self.table_b);
    }
}

fn permute_forward<T: Copy>(data: &mut [T], table: &[usize]) {
    let tmp: Vec<T> = table.iter().map(|&j| data[j]).collect();
    data.copy_from_slice(&tmp);
}

fn permute_inverse<T: Copy>(data: &mut [T], table: &[usize]) {
    let src = data.to_vec();
    for (i, &j) in table.iter().enumerate() {
        data[j] = src[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permutations_are_bijective() {
        for size in [64usize, 100, 1024, 2337] {
            for t0 in [13, 21] {
                let mut p = permutation(size, t0);
                p.sort_unstable();
                assert!(p.iter().enumerate().all(|(i, &v)| i == v), "size {size} t0 {t0}");
            }
        }
    }
}
