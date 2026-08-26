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

import time

import numpy as np

from .base import MeasurementSession


class RtaSession(MeasurementSession):
    """Real-time spectrum session: continuous RTA acquisition."""

    name = 'rta'

    DISPLAY_POINTS = 1001      # trace downsample target (matches SWP display)
    WATERFALL_WIDTH = 860      # waterfall row width after downsample

    def __init__(self, dev):
        super().__init__(dev)
        self._ready = False
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
        import htra_api as T
        dev = self.dev
        s = dev.state
        prof = T.RTA_Profile_TypeDef()
        out = T.RTA_Profile_TypeDef()
        info = T.RTA_FrameInfo_TypeDef()
        T.dll.RTA_ProfileDeInit(T.pointer(dev.dev), T.pointer(prof))
        prof.CenterFreq_Hz = s.center_hz
        prof.RefLevel_dBm = s.ref_level
        prof.DecimateFactor = 1
        prof.TriggerSource = T.RTA_TriggerSource_TypeDef.Bus
        prof.TriggerMode = T.TriggerMode_TypeDef.FixedPoints
        prof.TriggerAcqTime = 0.005   # short acq -> PacketCount=1, ~150fps (probe-verified)
        prof.SweepTimeMode = T.SweepTimeMode_TypeDef(self._sweep_mode)
        prof.SweepTime = float(self._sweep_time)
        st = T.dll.RTA_Configuration(T.pointer(dev.dev), T.pointer(prof), T.pointer(out), T.pointer(info))
        if st != 0:
            dev.state.last_error = 'RTA_Configuration status=%d' % st
            self._ready = False
            return
        self._info = info
        n = int(info.PacketSamplePoints) + 4096
        self._trace = (T.c_uint8 * n)()
        self._bitmap = (T.c_uint16 * (int(info.FrameHeight) * int(info.FrameWidth) + 65536))()
        self._plot = T.RTA_PlotInfo_TypeDef()
        self._trigger = T.RTA_TriggerInfo_TypeDef()
        self._aux = T.MeasAuxInfo_TypeDef()
        self._ready = True
        self._last_wf_time = 0.0
        self._last_get = 0.0
        # SWP->RTA hardware switch needs ~1.5s; Get before that triggers a GPF in libhtraapi
        self._ready_at = time.monotonic() + 1.5

    def set_params(self, center=None, span=None):
        """Update RTA center (bandwidth is fixed by device/decimate) and reconfigure."""
        s = self.dev.state
        if center is not None:
            s.center_hz = float(center)
        self._configure()

    def set_sweep(self, mode=0, time=0.0):
        """Update RTA sweep speed (SweepTimeMode) and reconfigure."""
        self._sweep_mode = max(0, min(8, int(mode)))
        self._sweep_time = float(time)
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
        import htra_api as T
        dev = self.dev
        info = self._info
        self._last_get = now
        st = T.dll.RTA_BusTriggerStart(T.pointer(dev.dev))
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
        for _ in range(ngets):
            s = T.dll.RTA_GetRealTimeSpectrum(
                dev.dev, self._trace, self._bitmap,
                T.pointer(self._plot), T.pointer(self._trigger), T.pointer(self._aux))
            if s != 0:
                break
            st = s
            nok += 1
        dt = (time.monotonic() - t0) * 1000.0
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
