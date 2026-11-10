//! Causal overlap-save for long, non-decimating FIRs. Each call emits exactly
//! its input length: partial blocks use zero padding, never wait for future IQ.
use rustfft::{num_complex::Complex64, Fft, FftPlanner};
use std::sync::Arc;

pub(crate) struct FirFft {
    forward: Arc<dyn Fft<f64>>,
    inverse: Arc<dyn Fft<f64>>,
    response: Vec<Complex64>,
    history: Vec<Complex64>,
    work: Vec<Complex64>,
    scratch: Vec<Complex64>,
}

impl FirFft {
    pub(crate) fn new(taps: &[f32]) -> Self {
        // Leave enough valid outputs even when the filter length lies just
        // below a power of two. All storage/planning is outside the IQ path.
        let size = (taps.len() + 256).next_power_of_two();
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let mut scratch = vec![Complex64::default();
            forward.get_inplace_scratch_len().max(inverse.get_inplace_scratch_len())];
        let mut response = vec![Complex64::default(); size];
        // The direct kernel dots coefficients against oldest-to-newest IQ.
        // Reverse them for a causal convolution, including asymmetric filters.
        for (target, &coefficient) in response.iter_mut().zip(taps.iter().rev()) {
            target.re = coefficient as f64 / size as f64;
        }
        forward.process_with_scratch(&mut response, &mut scratch);
        Self { forward, inverse, response, history:vec![Complex64::default(); taps.len() - 1],
            work:vec![Complex64::default(); size], scratch }
    }

    pub(crate) fn process(&mut self, iq: &mut [f32]) {
        assert_eq!(iq.len() % 2, 0);
        let overlap = self.history.len();
        let capacity = self.work.len() - overlap;
        for block in iq.chunks_mut(2 * capacity) {
            let count = block.len() / 2;
            self.work[..overlap].copy_from_slice(&self.history);
            for (target, sample) in self.work[overlap..].iter_mut().zip(block.chunks_exact(2)) {
                *target = Complex64::new(sample[0] as f64, sample[1] as f64);
            }
            self.work[overlap + count..].fill(Complex64::default());
            // Save raw samples before the transform overwrites the workspace.
            self.history.copy_from_slice(&self.work[count..count + overlap]);
            self.forward.process_with_scratch(&mut self.work, &mut self.scratch);
            for (value, response) in self.work.iter_mut().zip(&self.response) { *value *= response; }
            self.inverse.process_with_scratch(&mut self.work, &mut self.scratch);
            for (target, value) in block.chunks_exact_mut(2).zip(&self.work[overlap..]) {
                target[0] = value.re as f32;
                target[1] = value.im as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_calls_match_causal_f64_reference_without_buffered_output() {
        // Asymmetric coefficients catch history direction and off-by-one errors.
        let taps: Vec<f32> = (0..1048).map(|n| ((n as f64 * 0.17).cos() / 1048.0) as f32).collect();
        let input: Vec<f32> = (0..12_000).map(|n| ((n as f64 * 0.013).sin() +
            (n as f64 * 0.711).cos()) as f32).collect();
        let mut expected = vec![0.0; input.len()];
        for n in 0..input.len() / 2 {
            for component in 0..2 {
                expected[2 * n + component] = (0..taps.len().min(n + 1)).map(|k|
                    taps[taps.len() - 1 - k] as f64 * input[2 * (n - k) + component] as f64
                ).sum::<f64>() as f32;
            }
        }
        for sizes in [&[6000][..], &[0, 1, 7, 169, 170, 509, 3, 2048][..]] {
            let mut filter = FirFft::new(&taps);
            let mut actual = input.clone();
            let (mut at, mut turn) = (0, 0);
            while at < actual.len() {
                let end = (at + sizes[turn % sizes.len()] * 2).min(actual.len());
                filter.process(&mut actual[at..end]);
                at = end;
                turn += 1;
            }
            let error = actual.iter().zip(&expected).map(|(a,b)| (a-b).abs()).fold(0.0f32, f32::max);
            assert!(error < 2e-7, "maximum absolute error {error}");
            // A zero tail must also use the retained last input, not padded IQ.
            let mut tail = vec![0.0f32; 2 * taps.len()];
            filter.process(&mut tail);
            assert!(tail[tail.len()-2..].iter().all(|x| x.abs() < 1e-12));
        }
    }
}
