//! Streaming linear-interpolation resampler (`LinearResampler` in `web_sa/demod/filters.py`).
//!
//! The resampler is the last rate change in the chain, so its phase must survive the block
//! boundaries: the fractional position `t` and the last input sample are the whole state, and
//! they are advanced exactly as the Python reference does.
//!
//! `ComplexResampler` holds two of them behind one rate pair, so I and Q cannot be configured
//! differently (which is the one mistake that would rotate the constellation).

#[derive(Debug, Clone)]
pub struct LinearResampler {
    step: f64,
    t: f64,
    prev: Option<f64>,
}

impl LinearResampler {
    pub fn new(fs_in: f64, fs_out: f64) -> Self {
        Self { step: fs_in / fs_out, t: 0.0, prev: None }
    }

    /// Drop the interpolation history (a retune resumes with a clean phase).
    pub fn reset(&mut self) {
        self.t = 0.0;
        self.prev = None;
    }

    pub fn step(&self) -> f64 {
        self.step
    }

    pub fn output_length_hint(&self, input_len: usize) -> usize {
        let n = (self.prev.is_some() as usize) + input_len;
        if n < 2 || !(self.step > 0.0) {
            return 0;
        }
        let count = ((n as f64 - 1.0 - self.t) / self.step).floor() + 1.0;
        if count > 0.0 {
            count as usize
        } else {
            0
        }
    }

    /// Resample one block, appending to `out` (which is cleared first).
    pub fn process_into(&mut self, x: &[f64], out: &mut Vec<f32>) {
        out.clear();
        if x.is_empty() {
            return;
        }
        // The reference prepends the previous sample, which is what makes the output continuous.
        let mut buf: Vec<f64> = Vec::with_capacity(x.len() + 1);
        if let Some(prev) = self.prev {
            buf.push(prev);
        }
        buf.extend_from_slice(x);
        let n = buf.len();
        if n >= 2 {
            let count = (((n as f64 - 1.0 - self.t) / self.step).floor() + 1.0).max(0.0) as usize;
            out.reserve(count);
            for k in 0..count {
                let pos = self.t + k as f64 * self.step;
                let want = pos.trunc() as i64;
                let limit = n as i64 - 2;
                // trunc() toward zero matches numpy's astype(int64) for the non-negative
                // positions this produces; the clamp mirrors `np.minimum(pos, len-2)`.
                let i0 = if want > limit { limit } else { want };
                let i0 = if i0 < 0 { 0 } else { i0 } as usize;
                let i1 = (i0 + 1).min(n - 1);
                let frac = (pos - i0 as f64).clamp(0.0, 1.0);
                out.push((buf[i0] * (1.0 - frac) + buf[i1] * frac) as f32);
            }
            if count > 0 {
                self.t = self.t + (count - 1) as f64 * self.step + self.step;
            }
        }
        self.t -= n as f64 - 1.0;
        self.prev = Some(buf[n - 1]);
    }
}

/// I/Q resampler: two linear interpolators sharing one rate pair and one input clock.
#[derive(Debug, Clone)]
pub struct ComplexResampler {
    i: LinearResampler,
    q: LinearResampler,
    scratch_i: Vec<f64>,
    scratch_q: Vec<f64>,
    out_i: Vec<f32>,
    out_q: Vec<f32>,
}

impl ComplexResampler {
    pub fn new(fs_in: f64, fs_out: f64) -> Self {
        Self {
            i: LinearResampler::new(fs_in, fs_out),
            q: LinearResampler::new(fs_in, fs_out),
            scratch_i: Vec::new(),
            scratch_q: Vec::new(),
            out_i: Vec::new(),
            out_q: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.i.reset();
        self.q.reset();
    }

    /// Resample interleaved complex `iq` (f64) into interleaved complex `out` (f32).
    pub fn process_into(&mut self, iq: &[f64], out: &mut Vec<f32>) {
        let n = iq.len() / 2;
        self.scratch_i.clear();
        self.scratch_q.clear();
        self.scratch_i.extend((0..n).map(|k| iq[2 * k]));
        self.scratch_q.extend((0..n).map(|k| iq[2 * k + 1]));
        self.resample_scratch(out);
    }

    /// The same for a baseband that already arrives as f32 (the backend's DDC output).
    pub fn process_f32_into(&mut self, iq: &[f32], out: &mut Vec<f32>) {
        let n = iq.len() / 2;
        self.scratch_i.clear();
        self.scratch_q.clear();
        self.scratch_i.extend((0..n).map(|k| iq[2 * k] as f64));
        self.scratch_q.extend((0..n).map(|k| iq[2 * k + 1] as f64));
        self.resample_scratch(out);
    }

    fn resample_scratch(&mut self, out: &mut Vec<f32>) {
        self.i.process_into(&self.scratch_i, &mut self.out_i);
        self.q.process_into(&self.scratch_q, &mut self.out_q);
        out.clear();
        out.reserve(self.out_i.len() * 2);
        for k in 0..self.out_i.len() {
            out.push(self.out_i[k]);
            out.push(self.out_q.get(k).copied().unwrap_or(0.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimating_by_two_halves_the_rate_and_keeps_dc() {
        let x: Vec<f64> = vec![1.0; 32];
        let mut r = LinearResampler::new(48_000.0, 24_000.0);
        let mut out = Vec::new();
        r.process_into(&x, &mut out);
        assert!(out.len() >= 15 && out.len() <= 17, "len {}", out.len());
        for value in &out {
            assert!((value - 1.0).abs() < 1e-6, "DC must survive: {value}");
        }
    }

    #[test]
    fn blocks_are_continuous_across_the_boundary() {
        // A slow ramp resampled in one piece and in two must agree.
        let x: Vec<f64> = (0..64).map(|k| k as f64 * 0.01).collect();
        let mut whole = LinearResampler::new(10_000.0, 7_000.0);
        let mut one = Vec::new();
        whole.process_into(&x, &mut one);

        let mut split = LinearResampler::new(10_000.0, 7_000.0);
        let mut a = Vec::new();
        let mut b = Vec::new();
        split.process_into(&x[..32], &mut a);
        split.process_into(&x[32..], &mut b);
        a.extend_from_slice(&b);
        assert_eq!(a.len(), one.len());
        for (i, (u, v)) in one.iter().zip(a.iter()).enumerate() {
            assert!((u - v).abs() < 1e-5, "sample {i}: {u} vs {v}");
        }
    }

    #[test]
    fn an_empty_block_does_not_advance_the_phase() {
        let mut r = LinearResampler::new(48_000.0, 48_000.0);
        let mut out = Vec::new();
        r.process_into(&[], &mut out);
        assert!(out.is_empty());
        r.process_into(&[0.5, 0.5, 0.5], &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn the_complex_resampler_keeps_i_and_q_in_step() {
        let n = 64;
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            iq.push(k as f64);
            iq.push(-(k as f64));
        }
        let mut r = ComplexResampler::new(48_000.0, 24_000.0);
        let mut out = Vec::new();
        r.process_into(&iq, &mut out);
        for pair in out.chunks(2) {
            assert!((pair[0] + pair[1]).abs() < 1e-4, "I and Q must stay mirrored");
        }
    }

    #[test]
    fn the_length_hint_matches_the_produced_length() {
        let x: Vec<f64> = (0..100).map(|k| k as f64).collect();
        let mut r = LinearResampler::new(48_000.0, 32_000.0);
        let hint = r.output_length_hint(x.len());
        let mut out = Vec::new();
        r.process_into(&x, &mut out);
        assert_eq!(hint, out.len());
    }
}
