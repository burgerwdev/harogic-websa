//! Display-only long observations. Original IQ consumers and their clocks are untouched.
use super::*;
use std::time::Instant;
use std::{
    ffi::{c_char, c_int, c_void, CString},
    sync::mpsc::{self, TrySendError},
    thread,
};

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}
type Open = unsafe extern "C" fn(*mut *mut c_void, u32, *mut u32, *mut f64) -> i32;
type Execute = unsafe extern "C" fn(
    *mut c_void,
    *const i16,
    u32,
    f32,
    f64,
    f64,
    f64,
    *mut f64,
    *mut f32,
    u32,
) -> i32;
type Close = unsafe extern "C" fn(*mut c_void) -> i32;
struct Dsp {
    library: *mut c_void,
    handle: *mut c_void,
    execute: Execute,
    close: Close,
    points: usize,
    rbw: f64,
    size: usize,
}
impl Drop for Dsp {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
            dlclose(self.library);
        }
    }
}
impl Dsp {
    fn open(path: &str, size: usize) -> Result<Self, String> {
        let path = CString::new(path).map_err(|_| "invalid display FFT shim path")?;
        let library = unsafe { dlopen(path.as_ptr(), 2) };
        if library.is_null() {
            return Err("display FFT shim unavailable".into());
        }
        let result = (|| {
            let sym = |name: &[u8]| {
                let p = unsafe { dlsym(library, name.as_ptr().cast()) };
                if p.is_null() {
                    Err("display FFT requires updated SDK shim".to_owned())
                } else {
                    Ok(p)
                }
            };
            let open: Open = unsafe { std::mem::transmute(sym(b"san90_display_fft_open_v1\0")?) };
            let execute = unsafe {
                std::mem::transmute::<*mut c_void, Execute>(sym(b"san90_display_fft_execute_v1\0")?)
            };
            let close = unsafe {
                std::mem::transmute::<*mut c_void, Close>(sym(b"san90_display_fft_close_v1\0")?)
            };
            let (mut handle, mut points, mut rbw) = (std::ptr::null_mut(), 0, 0.0);
            let status = unsafe { open(&mut handle, size as u32, &mut points, &mut rbw) };
            if status != 0 || handle.is_null() || points < 2 || points as usize > size
                || !rbw.is_finite() || rbw <= 0.0 {
                if !handle.is_null() { unsafe { close(handle); } }
                return Err(format!("display FFT configuration rejected: status={status}, points={points}, rbw={rbw}"));
            }
            Ok(Self {
                library,
                handle,
                execute,
                close,
                points: points as usize,
                rbw,
                size,
            })
        })();
        if result.is_err() {
            unsafe {
                dlclose(library);
            }
        }
        result
    }
    fn process(&mut self, job: &Job) -> Result<(Vec<f64>, Vec<f32>), String> {
        let mut freq = vec![0.0; self.points];
        let mut power = vec![0.0; self.points];
        let status = unsafe {
            (self.execute)(
                self.handle,
                job.iq.as_ptr(),
                self.size as u32,
                job.meta.scale,
                job.meta.center,
                job.meta.rate,
                job.meta.bandwidth,
                freq.as_mut_ptr(),
                power.as_mut_ptr(),
                self.points as u32,
            )
        };
        if status != 0
            || freq.iter().any(|f| !f.is_finite())
            || power.iter().any(|p| !p.is_finite())
            || freq.windows(2).any(|f| f[0] >= f[1])
        {
            return Err(format!("display FFT data rejected: {status}"));
        }
        Ok((freq, power))
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Meta {
    pub epoch: u64,
    pub generation: u64,
    pub center: f64,
    pub rate: f64,
    pub bandwidth: f64,
    pub scale: f32,
}
struct Job {
    iq: Vec<i16>,
    meta: Meta,
    seq: u64,
    warning: i32,
}
pub(super) type Pan = (Vec<f64>, Vec<f32>, i32, u64, u64);

/// All samples within a job are adjacent. Skipped observations never join partial jobs.
#[derive(Default)]
struct Assembler {
    iq: Vec<i16>,
    meta: Option<Meta>,
    next_seq: Option<u64>,
    next_stamp: Option<u64>,
    next_start: u64,
    start: u64,
    warning: i32,
    resets: u64,
    stamp_resets: u64,
    sequence_resets: u64,
    geometry_resets: u64,
    scale_resets: u64,
    last_stamp_error: u64,
}
impl Assembler {
    fn push(
        &mut self,
        size: usize,
        meta: Meta,
        seq: u64,
        stamp: u64,
        iq: &[i16],
        warning: i32,
    ) -> Option<Job> {
        let tick = 125_000_000.0 / meta.rate;
        if !tick.is_finite()
            || tick < 1.0
            || (tick - tick.round()).abs() > 1e-6
            || iq.len() % 2 != 0
            || !meta.scale.is_finite()
            || meta.scale <= 0.0
            || !meta.center.is_finite()
            || meta.bandwidth <= 0.0
        {
            self.iq.clear();
            self.meta = None;
            self.resets += 1;
            return None;
        }
        // The production IQ consumer accepts +/-8 hardware timer ticks: the
        // 125 MHz timestamp is quantized independently of packet sample clocks.
        // Sequence and all capture/calibration fields must still match exactly.
        let stamp_error = self.next_stamp.map_or(0, |want| want.abs_diff(stamp));
        if self.meta != Some(meta) || self.next_seq != Some(seq) || stamp_error > 8 {
            self.stamp_resets += u64::from(stamp_error > 8);
            self.sequence_resets += u64::from(self.next_seq.is_some_and(|want| want != seq));
            self.geometry_resets += u64::from(self.meta.is_some_and(|old| {
                old.epoch != meta.epoch
                    || old.generation != meta.generation
                    || old.center != meta.center
                    || old.rate != meta.rate
                    || old.bandwidth != meta.bandwidth
            }));
            self.scale_resets += u64::from(self.meta.is_some_and(|old| old.scale != meta.scale));
            self.last_stamp_error = stamp_error;
            self.iq.clear();
            self.next_start = stamp;
            self.warning = 0;
            self.resets += 1;
        }
        self.meta = Some(meta);
        self.next_seq = seq.checked_add(1);
        self.next_stamp = stamp.checked_add(iq.len() as u64 / 2 * tick.round() as u64);
        if self.iq.is_empty() && stamp < self.next_start {
            return None;
        }
        if self.iq.is_empty() {
            self.start = stamp;
        }
        self.warning = if warning != 0 { warning } else { self.warning };
        let take = (2 * size - self.iq.len()).min(iq.len());
        self.iq.extend_from_slice(&iq[..take]);
        if self.iq.len() != 2 * size {
            return None;
        }
        // At most 20 independent observations/s, matching the default pan cadence.
        // Whole-packet start boundaries, no overlap and no zero padding.
        self.next_start = self
            .start
            .saturating_add(((125_000_000.0f64 / 20.0).max(size as f64 * tick)) as u64);
        Some(Job {
            iq: std::mem::take(&mut self.iq),
            meta,
            seq,
            warning: std::mem::take(&mut self.warning),
        })
    }
}

pub(super) struct Analyzer {
    pub size: usize,
    pub points: usize,
    pub rbw_ratio: f64,
    assembler: Assembler,
    tx: Option<mpsc::SyncSender<Job>>,
    rx: mpsc::Receiver<Result<(Meta, Pan), String>>,
    worker: Option<thread::JoinHandle<()>>,
    pub dropped: u64,
    pub completed: u64,
    timing: Arc<PLMutex<Timing>>,
}
impl Drop for Analyzer {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Analyzer {
    pub fn start(path: String, size: usize) -> Result<Self, String> {
        if ![32768, 65536].contains(&size) {
            return Err("display FFT size must be 0, 32768 or 65536".into());
        }
        let (tx, jobs) = mpsc::sync_channel::<Job>(1);
        let (output, rx) = mpsc::sync_channel(1);
        let (ready, init) = mpsc::sync_channel(1);
        let timing = Arc::new(PLMutex::new(Timing::default()));
        let times = timing.clone();
        let worker = thread::Builder::new()
            .name("sdr-display-fft".into())
            .spawn(move || {
                let mut dsp = match Dsp::open(&path, size) {
                    Ok(dsp) => dsp,
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                // Capability, calibration, finite output and CPU gate on actual SDK processing.
                let meta = Meta {
                    epoch: 0,
                    generation: 0,
                    center: 100e6,
                    rate: 62_500_000.0,
                    bandwidth: 50e6,
                    scale: 1e-6,
                };
                let iq = (0..size)
                    .flat_map(|i| {
                        let p = 2.0 * std::f64::consts::PI * 128.0 * i as f64 / size as f64;
                        [
                            (10000.0 * p.cos()).round() as i16,
                            (10000.0 * p.sin()).round() as i16,
                        ]
                    })
                    .collect();
                let job = Job {
                    iq,
                    meta,
                    seq: 0,
                    warning: 0,
                };
                let started = Instant::now();
                for _ in 0..8 {
                    match dsp.process(&job) {
                        Ok((_, power)) => {
                            let peak = power.into_iter().fold(f32::NEG_INFINITY, f32::max);
                            // Same vendor voltage/power convention as the default SDK path: V^2*20 mW.
                            if (peak + 26.9897).abs() > 0.05 {
                                let _ = ready.send(Err(format!(
                                    "display FFT calibration failed: {peak} dBm"
                                )));
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = ready.send(Err(e));
                            return;
                        }
                    }
                }
                let mean = started.elapsed().as_secs_f64() / 8.0;
                if mean > 0.010 {
                    let _ = ready.send(Err(format!(
                        "display FFT too slow: {:.2} ms",
                        mean * 1000.0
                    )));
                    return;
                }
                if ready.send(Ok((dsp.points, dsp.rbw))).is_err() {
                    return;
                }
                for job in jobs {
                    let start = Instant::now();
                    let result = dsp.process(&job).map(|(freq, power)| {
                        (
                            job.meta,
                            (freq, power, job.warning, job.meta.generation, job.seq),
                        )
                    });
                    times.lock().observe(start.elapsed());
                    let failed = result.is_err();
                    let _ = output.try_send(result);
                    if failed {
                        return;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        match init.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((points, rbw_ratio))) => Ok(Self {
                size,
                points,
                rbw_ratio,
                assembler: Assembler::default(),
                tx: Some(tx),
                rx,
                worker: Some(worker),
                dropped: 0,
                completed: 0,
                timing,
            }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => Err("display FFT initialization timed out; default retained".into()),
        }
    }
    pub fn push(
        &mut self,
        meta: Meta,
        seq: u64,
        stamp: u64,
        iq: &[i16],
        warning: i32,
    ) -> Result<Option<Pan>, String> {
        if !meta.scale.is_finite() || meta.scale <= 0.0 {
            return Err("display FFT calibration scale unavailable; using default".into());
        }
        let mut latest = None;
        loop {
            match self.rx.try_recv() {
                Ok(Ok((axis, pan))) => {
                    if axis == meta {
                        self.completed += 1;
                        latest = Some(pan);
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("display FFT worker stopped".into())
                }
            }
        }
        if let Some(job) = self
            .assembler
            .push(self.size, meta, seq, stamp, iq, warning)
        {
            match self.tx.as_ref().unwrap().try_send(job) {
                Ok(()) => (),
                Err(TrySendError::Full(_)) => self.dropped += 1,
                Err(TrySendError::Disconnected(_)) => {
                    return Err("display FFT worker stopped".into())
                }
            }
        }
        Ok(latest)
    }
    pub fn status(&self, rate: f64) -> Value {
        json!({"size":if self.completed>0 {self.size}else{0},"requested_size":self.size,
        "state":if self.completed>0 {"active"}else{"warming"},"output_points":self.points,"window":"FlatTop",
        "bin_spacing_hz":rate/self.size as f64,"enbw_hz":rate*self.rbw_ratio,"observation_ms":self.size as f64/rate*1000.0,
        "max_fps":20.0f64.min(rate/self.size as f64),"completed":self.completed,"dropped":self.dropped,
        "continuity_resets":self.assembler.resets,
        "continuity":{"stamp":self.assembler.stamp_resets,"sequence":self.assembler.sequence_resets,
            "geometry":self.assembler.geometry_resets,"scale":self.assembler.scale_resets,
            "last_stamp_error_ticks":self.assembler.last_stamp_error},
        "processing":self.timing.lock().snapshot()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_sdk_boundary_rejects_geometry_and_releases_failed_open() {
        struct Fixture(std::path::PathBuf, *mut c_void);
        impl Drop for Fixture {
            fn drop(&mut self) {
                unsafe { if !self.1.is_null() { dlclose(self.1); } }
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = std::env::temp_dir().join(format!("websa-display-geometry-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut fixture = Fixture(directory, std::ptr::null_mut());
        let source = fixture.0.join("shim.c");
        let library = fixture.0.join("shim.so");
        std::fs::write(&source, r#"
#include <stdint.h>
#include <math.h>
static int fault, active, marker;
void test_fault(int value) { fault = value; }
int test_active(void) { return active; }
int san90_display_fft_open_v1(void** out, uint32_t size, uint32_t* points, double* rbw) {
    *out = &marker; ++active; *points = size * 4 / 5; *rbw = 3.77 / size;
    if (fault == 1) *points = 0;
    if (fault == 2) *points = size + 1;
    if (fault == 3) *points = UINT32_MAX;
    if (fault == 4) *rbw = NAN;
    if (fault == 5) *rbw = INFINITY;
    if (fault == 6) *rbw = 0;
    if (fault == 7) *rbw = -1;
    return fault == 8 ? -120 : 0;
}
int san90_display_fft_execute_v1(void* handle, const int16_t* iq, uint32_t n, float scale,
    double center, double rate, double bandwidth, double* frequency, float* power, uint32_t count) { return 0; }
int san90_display_fft_close_v1(void* handle) { if (handle == &marker) --active; return 0; }
"#).unwrap();
        assert!(std::process::Command::new("cc").args(["-shared", "-fPIC", "-O1"])
            .arg(&source).arg("-o").arg(&library).status().unwrap().success());
        let path = library.to_str().unwrap();
        let cpath = CString::new(path).unwrap();
        fixture.1 = unsafe { dlopen(cpath.as_ptr(), 2) };
        assert!(!fixture.1.is_null());
        let fault: unsafe extern "C" fn(c_int) = unsafe { std::mem::transmute(dlsym(fixture.1, c"test_fault".as_ptr())) };
        let active: unsafe extern "C" fn() -> c_int = unsafe { std::mem::transmute(dlsym(fixture.1, c"test_active".as_ptr())) };
        let mut failures = Vec::new();
        for value in 1..=8 {
            unsafe { fault(value); }
            let opened = Dsp::open(path, 32768);
            if opened.is_ok() { failures.push(format!("accepted invalid geometry fault {value}")); }
            drop(opened);
            if unsafe { active() } != 0 { failures.push(format!("open fault {value} retained its native handle")); }
        }
        unsafe { fault(0); }
        let dsp = Dsp::open(path, 32768).expect("valid reopen after rejected metadata");
        assert_eq!(dsp.points, 32768 * 4 / 5);
        drop(dsp);
        if unsafe { active() } != 0 { failures.push("native handle retained after valid close".into()); }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }

    #[test]
    fn invalid_or_illustrative_sources_cannot_claim_a_real_fft_tier() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        let center = state.sdr_center;
        let version = state.config_version;
        for request in [
            json!({}),
            json!({"size":16384}),
            json!({"size":32768.5}),
            json!({"size":"65536"}),
            json!({"size":65536}),
        ] {
            assert!(dispatch(&mut state, "SET_SDR_FFT", &request).is_err());
            assert_eq!(state.config_version, version);
            assert_eq!(state.sdr_center, center);
            assert!(state.pipeline.is_none());
        }
    }
    fn meta() -> Meta {
        Meta {
            epoch: 1,
            generation: 1,
            center: 100e6,
            rate: 62_500_000.0,
            bandwidth: 50e6,
            scale: 1e-6,
        }
    }
    #[test]
    fn never_joins_discontinuous_observations() {
        for fault in 0..7 {
            let mut a = Assembler::default();
            let m = meta();
            assert!(a.push(32768, m, 0, 0, &vec![1; 32480], 0).is_none());
            let mut n = m;
            let (mut seq, mut stamp) = (1, 32480);
            match fault {
                0 => seq += 1,
                1 => stamp += 9,
                2 => n.epoch += 1,
                3 => n.generation += 1,
                4 => n.rate /= 2.0,
                5 => n.center += 1.0,
                _ => n.scale *= 2.0,
            }
            assert!(a.push(32768, n, seq, stamp, &vec![2; 32480], 0).is_none());
            assert_eq!(a.iq.len(), 32480);
            assert!(a.iq.iter().all(|&v| v == 2));
        }
    }
    #[test]
    fn accepts_the_same_timer_quantization_as_the_production_iq_consumer() {
        let mut a = Assembler::default();
        let m = meta();
        assert!(a.push(32768, m, 0, 0, &vec![1; 32480], 0).is_none());
        assert!(a.push(32768, m, 1, 32481, &vec![2; 32480], 0).is_none());
        let job = a.push(32768, m, 2, 64960, &vec![3; 32480], 0).unwrap();
        assert_eq!(job.iq.len(), 65536);
        assert_eq!(a.resets, 1); // Initial acquisition only.
    }
    #[test]
    fn exact_long_window_has_no_padding_overlap_or_time_average() {
        let mut a = Assembler::default();
        let m = meta();
        for seq in 0..2 {
            assert!(a
                .push(32768, m, seq, seq * 32480, &vec![seq as i16; 32480], 0)
                .is_none());
        }
        let job = a.push(32768, m, 2, 64960, &vec![2; 32480], 0).unwrap();
        assert_eq!(job.iq.len(), 65536);
        assert_eq!(&job.iq[64960..], &vec![2; 576]);
        assert!(a.push(32768, m, 3, 97440, &vec![3; 32480], 0).is_none());
        assert!(a.iq.is_empty());
    }
    #[test]
    #[ignore = "requires the installed vendor DSP library; no radio is opened"]
    fn sdk_tiers_pass_real_processing_and_calibration() {
        let path = std::env::var("WEBSA_TEST_FFT_SHIM").unwrap();
        for size in [32768, 65536] {
            let a = Analyzer::start(path.clone(), size).unwrap();
            assert!(a.points > size * 7 / 10);
        }
    }
}
