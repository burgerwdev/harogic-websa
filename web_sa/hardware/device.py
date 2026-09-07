"""
hardware/device.py -- device abstraction (full SAN series)

Source: HarogicDevice migrated from web_sa/server.py (v0.11.1); behavior kept,
structure refactored.
- The only device entry point for the business layer: open/configure/fetch/query
- All SDK calls are executed serially via hardware/sdk_bindings (inside the event loop)
"""
from __future__ import annotations

import math
import time
from dataclasses import dataclass, field

import numpy as np

from ..config import (
    DEFAULT_RTA_CENTER_HZ,
    DEFAULT_RTA_RBW_MODE,
    DEFAULT_RTA_REF_DBM,
    DEFAULT_RTA_SPAN_HZ,
    DEFAULT_RTA_SWEEP_MODE,
    DEFAULT_RTA_VBW_MODE,
    DeviceCapabilities,
    fit_center_span,
)
from . import sdk_bindings as sb

# Convenient aliases for hardware enums
SWP = sb
T = sb  # type aliases


class DeviceError(RuntimeError):
    pass


@dataclass
class DeviceState:
    """Device read-only state (serialized to WS STATUS)."""
    connected: bool = False
    label: str = ''
    detail: str = ''
    device_detail: dict = field(default_factory=dict)
    caps: DeviceCapabilities = None          # model capabilities
    center_hz: float = 1e9          # SWP-mode center
    span_hz: float = 100e6
    rta_center_hz: float = DEFAULT_RTA_CENTER_HZ
    rta_span_hz: float = DEFAULT_RTA_SPAN_HZ
    rta_ref_level: float = DEFAULT_RTA_REF_DBM
    rta_ref_mode: str = 'manual'
    rta_rbw_mode: str = DEFAULT_RTA_RBW_MODE
    rta_rbw_hz: float = 0.0
    rta_vbw_mode: str = DEFAULT_RTA_VBW_MODE
    rta_vbw_hz: float = 0.0
    rta_sweep_time_mode: int = DEFAULT_RTA_SWEEP_MODE
    rta_sweep_time: float = 0.0
    rta_actual: dict = field(default_factory=dict)
    ref_level: float = 0.0
    ref_mode: str = 'manual'
    rbw_mode: str = 'manual'
    rbw_hz: float = 100e3
    vbw_mode: str = 'manual'
    vbw_hz: float = 100e3
    points_req: int = 1000
    window: int = 1
    spur_mode: str = 'bypass'
    atten: int = -1
    preamplifier: int = 0
    ifgain: int = 2
    gain_strategy: int = 0
    amp_atten: int = -1
    preamplifier_actual: int | None = None
    ref_clock: str = 'internal'
    has_docxo: bool = False
    mode: str = 'std'
    pnm_supported: bool = False
    actual: dict = field(default_factory=dict)
    gnss: dict = field(default_factory=dict)
    sweep_ms: float = 0.0
    sweep_time_mode: int = 0     # SweepTimeMode_TypeDef: 0=minSWT 1=x2 2=x4 3=x10 4=x20 5=x50 6=xN 7=Manual 8=minSMPxN
    sweep_time: float = 0.0      # Manual=绝对秒; xN=倍率; 其他模式忽略
    freq_version: int = 0
    config_version: int = 0
    last_error: str = ''
    refclk_ppm: float = 0.0
    calibrating: bool = False
    last_cal_freq: float = 0.0
    refclk_out: bool = False   # reference clock output enable
    # Measurement results
    harm_results: list = field(default_factory=list)
    pnm_last: dict | None = None


class HarogicDevice:
    """Device wrapper: lifecycle + sweep + query + measurement session host."""

    def __init__(self):
        self.dev = sb.c_void_p()
        self.dsp = sb.c_void_p()
        self.state = DeviceState()
        self.session = None
        # Global hardware lock (re-entrant): EVERY SDK/DLL call on this device handle
        # happens under this lock, serializing the RTA step thread (asyncio.to_thread),
        # the asyncio WS config thread and the STATUS/HTTP query thread. libhtraapi is
        # not thread-safe; concurrent calls (observed: general protection fault /
        # segfault in the asyncio thread during RTA span switches) are the crash source.
        import threading
        self._hw = threading.RLock()
        # Sweep buffers (rebuilt when the size changes to avoid per-frame allocation) -- per the original implementation
        self._freq_buf = None
        self._spec_buf = None
        self._ifreq_buf = None
        self._ispec_buf = None
        self._cnt = sb.c_uint32(0)
        self._meas_aux = sb.Full_MeasAuxInfo()   # full structure (includes RefClkFreqOffset ppm)
        self._trace_points = 0
        self._user_start = 0.0
        self._user_stop = 0.0
        self.preset_defaults = None   # cached SWP default config read at startup
        self.last_freq = None      # most recent frequency axis (pushed to new WS clients on connect)
        self.last_freq_ver = 0
        self._sweep_ema = None
        self._auto_ref = {
            'std': {
                'candidate': None, 'candidate_since': 0.0,
                'last_change': 0.0, 'last_peak': None,
            },
            'rta': {
                'candidate': None, 'candidate_since': 0.0,
                'last_change': 0.0, 'last_peak': None,
            },
        }
        self._pending_auto_ref: tuple[str, float] | None = None

    # ---------------- Lifecycle ----------------
    def open(self) -> tuple[bool, str]:
        with self._hw:
            bp = sb.BootProfile_TypeDef()
            bi = sb.BootInfo_TypeDef()
            bp.DevicePowerSupply = sb.htra_api.DevicePowerSupply_TypeDef.USBPortAndPowerPort
            bp.PhysicalInterface = sb.htra_api.PhysicalInterface_TypeDef.USB
            st = sb.dll.Device_Open(sb.pointer(self.dev), sb.c_int(0), sb.pointer(bp), sb.pointer(bi))
            if st != 0 and st != -49:
                self.state.last_error = 'Device_Open status=%d' % st
                return False, self.state.last_error
            dsp_status = sb.dll.DSP_Open(sb.pointer(self.dsp))
            if dsp_status != 0:
                sb.dll.Device_Close(sb.pointer(self.dev))
                self.state.last_error = f'DSP_Open status={dsp_status}'
                return False, self.state.last_error
            self.state.pnm_supported = sb.PNM_SUPPORTED
            di = bi.DeviceInfo
            self.state.label = 'UID:%012X Model:%d HW:%d MFW:%d FFW:%d' % (
                di.DeviceUID, di.Model, di.HardwareVersion, di.MFWVersion, di.FFWVersion)
            self.state.caps = DeviceCapabilities.from_model(di.Model, sb.PNM_SUPPORTED)
            self.state.device_detail = dict(
                uid='%012X' % di.DeviceUID, model=di.Model, hw=di.HardwareVersion,
                mfw=di.MFWVersion, ffw=di.FFWVersion,
                bus_speed=bi.BusSpeed, bus_ver=bi.BusVersion,
                api_ver=bi.APIVersion, warnings=bi.Warnings, errors=bi.Errors)
            self.state.connected = True
            self.state.last_error = ''
            self.configure_swp()
            self._detect_docxo()
            self.load_preset_defaults()   # read the device default config for the preset
            # The temporary config used for DOCXO detection changes the device's actual
            # parameters (e.g. TracePoints), so the standard config must be re-issued and
            # the buffers refreshed, otherwise GetFullSweep writes out of bounds
            self.configure_swp()
            return True, 'ok'

    def close(self) -> None:
        with self._hw:
            try:
                if self.dsp.value:
                    sb.dll.DSP_Close(sb.pointer(self.dsp))
            except Exception:
                pass
            try:
                if self.dev.value:
                    sb.dll.Device_Close(sb.pointer(self.dev))
            except Exception:
                pass
            self.dsp = sb.c_void_p()
            self.dev = sb.c_void_p()
            self.state.connected = False

    # ---------------- Configuration (ported from web_sa/server.py) ----------------
    def load_preset_defaults(self) -> None:
        with self._hw:
            """Read once at startup the SWP_ProfileDeInit device default config and cache it
            (not hard-coded; supports different SAN models). Restore from the cache on preset."""
            try:
                p = sb.SWP_Profile_TypeDef()
                sb.dll.SWP_ProfileDeInit(sb.pointer(self.dev), sb.pointer(p))
                rbw_mode = 'auto' if int(getattr(p.RBWMode, 'value', p.RBWMode)) else 'manual'
                vbw_modes = {
                    0: 'manual', 1: 'equal', 2: 'tenth',
                    3: 'onethousandth', 4: 'bypass',
                }
                spur_modes = {0: 'bypass', 1: 'standard', 2: 'enhanced'}
                self.preset_defaults = dict(
                    center=float(p.CenterFreq_Hz), span=float(p.Span_Hz),
                    ref=float(p.RefLevel_dBm), rbw=float(p.RBW_Hz),
                    vbw=float(p.VBW_Hz), points=int(p.TracePoints), atten=int(p.Atten),
                    window=int(p.Window.value if hasattr(p.Window, 'value') else p.Window),
                    rbw_mode=rbw_mode,
                    vbw_mode=vbw_modes.get(int(getattr(p.VBWMode, 'value', p.VBWMode)), 'bypass'),
                    spur=spur_modes.get(int(getattr(p.SpurRejection, 'value', p.SpurRejection)), 'bypass'),
                    preamp=int(getattr(p.Preamplifier, 'value', p.Preamplifier)),
                    ifgain=int(p.IFGainGrade),
                    gain_strategy=int(getattr(p.GainStrategy, 'value', p.GainStrategy)),
                    sweep_time_mode=int(getattr(p.SweepTimeMode, 'value', p.SweepTimeMode)),
                    sweep_time=float(p.SweepTime),
                )
            except Exception:
                self.preset_defaults = None

    def reset_rta_state(self) -> None:
        s = self.state
        s.rta_center_hz = DEFAULT_RTA_CENTER_HZ
        s.rta_span_hz = DEFAULT_RTA_SPAN_HZ
        s.rta_ref_level = DEFAULT_RTA_REF_DBM
        s.rta_ref_mode = 'manual'
        s.rta_rbw_mode = DEFAULT_RTA_RBW_MODE
        s.rta_rbw_hz = 0.0
        s.rta_vbw_mode = DEFAULT_RTA_VBW_MODE
        s.rta_vbw_hz = 0.0
        s.rta_sweep_time_mode = DEFAULT_RTA_SWEEP_MODE
        s.rta_sweep_time = 0.0
        s.rta_actual = {}
        self.reset_auto_reference('rta')

    def preset_state(self) -> dict:
        """Write the cached device defaults into dev.state (SWP parameters) WITHOUT
        reconfiguring the device. Used by preset: the active mode is reconfigured by the
        caller; the other mode's parameters sit ready (applied on the next switch)."""
        with self._hw:
            d = self.preset_defaults
            if not d:
                self.load_preset_defaults()
                d = self.preset_defaults
            if not d:
                return {}
            s = self.state
            full_span = s.caps.freq_max_hz - s.caps.freq_min_hz
            if d['span'] >= full_span:
                s.center_hz = (s.caps.freq_min_hz + s.caps.freq_max_hz) / 2
                s.span_hz = full_span
            else:
                s.center_hz, s.span_hz = fit_center_span(
                    d['center'], d['span'], s.caps)
            s.ref_level = d['ref']
            s.ref_mode = 'manual'
            s.rbw_hz = d['rbw']
            s.vbw_hz = d['vbw']
            s.points_req = d['points']
            s.atten = d['atten']
            s.window = d['window']
            s.rbw_mode = d['rbw_mode']
            s.vbw_mode = d['vbw_mode']
            s.spur_mode = d['spur']
            s.preamplifier = d['preamp']
            s.ifgain = d['ifgain']
            s.gain_strategy = d['gain_strategy']
            s.sweep_time_mode = d['sweep_time_mode']
            s.sweep_time = d['sweep_time']
            self.reset_auto_reference('std')
            return d

    def apply_preset(self) -> dict:
        with self._hw:
            d = self.preset_state()
            self.configure_swp()
            return d

    def _profile(self):
        T = sb
        s = self.state
        p = T.SWP_Profile_TypeDef()
        with self._hw:
            sb.dll.SWP_ProfileDeInit(sb.pointer(self.dev), sb.pointer(p))
        start = max(s.caps.freq_min_hz, s.center_hz - s.span_hz / 2)
        stop = min(s.caps.freq_max_hz, s.center_hz + s.span_hz / 2)
        if stop <= start:
            stop = min(s.caps.freq_max_hz, start + 1e3)
        p.FreqAssignment = T.SWP_FreqAssignment_TypeDef.StartStop
        p.StartFreq_Hz = start
        p.StopFreq_Hz = stop
        p.RefLevel_dBm = max(-50, min(30, s.ref_level))
        if s.rbw_mode == 'auto':
            p.RBWMode = T.RBWMode_TypeDef.RBW_Auto
        else:
            p.RBWMode = T.RBWMode_TypeDef.RBW_Manual
            p.RBW_Hz = max(100.0, min(10e6, s.rbw_hz))
        p.VBWMode = {'equal': T.VBWMode_TypeDef.VBW_EqualToRBW,
                     'tenth': T.VBWMode_TypeDef.VBW_TenPercentRBW,
                     'bypass': T.VBWMode_TypeDef.VBW_TenTimesRBW,
                     'manual': T.VBWMode_TypeDef.VBW_Manual}.get(
                         s.vbw_mode, T.VBWMode_TypeDef.VBW_TenTimesRBW)
        p.VBW_Hz = max(10.0, min(10e6, s.vbw_hz))
        p.Window = T.Window_TypeDef(int(s.window))
        p.Atten = int(s.atten)
        p.Preamplifier = T.PreamplifierState_TypeDef.AutoOn if s.preamplifier == 0 \
            else T.PreamplifierState_TypeDef.ForcedOff
        p.IFGainGrade = int(s.ifgain)
        p.GainStrategy = T.GainStrategy_TypeDef.LowNoisePreferred if s.gain_strategy == 0 \
            else T.GainStrategy_TypeDef.HighLinearityPreferred
        rc_map = {'internal': T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal,
                  'external': T.ReferenceClockSource_TypeDef.ReferenceClockSource_External,
                  'premium': T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal_Premium,
                  'external_forced': T.ReferenceClockSource_TypeDef.ReferenceClockSource_External_Forced}
        p.ReferenceClockSource = rc_map.get(s.ref_clock, T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal)
        # External reference frequency: the user's external 10MHz signal source
        # (official SCPI example ROSC:EXT:FREQ 10MHz)
        # Note: switching SystemClockSource to external is dangerous (only under vendor
        # guidance); left unset to avoid hanging the device
        p.ExternalSystemClockFrequency = 10e6
        p.EnableReferenceClockOut = 1 if s.refclk_out else 0
        p.SweepTimeMode = T.SweepTimeMode_TypeDef(s.sweep_time_mode)
        p.SweepTime = float(s.sweep_time)   # Manual 时绝对秒; xN 时倍率; 其他模式忽略
        p.TracePoints = int(max(51, min(4000, s.points_req)))
        p.TracePointsStrategy = T.TracePointsStrategy_TypeDef.SweepSpeedPreferred
        p.TraceAlign = T.TraceAlign_TypeDef.AlignToStart
        p.SpurRejection = T.SpurRejection_TypeDef.Standard if s.spur_mode == 'standard' else (
            T.SpurRejection_TypeDef.Enhanced if s.spur_mode == 'enhanced'
            else T.SpurRejection_TypeDef.Bypass)
        return p

    def configure_swp(self):
        with self._hw:
            if not self.state.connected:
                return False, 'not connected'
            pin = self._profile()
            pout = sb.SWP_Profile_TypeDef()
            ti = sb.SWP_TraceInfo_TypeDef()
            st = sb.dll.SWP_Configuration(sb.pointer(self.dev), sb.pointer(pin),
                                          sb.pointer(pout), sb.pointer(ti))
            if st != 0:
                self.state.last_error = 'SWP_Configuration status=%d' % st
                return False, self.state.last_error
            self.state.actual = dict(center=pout.CenterFreq_Hz, span=pout.Span_Hz,
                                     start=pout.StartFreq_Hz, stop=pout.StopFreq_Hz,
                                     ref=pout.RefLevel_dBm, rbw=pout.RBW_Hz, vbw=pout.VBW_Hz,
                                     points=ti.FullsweepTracePoints, est_min=ti.EstimateMinSweepTime,
                                     refclk=pout.ReferenceClockFrequency,
                                     refclk_src=int(pout.ReferenceClockSource.value))
            # fill sweep_ms with the device-estimated sweep time (carried in the frame header -> SWT display on the frontend)
            self.state.sweep_ms = float(getattr(ti, 'EstimateMinSweepTime', 0.0) or 0.0)
            self._trace_points = int(ti.FullsweepTracePoints)
            self._user_start = float(pout.StartFreq_Hz)
            self._user_stop = float(pout.StopFreq_Hz)
            self.state.config_version += 1
            self.state.freq_version += 1
            self.state.last_error = ''
            self._read_amp_atten()
            return True, 'ok'

    def _read_amp_atten(self) -> None:
        with self._hw:
            """Read back the actual attenuation/preamplifier state (per Device_GetAmpAttenState from the original implementation)."""
            try:
                amp = sb.PreamplifierState_TypeDef()
                att = sb.c_int(0)
                sp = sb.c_uint8(0)
                sb.dll.Device_GetAmpAttenState(sb.pointer(self.dev), sb.pointer(amp),
                                               sb.pointer(att), sb.pointer(sp))
                self.state.amp_atten = att.value
                self.state.preamplifier_actual = int(amp.value)
            except Exception:
                pass

    # ---------------- Sweep ----------------
    def fetch_sweep(self):
        with self._hw:
            """Return (freq_np_float64, power_np_float32) or None. Called serially inside the event loop."""
            n = getattr(self, '_trace_points', 0)
            if n <= 0 or not self.state.connected:
                return None
            try:
                if self._freq_buf is None or len(self._freq_buf) != n:
                    self._freq_buf = (sb.c_double * n)()
                    self._spec_buf = (sb.c_float * n)()
                    self._ifreq_buf = (sb.c_double * n)()
                    self._ispec_buf = (sb.c_float * n)()
                st = sb.dll.SWP_GetFullSweep(sb.pointer(self.dev), self._freq_buf,
                                             self._spec_buf, sb.pointer(self._meas_aux))
                if st != 0:
                    return None
                self.state.refclk_ppm = float(getattr(self._meas_aux, 'RefClkFreqOffset', 0.0))
                sb.dll.DSP_InterceptSpectrum(
                    sb.c_double(self._user_start), sb.c_double(self._user_stop),
                    self._freq_buf, self._spec_buf, sb.c_uint32(n),
                    self._ifreq_buf, self._ispec_buf, sb.pointer(self._cnt))
                c = self._cnt.value
                if c < 2:
                    return None
                f = np.frombuffer(self._ifreq_buf, dtype=np.float64, count=c).copy()
                p = np.frombuffer(self._ispec_buf, dtype=np.float32, count=c).copy()
                # Unfilled bins in the device's invalid zone (outside the actual swept range)
                # return exact 0.0 (shown as a false vertical 0dBm line at the upper edge of
                # the full span) -> set to NaN, to be filled in by the frontend gapFill
                # (a real signal can never be exactly 0.0, so this is safe)
                p[p == 0.0] = np.nan
                mask = f >= 0.0
                if not mask.all():
                    f = f[mask]; p = p[mask]
                if len(f) < 2:
                    return None
                finite = p[np.isfinite(p)]
                if finite.size:
                    self.observe_reference_peak(self.state.mode, float(np.max(finite)))
                # Return the device-native trace (consistent with v0.5.3):
                # the backend does not resample; the frontend resampleTrace handles point counts;
                # backend np.interp upsampling would pull narrow signals into triangle waves
                return f, p
            except Exception as e:
                self.state.last_error = 'sweep: %r' % e
                return None

    def measure_sweep(self, dt):
        """Update the measured sweep time EMA (interval between two fetches -> sweep_ms)."""
        if dt <= 0:
            return
        if self._sweep_ema is None:
            self._sweep_ema = dt
        else:
            self._sweep_ema = 0.8 * self._sweep_ema + 0.2 * dt
        self.state.sweep_ms = round(self._sweep_ema * 1000.0, 1)

    # ---------------- Queries ----------------
    def _detect_docxo(self) -> None:
        with self._hw:
            try:
                prof = T.SWP_Profile_TypeDef()
                po = T.SWP_Profile_TypeDef()
                ti = T.SWP_TraceInfo_TypeDef()
                sb.dll.SWP_ProfileDeInit(sb.pointer(self.dev), sb.pointer(prof))
                prof.CenterFreq_Hz = self.state.center_hz
                prof.Span_Hz = self.state.span_hz
                prof.FreqAssignment = T.SWP_FreqAssignment_TypeDef.CenterSpan
                prof.ReferenceClockSource = T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal_Premium
                st = sb.dll.SWP_Configuration(sb.pointer(self.dev), sb.pointer(prof),
                                              sb.pointer(po), sb.pointer(ti))
                self.state.has_docxo = st == 0 and int(po.ReferenceClockSource) == 2
            except Exception:
                self.state.has_docxo = False

    def calibrate_ref_clock(self, trigger_count: int = 30) -> tuple[bool, float]:
        with self._hw:
            """Calibrate the internal reference clock with GNSS 1PPS (blocks; the caller
            needs a background thread + timeout protection). If GNSS 1PPS is unavailable
            (no antenna / not locked) the DLL may hang while waiting, so an external timeout
            is required."""
            self.state.calibrating = True
            try:
                out = sb.c_double(0.0)
                st = sb.dll.Device_CalibrateRefClock(
                    sb.pointer(self.dev), 1,  # CalibrateByGNSS1PPS
                    1.0, sb.c_uint64(max(3, int(trigger_count))),
                    sb.c_uint8(0), sb.pointer(out))
                if st != 0:
                    return False, 0.0
                return True, float(out.value)
            except Exception:
                return False, 0.0
            finally:
                self.state.calibrating = False

    def query_gnss(self) -> dict:
        with self._hw:
            try:
                g = T.GNSSInfo_TypeDef()
                st = sb.dll.Device_GetGNSSInfo(sb.pointer(self.dev), sb.pointer(g))
                if st != 0:
                    self.state.last_error = f'Device_GetGNSSInfo status={st}'
                    return {}
                # fill in all the SDK GNSS fields: lat/lon/altitude/date-time/antenna/DOCXO work mode
                try:
                    docxo_mode = int(g.DOCXO_WorkMode.value)
                except Exception:
                    docxo_mode = -1
                try:
                    antenna = int(g.GNSSAntennaState.value)
                except Exception:
                    antenna = -1
                return dict(
                    lock=int(g.GNSS_LockState), sats=int(g.SatsNum),
                    docxo=int(g.DOCXO_LockState), docxo_mode=docxo_mode,
                    antenna=antenna,
                    latitude=float(getattr(g, 'latitude', 0.0)),
                    longitude=float(getattr(g, 'longitude', 0.0)),
                    altitude=int(getattr(g, 'altitude', 0)),
                    year=int(getattr(g, 'Year', 0)), month=int(getattr(g, 'month', 0)),
                    day=int(getattr(g, 'day', 0)), hour=int(getattr(g, 'hour', 0)),
                    minute=int(getattr(g, 'minute', 0)), second=int(getattr(g, 'second', 0)),
                    time='%04d-%02d-%02d %02d:%02d:%02d' % (
                        getattr(g, 'Year', 0), getattr(g, 'month', 0), getattr(g, 'day', 0),
                        getattr(g, 'hour', 0), getattr(g, 'minute', 0), getattr(g, 'second', 0)),
                )
            except Exception:
                return {}

    def observe_reference_peak(self, mode: str, peak_dbm: float) -> None:
        """Queue a stable, hysteretic automatic reference-level adjustment."""
        with self._hw:
            self._observe_reference_peak_locked(mode, peak_dbm)

    def _observe_reference_peak_locked(self, mode: str, peak_dbm: float) -> None:
        if mode not in ('std', 'rta') or not math.isfinite(peak_dbm):
            return
        state = self.state
        ref_mode = state.rta_ref_mode if mode == 'rta' else state.ref_mode
        if ref_mode != 'auto' or state.atten != -1:
            return
        tracker = self._auto_ref[mode]
        tracker['last_peak'] = peak_dbm
        current = state.rta_ref_level if mode == 'rta' else state.ref_level
        target = max(-50.0, min(30.0, math.ceil((peak_dbm + 5.0) / 5.0) * 5.0))
        if abs(target - current) < 5.0:
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            return

        now = time.monotonic()
        if tracker['candidate'] != target:
            tracker['candidate'] = target
            tracker['candidate_since'] = now
        # Raise the reference immediately for overload safety. Lowering waits for a
        # time-stable peak so RTA settle/empty frames cannot collapse Ref to -50 dBm.
        stable_for = 0.15 if target > current else 1.5
        if (
            now - tracker['candidate_since'] >= stable_for
            and now - tracker['last_change'] >= 1.0
        ):
            self._pending_auto_ref = (mode, target)
            tracker['last_change'] = now

    def reset_auto_reference(self, mode: str) -> None:
        with self._hw:
            tracker = self._auto_ref[mode]
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            if self._pending_auto_ref and self._pending_auto_ref[0] == mode:
                self._pending_auto_ref = None

    def apply_pending_auto_reference(self) -> bool:
        """Apply one queued auto-reference update in the active acquisition worker."""
        with self._hw:
            pending = self._pending_auto_ref
            if pending is None or pending[0] != self.state.mode:
                return False
            self._pending_auto_ref = None
            mode, target = pending
            tracker = self._auto_ref[mode]
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            if mode == 'rta' and self.session is not None and self.session.name == 'rta':
                self.state.rta_ref_level = target
                self.session._configure()
            elif mode == 'std':
                self.state.ref_level = target
                ok, _ = self.configure_swp()
                if not ok:
                    return False
            else:
                return False
            return True

    # ---------------- Session host ----------------
    def set_session(self, session) -> None:
        with self._hw:
            self.session = session
            self.state.mode = session.name if session else 'std'

    def step(self):
        """publisher single step: forward to the current session."""
        return self.session.step() if self.session else None
