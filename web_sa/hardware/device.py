"""
hardware/device.py -- device abstraction (full SAN series)

Source: HarogicDevice migrated from web_sa/server.py (v0.11.1); behavior kept,
structure refactored.
- The only device entry point for the business layer: open/configure/fetch/query
- All SDK calls are executed serially via hardware/sdk_bindings (inside the event loop)
"""
from __future__ import annotations

import math
import os
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
    DEFAULT_TRIGGER_ACQ_TIME_S,
    DEFAULT_TRIGGER_DELAY_S,
    DEFAULT_TRIGGER_EDGE,
    DEFAULT_TRIGGER_LEVEL_DBM,
    DEFAULT_TRIGGER_OUT,
    DEFAULT_TRIGGER_OUT_POLARITY,
    DEFAULT_TRIGGER_PRE_TIME_S,
    DEFAULT_TRIGGER_RETRIGGER_COUNT,
    DEFAULT_TRIGGER_RETRIGGER_PERIOD_S,
    DEFAULT_TRIGGER_SAFE_TIME_S,
    DEFAULT_TRIGGER_SOURCE,
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
    # SDR mode (IQS streaming + channelizer + demod). Independent from SWP/RTA.
    sdr_center_hz: float = 1e9
    sdr_decimate: int = 16
    sdr_actual: dict = field(default_factory=dict)
    sdr_listen_hz: float = 1e9
    sdr_demod: str = 'am'
    sdr_if_bw: float = 6000.0
    sdr_squelch: float = -110.0
    sdr_squelch_open: bool = False
    sdr_volume: float = 0.8
    sdr_agc: bool = True
    sdr_pitch: float = 700.0
    # FM de-emphasis time constant in microseconds. -1 = auto (50 us for WFM, none
    # elsewhere); 0 = off; 50/75/300 = explicit (regional pre-emphasis complement).
    sdr_deemph_us: float = -1.0
    sdr_level_dbfs: float = -120.0
    sdr_adm: dict = field(default_factory=dict)
    # RTA acquisition trigger (applies to RTA sessions; SWP has no level trigger)
    trigger_source: str = DEFAULT_TRIGGER_SOURCE
    trigger_edge: str = DEFAULT_TRIGGER_EDGE
    trigger_level_dbm: float = DEFAULT_TRIGGER_LEVEL_DBM
    trigger_safe_time_s: float = DEFAULT_TRIGGER_SAFE_TIME_S
    trigger_delay_s: float = DEFAULT_TRIGGER_DELAY_S
    trigger_pre_time_s: float = DEFAULT_TRIGGER_PRE_TIME_S
    trigger_acq_time_s: float = DEFAULT_TRIGGER_ACQ_TIME_S
    trigger_retrigger_count: int = DEFAULT_TRIGGER_RETRIGGER_COUNT
    trigger_retrigger_period_s: float = DEFAULT_TRIGGER_RETRIGGER_PERIOD_S
    trigger_out: str = DEFAULT_TRIGGER_OUT
    trigger_out_polarity: str = DEFAULT_TRIGGER_OUT_POLARITY
    trigger_actual: dict = field(default_factory=dict)
    ref_level: float = 0.0
    ref_mode: str = 'manual'
    rbw_mode: str = 'manual'
    rbw_hz: float = 100e3
    vbw_mode: str = 'manual'
    vbw_hz: float = 100e3
    points_req: int = 1000
    window: int = 1
    spur_mode: str = 'bypass'
    detector: str = 'auto'
    atten: int = -1
    preamplifier: int = 0
    ifgain: int = 2
    # IF AGC (device-specific; see _profile). Off by default because the official
    # Profile.xml ships EnableIFAGC=0. WEBSA_IFAGC=1 flips the default for A/B testing.
    ifagc: int = 1 if os.getenv('WEBSA_IFAGC', '0').lower() not in (
        '0', '', 'false', 'no', 'off') else 0
    ifagc_target: float = float(os.getenv('WEBSA_IFAGC_TARGET', '-9'))
    ifagc_gain: float = 0.0
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
    # Height of the visible display window in dB (grid divisions x dB/div), pushed by the
    # frontend. Auto Ref anchors the noise floor just above the bottom of this window, so it
    # must know how tall the window is; 100 dB = the default 10 div x 10 dB/div.
    ref_range_db: float = 100.0
    # Last vendor WARNING status from the measurement stream (0 = none). -12 is IF
    # overflow: the IF saturates when Ref is set low, and the vendor's remedy is to raise
    # RefLevel_dBm. Surfaced so the UI can say so instead of the display appearing frozen.
    status_warning: int = 0
    last_error: str = ''
    refclk_ppm: float = 0.0
    calibrating: bool = False
    last_cal_freq: float = 0.0
    refclk_out: bool = False   # reference clock output enable
    # Measurement results
    harm_results: list = field(default_factory=list)
    pnm_last: dict | None = None


# Safety bound for the swept trace buffers: SWP_GetFullSweep writes the device's own trace
# length, so the buffer must be at least as large as the largest trace it can report. This is
# well above anything the UI can request (the points selector tops out at 4000; the widest
# vendor FFT is ~26k) and costs ~1.5 MB.
_SWP_TRACE_MAX = 65536


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
                'last_noise_floor': None, 'ignore_until': 0.0,
                'floor': -50.0,
            },
            'rta': {
                'candidate': None, 'candidate_since': 0.0,
                'last_change': 0.0, 'last_peak': None,
                'last_noise_floor': None, 'ignore_until': 0.0,
                'floor': -50.0,
            },
        }
        self._pending_auto_ref: tuple[str, float] | None = None
        # Geometry (span/RBW/points/window) each tracker's learned floor belongs to.
        self._auto_ref_geometry_seen: dict[str, tuple | None] = {'std': None, 'rta': None}

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
            self.apply_caps_from_defaults()   # device-reported full span beats the table
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
                    fmin=float(p.CenterFreq_Hz - p.Span_Hz / 2.0),
                    fmax=float(p.CenterFreq_Hz + p.Span_Hz / 2.0),
                    ref=float(p.RefLevel_dBm), rbw=float(p.RBW_Hz),
                    vbw=float(p.VBW_Hz), points=int(p.TracePoints), atten=int(p.Atten),
                    window=int(p.Window.value if hasattr(p.Window, 'value') else p.Window),
                    rbw_mode=rbw_mode,
                    vbw_mode=vbw_modes.get(int(getattr(p.VBWMode, 'value', p.VBWMode)), 'bypass'),
                    spur=spur_modes.get(int(getattr(p.SpurRejection, 'value', p.SpurRejection)), 'bypass'),
                    detector='auto',
                    preamp=int(getattr(p.Preamplifier, 'value', p.Preamplifier)),
                    ifgain=int(p.IFGainGrade),
                    gain_strategy=int(getattr(p.GainStrategy, 'value', p.GainStrategy)),
                    sweep_time_mode=int(getattr(p.SweepTimeMode, 'value', p.SweepTimeMode)),
                    sweep_time=float(p.SweepTime),
                )
            except Exception:
                self.preset_defaults = None

    def apply_caps_from_defaults(self) -> None:
        """Widen the model-table frequency range with the range the device reports as its
        own full span. On this SAN-90 the device (and the official SAStudio Full Span) spans
        8 kHz - 9.02 GHz, while the product manual table says 9 kHz - 9 GHz; the narrower
        table clipped the preset full span and the outermost tuning range."""
        d = self.preset_defaults
        caps = self.state.caps
        if not d or caps is None:
            return
        lo, hi = d.get('fmin'), d.get('fmax')
        if lo is None or hi is None or hi <= lo:
            return
        caps.freq_min_hz = min(caps.freq_min_hz, float(lo))
        caps.freq_max_hz = max(caps.freq_max_hz, float(hi))

    def reset_sdr_state(self) -> None:
        """Restore every SDR parameter to the power-on defaults.

        The dataclass defaults are the single source of truth for "initial state", so a
        fresh DeviceState is used instead of duplicating literals here.
        """
        s = self.state
        d = DeviceState()
        s.sdr_center_hz = d.sdr_center_hz
        s.sdr_decimate = d.sdr_decimate
        s.sdr_listen_hz = d.sdr_listen_hz
        s.sdr_demod = d.sdr_demod
        s.sdr_if_bw = d.sdr_if_bw
        s.sdr_squelch = d.sdr_squelch
        s.sdr_squelch_open = False
        s.sdr_volume = d.sdr_volume
        s.sdr_agc = d.sdr_agc
        s.sdr_pitch = d.sdr_pitch
        s.sdr_deemph_us = d.sdr_deemph_us
        s.sdr_level_dbfs = d.sdr_level_dbfs
        s.sdr_adm = {}
        s.sdr_actual = {}

    def reset_common_state(self) -> None:
        """Reset the front-end settings that are shared by every mode."""
        s = self.state
        d = DeviceState()
        s.ref_clock = d.ref_clock
        s.refclk_out = d.refclk_out
        s.refclk_ppm = 0.0
        s.last_cal_freq = 0.0
        s.last_error = ''

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
        s.trigger_source = DEFAULT_TRIGGER_SOURCE
        s.trigger_edge = DEFAULT_TRIGGER_EDGE
        s.trigger_level_dbm = DEFAULT_TRIGGER_LEVEL_DBM
        s.trigger_safe_time_s = DEFAULT_TRIGGER_SAFE_TIME_S
        s.trigger_delay_s = DEFAULT_TRIGGER_DELAY_S
        s.trigger_pre_time_s = DEFAULT_TRIGGER_PRE_TIME_S
        s.trigger_acq_time_s = DEFAULT_TRIGGER_ACQ_TIME_S
        s.trigger_retrigger_count = DEFAULT_TRIGGER_RETRIGGER_COUNT
        s.trigger_retrigger_period_s = DEFAULT_TRIGGER_RETRIGGER_PERIOD_S
        s.trigger_out = DEFAULT_TRIGGER_OUT
        s.trigger_out_polarity = DEFAULT_TRIGGER_OUT_POLARITY
        s.trigger_actual = {}
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
            s.detector = d.get('detector', 'auto')
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

    def _apply_ifagc(self) -> None:
        """Programme the IF AGC target before configuring a profile that enables it.

        Target is "dBFS from ADC saturation" (header: range 0..-30); the official
        Settings.ini uses -9. Errors are non-fatal: some models do not implement IF AGC.
        """
        target = sb.c_double(float(self.state.ifagc_target))
        sb.dll.Device_InitIFAGC(sb.pointer(self.dev))
        sb.dll.Device_SetIFAGCTarget(sb.pointer(self.dev), sb.byref(target))

    def _profile(self):
        T = sb
        s = self.state
        p = sb.SWP_Profile_TypeDef()
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
        p.EnableIFAGC = 1 if s.ifagc else 0
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
        detector_map = {
            'sample': T.TraceDetector_TypeDef.TraceDetector_Sample,
            'pos_peak': T.TraceDetector_TypeDef.TraceDetector_PosPeak,
            'neg_peak': T.TraceDetector_TypeDef.TraceDetector_NegPeak,
            'rms': T.TraceDetector_TypeDef.TraceDetector_RMS,
            'auto_peak': T.TraceDetector_TypeDef.TraceDetector_AutoPeak,
        }
        if s.detector in detector_map:
            p.TraceDetectMode = T.TraceDetectMode_TypeDef.TraceDetectMode_Manual
            p.TraceDetector = detector_map[s.detector]
        else:
            p.TraceDetectMode = T.TraceDetectMode_TypeDef.TraceDetectMode_Auto
        return p

    def configure_swp(self):
        with self._hw:
            if not self.state.connected:
                return False, 'not connected'
            pin = self._profile()
            if self.state.ifagc:
                self._apply_ifagc()
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
            self.begin_auto_reference_settle('std')
            self._read_amp_atten()
            return True, 'ok'

    def _clear_auto_ref_floor(self) -> None:
        """Forget the learned IF-overflow floor after a front-end change.

        The floor records "the IF saturated at this Ref" for a given attenuation/preamp;
        changing either moves the saturation point, so the old bound is meaningless.
        """
        for tracker in self._auto_ref.values():
            tracker['floor'] = -50.0

    def _read_amp_atten(self) -> None:
        with self._hw:
            """Read back the actual attenuation/preamplifier state (per Device_GetAmpAttenState from the original implementation)."""
            try:
                amp = sb.PreamplifierState_TypeDef()
                att = sb.c_int(0)
                sp = sb.c_uint8(0)
                sb.dll.Device_GetAmpAttenState(sb.pointer(self.dev), sb.pointer(amp),
                                               sb.pointer(att), sb.pointer(sp))
                if (self.state.amp_atten != att.value
                        or self.state.preamplifier_actual != int(amp.value)):
                    # Front-end changed: the learned IF-overflow floor no longer applies.
                    self._clear_auto_ref_floor()
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
                # SWP_GetFullSweep takes NO length argument: the device writes its own current
                # trace length, which can exceed the point count recorded at configuration time
                # (an auto point strategy, or an RBW/Ref change, re-derives it). Sizing these
                # buffers to exactly `n` let the device - and then DSP_InterceptSpectrum, which
                # writes `_cnt` points - run past them; the damage surfaced as a SIGSEGV inside
                # the DSP call. Allocate for the largest trace the device can produce and treat
                # the recorded count as what we hand on, not as the buffer size.
                cap = max(int(n), _SWP_TRACE_MAX)
                if self._freq_buf is None or len(self._freq_buf) != cap:
                    self._freq_buf = (sb.c_double * cap)()
                    self._spec_buf = (sb.c_float * cap)()
                    self._ifreq_buf = (sb.c_double * cap)()
                    self._ispec_buf = (sb.c_float * cap)()
                st = sb.dll.SWP_GetFullSweep(sb.pointer(self.dev), self._freq_buf,
                                             self._spec_buf, sb.pointer(self._meas_aux))
                if st != 0:
                    # Vendor warnings (e.g. -12 IF overflow) still mean "no usable frame",
                    # but they must be reported, not treated as a failure.
                    self.state.status_warning = int(st) if st in sb.WARN_STATUS else 0
                    return None
                self.state.status_warning = 0
                self.state.refclk_ppm = float(getattr(self._meas_aux, 'RefClkFreqOffset', 0.0))
                # IF AGC gain actually applied by the device (dB); useful to prove whether
                # the AGC is acting at all.
                self.state.ifagc_gain = float(getattr(self._meas_aux, 'IFAGCGain', 0.0))
                sb.dll.DSP_InterceptSpectrum(
                    sb.c_double(self._user_start), sb.c_double(self._user_stop),
                    self._freq_buf, self._spec_buf, sb.c_uint32(n),
                    self._ifreq_buf, self._ispec_buf, sb.pointer(self._cnt))
                c = self._cnt.value
                if c < 2:
                    return None
                if c > cap:
                    # More points than the device can legitimately produce: refuse the frame
                    # instead of building a view past the buffer (and report it).
                    self.state.last_error = 'sweep: %d intercept points (cap %d)' % (c, cap)
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
                    floor_index = int((finite.size - 1) * 0.3)
                    noise_floor = float(np.partition(finite, floor_index)[floor_index])
                    self.observe_reference_peak(
                        self.state.mode, float(np.max(finite)), noise_floor)
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
                prof = sb.SWP_Profile_TypeDef()
                po = sb.SWP_Profile_TypeDef()
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

    def observe_reference_peak(
        self, mode: str, peak_dbm: float, noise_floor_dbm: float | None = None
    ) -> None:
        """Queue a stable, hysteretic automatic reference-level adjustment."""
        with self._hw:
            self._observe_reference_peak_locked(mode, peak_dbm, noise_floor_dbm)

    def _observe_reference_peak_locked(
        self, mode: str, peak_dbm: float, noise_floor_dbm: float | None = None
    ) -> None:
        if mode not in ('std', 'rta') or not math.isfinite(peak_dbm):
            return
        state = self.state
        ref_mode = state.rta_ref_mode if mode == 'rta' else state.ref_mode
        if ref_mode != 'auto' or state.atten != -1:
            return
        tracker = self._auto_ref[mode]
        now = time.monotonic()
        if now < tracker['ignore_until']:
            return
        tracker['last_peak'] = peak_dbm
        tracker['last_noise_floor'] = noise_floor_dbm
        if (
            noise_floor_dbm is not None
            and math.isfinite(noise_floor_dbm)
            and peak_dbm - noise_floor_dbm < 15.0
        ):
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            if self._pending_auto_ref and self._pending_auto_ref[0] == mode:
                self._pending_auto_ref = None
            return
        current = state.rta_ref_level if mode == 'rta' else state.ref_level
        # Industry rule: anchor on the NOISE FLOOR so it sits just above the bottom of the
        # display window, and lift Ref only as far as needed to keep the peak off the top
        # edge. (Anchoring on the peak instead - the previous `peak + 5` - left the noise
        # floor up to 7 divisions above the bottom for weak signals, which is the opposite
        # of what a spectrum analyser does.) The window height (grid divisions x dB/div)
        # comes from the frontend; 100 dB is the default 10 div x 10 dB/div.
        window = max(20.0, float(getattr(state, 'ref_range_db', 100.0)))
        if noise_floor_dbm is None or not math.isfinite(noise_floor_dbm):
            # No floor estimate available: fall back to keeping the peak below the top edge.
            target = peak_dbm + 10.0
        else:
            target = max(noise_floor_dbm + window - 8.0, peak_dbm + 10.0)
        target = math.ceil(target / 5.0) * 5.0
        target = min(30.0, max(-50.0, target, tracker.get('floor', -50.0)))
        # Window criterion instead of a bare 5 dB dead-band: while the noise floor sits
        # between 4 and 12 dB above the bottom edge AND the peak keeps >= 8 dB of headroom,
        # the placement is already right, so a wobbling estimate (or a small RBW/point change
        # that moves the noise floor by a dB or two) must not trigger a reconfiguration -
        # each one costs a full device reconfigure and is visible as a jump.
        if noise_floor_dbm is not None and math.isfinite(noise_floor_dbm):
            noise_above_bottom = noise_floor_dbm - (current - window)
            headroom = current - peak_dbm
            if 4.0 <= noise_above_bottom <= 12.0 and headroom >= 8.0:
                tracker['candidate'] = None
                tracker['candidate_since'] = 0.0
                return
        # When the noise floor is high, keep ~30 dB of headroom above it.
        if noise_floor_dbm is not None and math.isfinite(noise_floor_dbm):
            target = max(target, noise_floor_dbm + 30.0)
        target = min(30.0, target)
        if abs(target - current) < 5.0:
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            return

        if tracker['candidate'] != target:
            tracker['candidate'] = target
            tracker['candidate_since'] = now
        # Raise the reference immediately for overload safety. Lowering waits for a
        # time-stable peak so RTA settle/empty frames cannot collapse Ref. After a re-arm
        # (the user pressed Auto, or a setting changed) the first decision is taken quickly
        # in both directions: the previous observation is known to be stale.
        fresh = bool(tracker.pop('fresh', False))
        stable_for = 0.15 if (fresh or target > current) else 1.5
        if (
            now - tracker['candidate_since'] >= stable_for
            and now - tracker['last_change'] >= 1.0
        ):
            tracker['last_change'] = now
            self._pending_auto_ref = (mode, target)

    def prepare_auto_reference_retune(self, mode: str) -> bool:
        """Use a safe Ref before changing frequency when Auto Ref had lowered it."""
        with self._hw:
            state = self.state
            ref_mode = state.rta_ref_mode if mode == 'rta' else state.ref_mode
            if ref_mode != 'auto':
                return False
            if mode == 'rta':
                changed = state.rta_ref_level < 0.0
                state.rta_ref_level = max(0.0, state.rta_ref_level)
            else:
                changed = state.ref_level < 0.0
                state.ref_level = max(0.0, state.ref_level)
            self.begin_auto_reference_settle(mode)
            return changed

    def _auto_ref_geometry(self, mode: str) -> tuple:
        """Signature of the measurement geometry the learned floor belongs to.

        The IF saturates at a Ref that depends on the in-band power, i.e. on span / RBW /
        points / window - not only on the front-end. A floor learned at another geometry
        either blocks a legitimate low Ref or invites saturation probing, so it is dropped
        when this signature changes. Applying a new Ref does NOT change it (verified by the
        signature itself), which is what keeps the auto-ref from clearing its own floor on
        every application.
        """
        s = self.state
        if mode == 'rta':
            # The RTA profile takes its window from the same user setting as SWP.
            return (s.rta_center_hz, s.rta_span_hz, s.rta_rbw_hz, s.rta_vbw_hz,
                    s.window, getattr(s, 'rta_decimate', 0))
        return (s.center_hz, s.span_hz, s.rbw_hz, s.vbw_hz, s.window, 0)

    def begin_auto_reference_settle(self, mode: str, delay: float = 0.75) -> None:
        """Discard stale auto-ref observations after any acquisition reconfiguration."""
        with self._hw:
            geometry = self._auto_ref_geometry(mode)
            if self._auto_ref_geometry_seen.get(mode) not in (None, geometry):
                # Span/RBW/points/window changed: the learned saturation floor no longer
                # describes this configuration.
                self._auto_ref[mode]['floor'] = -50.0
            self._auto_ref_geometry_seen[mode] = geometry
            tracker = self._auto_ref[mode]
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            tracker['last_peak'] = None
            tracker['last_noise_floor'] = None
            tracker['ignore_until'] = time.monotonic() + delay
            tracker['fresh'] = True
            if self._pending_auto_ref and self._pending_auto_ref[0] == mode:
                self._pending_auto_ref = None

    def reset_auto_reference(self, mode: str) -> None:
        """Re-arm: drop stale observations AND force a fresh decision.

        Called when the user enables Auto and whenever a setting changes that moves the
        trace (a reconfiguration calls begin_auto_reference_settle instead, which also
        re-arms). This is the event that answers "when should Auto act again?": a setting
        change invalidates the level the previous decision was based on, even if the new
        target ends up within the 5 dB dead-band.
        """
        with self._hw:
            tracker = self._auto_ref[mode]
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            tracker['last_peak'] = None
            tracker['last_noise_floor'] = None
            tracker['ignore_until'] = time.monotonic() + 0.25
            tracker['fresh'] = True
            if self._pending_auto_ref and self._pending_auto_ref[0] == mode:
                self._pending_auto_ref = None

    def nudge_reference_out_of_overflow(self) -> bool:
        """Raise Ref one step when the device reports IF overflow (-12).

        The vendor's remedy for -12 is to raise RefLevel_dBm. This cannot live in the normal
        auto-reference path because that path needs a measured peak, and an overflowing IF
        delivers no frames at all - so clicking Auto Ref after the warning appeared did
        nothing (deadlock). Queues one step per second at most.
        """
        with self._hw:
            s = self.state
            if s.status_warning != -12 or s.mode == 'sdr':
                return False
            mode = s.mode
            if mode not in ('std', 'rta'):
                return False
            if (s.rta_ref_mode if mode == 'rta' else s.ref_mode) != 'auto' or s.atten != -1:
                return False
            tracker = self._auto_ref[mode]
            now = time.monotonic()
            if now - tracker['last_change'] < 1.0:
                return False
            current = s.rta_ref_level if mode == 'rta' else s.ref_level
            target = min(30.0, current + 5.0)
            if target <= current:
                return False
            tracker['last_change'] = now
            tracker['candidate'] = None
            tracker['candidate_since'] = 0.0
            # Learn the usable lower bound: the IF overflows at this Ref, so never propose
            # one this low again. Without it the peak-based rule keeps trying to go back down
            # and the two mechanisms fight, oscillating 5-10 dB (measured).
            tracker['floor'] = max(tracker.get('floor', -50.0), target)
            # Cleared so the next tick does not queue another step before this one lands;
            # the device re-reports -12 on the following frame if it is still saturating.
            s.status_warning = 0
            self._pending_auto_ref = (mode, target)
            return True

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
