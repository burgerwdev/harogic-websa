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

import os
import threading
import time

import numpy as np

# Crash-localization debug log (independent file so it survives service-log rotation/rm)
_RTA_DBG = os.environ.get('RTA_DBG', '') or '/tmp/rta_dbg.log'
_dbgf = open(_RTA_DBG, 'a', buffering=1)


def _dbg(msg: str) -> None:
    try:
        _dbgf.write('[%s] %s\n' % (time.strftime('%H:%M:%S.%f')[:-3], msg))
    except Exception:
        pass

from .base import MeasurementSession


class RtaSession(MeasurementSession):
    """Real-time spectrum session: continuous RTA acquisition."""

    name = 'rta'

    DISPLAY_POINTS = 1001      # trace downsample target (matches SWP display)
    WATERFALL_WIDTH = 860      # waterfall row width after downsample

    def __init__(self, dev):
        super().__init__(dev)
        # Serializes every DLL call on the device handle against all other hardware
        # activity (RTA step on the asyncio.to_thread worker, WS reconfiguration,
        # STATUS/HTTP queries) via the device-wide re-entrant lock. libhtraapi is not
        # thread-safe; concurrent calls caused GPF/segfaults during RTA span switches.
        self._lock = dev._hw
        self._ready = False
        self._decimate = 1         # analysis bandwidth = 50.78M / decimate (1 = full, device Nyquist)
        self._rbw_mode = 'auto'    # RBW follows span (span/2000, official semantics); manual sends RBW_Hz
        self._rbw_hz = 0.0
        # VBW (video bandwidth) is independent per-session (device applies it exactly:
        # manual VBW_Hz passes through, equal/tenpercent/... follow RBW).
        self._vbw_mode = 'equal'   # device default maps to VBW_EqualToRBW
        self._vbw_hz = 0.0
        self._sweep_mode = 2       # default minSWTx4 (RTA sweep speed)
        self._sweep_time = 0.0
        self._trace = None
        self._bitmap = None
        self._plot = None
        self._trigger = None
        self._aux = None
        self._last_wf_time = 0.0

    def enter(self):
        """Snapshot standard config, then configure RTA."""
        super().enter()
        self._configure()

    def _configure(self):
        with self._lock:
            self._configure_locked()

    def _configure_locked(self):
        import htra_api as T
        from ..hardware import sdk_bindings as _sb
        dev = self.dev
        s = dev.state
        _dbg('CONF enter ready=%s dec=%s rbw=%s sweep=%s center=%.3e' % (
            self._ready, self._decimate, self._rbw_mode, self._sweep_mode, s.rta_center_hz))
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
        prof.RefLevel_dBm = s.ref_level
        prof.DecimateFactor = self._decimate
        if self._rbw_mode == 'manual' and self._rbw_hz > 0:
            prof.RBWMode = T.RBWMode_TypeDef.RBW_Manual
            prof.RBW_Hz = self._rbw_hz
        else:
            prof.RBWMode = T.RBWMode_TypeDef.RBW_Auto   # RBW follows span (span/2000)
        _vbw_map = {'manual': T.VBWMode_TypeDef.VBW_Manual,
                    'equal': T.VBWMode_TypeDef.VBW_EqualToRBW,
                    'tenth': T.VBWMode_TypeDef.VBW_TenPercentRBW,
                    'onethousandth': T.VBWMode_TypeDef.VBW_OnePercentRBW,
                    'bypass': T.VBWMode_TypeDef.VBW_TenTimesRBW}
        prof.VBWMode = _vbw_map.get(self._vbw_mode, T.VBWMode_TypeDef.VBW_EqualToRBW)
        if self._vbw_mode == 'manual' and self._vbw_hz > 0:
            prof.VBW_Hz = self._vbw_hz
        prof.TriggerSource = T.RTA_TriggerSource_TypeDef.Bus
        prof.TriggerMode = T.TriggerMode_TypeDef.FixedPoints
        prof.TriggerAcqTime = 0.005   # short acq -> PacketCount=1, ~150fps (probe-verified)
        prof.SweepTimeMode = T.SweepTimeMode_TypeDef(self._sweep_mode)
        prof.SweepTime = float(self._sweep_time)
        _dbg('CONF calling RTA_Configuration dec=%s ...' % self._decimate)
        st = T.dll.RTA_Configuration(T.pointer(dev.dev), T.pointer(prof), T.pointer(out), T.pointer(info))
        _dbg('CONF RTA_Configuration ret=%s' % st)
        if st != 0:
            dev.state.last_error = 'RTA_Configuration status=%d' % st
            return
        # Mirror effective values into dev.state (STATUS / frontend read actual.*):
        # - rbw_mode + actual RBW/VBW selected by the device (manual RBW IS honored: the
        #   device picks the nearest 2^n FFT size; show the applied value, not raw input)
        # - center/span/start/stop so the frequency input + readouts follow the RTA LO
        #   (the SWP "actual" cache would otherwise win and jump the input back)
        dev.state.rbw_mode = self._rbw_mode
        try:
            dev.state.actual['center'] = float(s.rta_center_hz)
            dev.state.actual['start'] = float(info.StartFrequency_Hz)
            dev.state.actual['stop'] = float(info.StopFrequency_Hz)
            dev.state.actual['span'] = float(info.StopFrequency_Hz - info.StartFrequency_Hz)
        except Exception:
            pass
        try:
            actual_rbw = float(out.RBW_Hz)
            if actual_rbw > 0:
                dev.state.rbw_hz = actual_rbw
                try:
                    dev.state.actual['rbw'] = actual_rbw
                except Exception:
                    pass
            actual_vbw = float(out.VBW_Hz)
            if actual_vbw > 0:
                dev.state.vbw_hz = actual_vbw
                try:
                    dev.state.actual['vbw'] = actual_vbw
                except Exception:
                    pass
        except Exception:
            pass
        self._info = info
        # Get writes SpectrumStream of PacketValidPoints bytes into the buffer; allocate
        # over both reported sizes + margin so a DLL write can never run past the end
        # (out-of-bounds write = segfault). PacketValidPoints can exceed PacketSamplePoints
        # depending on decimate/frame layout.
        n = int(max(int(info.PacketValidPoints), int(info.PacketSamplePoints))) + 4096
        self._trace = (T.c_uint8 * n)()
        self._bitmap = (T.c_uint16 * (int(info.FrameHeight) * int(info.FrameWidth) + 65536))()
        self._plot = T.RTA_PlotInfo_TypeDef()
        self._trigger = T.RTA_TriggerInfo_TypeDef()
        # CRITICAL: MeasAuxInfo_TypeDef in the official htra_api.py wrapper is 48 bytes but
        # the DLL writes the FULL structure (72 bytes incl. IFAGCGain/RefClkFreqOffset/
        # nsSinceEpoch -- see sdk_bindings.Full_MeasAuxInfo). Using the 48-byte struct made
        # every RTA_GetRealTimeSpectrum overrun the buffer by 24 bytes -> heap corruption ->
        # deterministic GPF on the 2nd fetch. Must use the full-size struct.
        self._aux = _sb.Full_MeasAuxInfo()
        self._ready = True
        self._last_wf_time = 0.0
        self._last_get = 0.0
        # Short settle after configuration (Configuration returned synchronously; the
        # device is ready quickly - official UI switches instantly). With all DLL calls
        # serialized under dev._hw there is no concurrent-Get hazard; too-early Gets just
        # fail cleanly (st != 0 -> skip). 0.35s is a safe margin.
        self._ready_at = time.monotonic() + 0.35
        self._dbg_n = 0

    def set_params(self, center=None, span=None):
        """Update RTA center and/or analysis span (bandwidth = 50.78M / 2^n) then reconfigure."""
        import math
        s = self.dev.state
        changed = False
        if center is not None:
            s.rta_center_hz = float(center)
            changed = True
        if span is not None and float(span) > 0:
            # map requested span (Hz) to the nearest 2^n decimate step of the full 50.78125M
            ratio = max(1.0, 50.78125e6 / float(span))
            dec = 2 ** max(0, int(round(math.log2(ratio))))
            if dec != self._decimate:
                self._decimate = dec
                changed = True
        if changed:
            self._configure()

    def set_sweep(self, mode=0, time=0.0):
        """Update RTA sweep speed (SweepTimeMode) and reconfigure."""
        self._sweep_mode = max(0, min(8, int(mode)))
        self._sweep_time = float(time)
        self._configure()

    def reset_defaults(self):
        """Preset: return the RTA session to its own defaults (center 1 GHz per
        RTA_ProfileDeInit, dec=1 full span, RBW auto, VBW equal, sweep minSWTx4) and
        reconfigure. Never touches the SWP parameters (preset_state handles those)."""
        self._decimate = 1
        self._rbw_mode = 'auto'
        self._rbw_hz = 0.0
        self._sweep_mode = 2
        self._sweep_time = 0.0
        self._vbw_mode = 'equal'
        self._vbw_hz = 0.0
        self.dev.state.rta_center_hz = 1e9
        self._configure()

    def set_vbw(self, mode='equal', vbw=0.0):
        """VBW for the RTA session (manual VBW_Hz passes through exactly; equal/
        tenth/onethousandth/bypass follow RBW). Independent from SWP (restored on exit)."""
        if mode not in ('manual', 'equal', 'tenth', 'onethousandth', 'bypass'):
            mode = 'equal'
        self._vbw_mode = mode
        self._vbw_hz = float(vbw) if float(vbw) > 0 else 0.0
        self.dev.state.vbw_mode = mode
        if mode == 'manual':
            self.dev.state.vbw_hz = self._vbw_hz
        self._configure()

    def set_rbw(self, mode='auto', rbw=0.0):
        """RBW auto (follows span as span/2000) or manual RBW_Hz; reconfigure under lock.
        Mirror into dev.state so the periodic STATUS push shows the RTA-selected RBW mode
        (otherwise the frontend reverts to the SWP value on every STATUS push)."""
        self._rbw_mode = 'manual' if mode == 'manual' else 'auto'
        self._rbw_hz = float(rbw) if float(rbw) > 0 else 0.0
        _dbg('set_rbw called mode=%s rbw=%s (session=%s)' % (self._rbw_mode, self._rbw_hz, self.dev.session.name if self.dev.session else None))
        self.dev.state.rbw_mode = self._rbw_mode
        if self._rbw_mode == 'manual':
            self.dev.state.rbw_hz = self._rbw_hz
        self._configure()

    def exit(self):
        """Exit RTA: drop acquisition, restore standard config (base handles snapshot)."""
        # Note: RTA_ProfileDeInit here occasionally segfaults libhtraapi; skipping it and
        # letting configure_swp (SWP re-entry) reset the device is more reliable.
        self._ready = False
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
        info = self._info
        self._last_get = now
        # Whole trigger+Get sequence under the lock so a concurrent reconfigure
        # (span/center/sweep) can never interleave with the DLL calls.
        with self._lock:
            self._dbg_n += 1
            log_this = (self._dbg_n % 100 == 1)
            if log_this:
                _dbg('STEP #%d trigger ...' % self._dbg_n)
            try:
                st = T.dll.RTA_BusTriggerStart(T.pointer(dev.dev))
            except Exception as e:
                _dbg('STEP #%d trigger EXC %r' % (self._dbg_n, e))
                return [], []
            if log_this:
                _dbg('STEP #%d trigger ret=%s' % (self._dbg_n, st))
            if st != 0:
                return [], []
            # Official pattern: after one BusTriggerStart, Get repeatedly for PacketCount
            # packets until the full acquisition is drained (single Get + throttle caused
            # device-state conflicts -> GPF in libhtraapi).
            # Wait for the acquisition window (TriggerAcqTime) to fill before Get;
            # Getting too early returns stale/cached data -> effective ~2 fps.
            acq = float(getattr(info, 'PacketAcqTime', 0.05) or 0.05)
            time.sleep(min(0.5, acq + 0.002))   # minimal wait; higher push rate (~130fps)
            st = -1
            nok = 0
            t0 = time.monotonic()
            ngets = 1
            try:
                for _ in range(ngets):
                    if log_this:
                        _dbg('STEP #%d Get ...' % self._dbg_n)
                    s = T.dll.RTA_GetRealTimeSpectrum(
                        dev.dev, self._trace, self._bitmap,
                        T.pointer(self._plot), T.pointer(self._trigger),
                        C.cast(C.byref(self._aux), T.POINTER(T.MeasAuxInfo_TypeDef)))
                    if log_this:
                        _dbg('STEP #%d Get ret=%s' % (self._dbg_n, s))
                    if s != 0:
                        break
                    st = s
                    nok += 1
            except Exception as e:
                _dbg('STEP #%d Get EXC %r' % (self._dbg_n, e))
                return [], []
            dt = (time.monotonic() - t0) * 1000.0
            # End this acquisition so the device is idle: next trigger restarts it, and a
            # concurrent reconfigure never meets a streaming device.
            if log_this:
                _dbg('STEP #%d Stop ...' % self._dbg_n)
            try:
                T.dll.RTA_BusTriggerStop(T.pointer(dev.dev))
            except Exception as e:
                if log_this:
                    _dbg('STEP #%d Stop EXC %r' % (self._dbg_n, e))
            if log_this:
                _dbg('STEP #%d Stop done' % self._dbg_n)
            if st != 0:
                return [], []
        pass
        n = int(info.PacketValidPoints)
        W = int(info.FrameWidth)
        # The device trace is FrameWidth × PacketFrame spectrum frames concatenated
        # in time; taking all of it would show repeated mirror images of each signal.
        # Use the FIRST frame as the current spectrum.
        s_full = np.frombuffer(self._trace, dtype=np.uint8, count=n).astype(np.float32) * self._plot.ScaleTodBm + self._plot.OffsetTodBm
        s = s_full[:W]
        f = info.StartFrequency_Hz + np.arange(W) * (info.StopFrequency_Hz - info.StartFrequency_Hz) / W
        idx = np.linspace(0, W - 1, self.DISPLAY_POINTS).astype(np.int64)
        s_d = s[idx].astype(np.float32)
        f_d = f[idx]
        # latest waterfall row: last row of bitmap, downsample width
        W, H = int(info.FrameWidth), int(info.FrameHeight)
        bm = np.frombuffer(self._bitmap, dtype=np.uint16, count=W * H).reshape(H, W)
        row = bm[-1].astype(np.uint16)   # latest time slice (bottom row)
        if W != self.WATERFALL_WIDTH:
            row = row[np.linspace(0, W - 1, self.WATERFALL_WIDTH).astype(np.int64)]
        maxd = int(info.MaxDensityValue)
        fv = dev.state.freq_version
        frame = _encode_rta(fv, f_d, s_d, row, maxd,
                            info.StartFrequency_Hz, info.StopFrequency_Hz)
        return [frame], []


def _encode_rta(version, freq_hz, spec_dbm, wf_row, max_density, start_hz, stop_hz):
    """RTA frame: hdr(magic+ver+pts+wfLen+maxD+startHz) + freq(f8×pts) + spec(f4×pts)
    + wfRow(u2×wfLen) + stopHz(f8)."""
    import struct
    pts = len(spec_dbm)
    hdr = b'RTAF' + struct.pack('<IIHHd', version, pts, len(wf_row), max_density, float(start_hz))
    payload = (freq_hz.astype(np.float64).tobytes() +
               spec_dbm.astype(np.float32).tobytes() +
               wf_row.astype(np.uint16).tobytes())
    meta = struct.pack('<d', float(stop_hz))
    return hdr + payload + meta
