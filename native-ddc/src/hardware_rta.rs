//! SAN-90 FPGA RTA request owner for the opt-in Rust WebSA path.
//! Production shim calls synchronously dispatch to its permanent SDK thread, which
//! retains the physical handle across mode owners and preserves vendor thread affinity.
//! Buffers are bounded by the SDK-reported frame geometry.
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

use crate::hardware_swp::{
    owner_slot_free, park_stuck_owner, read_defaults, FrontEnd, GnssInfo, SdkFailure, SwpDefaults,
    LINK_LOST_STATUS, OPEN_TIMEOUT, SDK_DLOPEN_FLAGS, register_sdk_session, release_sdk_mode,
};

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

type Open = unsafe extern "C" fn(*mut *mut c_void, *const RtaRequest, *mut RtaActual) -> c_int;
type Reconfig = unsafe extern "C" fn(*mut c_void, *const RtaRequest, *mut RtaActual) -> c_int;
type Next = unsafe extern "C" fn(*mut c_void, *mut f64, *mut f32, u32, *mut u32) -> i32;
type Close = unsafe extern "C" fn(*mut c_void) -> i32;
type TriggerActual = unsafe extern "C" fn(*mut c_void, *mut RtaTriggerActual) -> c_int;

/// Layout = versioned `San90RtaTriggerActual`; independent of the legacy actual ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RtaTriggerActual {
    pub valid: c_int,
    pub source: c_int,
    pub edge: c_int,
    pub out: c_int,
    pub polarity: c_int,
    pub retrigger: u32,
    pub level: f64,
    pub safetime: f64,
    pub delay: f64,
    pub pretime: f64,
    pub acqtime: f64,
    pub retriggerperiod: f64,
    pub poi: f64,
    pub generation: u64,
    pub packet_index: u64,
    pub packet_count: u32,
    pub complete: c_int,
    pub last_sdk_status: c_int,
    pub first_data_timestamp: u64,
    pub trigger_edges: u32,
}
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

/// Layout = `San90RtaRequest` in sdk/san90_sdk.cpp.
#[repr(C)]
struct RtaRequest {
    center_hz: f64,
    decimate: u32,
    fe: FrontEnd,
    rbw_manual: c_int,
    rbw_hz: f64,
    vbw_mode: c_int,
    vbw_hz: f64,
    sweep_time_mode: c_int,
    sweep_time: f64,
    trigger_source: c_int,
    trigger_edge: c_int,
    trigger_level_dbm: f64,
    trigger_safe_time: f64,
    trigger_delay: f64,
    trigger_pre_time: f64,
    trigger_acq_time: f64,
    retrigger_count: u32,
    retrigger_period: f64,
    trigger_out: c_int,
    trigger_out_polarity: c_int,
}

/// Layout = `San90RtaActual` in sdk/san90_sdk.cpp.
#[repr(C)]
#[derive(Default)]
struct RtaActual {
    start_hz: f64,
    stop_hz: f64,
    points: u32,
    ref_dbm: f64,
    rbw_hz: f64,
    vbw_hz: f64,
    refclk_hz: f64,
    refclk_src: c_int,
    atten: c_int,
    preamp: c_int,
    ifgain: c_int,
}

/// Vendor RTA profile request, one field per WebSA RTA wire field.
#[derive(Clone, PartialEq, Debug)]
pub struct RtaParams {
    pub center: f64,
    pub decimate: u32,
    pub front_end: FrontEnd,
    pub rbw_manual: bool,
    pub rbw: f64,
    /// manual | equal | tenth | onethousandth | bypass.
    pub vbw_mode: String,
    pub vbw: f64,
    pub sweep_time_mode: i64,
    pub sweep_time: f64,
    pub trigger: RtaTrigger,
}

/// RTA trigger fields. Times are in seconds.
#[derive(Clone, PartialEq, Debug)]
pub struct RtaTrigger {
    /// bus | freerun | level | external | timer.
    pub source: String,
    /// rising | falling | double.
    pub edge: String,
    pub level_dbm: f64,
    pub safe_time: f64,
    pub delay: f64,
    pub pre_time: f64,
    pub acq_time: f64,
    /// 0 = retrigger off.
    pub retrigger_count: u32,
    pub retrigger_period: f64,
    /// none | per_hop | per_sweep | per_profile.
    pub out: String,
    /// positive | negative.
    pub out_polarity: String,
}

/// Same values as web_sa/config.py DEFAULT_TRIGGER_*.
impl Default for RtaTrigger {
    fn default() -> Self {
        Self {
            source: "bus".into(),
            edge: "rising".into(),
            level_dbm: -40.0,
            safe_time: 0.0,
            delay: 0.0,
            pre_time: 0.0,
            acq_time: 0.005,
            retrigger_count: 0,
            retrigger_period: 0.0,
            out: "none".into(),
            out_polarity: "positive".into(),
        }
    }
}

/// Frame geometry and the values the device applied.
#[derive(Clone, Copy, Default, Debug)]
pub struct RtaGeometry {
    pub start: f64,
    pub stop: f64,
    pub points: u32,
    pub ref_dbm: f64,
    pub rbw: f64,
    pub vbw: f64,
    pub refclk: f64,
    pub refclk_src: i32,
    pub atten: i32,
    pub preamp: i32,
    pub ifgain: i32,
    pub trigger: RtaTriggerActual,
}

pub struct RtaFrame {
    pub geometry: RtaGeometry,
    pub freq: Vec<f64>,
    pub power: Vec<f32>,
    pub trigger: RtaTriggerActual,
}

/// `VBWMode_TypeDef`. Unknown names map to EqualToRBW (Python rta.py `_vbw_map` fallback).
fn vbw_mode_code(mode: &str) -> c_int {
    match mode {
        "manual" => 0,
        "tenth" => 2,
        "onethousandth" => 3,
        "bypass" => 4,
        _ => 1,
    }
}

/// `RTA_TriggerSource_TypeDef`. Unknown names map to Bus (Python `_TRIGGER_SOURCE`).
fn trigger_source_code(source: &str) -> c_int {
    match source {
        "freerun" => 0,
        "external" => 1,
        "level" => 3,
        "timer" => 4,
        _ => 2,
    }
}

/// `TriggerEdge_TypeDef`. Unknown names map to RisingEdge.
fn trigger_edge_code(edge: &str) -> c_int {
    match edge {
        "falling" => 1,
        "double" => 2,
        _ => 0,
    }
}

/// `TriggerOutMode_TypeDef`. Unknown names map to NNone.
fn trigger_out_code(out: &str) -> c_int {
    match out {
        "per_hop" => 1,
        "per_sweep" => 2,
        "per_profile" => 3,
        _ => 0,
    }
}

/// `TriggerOutPulsePolarity_TypeDef`. Unknown names map to Positive.
fn trigger_polarity_code(polarity: &str) -> c_int {
    c_int::from(polarity == "negative")
}

fn request_of(params: &RtaParams) -> RtaRequest {
    let trigger = &params.trigger;
    RtaRequest {
        center_hz: params.center,
        decimate: params.decimate,
        fe: params.front_end,
        rbw_manual: c_int::from(params.rbw_manual),
        rbw_hz: params.rbw,
        vbw_mode: vbw_mode_code(&params.vbw_mode),
        vbw_hz: params.vbw,
        sweep_time_mode: params.sweep_time_mode as c_int,
        sweep_time: params.sweep_time,
        trigger_source: trigger_source_code(&trigger.source),
        trigger_edge: trigger_edge_code(&trigger.edge),
        trigger_level_dbm: trigger.level_dbm,
        trigger_safe_time: trigger.safe_time,
        trigger_delay: trigger.delay,
        trigger_pre_time: trigger.pre_time,
        trigger_acq_time: trigger.acq_time,
        retrigger_count: trigger.retrigger_count,
        retrigger_period: trigger.retrigger_period,
        trigger_out: trigger_out_code(&trigger.out),
        trigger_out_polarity: trigger_polarity_code(&trigger.out_polarity),
    }
}

fn geometry_of(actual: &RtaActual) -> Result<RtaGeometry, String> {
    if actual.points < 2
        || !actual.start_hz.is_finite()
        || !actual.stop_hz.is_finite()
        || actual.start_hz >= actual.stop_hz
    {
        return Err(format!(
            "SDK RTA geometry rejected points={}",
            actual.points
        ));
    }
    Ok(RtaGeometry {
        start: actual.start_hz,
        stop: actual.stop_hz,
        points: actual.points,
        ref_dbm: actual.ref_dbm,
        rbw: actual.rbw_hz,
        vbw: actual.vbw_hz,
        refclk: actual.refclk_hz,
        refclk_src: actual.refclk_src,
        atten: actual.atten,
        preamp: actual.preamp,
        ifgain: actual.ifgain,
        trigger: RtaTriggerActual::default(),
    })
}

struct Device {
    library: *mut c_void,
    handle: *mut c_void,
    reconfig: Reconfig,
    next: Next,
    close: Close,
    gnss: Option<Gnss>,
    trigger_actual: TriggerActual,
    /// Stale after a bus error (-8/-9): never hand the handle back to the vendor library.
    abandoned: bool,
    geometry: RtaGeometry,
}

unsafe impl Send for Device {}

impl Device {
    fn open(path: &str, params: &RtaParams) -> Result<Self, String> {
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
            let open: Open = unsafe { std::mem::transmute(symbol(b"san90_rta_open_v2\0")?) };
            let reconfig: Reconfig =
                unsafe { std::mem::transmute(symbol(b"san90_rta_reconfig_v2\0")?) };
            let next: Next = unsafe { std::mem::transmute(symbol(b"san90_rta_next_v2\0")?) };
            let trigger_actual: TriggerActual = unsafe {
                std::mem::transmute(symbol(b"san90_rta_trigger_actual_v1\0")?)
            };
            let close: Close = unsafe { std::mem::transmute(symbol(b"san90_rta_close_v1\0")?) };
            let (mut handle, mut actual) = (std::ptr::null_mut(), RtaActual::default());
            let request = request_of(params);
            let status = unsafe { open(&mut handle, &request, &mut actual) };
            if status != 0 || handle.is_null() {
                return Err(format!("SDK RTA open rejected status={status}"));
            }
            let mut geometry = match geometry_of(&actual) {
                Ok(geometry) => geometry,
                Err(error) => {
                    unsafe {
                        close(handle);
                    }
                    return Err(error);
                }
            };
            let status = unsafe { trigger_actual(handle, &mut geometry.trigger) };
            if status != 0 || geometry.trigger.valid == 0 {
                unsafe { close(handle); }
                return Err(format!("SDK RTA trigger readback status={status}"));
            }
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
                trigger_actual,
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

    fn reconfigure(&mut self, params: &RtaParams) -> Result<RtaGeometry, SdkFailure> {
        let (mut actual, request) = (RtaActual::default(), request_of(params));
        let status = unsafe { (self.reconfig)(self.handle, &request, &mut actual) };
        if status != 0 {
            // Non-link failures are rolled back by reconfig_v2 on this same session.
            if LINK_LOST_STATUS.contains(&status) {
                unsafe { let _ = crate::hardware_swp::invalidate_sdk_session(self.library, self.handle, true); }
            }
            if LINK_LOST_STATUS.contains(&status) {
                self.abandoned = true;
            }
            return Err(SdkFailure {
                status,
                message: format!("SDK RTA reconfigure status={status}"),
            });
        }
        let mut geometry = geometry_of(&actual).map_err(|message| SdkFailure { status: 0, message })?;
        geometry.trigger = self.read_trigger()?;
        self.geometry = geometry;
        Ok(geometry)
    }
    fn read_trigger(&self) -> Result<RtaTriggerActual, SdkFailure> {
        let mut actual = RtaTriggerActual::default();
        let status = unsafe { (self.trigger_actual)(self.handle, &mut actual) };
        if status != 0 {
            return Err(SdkFailure { status, message: format!("SDK RTA trigger readback status={status}") });
        }
        // Zero denotes unavailable packet continuity, including a LastPacket
        // received after data loss; it cannot certify a complete assembled capture.
        if actual.packet_index == 0 { actual.complete = -1; }
        Ok(actual)
    }


    /// Buffers use the current geometry: a reconfigure can change the frame width.
    fn next(&mut self) -> Result<RtaFrame, SdkFailure> {
        let capacity = self.geometry.points;
        let mut freq = vec![0.0; capacity as usize];
        let mut power = vec![0.0f32; capacity as usize];
        let mut points = 0;
        let status = unsafe {
            (self.next)(
                self.handle,
                freq.as_mut_ptr(),
                power.as_mut_ptr(),
                capacity,
                &mut points,
            )
        };
        if status != 0 {
            if !crate::hardware_swp::WARN_STATUS.contains(&status) && status != -304 {
                unsafe { let _ = crate::hardware_swp::invalidate_sdk_session(self.library, self.handle, LINK_LOST_STATUS.contains(&status)); }
            }
            if LINK_LOST_STATUS.contains(&status) {
                self.abandoned = true;
            }
            return Err(SdkFailure {
                status,
                message: format!("SDK RTA next status={status}"),
            });
        }
        if points != capacity {
            return Err(SdkFailure {
                status: 0,
                message: format!("SDK RTA next points={points} expected={capacity}"),
            });
        }
        if freq.iter().any(|v| !v.is_finite()) || power.iter().any(|v| !v.is_finite()) {
            return Err(SdkFailure {
                status: 0,
                message: "SDK RTA returned nonfinite data".into(),
            });
        }
        Ok(RtaFrame {
            geometry: self.geometry,
            freq,
            power,
            trigger: self.read_trigger()?,
        })
    }

    fn defaults(&mut self) -> Result<SwpDefaults, String> {
        read_defaults(self.library, self.handle)
    }

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
            eprintln!("SDK RTA {error}");
        }
    }
}

enum OwnerCommand {
    Next(SyncSender<Result<RtaFrame, SdkFailure>>),
    Reconfigure(RtaParams, SyncSender<Result<RtaGeometry, SdkFailure>>),
    Defaults(SyncSender<Result<SwpDefaults, String>>),
    Gnss(SyncSender<Result<GnssInfo, String>>),
    TriggerActual(SyncSender<Result<RtaTriggerActual, SdkFailure>>),
}

pub struct HardwareRta {
    pub geometry: RtaGeometry,
    pub device_info: Option<crate::hardware_swp::DeviceInfo>,
    /// Single-flight requests: the caller is serialized behind the service lock.
    tx: SyncSender<OwnerCommand>,
    stop: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
}

unsafe impl Send for HardwareRta {}

impl HardwareRta {
    pub fn start(path: String, params: RtaParams) -> Result<Self, String> {
        let (meta_tx, meta_rx) = mpsc::sync_channel(1);
        let (tx, rx) = mpsc::sync_channel::<OwnerCommand>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let owner_stop = stop.clone();
        let owner = std::thread::spawn(move || match Device::open(&path, &params) {
            Ok(mut device) => {
                let info = crate::hardware_swp::read_device_info(device.library, device.handle).ok().flatten();
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
                                OwnerCommand::TriggerActual(reply) => {
                                    let _ = reply.send(device.read_trigger());
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
                return Err("SDK RTA startup timed out; owner outcome unknown".into());
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

    /// In-place profile change on the open handle (Python rta.py _configure_locked).
    pub fn reconfigure(&mut self, params: RtaParams) -> Result<RtaGeometry, SdkFailure> {
        let geometry = self
            .call(
                |reply| OwnerCommand::Reconfigure(params, reply),
                Duration::from_secs(10),
            )
            .unwrap_or_else(|| {
                Err(SdkFailure {
                    status: 0,
                    message: "SDK RTA reconfigure stalled".into(),
                })
            })?;
        self.geometry = geometry;
        Ok(geometry)
    }

    pub fn next(&self) -> Result<RtaFrame, SdkFailure> {
        self.call(OwnerCommand::Next, Duration::from_secs(10))
            .unwrap_or_else(|| {
                Err(SdkFailure {
                    status: 0,
                    message: "SDK RTA fetch stalled".into(),
                })
            })
    }
    pub fn trigger_actual(&self) -> Result<RtaTriggerActual, SdkFailure> {
        self.call(OwnerCommand::TriggerActual, Duration::from_secs(5))
            .unwrap_or_else(|| Err(SdkFailure { status: 0, message: "SDK RTA trigger query stalled".into() }))
    }


    /// GNSS snapshot; runs on the owner thread with every other vendor call.
    pub fn gnss(&self) -> Result<GnssInfo, String> {
        self.call(OwnerCommand::Gnss, Duration::from_secs(5))
            .unwrap_or_else(|| Err("SDK GNSS query stalled".into()))
    }

    /// Device power-on SWP defaults, read through this handle.
    pub fn defaults(&self) -> Result<SwpDefaults, String> {
        self.call(OwnerCommand::Defaults, Duration::from_secs(5))
            .unwrap_or_else(|| Err("SDK RTA defaults stalled".into()))
    }
}

impl Drop for HardwareRta {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Join the owner before allowing another SDK open; fetch/close must not
        // overlap a new session.
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> RtaParams {
        RtaParams {
            center: 1e9,
            decimate: 1,
            front_end: FrontEnd {
                ref_dbm: 0.0,
                atten: -1,
                preamp: 0,
                ifgain: 2,
                gain_strategy: 0,
                ref_clock: 0,
                refclk_out: 0,
            },
            rbw_manual: false,
            rbw: 1e5,
            vbw_mode: "bypass".into(),
            vbw: 1e5,
            sweep_time_mode: 0,
            sweep_time: 0.0,
            trigger: RtaTrigger::default(),
        }
    }

    #[test]
    fn missing_shim_or_old_shim_fails_without_device() {
        assert!(HardwareRta::start("/does/not/exist.so".into(), params()).is_err());
        // v10 has no san90_rta_open_v2: dlsym must fail cleanly before any vendor call.
        let v10 = "/home/hui/.local/share/harogic-websa/shims/libwebsa_san90_sdk_v10.so";
        if std::path::Path::new(v10).exists() {
            assert!(HardwareRta::start(v10.into(), params()).is_err());
        }
    }

    #[test]
    fn trigger_codes_match_python_tables() {
        // web_sa/measurements/rta.py _TRIGGER_SOURCE, fallback Bus.
        assert_eq!(trigger_source_code("freerun"), 0);
        assert_eq!(trigger_source_code("external"), 1);
        assert_eq!(trigger_source_code("bus"), 2);
        assert_eq!(trigger_source_code("level"), 3);
        assert_eq!(trigger_source_code("timer"), 4);
        assert_eq!(trigger_source_code("bogus"), 2);
        // _TRIGGER_EDGE, fallback RisingEdge.
        assert_eq!(trigger_edge_code("rising"), 0);
        assert_eq!(trigger_edge_code("falling"), 1);
        assert_eq!(trigger_edge_code("double"), 2);
        assert_eq!(trigger_edge_code("bogus"), 0);
        // _TRIGGER_OUT, fallback NNone.
        assert_eq!(trigger_out_code("none"), 0);
        assert_eq!(trigger_out_code("per_hop"), 1);
        assert_eq!(trigger_out_code("per_sweep"), 2);
        assert_eq!(trigger_out_code("per_profile"), 3);
        assert_eq!(trigger_out_code("bogus"), 0);
        // _TRIGGER_POLARITY, fallback Positive.
        assert_eq!(trigger_polarity_code("positive"), 0);
        assert_eq!(trigger_polarity_code("negative"), 1);
        assert_eq!(trigger_polarity_code("bogus"), 0);
        assert_eq!(vbw_mode_code("manual"), 0);
        assert_eq!(vbw_mode_code("equal"), 1);
        assert_eq!(vbw_mode_code("tenth"), 2);
        assert_eq!(vbw_mode_code("onethousandth"), 3);
        assert_eq!(vbw_mode_code("bypass"), 4);
    }
}
