//! Stateful rational final-rate FIR for a prefiltered coarse baseband.
//! Fractional-phase taps are configured once; process() carries both sample clock
//! and filter history across IQS packet boundaries.
use std::f64::consts::PI;

// The ratio controls the clock, never the allocation size. Adjacent fractional
// delay filters are interpolated on this fixed grid.
const PHASES: usize = 256;
const MAX_TAPS: usize = 16_384;
const MAX_FILTER_BYTES: usize = 32 * 1024 * 1024;
const MAX_RATIO_TERM: u128 = (u64::MAX / 2) as u128;

pub struct Rational {
    up: u128,
    down: u128,
    taps: usize,
    phase_taps: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    pos: usize,
    input_index: u128,
    // Next output's source coordinate, in exact input-sample units.
    next_whole: u128,
    next_remainder: u128,
    avx2: bool,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Rational {
    /// Compatibility constructor for callers that already have an integer ratio.
    pub fn new(fs: f64, up: u32, down: u32, passband_hz: f64) -> Option<Self> {
        Self::with_ratio(fs, up as u128, down as u128, passband_hz).ok()
    }

    /// Preserve the exact dyadic value of the binary64 capture rate. In particular,
    /// fractional rates such as 48_828.125 are never rounded to integer hertz.
    pub fn from_rates(fs: f64, output_rate: u32, passband_hz: f64) -> Result<Self, &'static str> {
        if !fs.is_finite() || fs <= 0.0 || output_rate == 0 {
            return Err("sample rates must be finite and positive");
        }
        let bits = fs.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i32;
        let mut denominator = (bits & ((1u64 << 52) - 1)) as u128;
        let mut shift = -1074;
        if exponent != 0 {
            denominator |= 1u128 << 52;
            shift = exponent - 1023 - 52;
        }
        let mut numerator = output_rate as u128;
        let common = gcd(numerator, denominator);
        numerator /= common;
        denominator /= common;
        // Cancel powers of two before shifting, including those in the target.
        if shift < 0 {
            let cancel = denominator.trailing_zeros().min((-shift) as u32);
            denominator >>= cancel;
            shift += cancel as i32;
            numerator = numerator.checked_mul(1u128.checked_shl((-shift) as u32)
                .ok_or("exact rate ratio exceeds clock bounds")?)
                .ok_or("exact rate ratio exceeds clock bounds")?;
        } else {
            let cancel = numerator.trailing_zeros().min(shift as u32);
            numerator >>= cancel;
            shift -= cancel as i32;
            denominator = denominator.checked_mul(1u128.checked_shl(shift as u32)
                .ok_or("exact rate ratio exceeds clock bounds")?)
                .ok_or("exact rate ratio exceeds clock bounds")?;
        }
        Self::with_ratio(fs, numerator, denominator, passband_hz)
    }

    fn with_ratio(fs: f64, up: u128, down: u128, passband_hz: f64) -> Result<Self, &'static str> {
        if !(fs > 0.0 && fs.is_finite() && passband_hz > 0.0 && passband_hz.is_finite())
            || up == 0 || down == 0
        {
            return Err("sample rates and passband must be finite and positive");
        }
        let common = gcd(up, down);
        let (up, down) = (up / common, down / common);
        if up > MAX_RATIO_TERM || down > MAX_RATIO_TERM {
            return Err("exact rate ratio exceeds clock bounds");
        }
        let fs_out = fs * up as f64 / down as f64;
        let nyquist = 0.5 * fs.min(fs_out);
        if !nyquist.is_finite() || passband_hz >= nyquist {
            return Err("passband must be below both sample-rate Nyquist limits");
        }
        let transition = nyquist - passband_hz;
        let cutoff = (passband_hz + nyquist) / 2.0;
        let required = ((80.0 - 8.0) / (2.285 * 2.0 * PI * transition / fs)).ceil() + 1.0;
        if !required.is_finite() || required > MAX_TAPS as f64 {
            return Err("FIR transition exceeds configured tap bound");
        }
        let taps = (required as usize).max(96).next_multiple_of(8);
        let coefficients = (PHASES + 1) * taps;
        if (coefficients + 4 * taps) * std::mem::size_of::<f32>() > MAX_FILTER_BYTES {
            return Err("FIR allocation exceeds configured memory bound");
        }
        let denom = super::narrow::bessel0(8.6);
        let mut phase_taps = vec![0.0f32; coefficients];
        for phase in 0..=PHASES {
            // At output coordinate q, the latest input is ceil(q + delay).
            // Its lag relative to q is delay + fraction, with fraction in [0,1).
            let lag = (taps - 1) as f64 / 2.0 + phase as f64 / PHASES as f64;
            let mut sum = 0.0;
            for k in 0..taps {
                let x = k as f64 - lag;
                let sinc = if x == 0.0 {
                    2.0 * cutoff / fs
                } else {
                    (2.0 * PI * cutoff / fs * x).sin() / (PI * x)
                };
                let t = 2.0 * k as f64 / (taps - 1) as f64 - 1.0;
                let win = super::narrow::bessel0(8.6 * (1.0 - t * t).max(0.0).sqrt()) / denom;
                phase_taps[phase * taps + taps - 1 - k] = (sinc * win) as f32;
                sum += sinc * win;
            }
            for tap in &mut phase_taps[phase * taps..(phase + 1) * taps] {
                *tap /= sum as f32;
            }
        }
        let avx2 = cfg!(target_arch = "x86_64") && {
            #[cfg(target_arch = "x86_64")]
            {
                std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };
        Ok(Self {
            up, down, taps, phase_taps,
            re: vec![0.0; 2 * taps],
            im: vec![0.0; 2 * taps],
            pos: 0, input_index: 0, next_whole: 0, next_remainder: 0, avx2,
        })
    }

    #[cfg(test)]
    fn ratio(&self) -> (u128, u128) {
        (self.up, self.down)
    }

    pub fn rate_ratio(&self) -> f64 {
        self.up as f64 / self.down as f64
    }

    pub fn output_rate(&self, fs: f64) -> f64 {
        fs * self.rate_ratio()
    }

    pub fn group_delay_input_samples(&self) -> f64 {
        (self.taps - 1) as f64 / 2.0
    }

    pub fn delay_outputs(&self) -> f64 {
        self.group_delay_input_samples() * self.rate_ratio()
    }

    /// Actual causal input frame at which an output becomes available. The signal
    /// center is output_index * down / up; retain the FIR delay as separate metadata.
    pub fn causal_input_index(&self, output_index: u64) -> Result<u64, &'static str> {
        let ticks = (output_index as u128).checked_mul(self.down)
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add((self.taps - 1) as u128 * self.up))
            .ok_or("causal input coordinate exceeds clock bounds")?;
        u64::try_from(ticks.div_ceil(2 * self.up))
            .map_err(|_| "causal input coordinate exceeds u64")
    }

    /// Exact output capacity (complex frames) for the next call, including history.
    /// No outputs precede source coordinate zero; the initial FIR lookahead is
    /// represented by group_delay_input_samples(), not by shifting that origin.
    pub fn output_frames_for_input(&self, input_frames: usize) -> usize {
        let Some(last) = (self.input_index + input_frames as u128).checked_sub(1) else {
            return 0;
        };
        let Some(room) = last.checked_sub(self.next_whole) else { return 0; };
        let Some(ticks) = (2 * room).checked_sub((self.taps - 1) as u128) else { return 0; };
        let Some(ticks) = (ticks * self.up).checked_sub(2 * self.next_remainder) else { return 0; };
        usize::try_from(ticks / (2 * self.down) + 1).unwrap_or(usize::MAX)
    }

    pub fn process(&mut self, iq: &[f32], out: &mut [f32]) -> usize {
        assert!(out.len() / 2 >= self.output_frames_for_input(iq.len() / 2), "FIR output buffer too small");
        let taps = self.taps;
        let mut count = 0;
        for sample in iq.chunks_exact(2) {
            let p = self.pos;
            self.re[p] = sample[0];
            self.re[p + taps] = sample[0];
            self.im[p] = sample[1];
            self.im[p + taps] = sample[1];
            self.pos = (p + 1) % taps;
            let n = self.input_index;
            self.input_index += 1;
            loop {
                let fraction_ticks = 2 * self.next_remainder + self.up;
                let target = self.next_whole + ((taps - 1) / 2) as u128
                    + fraction_ticks.div_ceil(2 * self.up);
                if n < target { break; }
                let lag_ticks = ((2 * (n - self.next_whole) - (taps - 1) as u128) * self.up)
                    - 2 * self.next_remainder;
                let scaled = lag_ticks * PHASES as u128;
                let phase = (scaled / (2 * self.up)) as usize;
                let blend = (scaled % (2 * self.up)) as f32 / (2 * self.up) as f32;
                let a = &self.phase_taps[phase * taps..(phase + 1) * taps];
                let b = &self.phase_taps[(phase + 1) * taps..(phase + 2) * taps];
                let re = &self.re[self.pos..self.pos + taps];
                let im = &self.im[self.pos..self.pos + taps];
                let ar = super::narrow::dot(a, re, self.avx2);
                let ai = super::narrow::dot(a, im, self.avx2);
                let (br, bi) = if blend == 0.0 {
                    (ar, ai)
                } else {
                    (super::narrow::dot(b, re, self.avx2), super::narrow::dot(b, im, self.avx2))
                };
                out[2 * count] = ar + blend * (br - ar);
                out[2 * count + 1] = ai + blend * (bi - ai);
                count += 1;
                self.next_whole += self.down / self.up;
                self.next_remainder += self.down % self.up;
                if self.next_remainder >= self.up {
                    self.next_remainder -= self.up;
                    self.next_whole += 1;
                }
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_wfm_passband_and_rejects_new_nyquist_alias() {
        let fs = 31_250_000.0 / 96.0;
        fn level(fs: f64, hz: f64) -> f64 {
            let mut r = Rational::new(fs, 8, 9, 90_000.0).unwrap();
            let mut out = vec![0.0f32; 512];
            let mut power = 0.0;
            let mut samples = 0;
            for block in 0..80 {
                let mut iq = Vec::with_capacity(338);
                for k in 0..169 {
                    let ph = 2.0 * PI * hz * (block * 169 + k) as f64 / fs;
                    iq.extend_from_slice(&[ph.cos() as f32, ph.sin() as f32]);
                }
                let n = r.process(&iq, &mut out);
                if block > 2 {
                    for z in out[..2 * n].chunks_exact(2) {
                        power += (z[0] * z[0] + z[1] * z[1]) as f64;
                        samples += 1;
                    }
                }
            }
            (power / samples as f64).sqrt()
        }
        let reference = level(fs, 1000.0);
        let pass = 20.0 * (level(fs, 90_000.0) / reference).log10();
        let alias = 20.0 * (level(fs, 150_000.0) / reference).log10();
        assert!(pass > -1.0, "WFM edge loss {pass:.1} dB");
        assert!(alias < -50.0, "new Nyquist alias {alias:.1} dB");
    }

    #[test]
    fn rational_rate_and_phase_carry_across_odd_packet_boundaries() {
        let fs = 31_250_000.0 / 96.0;
        let mut r = Rational::new(fs, 8, 9, 90_000.0).unwrap();
        let mut out = vec![0.0f32; 2048];
        let mut got = Vec::new();
        for block in 0..30 {
            let mut iq = Vec::new();
            for k in 0..169 {
                let t = (block * 169 + k) as f64 / fs;
                let ph = 2.0 * PI * 1000.0 * t;
                iq.extend_from_slice(&[ph.cos() as f32, ph.sin() as f32]);
            }
            let n = r.process(&iq, &mut out);
            got.extend_from_slice(&out[..2 * n]);
        }
        assert!((got.len() as f64 / 2.0 - 30.0 * 169.0 * 8.0 / 9.0).abs() < 50.0);
        let fs_out = r.output_rate(fs);
        let values: Vec<_> = got.chunks_exact(2).skip(100).collect();
        for pair in values.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let delta = (b[1] as f64).atan2(b[0] as f64) - (a[1] as f64).atan2(a[0] as f64);
            let wrapped = (delta + PI).rem_euclid(2.0 * PI) - PI;
            assert!((wrapped - 2.0 * PI * 1000.0 / fs_out).abs() < 0.002);
        }
    }

    #[test]
    fn exact_fractional_and_upconversion_clocks_survive_packet_boundaries() {
        for (fs, passband, expected_ratio) in [
            (48_828.125, 20_000.0, (3072, 3125)),
            (6_000.0, 2_800.0, (8, 1)),
        ] {
            let mut whole = Rational::from_rates(fs, 48_000, passband).unwrap();
            let mut packets = Rational::from_rates(fs, 48_000, passband).unwrap();
            assert_eq!(whole.ratio(), expected_ratio);
            let iq: Vec<f32> = (0..4096).flat_map(|n| {
                let phase = 2.0 * PI * 1_000.0 * n as f64 / fs;
                [phase.cos() as f32, phase.sin() as f32]
            }).collect();
            let expected = whole.output_frames_for_input(iq.len() / 2);
            let mut reference = vec![0.0; 2 * expected];
            assert_eq!(whole.process(&iq, &mut reference), expected);
            let mut got = Vec::new();
            for packet in iq.chunks(2 * 17) {
                let frames = packets.output_frames_for_input(packet.len() / 2);
                let mut output = vec![0.0; 2 * frames];
                assert_eq!(packets.process(packet, &mut output), frames);
                got.extend(output);
                assert_eq!(packets.output_frames_for_input(0), 0);
            }
            assert_eq!(got, reference);
            let (up, down) = expected_ratio;
            let twice_delay = (packets.taps - 1) as u128;
            for index in [0, 1, 7, 8, expected as u64 - 1] {
                let exact = (2 * index as u128 * down + twice_delay * up).div_ceil(2 * up);
                assert_eq!(packets.causal_input_index(index).unwrap() as u128, exact);
            }
            assert!(packets.causal_input_index(expected as u64 - 1).unwrap() < 4096);
            assert!(packets.causal_input_index(expected as u64).unwrap() >= 4096);
        }
    }

    #[test]
    fn drmplus_preserves_occupied_edge_and_rejects_folded_alias() {
        fn level(hz: f64) -> f64 {
            let mut r = Rational::from_rates(125_000.0, 96_000, 47_200.0).unwrap();
            assert_eq!(r.ratio(), (96, 125));
            let iq: Vec<f32> = (0..12_000).flat_map(|n| {
                let phase = 2.0 * PI * hz * n as f64 / 125_000.0;
                [phase.cos() as f32, phase.sin() as f32]
            }).collect();
            let mut output = vec![0.0; 2 * r.output_frames_for_input(12_000)];
            let frames = r.process(&iq, &mut output);
            let power: f64 = output[2 * 1_000..2 * frames].chunks_exact(2)
                .map(|z| (z[0] as f64).powi(2) + (z[1] as f64).powi(2)).sum();
            (power / (frames - 1_000) as f64).sqrt()
        }
        let reference = level(1_000.0);
        let edge = 20.0 * (level(47_111.0) / reference).log10();
        let alias = 20.0 * (level(49_000.0) / reference).log10();
        assert!(edge > -0.2, "DRMplus occupied-edge loss {edge:.2} dB");
        assert!(alias < -60.0, "DRMplus folded alias {alias:.2} dB");
    }
}
