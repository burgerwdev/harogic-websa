"""
hardware/device.py -- device abstraction (full SAN series)

Source: HarogicDevice migrated from web_sa/server.py (v0.11.1); behavior kept,
structure refactored.
- The only device entry point for the business layer: open/configure/fetch/query
- All SDK calls are executed serially via hardware/sdk_bindings (inside the event loop)
"""
from __future__ import annotations

import logging

import numpy as np

from ..config import (
    DeviceCapabilities,
    clamp_ref_dbm,
    fit_center_span,
)
from . import sdk_bindings as sb
from .auto_reference import AutoReferenceController
from .errors import DeviceError  # noqa: F401 (re-exported for the business layer)
from .state import DeviceState, RtaParams, SdrParams, TriggerParams  # noqa: F401 (re-export)

log = logging.getLogger(__name__)

# Convenient aliases for hardware enums
SWP = sb
T = sb  # type aliases


# Safety bound for the swept trace buffers: SWP_GetFullSweep writes the device's own trace
# length, so the buffer must be at least as large as the largest trace it can report. This is
# well above anything the UI can request (the points selector tops out at 4000; the widest
# vendor FFT is ~26k) and costs ~1.5 MB.
_SWP_TRACE_MAX = 65536

#: Consecutive transport errors (sdk_bindings.LINK_LOST_STATUS) with no successful SDK call in
#: between before the USB link is declared lost. A single bus error is tolerated because the
#: vendor remedies it by re-issuing the configuration; a physical unplug produces a run of them
#: (at the acquisition rate this declares the disconnect within a frame or two).
_LINK_ERROR_LIMIT = 8


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
        # Consecutive transport errors; a successful SDK call clears it (note_link_status).
        self._link_errors = 0
        # Auto-reference control loop: decision logic, not device I/O (report finding P1-8).
        self.auto_ref = AutoReferenceController(self)

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
            self._link_errors = 0
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

    def reopen(self) -> tuple[bool, str]:
        """Recover the USB link: close the dead handle and open the device again.

        The vendor's remedy for a bus error is to reopen the device (API guide, -8/-9), and a
        physical unplug makes the open fail - which is fine, the caller (the worker link loop,
        `main._link_loop`) simply retries until the analyzer is plugged back in.
        """
        with self._hw:
            self.close()
            self._link_errors = 0
            self.state.last_error = ''
            return self.open()

    # ---------------- Link health ----------------
    def note_link_status(self, status: int, where: str) -> bool:
        """Feed one SDK return status to the transport watchdog.

        Returns True when this call just declared the link lost. A run of
        :data:`sdk_bindings.LINK_LOST_STATUS` (bus error / bad data) with no successful call in
        between means the analyzer is no longer answering; a single one is tolerated because the
        vendor remedies it with a reconfiguration (and the SDR stream sees bad packets routinely).
        """
        if status == 0:
            self._link_errors = 0
            return False
        if status not in sb.LINK_LOST_STATUS:
            self._link_errors = 0
            return False
        self._link_errors += 1
        if self._link_errors >= _LINK_ERROR_LIMIT and self.state.connected:
            self.mark_link_lost(f'{where} status={status}')
            return True
        return False

    def mark_link_lost(self, reason: str) -> None:
        """Declare the USB link dead so STATUS reports `connected=false`.

        The acquisition loop stops stepping the (dead) handle and the worker link loop reopens
        the device; the page shows the disconnect instead of a silently frozen trace.
        """
        if self.state.connected:
            log.warning('device link lost: %s', reason)
        self.state.connected = False
        self.state.last_error = reason

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
        """Restore every SDR parameter to the power-on defaults (fresh group dataclass)."""
        self.state.sdr = SdrParams()

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
        """Restore the RTA window and the trigger defaults (fresh group dataclasses)."""
        self.state.rta = RtaParams()
        self.state.trigger = TriggerParams()
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
        # The device's own range, from its capability row (a preset or a stale client value can
        # still arrive outside it).
        p.RefLevel_dBm = clamp_ref_dbm(s.caps, s.ref_level)
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
                self.note_link_status(st, 'SWP_Configuration')
                self.state.last_error = 'SWP_Configuration status=%d' % st
                return False, self.state.last_error
            self.note_link_status(0, 'SWP_Configuration')
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
        self.auto_ref.clear_learned_floor()

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
                    # but they must be reported, not treated as a failure. A run of bus errors
                    # means the analyzer is gone: declare the link lost (connected=false), so
                    # the page stops showing a frozen trace as "connected" and the worker can
                    # reopen the device when it comes back.
                    self.note_link_status(st, 'SWP_GetFullSweep')
                    self.state.status_warning = int(st) if st in sb.WARN_STATUS else 0
                    return None
                self.note_link_status(0, 'SWP_GetFullSweep')
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

    # ---------------- Auto reference (control loop lives in auto_reference.py) ----------------

    def observe_reference_peak(self, mode: str, peak_dbm: float,
                               noise_floor_dbm: float | None = None) -> None:
        """Feed one trace observation to the Auto Ref control loop."""
        self.auto_ref.observe_peak(mode, peak_dbm, noise_floor_dbm)

    def prepare_auto_reference_retune(self, mode: str) -> bool:
        """Use a safe Ref before changing frequency when a fit had lowered it."""
        return self.auto_ref.prepare_retune(mode)

    def auto_scale(self, mode: str, current: float | None = None) -> tuple[str, float | None]:
        """Place the reference level once, from the newest trace (the user's Auto Scale)."""
        return self.auto_ref.fit(mode, current)

    def auto_reference_scope(self) -> str:
        """Which tracker the active session drives ('std' for plain SWP)."""
        return getattr(self.session, 'auto_ref_scope', 'std')

    def begin_auto_reference_settle(self, mode: str, delay: float = 0.75) -> None:
        """Discard stale auto-ref observations after any acquisition reconfiguration."""
        self.auto_ref.begin_settle(mode, delay)

    def reset_auto_reference(self, mode: str) -> None:
        """Re-arm Auto Ref so the next observation decides again."""
        self.auto_ref.reset(mode)

    def nudge_reference_out_of_overflow(self) -> bool:
        """Raise Ref one step when the device reports IF overflow (-12)."""
        return self.auto_ref.nudge_out_of_overflow()

    def apply_pending_auto_reference(self) -> bool:
        """Apply one queued auto-reference update in the active acquisition worker."""
        return self.auto_ref.apply_pending()

    # ---------------- Session host ----------------
    def set_session(self, session) -> None:
        with self._hw:
            self.session = session
            self.state.mode = session.name if session else 'std'

    def auto_reference_view(self) -> dict:
        """Auto-reference diagnostics for STATUS.

        The tracker state is private to this class (it is a control loop, not device
        state); the serializer must not reach into it directly (report finding P1-7).
        """
        return self.auto_ref.view(getattr(self.session, 'auto_ref_scope', 'std'))

    def session_health(self) -> dict:
        """Health counters of the active measurement session (empty for plain SWP)."""
        session = self.session
        if session is None:
            return {}
        return session.health()

    def step(self):
        """publisher single step: forward to the current session."""
        return self.session.step() if self.session else None
