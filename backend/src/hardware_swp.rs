//! SAN-90 SWP request owner for the opt-in Rust WebSA path.
//! Production shim calls synchronously dispatch to its permanent SDK thread, which
//! retains the physical handle across mode owners and preserves vendor thread affinity.
//! This module also holds the contracts shared by every hardware owner module.
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

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

/// Linux RTLD_NOW | RTLD_NODELETE: cached SDK state outlives every Rust mode owner,
/// including failed opens. The vendor module must remain mapped until process exit.
pub(crate) const SDK_DLOPEN_FLAGS: c_int = 2 | 0x1000;
type SessionFinish = unsafe extern "C" fn() -> c_int;
type SessionInvalidate = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
static SDK_SESSION: Mutex<Option<SessionFinish>> = Mutex::new(None);

/// Registers the production session ABI. Older stateless fixture shims need neither entry.
pub(crate) unsafe fn register_sdk_session(library: *mut c_void) -> Result<(), String> {
    let finish = unsafe { dlsym(library, b"san90_session_finish_v1\0".as_ptr().cast()) };
    let invalidate = unsafe { dlsym(library, b"san90_session_invalidate_v1\0".as_ptr().cast()) };
    if finish.is_null() && invalidate.is_null() { return Ok(()); }
    if finish.is_null() || invalidate.is_null() {
        return Err("SDK shim has an incomplete shared session ABI".into());
    }
    let mut session = SDK_SESSION.lock();
    if session.as_ref().is_some_and(|old| *old as usize != finish as usize) {
        return Err("cannot use two physical SDK session shims in one process".into());
    }
    *session = Some(unsafe { std::mem::transmute::<*mut c_void, SessionFinish>(finish) });
    Ok(())
}

/// Terminal process shutdown, after all mode owners have been dropped and joined.
pub fn finish_sdk_session() -> Result<(), String> {
    let session = SDK_SESSION.lock();
    let status = session.as_ref().map_or(0, |finish| unsafe { finish() });
    if status == 0 { Ok(()) } else { Err(format!("SDK session finish status={status}")) }
}

/// Marks the physical session for recovery; the following mode close retires its buffers.
pub(crate) unsafe fn invalidate_sdk_session(
    library: *mut c_void, handle: *mut c_void, abandon: bool,
) -> Result<bool, String> {
    let entry = unsafe { dlsym(library, b"san90_session_invalidate_v1\0".as_ptr().cast()) };
    if entry.is_null() { return Ok(false); }
    let invalidate: SessionInvalidate = unsafe { std::mem::transmute(entry) };
    let status = unsafe { invalidate(handle, i32::from(abandon)) };
    if status == 0 { Ok(true) } else { Err(format!("SDK session invalidate status={status}")) }
}

/// A stale wrapper is safe to destroy only through a shim that can abandon its SDK pointer.
pub(crate) unsafe fn release_sdk_mode(
    library: *mut c_void, handle: *mut c_void,
    close: unsafe extern "C" fn(*mut c_void) -> c_int, abandoned: bool,
) -> Result<(), String> {
    if abandoned && !unsafe { invalidate_sdk_session(library, handle, true)? } {
        return Ok(());
    }
    let status = unsafe { close(handle) };
    if status == 0 { Ok(()) } else { Err(format!("SDK mode release status={status}")) }
}

/// Front-end and clock fields shared by the SWP/RTA/IQS profiles. Layout = `San90FrontEnd`
/// in sdk/san90_sdk.cpp.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrontEnd {
    pub ref_dbm: f64,
    /// -1 = auto.
    pub atten: c_int,
    /// 0 = AutoOn, 1 = ForcedOff (Python `preamplifier`).
    pub preamp: c_int,
    pub ifgain: c_int,
    /// 0 = LowNoisePreferred, 1 = HighLinearityPreferred.
    pub gain_strategy: c_int,
    /// `ReferenceClockSource_TypeDef`, see [`ref_clock_code`].
    pub ref_clock: c_int,
    pub refclk_out: c_int,
}

/// WebSA `ref_clock` wire name to `ReferenceClockSource_TypeDef`.
pub fn ref_clock_code(name: &str) -> c_int {
    match name {
        "external" => 1,
        "premium" => 2,
        "external_forced" => 3,
        _ => 0,
    }
}

/// One failed vendor call. `status` is the vendor status; 0 marks a local rejection.
#[derive(Clone, Debug)]
pub struct SdkFailure {
    pub status: i32,
    pub message: String,
}

/// Vendor warnings (htra_api.h): the frame is missing, the handle stays usable.
pub const WARN_STATUS: [i32; 9] = [-10, -11, -12, -14, -15, -16, -17, -18, -19];
/// Vendor bus errors: the handle is stale; never pass it to Device_Close.
pub const LINK_LOST_STATUS: [i32; 2] = [-8, -9];

/// Owner thread whose vendor open did not return within the start timeout. A new session
/// must not open while it runs (vendor rule: sessions must not overlap).
static STUCK_OWNER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Wait time for one vendor open (Python LINK_CALL_TIMEOUT).
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Refuses a new vendor session while a previous owner thread is still inside the SDK.
pub fn owner_slot_free() -> Result<(), String> {
    let mut stuck = STUCK_OWNER.lock();
    match stuck.as_ref() {
        Some(owner) if !owner.is_finished() => {
            Err("previous SDK session is still inside the vendor library".into())
        }
        Some(_) => {
            if let Some(owner) = stuck.take() {
                let _ = owner.join();
            }
            Ok(())
        }
        None => Ok(()),
    }
}

/// Keeps an owner whose open timed out; [`owner_slot_free`] blocks new sessions until it exits.
pub fn park_stuck_owner(owner: JoinHandle<()>) {
    *STUCK_OWNER.lock() = Some(owner);
}

type Open = unsafe extern "C" fn(*mut *mut c_void, *const SwpRequest, *mut SwpActual) -> c_int;
type Reconfig = unsafe extern "C" fn(*mut c_void, *const SwpRequest, *mut SwpActual) -> c_int;
type Next = unsafe extern "C" fn(*mut c_void, *mut f64, *mut f32, u32, *mut u32, *mut f64) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
#[allow(clippy::too_many_arguments)]
type Defaults = unsafe extern "C" fn(
    *mut c_void,
    *mut f64,
    *mut f64,
    *mut f64,
    *mut f64,
    *mut f64,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut f64,
) -> c_int;
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

/// Layout = `San90SwpRequest` in sdk/san90_sdk.cpp.
#[repr(C)]
struct SwpRequest {
    center_hz: f64,
    span_hz: f64,
    fe: FrontEnd,
    rbw_mode: c_int,
    rbw_hz: f64,
    vbw_mode: c_int,
    vbw_hz: f64,
    window: c_int,
    sweep_time_mode: c_int,
    sweep_time: f64,
    trace_points: c_int,
    points_strategy: c_int,
    trace_align: c_int,
    spur: c_int,
    trace_detect_mode: c_int,
    trace_detector: c_int,
    frame_detector: c_int,
}

/// Layout = `San90SwpActual` in sdk/san90_sdk.cpp.
#[repr(C)]
#[derive(Default)]
struct SwpActual {
    start_hz: f64,
    stop_hz: f64,
    points: u32,
    sweep_ms: f64,
    ref_dbm: f64,
    rbw_hz: f64,
    vbw_hz: f64,
    refclk_hz: f64,
    refclk_src: c_int,
}

/// Versioned configuration readback shared by the SWP/IQS query exports.
/// Preamplifier is the SDK AutoOn/ForcedOff configuration, not a physical on bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrontEndActual {
    pub ref_dbm: f64,
    pub atten: c_int,
    pub preamp: c_int,
    pub ifgain: c_int,
}

type FrontEndQuery = unsafe extern "C" fn(*mut c_void, *mut FrontEndActual) -> c_int;

/// Missing/failed optional queries leave device facts unknown. Existing configure
/// ABIs keep their original size, so an older shim cannot overwrite a short struct.
pub fn read_front_end_actual(
    library: *mut c_void, handle: *mut c_void, symbol: &'static [u8],
) -> Option<FrontEndActual> {
    let entry = unsafe { dlsym(library, symbol.as_ptr().cast()) };
    if entry.is_null() { return None; }
    let query: FrontEndQuery = unsafe { std::mem::transmute(entry) };
    let mut actual = FrontEndActual::default();
    (unsafe { query(handle, &mut actual) } == 0 && actual.ref_dbm.is_finite())
        .then_some(actual)
}
/// Packet-refreshed vendor GNSS snapshot (`Device_GetGNSSInfo`).
/// Shared by every hardware owner; queried on the permanent SDK thread.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct GnssInfo {
    pub lock: i32,
    pub sats: i32,
    pub docxo: i32,
    pub docxo_mode: i32,
    pub antenna: i32,
    pub latitude: f32,
    pub longitude: f32,
    pub altitude: i32,
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
}

/// Physical session boot result. The prefix through `errors` matches `San90DeviceInfo`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceInfo {
    pub uid: u64,
    pub model: u32,
    pub hw: u32,
    pub mfw: u32,
    pub ffw: u32,
    pub bus_speed: u32,
    pub bus_ver: u32,
    pub api_ver: u32,
    pub pmu: u32,
    pub agu: u32,
    pub warnings: i32,
    pub errors: i32,
    // Optional hardware-state export; these fields are not part of the v1 boot ABI.
    pub ocxo_type: i32,
    pub internal_ocxo: i32,
}

impl DeviceInfo {
    pub fn detail(&self) -> serde_json::Value {
        let firmware = |v: u32| format!("{}.{}.{}", v >> 16, (v >> 8) & 255, v & 255);
        let auxiliary = |v: u32| format!("0.{}.{}({v})", (v >> 8) & 255, v & 255);
        let model = match self.model {
            10..=15 => format!("E90 R{}", self.model - 10),
            20..=23 => format!("E200 R{}", self.model - 20),
            33..=34 => format!("TRX60 R{}", self.model - 30),
            40 => "ZRX200 R0".into(),
            53 => "N60 R4".into(), 54 => "M60 R4".into(), 55 => "N45 R4".into(),
            56 => "M80 R5".into(), 57 => "M60 R5".into(), 58 => "N60 R5".into(),
            59 => "N45 R5".into(), 60 => "E310 R0".into(), 61 => "E310 R1".into(),
            65 => "N45 R6".into(), 66 => "N60 R6".into(), 67 => "N90 R0".into(),
            68 => "PN90 R0NDS".into(), 69 => "N91 R0".into(), 70 => "E330 R0".into(),
            80 => "TX90 R0".into(), 90 => "N400 R0".into(), 91 => "N400 R1".into(),
            92 => "N400 R2".into(), 93 => "N150 R1".into(), 94 => "N400 R2V2".into(),
            95 => "N400 R3".into(), 96 => "N400 R4".into(), 100 => "SIMULATOR".into(),
            110 => "RX90 R0".into(), 120 => "A24 R0".into(), 121 => "A24 R1".into(),
            130 => "C24 R0".into(), 131 => "C24 R1".into(), 140 => "W200 R3".into(),
            141 => "W200 R2".into(), 144 => "W200 R4".into(),
            other => format!("Model {other}"),
        };
        serde_json::json!({
            "uid": format!("{:016X}", self.uid), "model": self.model, "model_name": model,
            "hw": self.hw, "mfw": self.mfw, "mfw_str": firmware(self.mfw),
            "ffw": self.ffw, "ffw_str": firmware(self.ffw), "bus_speed": self.bus_speed,
            "bus_ver": self.bus_ver, "bus_str": firmware(self.bus_ver),
            "api_ver": self.api_ver, "api_str": firmware(self.api_ver),
            "pmu": self.pmu, "pmu_str": auxiliary(self.pmu),
            "agu": self.agu, "agu_str": auxiliary(self.agu),
            "warnings": self.warnings, "errors": self.errors,
            "ocxo_type": (self.ocxo_type >= 0).then_some(self.ocxo_type),
            "internal_ocxo": (self.internal_ocxo >= 0).then_some(self.internal_ocxo != 0),
            "has_docxo": self.ocxo_type == 2,
            "has_premium_ref_clock": matches!(self.ocxo_type, 1 | 2) || self.internal_ocxo > 0,
        })
    }
}

/// Optional for older fixture shims; a missing export never fabricates device values.
/// Call during mode startup on its owner, before arming continuous IQ capture.
pub fn read_device_info(library: *mut c_void, session: *mut c_void) -> Result<Option<DeviceInfo>, String> {
    let entry = unsafe { dlsym(library, b"san90_device_info_v1\0".as_ptr().cast()) };
    if entry.is_null() { return Ok(None); }
    let query: unsafe extern "C" fn(*mut c_void, *mut DeviceInfo) -> c_int =
        unsafe { std::mem::transmute(entry) };
    let mut info = DeviceInfo::default();
    let status = unsafe { query(session, &mut info) };
    if status != 0 { return Err(format!("SDK device info status={status}")); }
    // The v1 ABI's trailing padding overlaps extension fields; restore extension
    // sentinels after querying the prefix.
    info.ocxo_type = -1;
    info.internal_ocxo = -1;
    let capability = unsafe { dlsym(library, b"san90_clock_capabilities_v1\0".as_ptr().cast()) };
    if !capability.is_null() {
        let query: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> c_int =
            unsafe { std::mem::transmute(capability) };
        let mut ocxo_type = -1;
        let mut internal_ocxo = -1;
        if unsafe { query(session, &mut ocxo_type, &mut internal_ocxo) } == 0 {
            info.ocxo_type = ocxo_type;
            info.internal_ocxo = internal_ocxo;
        }
    }
    Ok(Some(info))
}

/// Vendor SWP profile request, one field per existing WebSA SWP wire field.
#[derive(Clone, PartialEq)]
pub struct SwpParams {
    pub center: f64,
    pub span: f64,
    pub front_end: FrontEnd,
    pub rbw_mode: String,
    pub rbw: f64,
    pub vbw_mode: String,
    pub vbw: f64,
    pub window: i64,
    pub sweep_time_mode: i64,
    pub sweep_time: f64,
    pub points: i64,
    pub spur: String,
    pub detector: String,
    pub frame_detector: String,
    pub trace_align: String,
    pub points_strategy: String,
}

#[derive(Clone, Copy)]
pub struct SwpGeometry {
    pub start: f64,
    pub stop: f64,
    pub points: u32,
    pub sweep_ms: f64,
    pub ref_dbm: f64,
    pub rbw: f64,
    pub vbw: f64,
    pub refclk: f64,
    pub refclk_src: i32,
    pub front_end: Option<FrontEndActual>,
}

pub struct SwpFrame {
    pub freq: Vec<f64>,
    /// NaN marks bins outside the swept range (the device writes exactly 0.0 dBm there).
    pub power: Vec<f32>,
    pub refclk_ppm: f64,
}

/// Device power-on SWP profile (`SWP_ProfileDeInit`), in WebSA wire names.
#[derive(Clone, Debug, PartialEq)]
pub struct SwpDefaults {
    pub center: f64,
    pub span: f64,
    pub ref_dbm: f64,
    pub rbw: f64,
    pub vbw: f64,
    pub points: i64,
    pub atten: i64,
    pub window: i64,
    pub rbw_mode: String,
    pub vbw_mode: String,
    pub spur: String,
    pub frame_detector: String,
    pub trace_align: String,
    pub points_strategy: String,
    pub preamp: i64,
    pub ifgain: i64,
    pub gain_strategy: i64,
    pub sweep_time_mode: i64,
    pub sweep_time: f64,
}

/// Reads the device SWP defaults through any open session handle. Must run on the owner
/// thread of that handle.
pub fn read_defaults(library: *mut c_void, session: *mut c_void) -> Result<SwpDefaults, String> {
    let ptr = unsafe { dlsym(library, b"san90_swp_defaults_v1\0".as_ptr().cast()) };
    if ptr.is_null() {
        return Err("SDK shim missing san90_swp_defaults_v1".into());
    }
    let query: Defaults = unsafe { std::mem::transmute(ptr) };
    let (mut center, mut span, mut ref_dbm, mut rbw, mut vbw, mut sweep_time) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut v = [0 as c_int; 13];
    let [points, atten, window, rbw_mode, vbw_mode, spur, frame, align, strategy, preamp, ifgain, gain, swt] =
        &mut v;
    let status = unsafe {
        query(
            session,
            &mut center,
            &mut span,
            &mut ref_dbm,
            &mut rbw,
            &mut vbw,
            points,
            atten,
            window,
            rbw_mode,
            vbw_mode,
            spur,
            frame,
            align,
            strategy,
            preamp,
            ifgain,
            gain,
            swt,
            &mut sweep_time,
        )
    };
    if status != 0 {
        return Err(format!("SDK SWP_ProfileDeInit status={status}"));
    }
    let [points, atten, window, rbw_mode, vbw_mode, spur, frame, align, strategy, preamp, ifgain, gain, swt] =
        v;
    Ok(SwpDefaults {
        center,
        span,
        ref_dbm,
        rbw,
        vbw,
        points: points.into(),
        atten: atten.into(),
        window: window.into(),
        rbw_mode: if rbw_mode == 0 { "manual" } else { "auto" }.into(),
        vbw_mode: match vbw_mode {
            0 => "manual",
            1 => "equal",
            2 => "tenth",
            3 => "onethousandth",
            _ => "bypass",
        }
        .into(),
        spur: match spur {
            1 => "standard",
            2 => "enhanced",
            _ => "bypass",
        }
        .into(),
        frame_detector: match frame {
            0 => "sample",
            1 => "pos_peak",
            2 => "average",
            3 => "neg_peak",
            4 => "max_power",
            5 => "raw_frames",
            6 => "rms",
            7 => "auto_peak",
            _ => "device_default",
        }
        .into(),
        trace_align: match align {
            0 => "native",
            2 => "center",
            _ => "start",
        }
        .into(),
        points_strategy: if strategy == 1 { "accuracy" } else { "speed" }.into(),
        preamp: preamp.into(),
        ifgain: ifgain.into(),
        gain_strategy: gain.into(),
        sweep_time_mode: swt.into(),
        sweep_time,
    })
}

// Enum ints match the vendor header; -1 means "keep the profile default".
fn rbw_mode_code(mode: &str, rbw: f64) -> (f64, i32) {
    match mode {
        "auto" => (rbw, 1),
        "one_thousandth_span" => (rbw, 2),
        "one_percent_span" => (rbw, 3),
        _ => (rbw.clamp(1.0, 10e6), 0),
    }
}

fn vbw_mode_code(mode: &str, vbw: f64) -> (f64, i32) {
    match mode {
        "equal" => (vbw, 1),
        "tenth" => (vbw, 2),
        "onethousandth" => (vbw, 3),
        "bypass" => (vbw, 4),
        _ => (vbw.clamp(10.0, 10e6), 0),
    }
}

fn trace_detector_code(detector: &str) -> (i32, i32) {
    match detector {
        "sample" => (1, 1),
        "pos_peak" => (1, 2),
        "neg_peak" => (1, 3),
        "rms" => (1, 4),
        "auto_peak" => (1, 6),
        _ => (0, -1),
    }
}

fn frame_detector_code(detector: &str) -> i32 {
    match detector {
        "sample" => 0,
        "pos_peak" => 1,
        "average" => 2,
        "neg_peak" => 3,
        "max_power" => 4,
        "raw_frames" => 5,
        "rms" => 6,
        "auto_peak" => 7,
        _ => -1,
    }
}

fn spur_code(spur: &str) -> i32 {
    match spur {
        "standard" => 1,
        "enhanced" => 2,
        _ => 0,
    }
}

fn trace_align_code(align: &str) -> i32 {
    match align {
        "native" => 0,
        "center" => 2,
        _ => 1,
    }
}

fn points_strategy_code(strategy: &str) -> i32 {
    u8::from(strategy == "accuracy").into()
}

struct Device {
    library: *mut c_void,
    handle: *mut c_void,
    reconfig: Reconfig,
    next: Next,
    close: Close,
    gnss: Option<Gnss>,
    geometry: SwpGeometry,
    /// Bus errors (-8/-9) invalidate the handle; never pass it to the vendor again,
    /// including for close.
    abandoned: bool,
}

unsafe impl Send for Device {}

fn request_of(params: &SwpParams) -> SwpRequest {
    let (rbw, rbw_mode) = rbw_mode_code(&params.rbw_mode, params.rbw);
    let (vbw, vbw_mode) = vbw_mode_code(&params.vbw_mode, params.vbw);
    let (detect_mode, detector) = trace_detector_code(&params.detector);
    SwpRequest {
        center_hz: params.center,
        span_hz: params.span,
        fe: params.front_end,
        rbw_mode,
        rbw_hz: rbw,
        vbw_mode,
        vbw_hz: vbw,
        window: params.window as c_int,
        sweep_time_mode: params.sweep_time_mode as c_int,
        sweep_time: params.sweep_time,
        trace_points: params.points as c_int,
        points_strategy: points_strategy_code(&params.points_strategy),
        trace_align: trace_align_code(&params.trace_align),
        spur: spur_code(&params.spur),
        trace_detect_mode: detect_mode,
        trace_detector: detector,
        frame_detector: frame_detector_code(&params.frame_detector),
    }
}

fn geometry_of(actual: &SwpActual) -> Result<SwpGeometry, String> {
    if actual.points < 2
        || !actual.start_hz.is_finite()
        || !actual.stop_hz.is_finite()
        || actual.start_hz >= actual.stop_hz
        || !actual.sweep_ms.is_finite()
    {
        return Err("SDK SWP geometry rejected".into());
    }
    Ok(SwpGeometry {
        start: actual.start_hz,
        stop: actual.stop_hz,
        points: actual.points,
        sweep_ms: actual.sweep_ms,
        ref_dbm: actual.ref_dbm,
        rbw: actual.rbw_hz,
        vbw: actual.vbw_hz,
        refclk: actual.refclk_hz,
        refclk_src: actual.refclk_src,
        front_end: None,
    })
}

impl Device {
    fn open(path: &str, params: &SwpParams) -> Result<Self, String> {
        owner_slot_free()?;
        let path = CString::new(path).map_err(|_| "invalid SDK shim path")?;
        let library = unsafe { dlopen(path.as_ptr(), SDK_DLOPEN_FLAGS) };
        if library.is_null() {
            return Err(format!(
                "SDK shim: {}",
                unsafe { CStr::from_ptr(dlerror()) }.to_string_lossy()
            ));
        }
        let result = (|| {
            unsafe { register_sdk_session(library)?; }
            let symbol = |name: &'static [u8]| -> Result<*mut c_void, String> {
                let ptr = unsafe { dlsym(library, name.as_ptr().cast()) };
                (!ptr.is_null())
                    .then_some(ptr)
                    .ok_or_else(|| format!("SDK shim missing {}", String::from_utf8_lossy(name)))
            };
            let open: Open = unsafe { std::mem::transmute(symbol(b"san90_swp_open_v2\0")?) };
            let reconfig: Reconfig =
                unsafe { std::mem::transmute(symbol(b"san90_swp_reconfig_v1\0")?) };
            let next: Next = unsafe { std::mem::transmute(symbol(b"san90_swp_next_v2\0")?) };
            let close: Close = unsafe { std::mem::transmute(symbol(b"san90_swp_close_v1\0")?) };
            let (mut handle, mut actual) = (std::ptr::null_mut(), SwpActual::default());
            let request = request_of(params);
            let status = unsafe { open(&mut handle, &request, &mut actual) };
            if status != 0 || handle.is_null() {
                return Err(format!("SDK SWP open rejected status={status}"));
            }
            let geometry = match geometry_of(&actual).map(|mut geometry| {
                geometry.front_end = read_front_end_actual(library, handle, b"san90_swp_front_end_actual_v1\0");
                if let Some(front_end) = geometry.front_end { geometry.ref_dbm = front_end.ref_dbm; }
                geometry
            }) {
                Ok(geometry) => geometry,
                Err(error) => {
                    unsafe {
                        close(handle);
                    }
                    return Err(error);
                }
            };
            let gnss_ptr = unsafe { dlsym(library, b"san90_gnss_v1\0".as_ptr().cast()) };
            let gnss: Option<Gnss> =
                (!gnss_ptr.is_null()).then(|| unsafe { std::mem::transmute(gnss_ptr) });
            Ok(Self {
                library,
                handle,
                reconfig,
                next,
                close,
                gnss,
                abandoned: false,
                geometry,
            })
        })();
        if result.is_err() {
            unsafe {
                dlclose(library);
            }
        }
        result
    }

    fn reconfigure(&mut self, params: &SwpParams) -> Result<SwpGeometry, SdkFailure> {
        let (mut actual, request) = (SwpActual::default(), request_of(params));
        let status = unsafe { (self.reconfig)(self.handle, &request, &mut actual) };
        if status != 0 {
            if LINK_LOST_STATUS.contains(&status) {
                unsafe { let _ = invalidate_sdk_session(self.library, self.handle, true); }
                self.abandoned = true;
            }
            return Err(SdkFailure {
                status,
                message: format!("SDK SWP reconfigure status={status}"),
            });
        }
        let mut geometry = geometry_of(&actual).map_err(|message| SdkFailure { status: 0, message })?;
        geometry.front_end = read_front_end_actual(self.library, self.handle, b"san90_swp_front_end_actual_v1\0");
        if let Some(front_end) = geometry.front_end { geometry.ref_dbm = front_end.ref_dbm; }
        self.geometry = geometry;
        Ok(geometry)
    }

    fn next(&mut self) -> Result<SwpFrame, SdkFailure> {
        let capacity = self.geometry.points as usize;
        let mut freq = vec![0.0; capacity];
        let mut power = vec![0.0f32; capacity];
        let (mut written, mut refclk_ppm) = (0, 0.0);
        let status = unsafe {
            (self.next)(
                self.handle,
                freq.as_mut_ptr(),
                power.as_mut_ptr(),
                self.geometry.points,
                &mut written,
                &mut refclk_ppm,
            )
        };
        if status != 0 {
            if !WARN_STATUS.contains(&status) {
                unsafe { let _ = invalidate_sdk_session(self.library, self.handle, LINK_LOST_STATUS.contains(&status)); }
            }
            if LINK_LOST_STATUS.contains(&status) {
                self.abandoned = true;
            }
            return Err(SdkFailure {
                status,
                message: format!("SDK SWP next status={status}"),
            });
        }
        if written < 2 || written as usize > capacity {
            return Err(SdkFailure {
                status: 0,
                message: format!("SDK SWP next points={written}"),
            });
        }
        freq.truncate(written as usize);
        power.truncate(written as usize);
        Ok(SwpFrame {
            freq,
            power,
            refclk_ppm,
        })
    }

    fn defaults(&mut self) -> Result<SwpDefaults, String> {
        read_defaults(self.library, self.handle)
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
            if LINK_LOST_STATUS.contains(&status) {
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
            release_sdk_mode(self.library, self.handle, self.close, self.abandoned)
        } {
            eprintln!("SDK SWP {error}");
        }
    }
}

enum OwnerCommand {
    Next(SyncSender<Result<SwpFrame, SdkFailure>>),
    Reconfigure(SwpParams, SyncSender<Result<SwpGeometry, SdkFailure>>),
    Defaults(SyncSender<Result<SwpDefaults, String>>),
    Gnss(SyncSender<Result<GnssInfo, String>>),
}

pub struct HardwareSwp {
    pub geometry: SwpGeometry,
    pub device_info: Option<DeviceInfo>,
    /// Single-flight requests: the caller is serialized behind the service lock.
    tx: SyncSender<OwnerCommand>,
    stop: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
}

unsafe impl Send for HardwareSwp {}

impl HardwareSwp {
    pub fn start(path: String, params: SwpParams) -> Result<Self, String> {
        let (meta_tx, meta_rx) = mpsc::sync_channel(1);
        let (tx, rx) = mpsc::sync_channel::<OwnerCommand>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let owner_stop = stop.clone();
        let owner = thread_spawn(move || match Device::open(&path, &params) {
            Ok(mut device) => {
                let info = read_device_info(device.library, device.handle).ok().flatten();
                if meta_tx.send(Ok((device.geometry, info))).is_err() {
                    return;
                }
                loop {
                    if owner_stop.load(Ordering::Acquire) {
                        break;
                    }
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(command) => {
                            if owner_stop.load(Ordering::Acquire) {
                                break;
                            }
                            match command {
                                OwnerCommand::Next(reply) => {
                                    let _ = reply.send(device.next());
                                }
                                OwnerCommand::Reconfigure(params, reply) => {
                                    let _ = reply.send(device.reconfigure(&params));
                                }
                                OwnerCommand::Defaults(reply) => {
                                    let _ = reply.send(device.defaults());
                                }
                                OwnerCommand::Gnss(reply) => {
                                    let _ = reply.send(device.gnss());
                                }
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                // `device` drops here: vendor close runs on this owner thread.
            }
            Err(e) => {
                let _ = meta_tx.send(Err(e));
            }
        });
        // An open that does not return stays inside the vendor library; park the owner so no
        // new session can open concurrently (vendor sessions must not overlap).
        let (geometry, device_info) = match meta_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(value) => value?,
            Err(_) => {
                park_stuck_owner(owner);
                return Err("SDK SWP startup timed out; owner outcome unknown".into());
            }
        };
        Ok(Self {
            geometry,
            device_info,
            tx,
            stop,
            owner: Some(owner),
        })
    }

    fn call<T>(
        &self,
        make: impl FnOnce(SyncSender<T>) -> OwnerCommand,
        timeout: Duration,
    ) -> Option<T> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx.send(make(reply_tx)).ok()?;
        reply_rx.recv_timeout(timeout).ok()
    }

    /// In-place profile change on the open handle (Python configure_swp).
    pub fn reconfigure(&mut self, params: SwpParams) -> Result<SwpGeometry, SdkFailure> {
        let geometry = self
            .call(
                |reply| OwnerCommand::Reconfigure(params, reply),
                Duration::from_secs(10),
            )
            .unwrap_or_else(|| {
                Err(SdkFailure {
                    status: 0,
                    message: "SDK SWP reconfigure stalled".into(),
                })
            })?;
        self.geometry = geometry;
        Ok(geometry)
    }

    /// Device power-on defaults, read through this handle.
    pub fn defaults(&self) -> Result<SwpDefaults, String> {
        self.call(OwnerCommand::Defaults, Duration::from_secs(5))
            .unwrap_or_else(|| Err("SDK SWP defaults stalled".into()))
    }

    /// GNSS snapshot; runs on the owner thread with every other vendor call.
    pub fn gnss(&self) -> Result<GnssInfo, String> {
        self.call(OwnerCommand::Gnss, Duration::from_secs(5))
            .unwrap_or_else(|| Err("SDK GNSS query stalled".into()))
    }

    pub fn next(&self) -> Result<SwpFrame, SdkFailure> {
        self.call(OwnerCommand::Next, Duration::from_secs(10))
            .unwrap_or_else(|| {
                Err(SdkFailure {
                    status: 0,
                    message: "SDK SWP fetch stalled".into(),
                })
            })
    }
}

impl Drop for HardwareSwp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Join the owner before allowing another SDK open; fetch/close must not
        // overlap a new session.
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

fn thread_spawn<F>(f: F) -> JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::spawn(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> SwpParams {
        SwpParams {
            center: 1e9,
            span: 1e8,
            front_end: FrontEnd {
                ref_dbm: 0.0,
                atten: -1,
                preamp: 0,
                ifgain: 2,
                gain_strategy: 0,
                ref_clock: 0,
                refclk_out: 0,
            },
            rbw_mode: "auto".into(),
            rbw: 1e5,
            vbw_mode: "bypass".into(),
            vbw: 1e5,
            window: 1,
            sweep_time_mode: 0,
            sweep_time: 0.0,
            points: 1000,
            spur: "bypass".into(),
            detector: "auto".into(),
            frame_detector: "device_default".into(),
            trace_align: "start".into(),
            points_strategy: "speed".into(),
        }
    }

    #[test]
    fn missing_shim_or_old_shim_fails_without_device() {
        assert!(HardwareSwp::start("/does/not/exist.so".into(), params()).is_err());
        // v10 has no san90_swp_open_v2: dlsym must fail cleanly before any vendor call.
        if let Ok(home) = std::env::var("HOME") {
            let v10 = format!("{home}/.local/share/harogic-websa/shims/libwebsa_san90_sdk_v10.so");
            if std::path::Path::new(&v10).exists() {
                assert!(HardwareSwp::start(v10, params()).is_err());
            }
        }
    }

    #[test]
    fn enum_codes_match_python_service_mapping() {
        assert_eq!(rbw_mode_code("auto", 1e5), (1e5, 1));
        assert_eq!(rbw_mode_code("manual", 0.0), (1.0, 0));
        assert_eq!(vbw_mode_code("bypass", 1e5), (1e5, 4));
        assert_eq!(vbw_mode_code("equal", 1e5), (1e5, 1));
        assert_eq!(vbw_mode_code("manual", 0.0), (10.0, 0));
        assert_eq!(trace_detector_code("pos_peak"), (1, 2));
        assert_eq!(trace_detector_code("auto"), (0, -1));
        assert_eq!(frame_detector_code("device_default"), -1);
        assert_eq!(frame_detector_code("max_power"), 4);
        assert_eq!(spur_code("enhanced"), 2);
        assert_eq!(spur_code("bypass"), 0);
        assert_eq!(trace_align_code("center"), 2);
        assert_eq!(trace_align_code("start"), 1);
        assert_eq!(points_strategy_code("accuracy"), 1);
        assert_eq!(points_strategy_code("speed"), 0);
    }
}

#[cfg(test)]
#[path = "hardware_swp_transaction_tests.rs"]
mod transaction_tests;
