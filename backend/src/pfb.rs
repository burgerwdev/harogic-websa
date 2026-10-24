//! Shared PFB coarse channelizer for streaming IQ.
//! Bin spacing fs/M; each bin emits fs/hop complex samples/s. Multiple fine
//! channels may reuse one bin; no per-subscriber pass over the full input.
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::f32::consts::PI;
use std::sync::Arc;
#[cfg(feature = "bench-instrument")]
use std::time::Instant;

#[cfg(feature = "bench-instrument")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PfbStageTimes {
    pub fir_ns: u64,
    pub fft_ns: u64,
    pub output_ns: u64,
    pub groups: u64,
}

#[derive(Clone)]
pub struct Pfb {
    fs: f64,
    pub(crate) m: usize,
    pub(crate) hop: usize,
    oversample: usize,
    pub(crate) taps_per_phase: usize,
    taps: Vec<f32>,
    tap_phase: Vec<f32>,
    tap_group: Vec<f32>,
    // Kernel 0: branch × duplicated taps; kernel 1: duplicated tap rows × reversed branches.
    // Select the kernel before ingesting samples or rebuilding history.
    history_re: Vec<f32>,
    history_im: Vec<f32>,
    history_pos: Vec<usize>,
    pub(crate) avx2: bool,
    phase: usize,
    input_samples: u64,
    output_groups: u64,
    fft: Arc<dyn Fft<f32>>,
    fft_buffer: Vec<Complex32>,
    scratch: Vec<Complex32>,
    phase_cycle: Vec<Complex32>,
    pub(crate) fir_kernel: u8,
    #[cfg(feature = "bench-instrument")]
    profile: PfbStageTimes,
}

impl Pfb {
    pub(crate) fn new(fs: f64, oversample: usize) -> Option<Self> {
        Self::with_taps(fs, oversample, if oversample == 4 { 4 } else { 16 })
    }

    pub(crate) fn with_taps(fs: f64, oversample: usize, taps_per_phase: usize) -> Option<Self> {
        let quantum = super::narrow::channel_quantum(fs) as usize;
        if quantum == 0 || !matches!(oversample, 1 | 2 | 4)
            || !matches!(taps_per_phase, 4 | 8 | 16)
        {
            return None;
        }
        let m = oversample.max(2) * quantum;
        let hop = m / oversample;
        let len = taps_per_phase * m;
        let fft = FftPlanner::<f32>::new().plan_fft_inverse(m);
        let scratch_len = fft.get_inplace_scratch_len();
        let phase_cycle = (0..oversample)
            .flat_map(|group| {
                (0..m).map(move |k| {
                    let t = hop - 1 + group * hop;
                    let angle = -2.0 * PI * k as f32 * t as f32 / m as f32;
                    Complex32::new(angle.cos(), angle.sin())
                })
            })
            .collect();
        let taps = super::narrow::design(len, 0.5 / hop as f64);
        let mut tap_phase = vec![0.0f32; len];
        for r in 0..m {
            for j in 0..taps_per_phase {
                tap_phase[r * taps_per_phase + j] = taps[r + (taps_per_phase - 1 - j) * m];
            }
        }
        let mut tap_group = vec![0.0f32; len];
        if hop >= 8 {
            for r in 0..m {
                for j in 0..taps_per_phase {
                    tap_group[(r / 8 * taps_per_phase + j) * 8 + r % 8] =
                        tap_phase[r * taps_per_phase + j];
                }
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
        Some(Self {
            fs,
            m,
            hop,
            oversample,
            taps_per_phase,
            taps,
            tap_phase,
            tap_group,
            history_re: vec![0.0; m * 2 * taps_per_phase],
            history_im: vec![0.0; m * 2 * taps_per_phase],
            history_pos: vec![0; m],
            avx2,
            phase: 0,
            input_samples: 0,
            output_groups: 0,
            fft,
            fft_buffer: vec![Complex32::new(0.0, 0.0); m],
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
            phase_cycle,
            fir_kernel: 0,
            #[cfg(feature = "bench-instrument")]
            profile: PfbStageTimes::default(),
        })
    }

    fn grouped_history(&self) -> bool {
        self.fir_kernel == 1 && self.avx2 && self.hop >= 8
    }

    fn fir_bank_baseline(&mut self, tmod: usize) {
        for r in 0..self.m {
            let branch = (tmod + self.m - r) & (self.m - 1);
            let start = branch * 2 * self.taps_per_phase + self.history_pos[branch];
            let h = &self.tap_phase[r * self.taps_per_phase..(r + 1) * self.taps_per_phase];
            let real = &self.history_re[start..start + self.taps_per_phase];
            let imag = &self.history_im[start..start + self.taps_per_phase];
            let (re, im) = if self.taps_per_phase == 4 {
                (
                    h[0] * real[0] + h[1] * real[1] + h[2] * real[2] + h[3] * real[3],
                    h[0] * imag[0] + h[1] * imag[1] + h[2] * imag[2] + h[3] * imag[3],
                )
            } else {
                (
                    super::narrow::dot(h, real, self.avx2),
                    super::narrow::dot(h, imag, self.avx2),
                )
            };
            self.fft_buffer[r] = Complex32::new(re, im);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn fir_bank_avx2(&mut self, tmod: usize) {
        match self.taps_per_phase {
            4 => self.fir_bank_avx2_taps::<4>(tmod),
            8 => self.fir_bank_avx2_taps::<8>(tmod),
            16 => self.fir_bank_avx2_taps::<16>(tmod),
            _ => unreachable!("unsupported FIR tap count"),
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn fir_bank_avx2_taps<const TAPS: usize>(&mut self, tmod: usize) {
        use std::arch::x86_64::*;
        debug_assert!(self.hop >= 8);
        let taps = TAPS;
        let row = ((self.input_samples - 1) >> self.m.trailing_zeros()) as usize;
        for r in (0..self.m).step_by(8) {
            let branch = (tmod + self.m - r) & (self.m - 1);
            // Rows hold descending branches; hop boundaries align to eight lanes.
            let oldest = (row + usize::from(r <= tmod)) & (taps - 1);
            let start = oldest * self.m + self.m - 1 - branch;
            let h = self.tap_group.as_ptr().add(r * taps);
            let real = self.history_re.as_ptr().add(start);
            let imag = self.history_im.as_ptr().add(start);
            let mut re = _mm256_setzero_ps();
            let mut im = _mm256_setzero_ps();
            // Keep the reference's (j, j + 8) FMA and left-to-right lane sum.
            for j in 0..taps.min(8) {
                let coeff = _mm256_loadu_ps(h.add(j * 8));
                let mut re_term = _mm256_mul_ps(coeff, _mm256_loadu_ps(real.add(j * self.m)));
                let mut im_term = _mm256_mul_ps(coeff, _mm256_loadu_ps(imag.add(j * self.m)));
                if taps == 16 {
                    let coeff = _mm256_loadu_ps(h.add((j + 8) * 8));
                    re_term = _mm256_fmadd_ps(
                        coeff, _mm256_loadu_ps(real.add((j + 8) * self.m)), re_term,
                    );
                    im_term = _mm256_fmadd_ps(
                        coeff, _mm256_loadu_ps(imag.add((j + 8) * self.m)), im_term,
                    );
                }
                re = _mm256_add_ps(re, re_term);
                im = _mm256_add_ps(im, im_term);
            }
            let lo = _mm256_unpacklo_ps(re, im);
            let hi = _mm256_unpackhi_ps(re, im);
            let out = self.fft_buffer.as_mut_ptr().add(r).cast::<f32>();
            _mm256_storeu_ps(out, _mm256_permute2f128_ps::<0x20>(lo, hi));
            _mm256_storeu_ps(out.add(8), _mm256_permute2f128_ps::<0x31>(lo, hi));
        }
    }

    pub(crate) fn history_len(&self) -> usize {
        self.taps.len()
    }

    /// Rebuild one worker's causal FIR history from exactly L preceding IQ samples.
    /// `input_index` is the absolute sample index of the next packet; L is a multiple of M,
    /// so the oldest tail sample has the same modulo-M branch as `input_index`.
    pub(crate) fn load_history(&mut self, input_index: u64, tail: &[i16]) -> bool {
        let len = self.taps.len();
        if tail.len() != 2 * len {
            return false;
        }
        let phase_len = self.taps_per_phase;
        let mask = self.m - 1;
        self.history_pos.fill(0);
        if self.grouped_history() {
            let mut j = 0;
            while j < len {
                let index = input_index.wrapping_sub(len as u64).wrapping_add(j as u64);
                let count = (self.m - (index as usize & mask)).min(len - j);
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    self.write_history_avx2(index, &tail[2 * j..2 * (j + count)]);
                }
                j += count;
            }
        } else {
            for (j, sample) in tail.chunks_exact(2).enumerate() {
                let branch = (input_index as usize + j) & mask;
                let base = branch * 2 * phase_len + j / self.m;
                self.history_re[base] = sample[0] as f32;
                self.history_re[base + phase_len] = sample[0] as f32;
                self.history_im[base] = sample[1] as f32;
                self.history_im[base + phase_len] = sample[1] as f32;
            }
        }
        self.input_samples = input_index;
        self.output_groups = input_index / self.hop as u64;
        self.phase = input_index as usize & (self.hop - 1);
        true
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn write_history_avx2(&mut self, input_index: u64, iq: &[i16]) {
        use std::arch::x86_64::*;
        let branch = input_index as usize & (self.m - 1);
        let row = (input_index >> self.m.trailing_zeros()) as usize & (self.taps_per_phase - 1);
        let len = self.taps.len();
        let end = row * self.m + self.m - branch;
        let samples = iq.len() / 2;
        debug_assert!(samples <= self.m - branch);
        let reverse = _mm256_setr_epi32(7, 6, 5, 4, 3, 2, 1, 0);
        let mut j = 0;
        while j + 8 <= samples {
            let packed = _mm256_loadu_si256(iq.as_ptr().add(2 * j).cast::<__m256i>());
            let real = _mm256_cvtepi32_ps(_mm256_srai_epi32::<16>(_mm256_slli_epi32::<16>(packed)));
            let imag = _mm256_cvtepi32_ps(_mm256_srai_epi32::<16>(packed));
            let real = _mm256_permutevar8x32_ps(real, reverse);
            let imag = _mm256_permutevar8x32_ps(imag, reverse);
            let base = end - j - 8;
            _mm256_storeu_ps(self.history_re.as_mut_ptr().add(base), real);
            _mm256_storeu_ps(self.history_re.as_mut_ptr().add(base + len), real);
            _mm256_storeu_ps(self.history_im.as_mut_ptr().add(base), imag);
            _mm256_storeu_ps(self.history_im.as_mut_ptr().add(base + len), imag);
            j += 8;
        }
        while j < samples {
            let base = end - j - 1;
            self.history_re[base] = iq[2 * j] as f32;
            self.history_re[base + len] = iq[2 * j] as f32;
            self.history_im[base] = iq[2 * j + 1] as f32;
            self.history_im[base + len] = iq[2 * j + 1] as f32;
            j += 1;
        }
    }

    pub(crate) fn process(&mut self, iq: &[i16], bins: &[u32], out: &mut [f32]) -> usize {
        let mut groups = 0;
        let m_mask = self.m - 1;
        let tap_mask = self.taps_per_phase - 1;
        let grouped = self.grouped_history();
        let mut consumed = 0;
        let samples = iq.len() / 2;
        while consumed < samples {
            let count = (self.hop - self.phase).min(samples - consumed);
            let chunk = &iq[2 * consumed..2 * (consumed + count)];
            if grouped {
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    self.write_history_avx2(self.input_samples, chunk);
                }
            } else {
                for (j, sample) in chunk.chunks_exact(2).enumerate() {
                    let branch = (self.input_samples as usize + j) & m_mask;
                    let pos = self.history_pos[branch];
                    let base = branch * 2 * self.taps_per_phase;
                    self.history_re[base + pos] = sample[0] as f32;
                    self.history_re[base + pos + self.taps_per_phase] = sample[0] as f32;
                    self.history_im[base + pos] = sample[1] as f32;
                    self.history_im[base + pos + self.taps_per_phase] = sample[1] as f32;
                    self.history_pos[branch] = (pos + 1) & tap_mask;
                }
            }
            consumed += count;
            self.input_samples += count as u64;
            self.phase += count;
            if self.phase < self.hop {
                continue;
            }
            self.phase = 0;
            if !bins.is_empty() {
                #[cfg(feature = "bench-instrument")]
                let fir_at = Instant::now();
                let tmod = (self.input_samples - 1) as usize & m_mask;
                if grouped {
                    #[cfg(target_arch = "x86_64")]
                    unsafe {
                        self.fir_bank_avx2(tmod);
                    }
                } else {
                    self.fir_bank_baseline(tmod);
                }
                #[cfg(feature = "bench-instrument")]
                let fft_at = Instant::now();
                self.fft
                    .process_with_scratch(&mut self.fft_buffer, &mut self.scratch);
                #[cfg(feature = "bench-instrument")]
                let output_at = Instant::now();
                for (col, &bin) in bins.iter().enumerate() {
                    let k = bin as usize;
                    let ph = self.phase_cycle
                        [(self.output_groups as usize & (self.oversample - 1)) * self.m + k];
                    let z = self.fft_buffer[k] * ph;
                    let pos = 2 * (groups * bins.len() + col);
                    out[pos] = z.re;
                    out[pos + 1] = z.im;
                }
                #[cfg(feature = "bench-instrument")]
                {
                    let done = Instant::now();
                    self.profile.fir_ns += (fft_at - fir_at).as_nanos() as u64;
                    self.profile.fft_ns += (output_at - fft_at).as_nanos() as u64;
                    self.profile.output_ns += (done - output_at).as_nanos() as u64;
                    self.profile.groups += 1;
                }
            }
            groups += 1;
            self.output_groups += 1;
        }
        groups
    }
}

#[no_mangle]
pub extern "C" fn pfb_open(fs: f64) -> *mut Pfb {
    pfb_open_os(fs, 2)
}

#[no_mangle]
pub extern "C" fn pfb_open_os(fs: f64, oversample: u32) -> *mut Pfb {
    Pfb::new(fs, oversample as usize).map_or(std::ptr::null_mut(), |p| Box::into_raw(Box::new(p)))
}

#[no_mangle]
pub extern "C" fn pfb_open_kernel(fs: f64, oversample: u32, kernel: u32) -> *mut Pfb {
    if kernel == 0 {
        return pfb_open_os(fs, oversample);
    }
    if kernel != 1 {
        return std::ptr::null_mut();
    }
    let Some(mut p) = Pfb::new(fs, oversample as usize) else {
        return std::ptr::null_mut();
    };
    if !p.avx2 || !matches!(p.taps_per_phase, 4 | 8 | 16) {
        return std::ptr::null_mut();
    }
    p.fir_kernel = 1;
    Box::into_raw(Box::new(p))
}

#[no_mangle]
pub extern "C" fn pfb_open_config(
    fs: f64,
    oversample: u32,
    taps_per_phase: u32,
    kernel: u32,
) -> *mut Pfb {
    let Some(mut p) = Pfb::with_taps(fs, oversample as usize, taps_per_phase as usize) else {
        return std::ptr::null_mut();
    };
    if kernel > 1 || (kernel == 1 && !p.avx2) {
        return std::ptr::null_mut();
    }
    p.fir_kernel = kernel as u8;
    Box::into_raw(Box::new(p))
}

#[no_mangle]
pub unsafe extern "C" fn pfb_clone(ptr: *const Pfb) -> *mut Pfb {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new((*ptr).clone()))
}

#[no_mangle]
pub unsafe extern "C" fn pfb_close(ptr: *mut Pfb) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

/// Returns bin count and hop; caller owns the handle. Each bin may have many fine subscribers.
#[no_mangle]
pub unsafe extern "C" fn pfb_geometry(
    ptr: *const Pfb,
    bins: *mut u32,
    hop: *mut u32,
    output_rate: *mut f64,
    delay: *mut f64,
) -> i32 {
    if ptr.is_null() || bins.is_null() || hop.is_null() || output_rate.is_null() || delay.is_null()
    {
        return -1;
    }
    let p = &*ptr;
    *bins = p.m as u32;
    *hop = p.hop as u32;
    *output_rate = p.fs / p.hop as f64;
    *delay = (p.taps.len() - 1) as f64 / (2.0 * p.hop as f64);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pfb_diagnostics(
    ptr: *const Pfb,
    taps: *mut u32,
    per_phase: *mut u32,
    simd: *mut u32,
) -> i32 {
    if ptr.is_null() || taps.is_null() || per_phase.is_null() || simd.is_null() {
        return -1;
    }
    let p = &*ptr;
    *taps = p.taps.len() as u32;
    *per_phase = p.taps_per_phase as u32;
    // RustFFT's FftPlanner tries the AVX planner first on x86; these are the runtime gates.
    #[cfg(target_arch = "x86_64")]
    let fft_avx = rustfft::FftPlannerAvx::<f32>::new().is_ok();
    #[cfg(not(target_arch = "x86_64"))]
    let fft_avx = false;
    *simd = u32::from(p.grouped_history() || (p.avx2 && p.taps_per_phase % 8 == 0))
        | (u32::from(fft_avx) << 1);
    0
}

#[cfg(feature = "bench-instrument")]
#[no_mangle]
pub unsafe extern "C" fn pfb_take_profile(ptr: *mut Pfb, out: *mut PfbStageTimes) -> i32 {
    if ptr.is_null() || out.is_null() {
        return -1;
    }
    *out = std::mem::take(&mut (*ptr).profile);
    0
}

/// Bin-major columns within each output time group. out_cap counts complex values.
#[no_mangle]
pub unsafe extern "C" fn pfb_process(
    ptr: *mut Pfb,
    input: *const i16,
    n: i32,
    bins: *const u32,
    bin_count: i32,
    out: *mut f32,
    out_cap: i32,
) -> i32 {
    if ptr.is_null()
        || input.is_null()
        || n < 0
        || n > 65536
        || bin_count < 0
        || out_cap < 0
        || (bin_count > 0 && (bins.is_null() || out.is_null()))
    {
        return -1;
    }
    let p = &mut *ptr;
    if bin_count as usize > p.m {
        return -1;
    } // unique coarse bins; fan-out occurs after PFB
    let selected = if bin_count == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(bins, bin_count as usize)
    };
    if selected.iter().any(|&k| k as usize >= p.m) {
        return -1;
    }
    let max_groups = (p.phase + n as usize) / p.hop;
    if !selected.is_empty() && out_cap as usize / selected.len() < max_groups {
        return -1;
    }
    let iq = std::slice::from_raw_parts(input, n as usize * 2);
    let output = if selected.is_empty() {
        &mut [][..]
    } else {
        std::slice::from_raw_parts_mut(out, out_cap as usize * 2)
    };
    p.process(iq, selected, output) as i32
}

#[cfg(test)]
mod parallel_bench;

#[cfg(test)]
mod quality_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "bench-instrument")]
    #[test]
    fn profiling_counts_groups_and_resets_per_packet() {
        let mut p = Pfb::new(31_250_000.0, 2).unwrap();
        let input = vec![0i16; 2 * 16240];
        let mut output = vec![0f32; 2 * (16240 / p.hop + 2)];
        let groups = p.process(&input, &[0], &mut output);
        let mut record = PfbStageTimes::default();
        assert_eq!(unsafe { pfb_take_profile(&mut p, &mut record) }, 0);
        assert_eq!(record.groups, groups as u64);
        assert_eq!(unsafe { pfb_take_profile(&mut p, &mut record) }, 0);
        assert_eq!(record.groups, 0);
    }

    #[test]
    fn ffi_clone_keeps_history_phase_and_future_output() {
        for kernel in [0, 1] {
            let mut source = Pfb::new(62_500_000.0, 2).unwrap();
            if kernel == 1 && !source.avx2 {
                continue;
            }
            source.fir_kernel = kernel;
            let first: Vec<i16> = (0..2 * 777).map(|k| (k as i16).wrapping_mul(17)).collect();
            source.process(&first, &[], &mut []);
            let ptr = unsafe { pfb_clone(&source) };
            assert!(!ptr.is_null());
            let second: Vec<i16> = (0..2 * 16240)
                .map(|k| (k as i16).wrapping_mul(-13))
                .collect();
            let bins = [0, 5, 127];
            let mut a = vec![0f32; 2 * bins.len() * (16240 / source.hop + 2)];
            let mut b = a.clone();
            let n = source.process(&second, &bins, &mut a);
            let cloned = unsafe { &mut *ptr };
            let m = cloned.process(&second, &bins, &mut b);
            assert_eq!(n, m);
            assert_eq!(&a[..2 * n * bins.len()], &b[..2 * m * bins.len()]);
            unsafe {
                pfb_close(ptr);
            }
        }
    }

    fn test_pfb(fs: f64, oversample: usize, taps_per_phase: usize) -> Pfb {
        Pfb::with_taps(fs, oversample, taps_per_phase).unwrap()
    }

    #[test]
    fn reconstructed_history_matches_streaming_at_partial_packet_boundaries() {
        for fs in [800_000.0, 4_000_000.0, 8_000_000.0, 31_250_000.0, 62_500_000.0] {
            for (oversample, taps) in [(2, 4), (2, 8), (2, 16), (4, 4)] {
                let mut serial = test_pfb(fs, oversample, taps);
                let mut template = serial.clone();
                if template.avx2 {
                    template.fir_kernel = 1;
                }
                let len = template.taps.len();
                let mut tail = vec![0i16; 2 * len];
                let bins = [0, 1, (serial.m / 2) as u32, (serial.m - 1) as u32];
                let mut input_index = 0u64;
                let mut seed = 0x7105_1993u32;
                let hop = serial.hop;
                let sizes = (0..hop).flat_map(|_| [1, hop])
                    .chain([0, len - 1, len, len + 1, 777, 16240, 4133, 127, 16240, 431]);
                for n in sizes {
                    let iq: Vec<i16> = (0..2 * n)
                        .map(|_| {
                            seed ^= seed << 13;
                            seed ^= seed >> 17;
                            seed ^= seed << 5;
                            (seed >> 8) as i16
                        })
                        .collect();
                    let mut a = vec![f32::NAN; 2 * bins.len() * (n / serial.hop + 2)];
                    let mut b = a.clone();
                    let mut block = template.clone();
                    assert!(!block.load_history(input_index, &tail[..tail.len() - 2]));
                    let mut long_tail = tail.clone();
                    long_tail.extend_from_slice(&[0, 0]);
                    assert!(!block.load_history(input_index, &long_tail));
                    assert!(block.load_history(input_index, &tail));
                    let expected = serial.process(&iq, &bins, &mut a);
                    let got = block.process(&iq, &bins, &mut b);
                    assert_eq!(got, expected);
                    for (x, y) in a[..2 * got * bins.len()].iter().zip(&b[..2 * got * bins.len()]) {
                        assert!(x.is_finite() && y.is_finite());
                        assert!((x - y).abs() <= 0.001,
                            "fs={fs}, os={oversample}, taps={taps}, index={input_index}, n={n}: {x} != {y}");
                    }
                    assert!(a[2 * got * bins.len()..].iter().all(|x| x.is_nan()));
                    assert!(b[2 * got * bins.len()..].iter().all(|x| x.is_nan()));
                    if n >= len {
                        tail.copy_from_slice(&iq[2 * (n - len)..]);
                    } else {
                        tail.copy_within(2 * n.., 0);
                        tail[2 * (len - n)..].copy_from_slice(&iq);
                    }
                    input_index += n as u64;
                }
            }
        }
    }

    #[test]
    fn grouped_avx_fir_matches_baseline_across_packets_and_bins() {
        for fs in [800_000.0, 4_000_000.0, 8_000_000.0, 31_250_000.0, 62_500_000.0] {
            for (oversample, taps) in [(2, 4), (2, 8), (2, 16), (4, 4)] {
                let mut baseline = test_pfb(fs, oversample, taps);
                if !baseline.avx2 {
                    continue;
                }
                assert!(pfb_open_kernel(fs, oversample as u32, 99).is_null());
                let mut candidate = baseline.clone();
                candidate.fir_kernel = 1;
                let bins: Vec<u32> = (0..baseline.m as u32).collect();
                let mut seed = 0x1234_5678u32;
                let sizes = [0, 1, baseline.hop - 1, baseline.hop + 1, baseline.m - 1,
                    16240, 777, 16240, 4133, 127, 16240];
                for (packet, n) in sizes.into_iter().enumerate() {
                    let mut iq: Vec<i16> = (0..2 * n)
                        .map(|j| {
                            seed ^= seed << 13;
                            seed ^= seed >> 17;
                            seed ^= seed << 5;
                            match packet {
                                3 => if j % 2 == 0 { i16::MIN } else { i16::MAX },
                                4 => i16::MAX,
                                _ => (seed >> 8) as i16,
                            }
                        })
                        .collect();
                    // A trailing unpaired component does not advance sample time.
                    iq.push(i16::MIN);
                    let mut a = vec![f32::NAN; 2 * bins.len() * (n / baseline.hop + 2)];
                    let mut b = a.clone();
                    let x = baseline.process(&iq, &bins, &mut a);
                    let split = n / 3;
                    let y0 = candidate.process(&iq[..2 * split], &bins, &mut b);
                    let y1 = candidate.process(&iq[2 * split..], &bins, &mut b[2 * y0 * bins.len()..]);
                    assert_eq!(x, y0 + y1);
                    for (v, w) in a[..2 * x * bins.len()].iter().zip(&b[..2 * x * bins.len()]) {
                        assert!(v.is_finite() && w.is_finite());
                        assert!((v - w).abs() <= 0.001,
                            "fs={fs}, os={oversample}, taps={taps}, packet={packet}: {v} != {w}");
                    }
                    assert!(a[2 * x * bins.len()..].iter().all(|v| v.is_nan()));
                    assert!(b[2 * x * bins.len()..].iter().all(|v| v.is_nan()));
                }
            }
        }
    }

    #[test]
    fn grouped_fir_preserves_passband_alias_rejection_and_phase() {
        fn response(fs: f64, bin: u32, residual: f64, kernel: u32) -> (f64, f64) {
            let ptr = pfb_open_kernel(fs, 2, kernel);
            assert!(!ptr.is_null());
            let p = unsafe { &mut *ptr };
            let output_rate = fs / p.hop as f64;
            let tone = bin as f64 * fs / p.m as f64 + residual;
            let mut out = vec![0f32; 2 * (16240 / p.hop + 2)];
            let mut power = 0.0f64;
            let mut count = 0;
            let mut last: Option<Complex32> = None;
            let mut max_phase_error = 0.0f32;
            for block in 0..8 {
                let mut iq = Vec::with_capacity(2 * 16240);
                for k in 0..16240 {
                    let phase = 2.0 * std::f64::consts::PI * tone * (block * 16240 + k) as f64 / fs;
                    iq.push((12000.0 * phase.cos()) as i16);
                    iq.push((12000.0 * phase.sin()) as i16);
                }
                let groups = p.process(&iq, &[bin], &mut out);
                if block == 0 {
                    continue;
                }
                for z in out[..2 * groups].chunks_exact(2) {
                    let cur = Complex32::new(z[0], z[1]);
                    power += cur.norm_sqr() as f64;
                    count += 1;
                    if let Some(prev) = last {
                        let got = (cur * prev.conj()).arg();
                        let want = (2.0 * std::f64::consts::PI * residual / output_rate) as f32;
                        let diff = (got - want + PI).rem_euclid(2.0 * PI) - PI;
                        max_phase_error = max_phase_error.max(diff.abs());
                    }
                    last = Some(cur);
                }
            }
            unsafe {
                pfb_close(ptr);
            }
            ((power / count as f64).sqrt(), max_phase_error as f64)
        }
        for fs in [31_250_000.0, 62_500_000.0] {
            if !Pfb::new(fs, 2).unwrap().avx2 {
                continue;
            }
            let out_rate = fs / super::super::narrow::channel_quantum(fs) as f64;
            let (want_base, phase_base) = response(fs, 5, 0.3 * out_rate, 0);
            let (want_grouped, phase_grouped) = response(fs, 5, 0.3 * out_rate, 1);
            let (alias_base, _) = response(fs, 5, 0.7 * out_rate, 0);
            let (alias_grouped, _) = response(fs, 5, 0.7 * out_rate, 1);
            let rejection = 20.0 * (alias_base / want_base).log10();
            assert!(
                rejection < -40.0,
                "fs={fs}: baseline rejection {rejection} dB"
            );
            assert!((want_grouped - want_base).abs() / want_base < 1e-6);
            assert!((alias_grouped - alias_base).abs() < 0.001);
            assert!(
                phase_base < 0.005 && phase_grouped < 0.005,
                "fs={fs}: phase error baseline={phase_base}, grouped={phase_grouped}"
            );
        }
    }

    #[test]
    fn reports_filter_geometry_and_selected_simd_path() {
        for (oversample, expected_taps, expected_phase) in [(2, 1024, 16), (4, 512, 4)] {
            let p = Pfb::new(31_250_000.0, oversample).unwrap();
            let (mut taps, mut per_phase, mut simd) = (0, 0, 0);
            assert_eq!(
                unsafe { pfb_diagnostics(&p, &mut taps, &mut per_phase, &mut simd) },
                0
            );
            assert_eq!((taps, per_phase), (expected_taps, expected_phase));
            assert_eq!(simd & 1 != 0, oversample == 2 && p.avx2);
            println!(
                "PFB {oversample}x: taps={taps}, per_phase={per_phase}, FIR AVX2={}, FFT AVX={}",
                simd & 1 != 0,
                simd & 2 != 0
            );
        }
    }

    #[cfg(feature = "bench-instrument")]
    #[test]
    fn profile_stage_costs() {
        use std::hint::black_box;
        use std::time::Instant;
        let n = 16240;
        let input: Vec<i16> = (0..2 * n).map(|j| (j as i16).wrapping_mul(17)).collect();
        let loops = 80;
        for fs in [31_250_000.0, 62_500_000.0] {
            for kernel in [0, 1] {
                let mut p = Pfb::new(fs, 2).unwrap();
                if kernel == 1 && !p.avx2 {
                    continue;
                }
                p.fir_kernel = kernel;
                let mut output = vec![0.0f32; 2 * (n / p.hop + 1)];
                for _ in 0..8 {
                    p.process(black_box(&input), &[5], black_box(&mut output));
                }
                let mut input_only = p.clone();
                let start = Instant::now();
                for _ in 0..loops {
                    input_only.process(black_box(&input), &[], &mut []);
                }
                black_box(&input_only);
                let input_us = start.elapsed().as_secs_f64() * 1e6 / loops as f64;
                p.profile = PfbStageTimes::default();
                let start = Instant::now();
                for _ in 0..loops {
                    p.process(black_box(&input), &[5], black_box(&mut output));
                }
                let total_us = start.elapsed().as_secs_f64() * 1e6 / loops as f64;
                let fir_us = p.profile.fir_ns as f64 / (1000.0 * loops as f64);
                let fft_us = p.profile.fft_ns as f64 / (1000.0 * loops as f64);
                let output_us = p.profile.output_ns as f64 / (1000.0 * loops as f64);
                println!("PFB fs={fs:.0} kernel={kernel} taps={} n={n}: FIR={fir_us:.1}us FFT={fft_us:.1}us input={input_us:.1}us output={output_us:.1}us total={total_us:.1}us",
                    p.taps_per_phase);
            }
        }
    }

    #[test]
    fn fft_bins_match_direct_windowed_mixer() {
        let fs = 31_250_000.0;
        for oversample in [2, 4] {
            let mut p = Pfb::new(fs, oversample).unwrap();
            let bin = 5u32;
            let n = 3200;
            let f = (bin as f64 + 0.15) * fs / p.m as f64;
            let mut iq = Vec::with_capacity(n * 2);
            for j in 0..n {
                let ph = 2.0 * std::f64::consts::PI * f * j as f64 / fs;
                iq.push((ph.cos() * 12000.0) as i16);
                iq.push((ph.sin() * 12000.0) as i16);
            }
            let mut output = vec![0.0f32; 2 * (n / p.hop + 1)];
            let groups = unsafe {
                pfb_process(
                    &mut p,
                    iq.as_ptr(),
                    n as i32,
                    &bin,
                    1,
                    output.as_mut_ptr(),
                    output.len() as i32 / 2,
                )
            };
            assert_eq!(groups as usize, n / p.hop);
            for group in [35, 60, 90] {
                let t = (group + 1) * p.hop - 1;
                let mut direct = Complex32::new(0.0, 0.0);
                for lag in 0..p.taps.len() {
                    if lag > t {
                        break;
                    }
                    let j = t - lag;
                    let sample = Complex32::new(iq[2 * j] as f32, iq[2 * j + 1] as f32);
                    let phase = -2.0 * PI * bin as f32 * j as f32 / p.m as f32;
                    direct += sample * Complex32::new(phase.cos(), phase.sin()) * p.taps[lag];
                }
                let actual = Complex32::new(output[group * 2], output[group * 2 + 1]);
                assert!(
                    (actual - direct).norm() < 2.0,
                    "group={group} error={}",
                    (actual - direct).norm()
                );
            }
        }
    }

    #[test]
    fn requested_bins_have_correct_frequency_and_phase_across_packets() {
        let fs = 31_250_000.0;
        for oversample in [2, 4] {
            let mut p = Pfb::new(fs, oversample).unwrap();
            assert_eq!((p.m, p.hop), (oversample * 32, 32));
            let bin = 5u32;
            let delta = 1000.0;
            let f = bin as f64 * fs / p.m as f64 + delta;
            let mut prev: Option<Complex32> = None;
            let mut outputs = 0;
            for block in 0..12 {
                let n = 16240;
                let mut iq = Vec::with_capacity(2 * n);
                for j in 0..n {
                    let t = (block * n + j) as f64 / fs;
                    let ph = 2.0 * std::f64::consts::PI * f * t;
                    iq.push((ph.cos() * 12000.0) as i16);
                    iq.push((ph.sin() * 12000.0) as i16);
                }
                let mut out = vec![0.0f32; 2 * (n / p.hop + 1)];
                let groups = unsafe {
                    pfb_process(
                        &mut p,
                        iq.as_ptr(),
                        n as i32,
                        &bin,
                        1,
                        out.as_mut_ptr(),
                        out.len() as i32 / 2,
                    )
                };
                assert!(groups > 0);
                for z in out
                    .chunks_exact(2)
                    .take(groups as usize)
                    .skip(if block == 0 { 20 } else { 0 })
                {
                    let cur = Complex32::new(z[0], z[1]);
                    if let Some(old) = prev {
                        let d = (cur * old.conj()).arg();
                        let expected = 2.0 * PI * delta as f32 * p.hop as f32 / fs as f32;
                        assert!((d - expected).abs() < 0.003, "phase discontinuity {d}");
                    }
                    prev = Some(cur);
                    outputs += 1;
                }
            }
            assert!(outputs > 6000);
        }
    }
}
