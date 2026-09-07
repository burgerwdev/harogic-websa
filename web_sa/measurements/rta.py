"""
measurements/rta.py -- Real-Time Spectrum (RTA) session

Uses the device FPGA real-time spectrum engine (RTA_Configuration +
RTA_BusTriggerStart + RTA_GetRealTimeSpectrum). Emits compact frames:
  - RTAF: real-time trace (float32 dBm, downsampled to ~1001 points) + latest
    waterfall row (uint16 density, downsampled to display width) + meta.

Waterfall rows are pushed one per step so the frontend can accumulate a rolling
waterfall without huge per-frame payloads.
"""
from __future__ import annotations

import logging
import time

import numpy as np

from ..hardware.device import DeviceError
from .base import MeasurementSession

log = logging.getLogger(__name__)


def _dbg(msg: str) -> None:
    log.debug('RTA %s', msg)


class RtaSession(MeasurementSession):
    """Real-time spectrum session: continuous RTA acquisition."""

    name = 'rta'

    FULL_SPAN_HZ = 50.78125e6
    DISPLAY_POINTS = 1001
    WATERFALL_WIDTH = 860      # waterfall row width after downsample

    def __init__(self, dev):
        super().__init__(dev)
        # Serializes every DLL call on the device handle against all other hardware
        # activity (RTA step on the asyncio.to_thread worker, WS reconfiguration,
        # STATUS/HTTP queries) via the device-wide re-entrant lock. libhtraapi is not
        # thread-safe; concurrent calls caused GPF/segfaults during RTA span switches.
        self._lock = dev._hw
        self._ready = False
        ratio = max(1.0, self.FULL_SPAN_HZ / dev.state.rta_span_hz)
        self._decimate = 2 ** max(0, int(round(np.log2(ratio))))
        self._trace = None
        self._bitmap = None
        self._plot = None
        self._trigger = None
        self._aux = None
        self._last_wf_time = 0.0
        self._error_streak = 0
        self._recovery_attempts = 0
        self._last_recovery = 0.0

    def enter(self):
        """Snapshot standard config, then configure RTA."""
        super().enter()
        self._configure()

    def _configure(self):
        with self._lock:
            self._configure_locked(recovery=False)

    def _configure_locked(self, recovery=False):
        import htra_api as T

        from ..hardware import sdk_bindings as _sb
        dev = self.dev
        s = dev.state
        _dbg('CONF enter ready=%s dec=%s rbw=%s sweep=%s center=%.3e' % (
            self._ready, self._decimate, s.rta_rbw_mode,
            s.rta_sweep_time_mode, s.rta_center_hz))
        # Drop stale buffers first: a mid-stream reconfigure while old trace/bitmap
        # buffers are still referenced by an in-flight Get would let the DLL write into
        # freed/reallocated memory -> GPF. Fresh config = fresh buffers.
        # Also stop any in-progress acquisition first: RTA_Configuration called while the
        # device is still streaming (after BusTriggerStart) can hang the firmware.
        self._ready = False
        _dbg('CONF calling RTA_BusTriggerStop ...')
        try:
            T.dll.RTA_BusTriggerStop(T.pointer(dev.dev))
        except Exception as e:
            _dbg('CONF BusTriggerStop EXC %r' % (e,))
        _dbg('CONF BusTriggerStop done')
        prof = T.RTA_Profile_TypeDef()
        out = T.RTA_Profile_TypeDef()
        info = T.RTA_FrameInfo_TypeDef()
        _dbg('CONF calling RTA_ProfileDeInit ...')
        T.dll.RTA_ProfileDeInit(T.pointer(dev.dev), T.pointer(prof))
        _dbg('CONF RTA_ProfileDeInit done')
        prof.CenterFreq_Hz = s.rta_center_hz
        prof.RefLevel_dBm = s.rta_ref_level
        prof.DecimateFactor = self._decimate
        prof.Atten = int(s.atten)
        prof.Preamplifier = (
            T.PreamplifierState_TypeDef.AutoOn
            if s.preamplifier == 0
            else T.PreamplifierState_TypeDef.ForcedOff
        )
        prof.IFGainGrade = int(s.ifgain)
        prof.GainStrategy = (
            T.GainStrategy_TypeDef.LowNoisePreferred
            if s.gain_strategy == 0
            else T.GainStrategy_TypeDef.HighLinearityPreferred
        )
        ref_clock_map = {
            'internal': T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal,
            'external': T.ReferenceClockSource_TypeDef.ReferenceClockSource_External,
            'premium': T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal_Premium,
            'external_forced': T.ReferenceClockSource_TypeDef.ReferenceClockSource_External_Forced,
        }
        prof.ReferenceClockSource = ref_clock_map.get(
            s.ref_clock, T.ReferenceClockSource_TypeDef.ReferenceClockSource_Internal)
        prof.ExternalSystemClockFrequency = 10e6
        prof.EnableReferenceClockOut = 1 if s.refclk_out else 0
        if s.rta_rbw_mode == 'manual' and s.rta_rbw_hz > 0:
            prof.RBWMode = T.RBWMode_TypeDef.RBW_Manual
            prof.RBW_Hz = s.rta_rbw_hz
        else:
            prof.RBWMode = T.RBWMode_TypeDef.RBW_Auto   # RBW follows span (span/2000)
        _vbw_map = {'manual': T.VBWMode_TypeDef.VBW_Manual,
                    'equal': T.VBWMode_TypeDef.VBW_EqualToRBW,
                    'tenth': T.VBWMode_TypeDef.VBW_TenPercentRBW,
                    'onethousandth': T.VBWMode_TypeDef.VBW_OnePercentRBW,
                    'bypass': T.VBWMode_TypeDef.VBW_TenTimesRBW}
        prof.VBWMode = _vbw_map.get(s.rta_vbw_mode, T.VBWMode_TypeDef.VBW_EqualToRBW)
        if s.rta_vbw_mode == 'manual' and s.rta_vbw_hz > 0:
            prof.VBW_Hz = s.rta_vbw_hz
        prof.TriggerSource = T.RTA_TriggerSource_TypeDef.Bus
        prof.TriggerMode = T.TriggerMode_TypeDef.FixedPoints
        prof.TriggerAcqTime = 0.005   # short acq -> PacketCount=1, ~150fps (probe-verified)
        prof.SweepTimeMode = T.SweepTimeMode_TypeDef(s.rta_sweep_time_mode)
        prof.SweepTime = float(s.rta_sweep_time)
        _dbg('CONF calling RTA_Configuration dec=%s ...' % self._decimate)
        st = T.dll.RTA_Configuration(T.pointer(dev.dev), T.pointer(prof), T.pointer(out), T.pointer(info))
        _dbg('CONF RTA_Configuration ret=%s' % st)
        if st != 0:
            dev.state.last_error = 'RTA_Configuration status=%d' % st
            raise RuntimeError(dev.state.last_error)
        s.rta_span_hz = float(info.StopFrequency_Hz - info.StartFrequency_Hz)
        s.rta_actual = {
            'center': float(s.rta_center_hz),
            'span': s.rta_span_hz,
            'start': float(info.StartFrequency_Hz),
            'stop': float(info.StopFrequency_Hz),
            'ref': float(out.RefLevel_dBm),
            'rbw': float(out.RBW_Hz),
            'vbw': float(out.VBW_Hz),
            'points': int(info.FrameWidth),
            'refclk': float(out.ReferenceClockFrequency),
            'refclk_src': int(out.ReferenceClockSource.value),
            'refclk_out': bool(out.EnableReferenceClockOut),
            'atten': int(out.Atten),
            'preamp': int(out.Preamplifier.value),
            'ifgain': int(out.IFGainGrade),
        }
        dev._read_amp_atten()
        self._info = info
        # Get writes SpectrumStream of PacketValidPoints bytes into the buffer; allocate
        # over both reported sizes + margin so a DLL write can never run past the end
        # (out-of-bounds write = segfault). PacketValidPoints can exceed PacketSamplePoints
        # depending on decimate/frame layout.
        n = int(max(int(info.PacketValidPoints), int(info.PacketSamplePoints))) + 4096
        bitmap_points = int(info.FrameHeight) * int(info.FrameWidth)
        max_buffer_points = 16_000_000
        if (
            n <= 4096
            or n > max_buffer_points
            or bitmap_points <= 0
            or bitmap_points > max_buffer_points
        ):
            raise RuntimeError('invalid RTA buffer dimensions')
        self._trace = (T.c_uint8 * n)()
        self._bitmap = (T.c_uint16 * (bitmap_points + 65536))()
        self._plot = T.RTA_PlotInfo_TypeDef()
        self._trigger = T.RTA_TriggerInfo_TypeDef()
        # CRITICAL: MeasAuxInfo_TypeDef in the official htra_api.py wrapper is 48 bytes but
        # the DLL writes the FULL structure (72 bytes incl. IFAGCGain/RefClkFreqOffset/
        # nsSinceEpoch -- see sdk_bindings.Full_MeasAuxInfo). Using the 48-byte struct made
        # every RTA_GetRealTimeSpectrum overrun the buffer by 24 bytes -> heap corruption ->
        # deterministic GPF on the 2nd fetch. Must use the full-size struct.
        self._aux = _sb.Full_MeasAuxInfo()
        self._ready = True
        self._error_streak = 0
        if not recovery:
            self._recovery_attempts = 0
        dev.state.config_version += 1
        dev.state.freq_version += 1
        dev.state.last_error = ''
        self._last_wf_time = 0.0
        self._last_get = 0.0
        # Short settle after configuration (Configuration returned synchronously; the
        # device is ready quickly - official UI switches instantly). With all DLL calls
        # serialized under dev._hw there is no concurrent-Get hazard; too-early Gets just
        # fail cleanly (st != 0 -> skip). 0.35s is a safe margin.
        self._ready_at = time.monotonic() + 0.35
        self._dbg_n = 0

    def _step_failed_locked(self, stage: str, status) -> None:
        self._error_streak += 1
        if self._error_streak < 8:
            return
        now = time.monotonic()
        if now - self._last_recovery < 1.0:
            return
        message = f'RTA {stage} failed repeatedly (status={status})'
        self.dev.state.last_error = message
        if self._recovery_attempts >= 2:
            raise DeviceError(message)
        self._recovery_attempts += 1
        self._last_recovery = now
        log.warning('%s; reconfigure attempt %d/2', message, self._recovery_attempts)
        try:
            self._configure_locked(recovery=True)
        except Exception as exc:
            raise DeviceError(f'RTA recovery configuration failed: {exc}') from exc

    def set_params(self, center=None, span=None):
        """Update the mode-private RTA frequency window and configure exactly once."""
        import math

        s = self.dev.state
        if span is not None:
            ratio = max(1.0, self.FULL_SPAN_HZ / float(span))
            self._decimate = min(65536, 2 ** max(0, int(round(math.log2(ratio)))))
            s.rta_span_hz = self.FULL_SPAN_HZ / self._decimate
        requested_center = s.rta_center_hz if center is None else float(center)
        half_span = s.rta_span_hz / 2
        s.rta_center_hz = max(
            s.caps.freq_min_hz + half_span,
            min(s.caps.freq_max_hz - half_span, requested_center),
        )
        self._configure()

    def set_sweep(self, mode=0, time=0.0):
        s = self.dev.state
        s.rta_sweep_time_mode = max(0, min(8, int(mode)))
        s.rta_sweep_time = float(time)
        self._configure()

    def reset_defaults(self):
        """Restore documented RTA defaults without changing SWP settings."""
        self._decimate = 1
        self.dev.reset_rta_state()
        self._configure()

    def set_vbw(self, mode='equal', vbw=0.0):
        s = self.dev.state
        s.rta_vbw_mode = mode
        s.rta_vbw_hz = float(vbw) if mode == 'manual' else 0.0
        self._configure()

    def set_rbw(self, mode='auto', rbw=0.0):
        s = self.dev.state
        s.rta_rbw_mode = 'manual' if mode == 'manual' else 'auto'
        s.rta_rbw_hz = float(rbw) if s.rta_rbw_mode == 'manual' else 0.0
        self._configure()

    def reconfigure(self):
        self._configure()

    def set_reference(self, mode='manual', ref=None):
        s = self.dev.state
        s.rta_ref_mode = mode
        self.dev.reset_auto_reference('rta')
        if mode == 'manual':
            s.rta_ref_level = float(ref)
            self._configure()

    def exit(self):
        """Exit RTA after stopping acquisition, then restore the SWP snapshot."""
        import htra_api as T

        with self._lock:
            self._ready = False
            try:
                T.dll.RTA_BusTriggerStop(T.pointer(self.dev.dev))
            except Exception:
                log.exception('RTA trigger stop failed during session exit')
        super().exit()

    # Get 节流: 避免 publisher 高频(4ms)调用导致 DLL 不稳定/段错误
    GET_MIN_INTERVAL = 0.0   # no throttle; publisher awaits each step serially (frame rate = 1/step time)

    def step(self):
        """Bus-trigger + fetch one RTA packet (throttled); push trace + waterfall row."""
        if not self._ready:
            return [], []
        now = time.monotonic()
        if now < self._ready_at:
            return [], []      # wait for SWP->RTA switch to settle (avoids GPF)
        if now - self._last_get < self.GET_MIN_INTERVAL:
            return [], []
        import ctypes as C

        import htra_api as T

        dev = self.dev
        self._last_get = now
        # Snapshot frame dimensions and copy the DLL-owned buffers while holding the
        # same lock as reconfiguration. Lock-free work below only touches local arrays.
        with self._lock:
            if not self._ready or self._info is None:
                return [], []
            info = self._info
            self._dbg_n += 1
            log_this = self._dbg_n % 100 == 1
            try:
                st = T.dll.RTA_BusTriggerStart(T.pointer(dev.dev))
            except Exception as exc:
                _dbg('STEP #%d trigger EXC %r' % (self._dbg_n, exc))
                self._step_failed_locked('trigger exception', repr(exc))
                return [], []
            if log_this:
                _dbg('STEP #%d trigger ret=%s' % (self._dbg_n, st))
            if st != 0:
                self._step_failed_locked('trigger', st)
                return [], []
            try:
                status = T.dll.RTA_GetRealTimeSpectrum(
                    dev.dev, self._trace, self._bitmap,
                    T.pointer(self._plot), T.pointer(self._trigger),
                    C.cast(C.byref(self._aux), T.POINTER(T.MeasAuxInfo_TypeDef)))
            except Exception as exc:
                _dbg('STEP #%d Get EXC %r' % (self._dbg_n, exc))
                self._step_failed_locked('get exception', repr(exc))
                return [], []
            if log_this:
                _dbg('STEP #%d Get ret=%s' % (self._dbg_n, status))
            if status != 0:
                self._step_failed_locked('get', status)
                return [], []
            self._error_streak = 0
            self._recovery_attempts = 0

            valid_points = int(info.PacketValidPoints)
            width, height = int(info.FrameWidth), int(info.FrameHeight)
            if width < 2 or height < 1 or valid_points < width:
                raise RuntimeError('invalid RTA frame dimensions')
            trace = np.frombuffer(
                self._trace, dtype=np.uint8, count=valid_points).copy()
            row = np.frombuffer(
                self._bitmap, dtype=np.uint16, count=width * height
            ).reshape(height, width)[-1].copy()
            scale = float(self._plot.ScaleTodBm)
            offset = float(self._plot.OffsetTodBm)
            start_hz = float(info.StartFrequency_Hz)
            stop_hz = float(info.StopFrequency_Hz)
            max_density = int(info.MaxDensityValue)
            freq_version = dev.state.freq_version

        # The stream contains PacketFrame spectra. The current implementation uses the
        # first spectrum; hardware-density semantics remain a separate validation item.
        spectrum = trace.astype(np.float32) * scale + offset
        spectrum = spectrum[:width]
        freq = start_hz + np.arange(width) * (stop_hz - start_hz) / width
        index = np.linspace(0, width - 1, self.DISPLAY_POINTS).astype(np.int64)
        display_spectrum = spectrum[index].astype(np.float32)
        display_freq = freq[index]
        finite = spectrum[np.isfinite(spectrum)]
        if finite.size:
            dev.observe_reference_peak('rta', float(np.max(finite)))
        if width != self.WATERFALL_WIDTH:
            row = row[np.linspace(0, width - 1, self.WATERFALL_WIDTH).astype(np.int64)]
        frame = _encode_rta(
            freq_version, display_freq, display_spectrum, row, max_density,
            start_hz, stop_hz)
        return [frame], []


def _encode_rta(version, freq_hz, spec_dbm, wf_row, max_density, start_hz, stop_hz):
    """RTA frame: hdr(magic+ver+pts+wfLen+maxD+startHz) + freq(f8×pts) + spec(f4×pts)
    + wfRow(u2×wfLen) + stopHz(f8)."""
    import struct
    pts = len(spec_dbm)
    max_density = max(0, min(65535, int(max_density)))
    hdr = b'RTAF' + struct.pack(
        '<IIHHd', version, pts, len(wf_row), max_density, float(start_hz))
    payload = (freq_hz.astype(np.float64).tobytes() +
               spec_dbm.astype(np.float32).tobytes() +
               wf_row.astype(np.uint16).tobytes())
    meta = struct.pack('<d', float(stop_hz))
    return hdr + payload + meta
