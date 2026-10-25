//! Native per-channel two-stage IQ DDC. Build with `cargo build --release --manifest-path backend/Cargo.toml`.
use std::f64::consts::PI;

pub struct Channel {
    fs: f64,
    d1: usize,
    d2: usize,
    input_samples: u64,
    output_samples: u64,
    phase1: usize,
    phase2: usize,
    normalize: usize,
    re: f64,
    im: f64,
    wr: f64,
    wi: f64,
    pos1: usize,
    pos2: usize,
    h1: Vec<f32>,
    h2: Vec<f32>,
    fused: Option<Vec<f32>>,
    avx2: bool,
    r1: Vec<f32>,
    i1: Vec<f32>,
    r2: Vec<f32>,
    i2: Vec<f32>,
    rational: Option<super::rational::Rational>,
    intermediate: Vec<f32>,
    stages: Option<Vec<Decimator>>,
}

/// Streaming FIR stage. Only output instants evaluate taps; doubled history is contiguous for SIMD.
struct Decimator {
    factor: usize,
    phase: usize,
    pos: usize,
    taps: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Decimator {
    fn new(fs: f64, factor: usize, pass: f64, stop: f64) -> Option<Self> {
        // Kaiser beta 8.6 gives approximately 80 dB stopband rejection.
        let n = crate::rate_plan::filter_taps(fs, pass, stop)?;
        let history = n.checked_mul(2)?;
        Some(Self { factor, phase: 0, pos: 0, taps: design(n, (pass / fs + stop / fs) / 2.0),
            re: vec![0.0; history], im: vec![0.0; history] })
    }

    fn push(&mut self, re: f32, im: f32, avx2: bool) -> Option<(f32, f32)> {
        let n = self.taps.len();
        self.re[self.pos] = re;
        self.re[self.pos + n] = re;
        self.im[self.pos] = im;
        self.im[self.pos + n] = im;
        self.pos += 1;
        if self.pos == n { self.pos = 0; }
        self.phase += 1;
        if self.phase < self.factor { return None; }
        self.phase = 0;
        Some((dot(&self.taps, &self.re[self.pos..self.pos + n], avx2),
            dot(&self.taps, &self.im[self.pos..self.pos + n], avx2)))
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let mut acc = _mm256_setzero_ps();
    for k in (0..a.len()).step_by(8) {
        acc = _mm256_fmadd_ps(
            _mm256_loadu_ps(a.as_ptr().add(k)),
            _mm256_loadu_ps(b.as_ptr().add(k)),
            acc,
        );
    }
    let mut lanes = [0.0f32; 8];
    _mm256_storeu_ps(lanes.as_mut_ptr(), acc);
    lanes.iter().sum()
}

pub(crate) fn dot(a: &[f32], b: &[f32], avx2: bool) -> f32 {
    #[cfg(target_arch = "x86_64")]
    if avx2 {
        return unsafe { dot_avx2(a, b) };
    }
    let _ = avx2;
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub(crate) fn bessel0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..40 {
        term *= (x * x / 4.0) / (k * k) as f64;
        sum += term;
        if term < sum * 1e-15 {
            break;
        }
    }
    sum
}

pub(crate) fn design(n: usize, cutoff: f64) -> Vec<f32> {
    let denom = bessel0(8.6);
    let mut h: Vec<f32> = (0..n)
        .map(|j| {
            let x = j as f64 - (n - 1) as f64 / 2.0;
            let t = 2.0 * j as f64 / (n - 1) as f64 - 1.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * x).sin() / (PI * x)
            };
            (sinc * bessel0(8.6 * (1.0 - t * t).max(0.0).sqrt()) / denom) as f32
        })
        .collect();
    let norm: f64 = h.iter().map(|&v| v as f64).sum();
    for v in &mut h {
        *v /= norm as f32;
    }
    h
}

#[no_mangle]
pub extern "C" fn channel_quantum(fs: f64) -> i32 {
    if !fs.is_finite() || fs <= 0.0 {
        return 0;
    }
    let mut d1 = 1usize;
    while fs / (d1 * 2) as f64 >= 800_000.0 && d1 < 1024 {
        d1 *= 2;
    }
    if (800_000.0..=1_250_000.0).contains(&(fs / d1 as f64)) {
        d1 as i32
    } else {
        0
    }
}

/// Prefer a ~1 MSps first stage. For IF rates between the normal quantized
/// steps, use a smaller power-of-two first stage (up to 4 MSps) instead of
/// widening the browser's baseband or shrinking the requested IF passband.
fn geometry(fs: f64, decimate: i32) -> Option<(usize, usize, usize)> {
    let mut d1 = channel_quantum(fs) as usize;
    if d1 == 0 || decimate < 2 {
        return None;
    }
    while d1 > 0 {
        if decimate as usize % d1 == 0 {
            let d2 = decimate as usize / d1;
            if (2..=24).contains(&d2) && fs / d1 as f64 <= 4_000_000.0 {
                return Some((d1, d2, (12 * d1).next_multiple_of(8)));
            }
        }
        d1 /= 2;
    }
    None
}

/// Largest supported decimation not exceeding ceiling; output rate never falls below need.
#[no_mangle]
pub extern "C" fn channel_select_decimate(fs: f64, ceiling: i32) -> i32 {
    if ceiling < 2 || !fs.is_finite() {
        return 0;
    }
    for decimate in (2..=ceiling.min(65536)).rev() {
        if geometry(fs, decimate).is_some() {
            return decimate;
        }
    }
    0
}

/// Returns null for unsupported geometry; never calls the vendor SDK.
#[no_mangle]
pub extern "C" fn channel_open(fs: f64, offset: f64, decimate: i32) -> *mut Channel {
    if !offset.is_finite() || offset.abs() >= fs / 2.0 {
        return std::ptr::null_mut();
    }
    let Some((d1, d2, n1)) = geometry(fs, decimate) else {
        return std::ptr::null_mut();
    };
    let n2 = 32 * d2;
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
    Box::into_raw(Box::new(Channel {
        fs,
        d1,
        d2,
        input_samples: 0,
        output_samples: 0,
        phase1: 0,
        phase2: 0,
        normalize: 0,
        re: 1.0,
        im: 0.0,
        wr: (2.0 * PI * offset / fs).cos(),
        wi: (2.0 * PI * offset / fs).sin(),
        pos1: 0,
        pos2: 0,
        h1: design(n1, 0.40 / d1 as f64),
        h2: design(n2, 0.49 / d2 as f64),
        fused: None,
        avx2,
        r1: vec![0.0; 2 * n1],
        i1: vec![0.0; 2 * n1],
        r2: vec![0.0; 2 * n2],
        i2: vec![0.0; 2 * n2],
        rational: None,
        intermediate: Vec::new(),
        stages: None,
    }))
}

/// Explicit coarse-bin candidate: when d1=1, fuse both FIR stages at setup.
/// Keeps the original channel_open path as the reference/fallback.
pub fn channel_open_coarse(fs: f64, offset: f64, decimate: i32) -> *mut Channel {
    let handle = channel_open(fs, offset, decimate);
    if handle.is_null() {
        return handle;
    }
    let channel = unsafe { &mut *handle };
    if channel.d1 != 1 {
        unsafe { channel_close(handle) };
        return std::ptr::null_mut();
    }
    let length = channel.h1.len() + channel.h2.len() - 1;
    let mut fused = vec![0.0f32; length.next_multiple_of(8)];
    let pad = fused.len() - length;
    for (i, &first) in channel.h1.iter().enumerate() {
        for (j, &second) in channel.h2.iter().enumerate() {
            fused[pad + i + j] += first * second;
        }
    }
    channel.r1 = vec![0.0; 2 * fused.len()];
    channel.i1 = vec![0.0; 2 * fused.len()];
    channel.fused = Some(fused);
    handle
}

/// Per-channel anti-alias cascade with a final narrow filter evaluated at the selected IQ rate.
/// Stages retain their FIR history and integer phase across every process call.
pub fn channel_open_planned(fs: f64, offset: f64, plan: &crate::rate_plan::RatePlan) -> *mut Channel {
    if !plan.valid_for(fs) || !offset.is_finite() || offset.abs() >= fs / 2.0 {
        return std::ptr::null_mut();
    }
    let mut stages = Vec::new();
    let mut rate = fs;
    for factor in crate::rate_plan::stage_factors(plan.decimate).unwrap() {
        let next_rate = rate / factor as f64;
        // Protect the occupied passband from every alias image. The low-rate final
        // stage removes the remaining transition-band energy before delivery.
        let Some(stage) = Decimator::new(rate, factor, plan.occupied_bandwidth_hz / 2.0,
            next_rate - plan.occupied_bandwidth_hz / 2.0) else { return std::ptr::null_mut(); };
        stages.push(stage);
        rate = next_rate;
    }
    let Some(stage) = Decimator::new(rate, 1, plan.occupied_bandwidth_hz / 2.0, plan.stopband_hz)
        else { return std::ptr::null_mut(); };
    stages.push(stage);
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma");
    #[cfg(not(target_arch = "x86_64"))]
    let avx2 = false;
    Box::into_raw(Box::new(Channel {
        fs, d1: 1, d2: plan.decimate as usize, input_samples: 0, output_samples: 0,
        phase1: 0, phase2: 0, normalize: 0, re: 1.0, im: 0.0,
        wr: (2.0 * PI * offset / fs).cos(), wi: (2.0 * PI * offset / fs).sin(),
        pos1: 0, pos2: 0, h1: Vec::new(), h2: Vec::new(), fused: None, avx2,
        r1: Vec::new(), i1: Vec::new(), r2: Vec::new(), i2: Vec::new(),
        rational: None, intermediate: Vec::new(), stages: Some(stages),
    }))
}

#[no_mangle]
pub extern "C" fn channel_open_ratio(
    fs: f64,
    offset: f64,
    base_decimate: i32,
    up: u32,
    down: u32,
    passband_hz: f64,
) -> *mut Channel {
    let handle = channel_open(fs, offset, base_decimate);
    if handle.is_null() {
        return handle;
    }
    let base_rate = fs / base_decimate as f64;
    let Some(rational) = super::rational::Rational::new(base_rate, up, down, passband_hz) else {
        unsafe {
            channel_close(handle);
        }
        return std::ptr::null_mut();
    };
    unsafe {
        (*handle).rational = Some(rational);
    }
    handle
}

#[no_mangle]
pub unsafe extern "C" fn channel_delay(ptr: *const Channel) -> f64 {
    if ptr.is_null() {
        return -1.0;
    }
    let c = &*ptr;
    if let Some(stages) = &c.stages {
        let mut delay_inputs = 0.0;
        let mut hop = 1usize;
        for stage in stages {
            delay_inputs += (stage.taps.len() - 1) as f64 / 2.0 * hop as f64;
            hop *= stage.factor;
        }
        return delay_inputs / hop as f64;
    }
    let base_delay = ((c.h1.len() - 1) as f64 / 2.0 + c.d1 as f64 * (c.h2.len() - 1) as f64 / 2.0)
        / (c.d1 * c.d2) as f64;
    match &c.rational {
        Some(r) => base_delay * r.rate_ratio() + r.delay_outputs(),
        None => base_delay,
    }
}

#[no_mangle]
pub unsafe extern "C" fn channel_output_rate(ptr: *const Channel) -> f64 {
    if ptr.is_null() {
        return 0.0;
    }
    let c = &*ptr;
    let base_rate = c.fs / (c.d1 * c.d2) as f64;
    c.rational
        .as_ref()
        .map_or(base_rate, |r| r.output_rate(base_rate))
}

#[no_mangle]
pub unsafe extern "C" fn channel_clock(
    ptr: *const Channel,
    input: *mut u64,
    output: *mut u64,
) -> i32 {
    if ptr.is_null() || input.is_null() || output.is_null() {
        return -1;
    }
    *input = (*ptr).input_samples;
    *output = (*ptr).output_samples;
    0
}

#[no_mangle]
pub unsafe extern "C" fn channel_close(ptr: *mut Channel) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

/// Caller owns >= 2*n input i16 and >= 2*out_cap output f32. Returns -1 on invalid shape.
#[no_mangle]
pub unsafe extern "C" fn channel_process(
    ptr: *mut Channel,
    input: *const i16,
    n: i32,
    output: *mut f32,
    out_cap: i32,
) -> i32 {
    if ptr.is_null()
        || input.is_null()
        || output.is_null()
        || !(0..=65536).contains(&n)
        || out_cap < 0
    {
        return -1;
    }
    let c = &mut *ptr;
    if (out_cap as usize) < n as usize / (c.d1 * c.d2) + 2 {
        return -1;
    }
    let iq = std::slice::from_raw_parts(input, n as usize * 2);
    let out = std::slice::from_raw_parts_mut(output, out_cap as usize * 2);
    process_samples(c, iq, out)
}

/// Float coarse-bin input; same NCO/FIR and clock as `channel_process`, no i16 requantization.
#[no_mangle]
pub unsafe extern "C" fn channel_process_float(
    ptr: *mut Channel,
    input: *const f32,
    n: i32,
    output: *mut f32,
    out_cap: i32,
) -> i32 {
    if ptr.is_null()
        || input.is_null()
        || output.is_null()
        || !(0..=65536).contains(&n)
        || out_cap < 0
    {
        return -1;
    }
    let c = &mut *ptr;
    if (out_cap as usize) < n as usize / (c.d1 * c.d2) + 2 {
        return -1;
    }
    let iq = std::slice::from_raw_parts(input, n as usize * 2);
    let out = std::slice::from_raw_parts_mut(output, out_cap as usize * 2);
    process_samples(c, iq, out)
}

fn process_samples<T: Copy + Into<f64>>(c: &mut Channel, iq: &[T], out: &mut [f32]) -> i32 {
    let n = iq.len() / 2;
    if c.stages.is_some() { return process_multistage(c, iq, out); }
    let n1 = c.fused.as_ref().map_or(c.h1.len(), Vec::len);
    let n2 = c.h2.len();
    if c.rational.is_some() {
        c.intermediate.resize(2 * (n / (c.d1 * c.d2) + 2), 0.0);
    }
    let base_out = if c.rational.is_some() {
        &mut c.intermediate[..]
    } else {
        &mut out[..]
    };
    let mut produced = 0;
    for sample in iq.chunks_exact(2) {
        let (ir, ii): (f64, f64) = (sample[0].into(), sample[1].into());
        let r = (ir * c.re - ii * c.im) as f32;
        let i = (ir * c.im + ii * c.re) as f32;
        c.r1[c.pos1] = r;
        c.r1[c.pos1 + n1] = r;
        c.i1[c.pos1] = i;
        c.i1[c.pos1 + n1] = i;
        let next_re = c.re * c.wr - c.im * c.wi;
        c.im = c.re * c.wi + c.im * c.wr;
        c.re = next_re;
        c.normalize += 1;
        if c.normalize == 4096 {
            let mag = c.re.hypot(c.im);
            c.re /= mag;
            c.im /= mag;
            c.normalize = 0;
        }
        c.pos1 += 1;
        if c.pos1 == n1 {
            c.pos1 = 0;
        }
        if let Some(fused) = &c.fused {
            c.phase2 += 1;
            if c.phase2 < c.d2 {
                continue;
            }
            c.phase2 = 0;
            base_out[2 * produced] = dot(fused, &c.r1[c.pos1..c.pos1 + n1], c.avx2);
            base_out[2 * produced + 1] = dot(fused, &c.i1[c.pos1..c.pos1 + n1], c.avx2);
            produced += 1;
            continue;
        }
        c.phase1 += 1;
        if c.phase1 < c.d1 {
            continue;
        }
        c.phase1 = 0;
        let ar = dot(&c.h1, &c.r1[c.pos1..c.pos1 + n1], c.avx2);
        let ai = dot(&c.h1, &c.i1[c.pos1..c.pos1 + n1], c.avx2);
        c.r2[c.pos2] = ar;
        c.r2[c.pos2 + n2] = ar;
        c.i2[c.pos2] = ai;
        c.i2[c.pos2 + n2] = ai;
        c.pos2 += 1;
        if c.pos2 == n2 {
            c.pos2 = 0;
        }
        c.phase2 += 1;
        if c.phase2 < c.d2 {
            continue;
        }
        c.phase2 = 0;
        let ar = dot(&c.h2, &c.r2[c.pos2..c.pos2 + n2], c.avx2);
        let ai = dot(&c.h2, &c.i2[c.pos2..c.pos2 + n2], c.avx2);
        base_out[2 * produced] = ar;
        base_out[2 * produced + 1] = ai;
        produced += 1;
    }
    c.input_samples += n as u64;
    let final_count = match c.rational.as_mut() {
        Some(r) => r.process(&c.intermediate[..2 * produced], out),
        None => produced,
    };
    c.output_samples += final_count as u64;
    final_count as i32
}

fn process_multistage<T: Copy + Into<f64>>(c: &mut Channel, iq: &[T], out: &mut [f32]) -> i32 {
    let mut produced = 0;
    for sample in iq.chunks_exact(2) {
        let (ir, ii): (f64, f64) = (sample[0].into(), sample[1].into());
        let mut value = Some(((ir * c.re - ii * c.im) as f32, (ir * c.im + ii * c.re) as f32));
        let next_re = c.re * c.wr - c.im * c.wi;
        c.im = c.re * c.wi + c.im * c.wr;
        c.re = next_re;
        c.normalize += 1;
        if c.normalize == 4096 {
            let mag = c.re.hypot(c.im);
            c.re /= mag;
            c.im /= mag;
            c.normalize = 0;
        }
        for stage in c.stages.as_mut().unwrap() {
            let Some((re, im)) = value else { break; };
            value = stage.push(re, im, c.avx2);
        }
        if let Some((re, im)) = value {
            out[2 * produced] = re;
            out[2 * produced + 1] = im;
            produced += 1;
        }
    }
    c.input_samples += (iq.len() / 2) as u64;
    c.output_samples += produced as u64;
    produced as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared RF frequency-response probe through the real planned cascade with
    /// packetized input; also asserts exact per-packet sample conservation.
    fn planned_cascade_gain(mode: &str, occupied_hz: f64, probe_hz: f64) -> f64 {
        let fs = 976_562.5;
        let plan = crate::rate_plan::RatePlan::new(fs, mode, occupied_hz).unwrap();
        assert!(plan.valid_for(fs));
        let channel = channel_open_planned(fs, 0.0, &plan);
        assert!(!channel.is_null());
        let delay = unsafe { channel_delay(channel) };
        let (mut power, mut count) = (0.0f64, 0usize);
        let (mut input_index, mut output_index) = (0usize, 0usize);
        for block in 0..160 {
            let n = if block % 2 == 0 { 255 } else { 4093 };
            let iq: Vec<f32> = (0..n).flat_map(|sample| {
                let phase = 2.0 * PI * probe_hz * (input_index + sample) as f64 / fs;
                [phase.cos() as f32, phase.sin() as f32]
            }).collect();
            let mut output = vec![0.0; 2 * (n / plan.decimate as usize + 2)];
            let produced = unsafe { channel_process_float(channel, iq.as_ptr(), n as i32,
                output.as_mut_ptr(), (output.len() / 2) as i32) };
            assert!(produced >= 0);
            for (at, sample) in output[..2 * produced as usize].chunks_exact(2).enumerate() {
                if (output_index + at) as f64 > 2.0 * delay + 16.0 {
                    power += sample[0] as f64 * sample[0] as f64 + sample[1] as f64 * sample[1] as f64;
                    count += 1;
                }
            }
            input_index += n;
            output_index += produced as usize;
            assert_eq!(output_index, input_index / plan.decimate as usize);
        }
        let (mut consumed, mut produced) = (0, 0);
        unsafe { channel_clock(channel, &mut consumed, &mut produced); channel_close(channel); }
        assert_eq!((consumed, produced), (input_index as u64, output_index as u64));
        assert!(count > 0);
        (power / count as f64).sqrt()
    }

    #[test]
    fn planned_cascade_frequency_response_matches_requested_rf_filter() {
        let wfm5 = crate::rate_plan::RatePlan::new(976_562.5, "wfm", 5_000.0).unwrap();
        // The same RF probe must pass the wide plan and be rejected by the narrow one;
        // rejection rows include decimation-alias probes beyond the output Nyquist.
        for (mode, occupied_hz, probe_hz, pass) in [
            ("wfm", 5_000.0, 2_500.0, true),
            ("wfm", 5_000.0, 20_000.0, false),
            ("wfm", 5_000.0, 50_000.0, false),
            ("wfm", 5_000.0, wfm5.output_rate - 1_250.0, false),
            ("wfm", 300_000.0, 20_000.0, true),
            ("wfm", 300_000.0, 50_000.0, true),
            ("am", 6_000.0, 2_500.0, true),
            ("am", 6_000.0, 6_000.0, false),
            ("usb", 6_000.0, 2_500.0, true),
            ("lsb", 6_000.0, 6_000.0, false),
            ("cw", 1_900.0, 600.0, true),
            ("cw", 1_900.0, 1_900.0, false),
            ("nfm", 12_000.0, 4_000.0, true),
            ("nfm", 12_000.0, 12_000.0, false),
        ] {
            let gain = planned_cascade_gain(mode, occupied_hz, probe_hz);
            if pass {
                assert!(gain > 0.97, "{mode} {occupied_hz} Hz passband probe {probe_hz} Hz gain {gain}");
            } else {
                assert!(gain < 0.001, "{mode} {occupied_hz} Hz rejection probe {probe_hz} Hz gain {gain}");
            }
        }
    }
    #[test]
    fn phase_and_sample_count_carry_across_packets() {
        assert_eq!(channel_quantum(31_250_000.0), 32);
        assert_eq!(channel_quantum(62_500_000.0), 64);
        assert_eq!(channel_quantum(15_625_000.0), 16);
        assert_eq!(channel_quantum(f64::NAN), 0);
        assert_eq!(channel_select_decimate(31_250_000.0, 651), 640);
        assert_eq!(channel_select_decimate(31_250_000.0, 108), 104);
        assert_eq!(channel_select_decimate(62_500_000.0, 1302), 1280);
        assert!(channel_open(31_250_000.0, 0.0, 651).is_null());
        let handle = channel_open(31_250_000.0, -300_000.0, 640);
        assert!(!handle.is_null());
        let mut out = [0.0f32; 60];
        let input = [0i16; 16240 * 2];
        let mut count = 0;
        for _ in 0..32 {
            count +=
                unsafe { channel_process(handle, input.as_ptr(), 16240, out.as_mut_ptr(), 30) };
        }
        assert_eq!(count, 812);
        let expected_delay = (383.0 / 2.0 + 32.0 * 639.0 / 2.0) / 640.0;
        assert!((unsafe { channel_delay(handle) } - expected_delay).abs() < 1e-12);
        assert_eq!(unsafe { channel_output_rate(handle) }, 31_250_000.0 / 640.0);
        let (mut input_clock, mut output_clock) = (0u64, 0u64);
        assert_eq!(
            unsafe { channel_clock(handle, &mut input_clock, &mut output_clock) },
            0
        );
        assert_eq!((input_clock, output_clock), (16240 * 32, 812));
        assert_eq!(
            unsafe { channel_process(handle, input.as_ptr(), 16240, out.as_mut_ptr(), 25) },
            -1
        );
        unsafe {
            channel_close(handle);
        }
        let wide = channel_open(62_500_000.0, -300_000.0, 1280);
        assert!(!wide.is_null());
        assert_eq!(unsafe { channel_output_rate(wide) }, 62_500_000.0 / 1280.0);
        unsafe {
            channel_close(wide);
        }
    }

    #[test]
    fn composed_integer_and_rational_rates_keep_one_clock() {
        let fs = 31_250_000.0;
        let handle = channel_open_ratio(fs, -200_000.0, 96, 8, 9, 90_000.0);
        assert!(!handle.is_null());
        assert!((unsafe { channel_output_rate(handle) } - fs / 108.0).abs() < 1e-9);
        assert!(unsafe { channel_delay(handle) } > 40.0);
        let input = [0i16; 16240 * 2];
        let mut output = [0.0f32; 400];
        let mut sum = 0;
        for _ in 0..32 {
            let n =
                unsafe { channel_process(handle, input.as_ptr(), 16240, output.as_mut_ptr(), 200) };
            assert!(n > 0);
            sum += n;
        }
        let (mut input_clock, mut output_clock) = (0, 0);
        assert_eq!(
            unsafe { channel_clock(handle, &mut input_clock, &mut output_clock) },
            0
        );
        assert_eq!(input_clock, 32 * 16240);
        assert_eq!(output_clock, sum as u64);
        assert!(sum > 4700 && sum < 4820);
        unsafe {
            channel_close(handle);
        }
    }

    #[test]
    fn float_coarse_iq_matches_integer_path_without_requantizing() {
        let fs = 976_562.5;
        let i16_channel = channel_open(fs, -120_000.0, 12);
        let f32_channel = channel_open(fs, -120_000.0, 12);
        assert!(!i16_channel.is_null() && !f32_channel.is_null());
        let mut seed = 20261019u32;
        for n in [17, 255, 4096, 121, 8192, 27, 255] {
            let input: Vec<i16> = (0..2 * n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 16) as i16
                })
                .collect();
            let float_input: Vec<f32> = input.iter().map(|&v| v as f32).collect();
            let cap = n / 12 + 2;
            let mut integer_out = vec![0.0f32; 2 * cap];
            let mut float_out = vec![0.0f32; 2 * cap];
            let a = unsafe {
                channel_process(
                    i16_channel,
                    input.as_ptr(),
                    n as i32,
                    integer_out.as_mut_ptr(),
                    cap as i32,
                )
            };
            let b = unsafe {
                channel_process_float(
                    f32_channel,
                    float_input.as_ptr(),
                    n as i32,
                    float_out.as_mut_ptr(),
                    cap as i32,
                )
            };
            assert_eq!(a, b);
            assert_eq!(&integer_out[..2 * a as usize], &float_out[..2 * b as usize]);
        }
        let (mut i16_clock, mut f32_clock, mut out_a, mut out_b) = (0, 0, 0, 0);
        unsafe {
            channel_clock(i16_channel, &mut i16_clock, &mut out_a);
            channel_clock(f32_channel, &mut f32_clock, &mut out_b);
        }
        assert_eq!((i16_clock, out_a), (f32_clock, out_b));
        let fractional_channel = channel_open(fs, 0.0, 12);
        assert!(!fractional_channel.is_null());
        let fractional = vec![0.5f32; 2 * 4096];
        let mut output = vec![0.0f32; 2 * (4096 / 12 + 2)];
        assert_eq!(
            unsafe {
                channel_process_float(
                    fractional_channel,
                    fractional.as_ptr(),
                    4096,
                    output.as_mut_ptr(),
                    1,
                )
            },
            -1
        );
        let count = unsafe {
            channel_process_float(
                fractional_channel,
                fractional.as_ptr(),
                4096,
                output.as_mut_ptr(),
                (output.len() / 2) as i32,
            )
        };
        assert!(count > 0);
        assert!(output[2 * (count as usize - 1)].abs() > 0.3);
        unsafe {
            channel_close(i16_channel);
            channel_close(f32_channel);
            channel_close(fractional_channel);
        }
    }

    #[test]
    fn fused_coarse_fir_preserves_reference_response_and_clock() {
        assert!(channel_open_coarse(31_250_000.0, 0.0, 640).is_null());
        for decimate in [4, 12] {
            let fs = 976_562.5;
            let reference = channel_open(fs, -120_000.0, decimate);
            let fused = channel_open_coarse(fs, -120_000.0, decimate);
            assert!(!reference.is_null() && !fused.is_null());
            assert_eq!(unsafe { channel_delay(reference) }, unsafe {
                channel_delay(fused)
            });
            assert_eq!(unsafe { channel_output_rate(reference) }, unsafe {
                channel_output_rate(fused)
            });
            let mut seed = 20261019u32;
            let (mut error, mut energy, mut total) = (0.0f64, 0.0f64, 0usize);
            for n in [17, 255, 4096, 121, 8192, 27, 16240] {
                let iq: Vec<f32> = (0..2 * n)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        (seed as i16 as f32) + 0.25
                    })
                    .collect();
                let cap = n / decimate as usize + 2;
                let mut expected = vec![0.0f32; 2 * cap];
                let mut got = vec![0.0f32; 2 * cap];
                let a = unsafe {
                    channel_process_float(
                        reference,
                        iq.as_ptr(),
                        n as i32,
                        expected.as_mut_ptr(),
                        cap as i32,
                    )
                };
                let b = unsafe {
                    channel_process_float(
                        fused,
                        iq.as_ptr(),
                        n as i32,
                        got.as_mut_ptr(),
                        cap as i32,
                    )
                };
                assert_eq!(a, b);
                for (x, y) in expected[..2 * a as usize].iter().zip(&got) {
                    error += (*x as f64 - *y as f64).powi(2);
                    energy += (*x as f64).powi(2);
                    total += 1;
                }
            }
            assert!(total > 1000);
            assert!(
                (error / energy).sqrt() < 1e-4,
                "relative RMS error {}",
                (error / energy).sqrt()
            );
            let (mut input_a, mut input_b, mut output_a, mut output_b) = (0, 0, 0, 0);
            unsafe {
                channel_clock(reference, &mut input_a, &mut output_a);
                channel_clock(fused, &mut input_b, &mut output_b);
                channel_close(reference);
                channel_close(fused);
            }
            assert_eq!((input_a, output_a), (input_b, output_b));
        }
    }
}
