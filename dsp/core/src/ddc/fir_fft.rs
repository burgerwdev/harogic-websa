//! Native f64 complex FIR. The coefficient order, causal delay, and per-call
//! sample count match the four real convolutions in ComplexBandFilter.
use rustfft::{num_complex::Complex64, Fft, FftPlanner};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct ComplexFir {
    forward: Arc<dyn Fft<f64>>,
    inverse: Arc<dyn Fft<f64>>,
    response: Vec<Complex64>,
    history: Vec<Complex64>,
    work: Vec<Complex64>,
    scratch: Vec<Complex64>,
}
impl std::fmt::Debug for ComplexFir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComplexFir")
            .field("taps", &(self.history.len() + 1))
            .finish()
    }
}
impl ComplexFir {
    pub fn new(re: &[f64], im: &[f64]) -> Self {
        assert_eq!(re.len(), im.len());
        let size = (re.len() + 256).next_power_of_two();
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let mut scratch = vec![
            Complex64::default();
            forward
                .get_inplace_scratch_len()
                .max(inverse.get_inplace_scratch_len())
        ];
        let mut response = vec![Complex64::default(); size];
        for (k, value) in response.iter_mut().enumerate().take(re.len()) {
            *value = Complex64::new(re[k] / size as f64, im[k] / size as f64);
        }
        forward.process_with_scratch(&mut response, &mut scratch);
        Self {
            forward,
            inverse,
            response,
            history: vec![Complex64::default(); re.len() - 1],
            work: vec![Complex64::default(); size],
            scratch,
        }
    }
    pub fn reset(&mut self) {
        self.history.fill(Complex64::default());
    }
    pub fn process(&mut self, iq: &[f64], out: &mut Vec<f64>) {
        let overlap = self.history.len();
        let capacity = self.work.len() - overlap;
        out.clear();
        out.reserve(iq.len());
        for block in iq.chunks(2 * capacity) {
            let count = block.len() / 2;
            self.work[..overlap].copy_from_slice(&self.history);
            for (target, sample) in self.work[overlap..].iter_mut().zip(block.chunks_exact(2)) {
                *target = Complex64::new(sample[0], sample[1]);
            }
            self.work[overlap + count..].fill(Complex64::default());
            self.history
                .copy_from_slice(&self.work[count..count + overlap]);
            self.forward
                .process_with_scratch(&mut self.work, &mut self.scratch);
            for (value, response) in self.work.iter_mut().zip(&self.response) {
                *value *= response;
            }
            self.inverse
                .process_with_scratch(&mut self.work, &mut self.scratch);
            for value in &self.work[overlap..overlap + count] {
                out.extend_from_slice(&[value.re, value.im]);
            }
        }
    }
}
