//! Exclusive SAN-90 vendor phase-noise (PNM) owner for the opt-in Rust WebSA path.
//! Calls synchronously dispatch to the production shim's permanent SDK thread;
//! the vendor cycle machine (start/partial updates/done) lives there and retains
//! the physical session between Rust mode owners.
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

use parking_lot::Mutex;

use crate::hardware_swp::GnssInfo;

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

type Open = unsafe extern "C" fn(
    *mut *mut c_void,
    f64,
    f32,
    f64,
    f64,
    f64,
    u32,
    *mut u32,
    *mut u32,
    *mut u32,
) -> c_int;
type Next = unsafe extern "C" fn(
    *mut c_void,
    *mut f64,
    *mut f32,
    *mut f64,
    *mut f32,
    u32,
    *mut u32,
    *mut f32,
    *mut u32,
    *mut u32,
) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
#[allow(clippy::too_many_arguments)]
type Gnss = unsafe extern "C" fn(
    *mut c_void,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut f32,
    *mut f32,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
) -> c_int;

/// Vendor PNM profile request, one field per WebSA SET_PNM wire field.
#[derive(Clone, Copy)]
pub struct PnmParams {
    pub center: f64,
    pub threshold: f32,
    pub rbw_ratio: f64,
    pub start_offset: f64,
    pub stop_offset: f64,
    pub trace_average: u32,
}

pub struct PnmFrame {
    pub carrier_hz: f64,
    pub carrier_dbm: f32,
    pub offset: Vec<f64>,
    pub pn: Vec<f32>,
    pub ref_level: f32,
    pub progress: u32,
    pub done: bool,
}

struct Device {
    library: *mut c_void,
    handle: *mut c_void,
    next: Next,
    close: Close,
    gnss: Option<Gnss>,
    /// Stale after a bus error (-8/-9): never hand the handle back to the vendor library.
    abandoned: bool,
    points: u32,
}

unsafe impl Send for Device {}

impl Device {
    fn open(path: &str, params: &PnmParams) -> Result<Self, String> {
        let path = CString::new(path).map_err(|_| "invalid SDK shim path")?;
        let library = unsafe { dlopen(path.as_ptr(), crate::hardware_swp::SDK_DLOPEN_FLAGS) };
        if library.is_null() {
            return Err(format!(
                "SDK shim: {}",
                unsafe { CStr::from_ptr(dlerror()) }.to_string_lossy()
            ));
        }
        let result = (|| {
            unsafe { crate::hardware_swp::register_sdk_session(library)?; }
            let symbol = |name: &'static [u8]| -> Result<*mut c_void, String> {
                let ptr = unsafe { dlsym(library, name.as_ptr().cast()) };
                (!ptr.is_null())
                    .then_some(ptr)
                    .ok_or_else(|| format!("SDK shim missing {}", String::from_utf8_lossy(name)))
            };
            let open: Open = unsafe { std::mem::transmute(symbol(b"san90_pnm_open_v1\0")?) };
            let next: Next = unsafe { std::mem::transmute(symbol(b"san90_pnm_next_v1\0")?) };
            let close: Close = unsafe { std::mem::transmute(symbol(b"san90_pnm_close_v1\0")?) };
            let (mut handle, mut points, mut segments, mut partial) =
                (std::ptr::null_mut(), 0, 0, 0);
            let status = unsafe {
                open(
                    &mut handle,
                    params.center,
                    params.threshold,
                    params.rbw_ratio,
                    params.start_offset,
                    params.stop_offset,
                    params.trace_average,
                    &mut points,
                    &mut segments,
                    &mut partial,
                )
            };
            if status != 0 || handle.is_null() || points < 2 || segments < 1 || partial < 1 {
                if !handle.is_null() {
                    unsafe {
                        close(handle);
                    }
                }
                return Err(format!(
                    "SDK PNM open rejected status={status} points={points} segments={segments}"
                ));
            }
            let gnss_ptr = unsafe { dlsym(library, b"san90_gnss_v1\0".as_ptr().cast()) };
            let gnss: Option<Gnss> =
                (!gnss_ptr.is_null()).then(|| unsafe { std::mem::transmute(gnss_ptr) });
            Ok(Self {
                library,
                handle,
                next,
                close,
                gnss,
                abandoned: false,
                points,
            })
        })();
        if result.is_err() {
            unsafe {
                dlclose(library);
            }
        }
        result
    }

    /// One partial-update attempt: `Ok(None)` means "no new data yet, poll again".
    fn next(&mut self) -> Result<Option<PnmFrame>, String> {
        let points = self.points as usize;
        let mut carrier_hz = 0.0;
        let mut carrier_dbm = 0.0f32;
        let mut offset = vec![0.0f64; points];
        let mut pn = vec![0.0f32; points];
        let mut written = 0;
        let mut ref_level = 0.0f32;
        let (mut progress, mut done) = (0, 0);
        let status = unsafe {
            (self.next)(
                self.handle,
                &mut carrier_hz,
                &mut carrier_dbm,
                offset.as_mut_ptr(),
                pn.as_mut_ptr(),
                self.points,
                &mut written,
                &mut ref_level,
                &mut progress,
                &mut done,
            )
        };
        match status {
            0 => {
                if written as usize != points
                    || offset.iter().any(|v| !v.is_finite())
                    || pn.iter().any(|v| !v.is_finite())
                    || !carrier_hz.is_finite()
                {
                    return Err("SDK PNM returned invalid data".into());
                }
                Ok(Some(PnmFrame {
                    carrier_hz,
                    carrier_dbm,
                    offset,
                    pn,
                    ref_level,
                    progress,
                    done: done != 0,
                }))
            }
            1 => Ok(None),
            other if other >= 2_000_000 => Err(format!(
                "SDK PNM StartMeasure status={}",
                other - 2_000_000
            )),
            other => {
                unsafe { let _ = crate::hardware_swp::invalidate_sdk_session(self.library, self.handle, other == -8 || other == -9); }
                if other == -8 || other == -9 {
                    self.abandoned = true;
                }
                Err(format!("SDK PNM next status={other}"))
            }
        }
    }
}

impl Device {
    fn gnss(&mut self) -> Result<GnssInfo, String> {
        let query = self.gnss.ok_or("SDK shim has no GNSS entry")?;
        let mut info = GnssInfo::default();
        let status = unsafe {
            query(
                self.handle,
                &mut info.lock,
                &mut info.sats,
                &mut info.docxo,
                &mut info.docxo_mode,
                &mut info.antenna,
                &mut info.latitude,
                &mut info.longitude,
                &mut info.altitude,
                &mut info.year,
                &mut info.month,
                &mut info.day,
                &mut info.hour,
                &mut info.minute,
                &mut info.second,
            )
        };
        if status != 0 {
            if status == -8 || status == -9 {
                self.abandoned = true;
            }
            return Err(format!("SDK GNSS status={status}"));
        }
        Ok(info)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Err(error) = unsafe {
            crate::hardware_swp::release_sdk_mode(
                self.library, self.handle, self.close, self.abandoned,
            )
        } {
            eprintln!("SDK PNM {error}");
        }
    }
}

pub struct HardwarePnm {
    pub device_info: Option<crate::hardware_swp::DeviceInfo>,
    next_tx: SyncSender<SyncSender<Result<Option<PnmFrame>, String>>>,
    gnss_tx: SyncSender<SyncSender<Result<GnssInfo, String>>>,
    error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
}

unsafe impl Send for HardwarePnm {}

impl HardwarePnm {
    pub fn start(path: String, params: PnmParams) -> Result<Self, String> {
        let (meta_tx, meta_rx) = mpsc::sync_channel(1);
        let (gnss_tx, gnss_rx) = mpsc::sync_channel::<SyncSender<Result<GnssInfo, String>>>(1);
        let (next_tx, next_rx) =
            mpsc::sync_channel::<SyncSender<Result<Option<PnmFrame>, String>>>(1);
        let error = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let owner_error = error.clone();
        let owner_stop = stop.clone();
        let owner = std::thread::spawn(move || match Device::open(&path, &params) {
            Ok(mut device) => {
                let info = crate::hardware_swp::read_device_info(device.library, device.handle).ok().flatten();
                if meta_tx.send(Ok(info)).is_err() {
                    return;
                }
                loop {
                    if owner_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(reply) = gnss_rx.try_recv() {
                        let _ = reply.send(device.gnss());
                        continue;
                    }
                    match next_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(reply) => {
                            if owner_stop.load(Ordering::Acquire) {
                                break;
                            }
                            let result = device.next();
                            if let Err(err) = &result {
                                *owner_error.lock() = Some(err.clone());
                            }
                            if reply.send(result).is_err() {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
            Err(e) => {
                let _ = meta_tx.send(Err(e));
            }
        });
        let device_info = meta_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "SDK PNM startup timed out; owner outcome unknown".to_owned())??;
        Ok(Self {
            device_info,
            next_tx,
            gnss_tx,
            error,
            stop,
            owner: Some(owner),
        })
    }

    /// GNSS snapshot; runs on the owner thread with every other vendor call.
    pub fn gnss(&self) -> Result<GnssInfo, String> {
        if let Some(e) = self.error.lock().clone() {
            return Err(e);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.gnss_tx
            .send(reply_tx)
            .map_err(|_| "SDK PNM owner closed".to_owned())?;
        reply_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "SDK GNSS query stalled".to_owned())?
    }

    pub fn next(&self) -> Result<Option<PnmFrame>, String> {
        if let Some(e) = self.error.lock().clone() {
            return Err(e);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.next_tx
            .send(reply_tx)
            .map_err(|_| "SDK PNM owner closed".to_owned())?;
        reply_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "SDK PNM fetch stalled".to_owned())?
    }
}

impl Drop for HardwarePnm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Joining is required: the vendor SDK cannot overlap sessions (see hardware_swp).
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires /opt/htraapi SDK headers and libhtraapi.so.0.55.100; no hardware access"]
    fn production_shim_retains_physical_handle_slot_until_workers_retire() {
        let sdk = std::path::Path::new("/opt/htraapi");
        let source = format!("#include {:?}\n{}", concat!(env!("CARGO_MANIFEST_DIR"), "/sdk/san90_sdk.cpp"), r#"
#include <cassert>
static int physical_device;
static void** retained_slot = nullptr;
static bool workers_live = false;
static bool fail_open = false;
static int opens = 0;
static int closes = 0;
static int close_attempts = 0;
static int stops = 0;
static std::thread::id sdk_thread;
static void observe_worker() {
    assert(san90_session.on_thread());
    assert(std::this_thread::get_id() == sdk_thread);
    assert(workers_live);
    // Check identity before dereferencing: the old local slot is already dead.
    assert(retained_slot == &san90_session.handle);
    assert(*retained_slot == &physical_device);
}
extern "C" int Device_Open(void** slot, int, const BootProfile_TypeDef*, BootInfo_TypeDef* info) {
    assert(san90_session.on_thread());
    assert(!workers_live);
    sdk_thread = std::this_thread::get_id();
    ++opens;
    *info = {};
    *slot = &physical_device;
    if (fail_open) return -1;
    retained_slot = slot;
    workers_live = true;
    return 0;
}
extern "C" int Device_GetHardwareState(void** slot, HardWareState_TypeDef* state) {
    observe_worker();
    assert(slot == retained_slot);
    *state = {};
    return 0;
}
extern "C" int Device_Close(void** slot) {
    ++close_attempts;
    observe_worker();
    assert(slot == retained_slot);
    ++closes;
    // Model the SDK joining its retained-slot workers before clearing the handle.
    workers_live = false;
    retained_slot = nullptr;
    *slot = nullptr;
    return 0;
}
extern "C" int PNM_StopMeasure(void** slot) {
    observe_worker();
    assert(*slot == &physical_device);
    ++stops;
    return 0;
}
// Exercise the real session dispatcher and shared production open path without
// configuring RF. Mode retirement below uses the actual exported close paths.
template <typename State> static int open_probe(State* state) {
    return san90_session.call(nullptr, [&] { return san90_open_device(state); });
}
template <typename State> static void next_probe(State* state) {
    assert(san90_session.call(state, [&] {
        observe_worker();
        assert(state->handle == *retained_slot);
        assert(san90_session.active == state);
        return 0;
    }) == 0);
}
int main() {
    auto* pnm = new San90PnmDevice{};
    assert(open_probe(pnm) == 0);
    next_probe(pnm); // A separate dispatch after Device_Open's stack has returned.
    assert(san90_pnm_close_v1(pnm) == 0);
    assert(stops == 1 && closes == 0 && workers_live);

    auto* iqs = new San90Device{};
    assert(open_probe(iqs) == 0);
    next_probe(iqs); // Cached physical session survives replacing the mode wrapper.
    assert(opens == 1);
    assert(san90_close(iqs) == 0);

    pnm = new San90PnmDevice{};
    assert(open_probe(pnm) == 0);
    next_probe(pnm);
    assert(san90_session_invalidate_v1(pnm, 0) == 0);
    assert(san90_pnm_close_v1(pnm) == 0);
    assert(stops == 2 && closes == 1 && !workers_live);

    // A rejected open may write its output; it must not be treated as a live
    // session or passed to Device_Close, and the next open must still work.
    fail_open = true;
    pnm = new San90PnmDevice{};
    assert(open_probe(pnm) == -1);
    delete pnm;
    assert(san90_session.call(nullptr, [&] {
        assert(!san90_session.handle && !san90_session.active);
        return 0;
    }) == 0);
    assert(closes == 1 && !workers_live);
    fail_open = false;
    pnm = new San90PnmDevice{};
    const int opens_at_reject = opens;
    const int closes_at_reject = closes;
    const int stops_at_reject = stops;
    assert(open_probe(pnm) == 0);
    next_probe(pnm);
    assert(opens == opens_at_reject + 1 && workers_live);
    assert(san90_pnm_close_v1(pnm) == 0);
    // Healthy mode close stops PNM and retains the physical handle. The
    // invalidate above is still the only Device_Close.
    assert(stops == stops_at_reject + 1 && closes == closes_at_reject && workers_live);
    // Idle release closes the physical handle and leaves the worker reusable.
    // A second release owns nothing: success, and Device_Close is not repeated.
    const int closes_before_idle = closes;
    assert(san90_session_release_v1() == 0);
    assert(san90_session_release_v1() == 0);
    assert(closes == closes_before_idle + 1 && close_attempts == closes && !workers_live);
    assert(san90_session.call(nullptr, [&] {
        assert(!san90_session.handle && !san90_session.active && !san90_session.healthy);
        assert(san90_session.on_thread());
        return 0;
    }) == 0);
    // A non-null unhealthy pointer is stale: refuse and never Device_Close it.
    const int close_attempts_before_stale = close_attempts;
    assert(san90_session.call(nullptr, [&] {
        san90_session.handle = &physical_device;
        san90_session.healthy = false;
        return 0;
    }) == 0);
    assert(san90_session_release_v1() == -111);
    assert(close_attempts == close_attempts_before_stale);
    assert(san90_session.call(nullptr, [&] {
        assert(san90_session.handle == &physical_device && !san90_session.healthy);
        san90_session.handle = nullptr;
        return 0;
    }) == 0);
    pnm = new San90PnmDevice{};
    const int opens_before_reuse = opens;
    const int closes_before_reuse = closes;
    assert(open_probe(pnm) == 0);
    next_probe(pnm);
    assert(opens == opens_before_reuse + 1 && closes == closes_before_reuse && workers_live);
    // An active mode must not overlap Device_Close.
    const int close_attempts_while_active = close_attempts;
    assert(san90_session_release_v1() == -110);
    assert(opens == opens_before_reuse + 1 && closes == closes_before_reuse
           && close_attempts == close_attempts_while_active && workers_live);
    const int stops_before_retire = stops;
    assert(san90_pnm_close_v1(pnm) == 0);
    assert(stops == stops_before_retire + 1 && closes == closes_before_reuse && workers_live);
    assert(san90_session_release_v1() == 0);
    assert(san90_session_release_v1() == 0);
    assert(opens == opens_before_reuse + 1 && closes == closes_before_reuse + 1
           && close_attempts == closes && stops == stops_before_retire + 1 && !workers_live);
    assert(san90_session_finish_v1() == 0);
    assert(opens == opens_before_reuse + 1 && closes == closes_before_reuse + 1
           && close_attempts == closes && stops == stops_before_retire + 1 && !workers_live);
}
"#);
        let dir = std::env::temp_dir().join(format!("websa_pnm_slot_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("adapter.cpp");
        let executable = dir.join("adapter");
        std::fs::write(&input, source).unwrap();
        let status = std::process::Command::new("g++")
            .args(["-O2", "-std=c++17", "-pthread", "-I"])
            .arg(sdk.join("inc"))
            .arg(&input)
            .arg("-L").arg(sdk.join("lib/x86_64"))
            .arg("-Wl,-rpath,/opt/htraapi/lib/x86_64")
            .arg("-l:libhtraapi.so.0.55.100")
            .arg("-o").arg(&executable)
            .status().expect("g++ and SDK development files required for production shim contract");
        assert!(status.success(), "production adapter fixture failed to compile");
        assert!(std::process::Command::new(&executable).status().unwrap().success());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_shim_or_old_v7_shim_fails_without_device() {
        let params = PnmParams {
            center: 100e6,
            threshold: -50.0,
            rbw_ratio: 0.02,
            start_offset: 100.0,
            stop_offset: 10e6,
            trace_average: 4,
        };
        assert!(HardwarePnm::start("/does/not/exist.so".into(), params).is_err());
        // v7 has SWP but no PNM symbols: dlsym must fail cleanly.
        if let Ok(home) = std::env::var("HOME") {
            let v7 = format!("{home}/.local/share/harogic-websa/shims/libwebsa_san90_sdk_v7.so");
            if std::path::Path::new(&v7).exists() {
                assert!(HardwarePnm::start(v7, params).is_err());
            }
        }
    }
}
