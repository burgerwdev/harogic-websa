#!/usr/bin/env python3
"""IQS capture primitives shared by the VSA probes (hardware side).

``web_sa.hardware.sdk_bindings`` is imported on purpose: it re-declares
``IQStream_TypeDef`` at the header-correct 728 bytes (the vendor wrapper's copy is
8 bytes short, and the SDK writes past it on *every* packet) and rebinds
``IQS_GetIQStream_PM1`` to that struct. Probes that used the wrapper's struct
would both corrupt the heap and, once sdk_bindings is loaded, fail the ctypes
type check.

Thin, well-named wrappers over the verified SDK calls
(``IQS_Configuration`` / ``IQS_BusTriggerStart`` / ``IQS_GetIQStream_PM1``) plus
the conversions a vector-signal analyser needs: int16 -> volts via
``IQS_ScaleToV``, complex baseband arrays, and per-packet statistics.

Used by ``probe_iq_capture.py`` (bench measurements) and by the Tier1/Tier2
probes when they want a real hardware capture instead of synthetic IQ.

Usage:  from capture import CaptureConfig, framed_capture, stream_capture
"""
from __future__ import annotations

import ctypes
import os
import sys
import time

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
if REPO not in sys.path:
    sys.path.insert(0, REPO)
from web_sa.hardware import sdk_bindings as sb  # noqa: E402


def new_stream():
    """One IQStream of the header-correct size (see the module docstring)."""
    return sb.IQStream_TypeDef()

#: documented transient bus warnings: re-issue the call instead of failing
TRANSIENT = (-10, -11, -9)


class CaptureConfig:
    """IQS parameters plus the bench conditions they were measured under."""

    def __init__(self, center_hz: float = 100e6, decimate: int = 8,
                 trigger_mode: str = 'adaptive', trigger_length: int = 262144,
                 ref_level_dbm: float = 0.0, bus_timeout_ms: int = 2000,
                 dcc: str = 'high_pass', qdc: str = 'off',
                 data_format: str = 'complex16', gain_parameter: int | None = None,
                 antenna: str = 'rf_in'):
        self.center_hz = float(center_hz)
        self.decimate = int(decimate)
        self.trigger_mode = trigger_mode
        self.trigger_length = int(trigger_length)
        self.ref_level_dbm = float(ref_level_dbm)
        self.bus_timeout_ms = int(bus_timeout_ms)
        self.dcc = dcc
        self.qdc = qdc
        self.data_format = data_format
        self.gain_parameter = gain_parameter
        self.antenna = antenna

    def bench(self) -> dict:
        """Fields that must accompany every measured number in the report."""
        return dict(center_hz=self.center_hz, decimate=self.decimate,
                    trigger_mode=self.trigger_mode,
                    trigger_length=self.trigger_length,
                    ref_level_dbm=self.ref_level_dbm, dcc=self.dcc, qdc=self.qdc,
                    data_format=self.data_format, antenna=self.antenna,
                    bus_timeout_ms=self.bus_timeout_ms)


#: htra_api.py enum spellings (verified against API 0.55.89)
TRIGGER_MODES = {'fixed_points': 'FixedPoints', 'adaptive': 'Adaptive'}
DATA_FORMATS = {'complex8': 'Complex8bit', 'complex16': 'Complex16bit',
                'complex32': 'Complex32bit', 'complexfloat': 'Complexfloat'}
DCC_MODES = {'off': 'DCCOff', 'high_pass': 'DCCHighPassFilterMode',
             'manual': 'DCCManualOffsetMode', 'auto': 'DCCAutoOffsetMode'}
QDC_MODES = {'off': 'QDCOff', 'auto': 'QDCAutoMode', 'manual': 'QDCManualMode'}


def configure(dev, T, cfg: CaptureConfig):
    """Apply an IQS profile; returns (status, out_profile, stream_info)."""
    p = T.IQS_Profile_TypeDef()
    T.dll.IQS_ProfileDeInit(T.pointer(dev), T.pointer(p))
    p.CenterFreq_Hz = cfg.center_hz
    p.RefLevel_dBm = cfg.ref_level_dbm
    p.DecimateFactor = cfg.decimate
    p.DataFormat = getattr(T.DataFormat_TypeDef, DATA_FORMATS[cfg.data_format])
    p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
    p.TriggerMode = getattr(T.TriggerMode_TypeDef, TRIGGER_MODES[cfg.trigger_mode])
    p.TriggerLength = cfg.trigger_length
    p.BusTimeout_ms = cfg.bus_timeout_ms
    p.DCCancelerMode = getattr(T.DCCancelerMode_TypeDef, DCC_MODES[cfg.dcc])
    p.QDCMode = getattr(T.QDCMode_TypeDef, QDC_MODES[cfg.qdc])
    out = T.IQS_Profile_TypeDef()
    info = T.IQS_StreamInfo_TypeDef()
    st = T.dll.IQS_Configuration(T.pointer(dev), T.pointer(p), T.pointer(out),
                                 T.pointer(info))
    return int(st), out, info


def drain(dev, T, stream, seconds: float = 0.25) -> int:
    """Fetch and discard after a configuration so the device FIFO cannot overflow.

    Production lesson (docs/*/SDR_MODE.md): right after IQS_Configuration the
    device still holds packets from the previous geometry; fetching them without
    draining produced a run of errors in the VSA soak until this was added.
    Returns the number of packets discarded.
    """
    n = 0
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        st = int(sb.dll.IQS_GetIQStream_PM1(T.pointer(dev), T.pointer(stream)))
        del st
        n += 1
    return n


def packet(dev, T, stream):
    """Fetch one packet; returns (status, np.int16 array or None)."""
    st = int(sb.dll.IQS_GetIQStream_PM1(T.pointer(dev), T.pointer(stream)))
    if st != 0:
        return st, None
    n = int(stream.IQS_StreamInfo.PacketSamples)
    if n <= 0:
        return 0, np.zeros(0, dtype=np.int16)
    src = ctypes.cast(stream.AlternIQStream,
                      ctypes.POINTER(ctypes.c_int16 * (n * 2))).contents
    return 0, np.ctypeslib.as_array(src).copy()


def to_volts(raw: np.ndarray, scale_to_v: float) -> np.ndarray:
    """Interleaved int16 packet -> complex128 volts (I + jQ)."""
    x = raw[0::2].astype(np.float64) + 1j * raw[1::2].astype(np.float64)
    return x * (scale_to_v or 1.0)


class FramedCapture:
    """Result of one FixedPoints frame capture."""

    def __init__(self):
        self.iq = None                 # complex64/128 volts
        self.scale_to_v = 0.0
        self.fs = 0.0
        self.center_hz = 0.0
        self.info = {}
        self.packets_ok = 0
        self.packets_err = 0
        self.first_status = 0
        self.elapsed_s = 0.0
        self.trigger_s = 0.0
        self.requested_samples = 0
        self.packet_samples = []
        self.statuses = []

    def summary(self) -> dict:
        got = 0 if self.iq is None else len(self.iq)
        return dict(requested=self.requested_samples, samples=got,
                    packets_ok=self.packets_ok, packets_err=self.packets_err,
                    trigger_s=round(self.trigger_s, 4),
                    elapsed_s=round(self.elapsed_s, 4),
                    first_status=self.first_status,
                    packets_reported=self.info.get('packet_count'),
                    shortfall=self.requested_samples - got,
                    scale_to_v=self.scale_to_v, fs=self.fs)


def framed_capture(dev, T, cfg: CaptureConfig, *, pre_trigger: bool = True,
                   trigger_wait_ms: int | None = None) -> FramedCapture:
    """Capture exactly one FixedPoints frame (multi-packet reassembly)."""
    res = FramedCapture()
    res.requested_samples = cfg.trigger_length
    st, out, info = configure(dev, T, cfg)
    res.first_status = st
    res.center_hz = float(out.CenterFreq_Hz)
    res.fs = float(info.IQSampleRate)
    res.info = dict(packet_count=int(info.PacketCount),
                    packet_samples=int(info.PacketSamples),
                    packet_data_size=int(info.PacketDataSize),
                    stream_samples=int(info.StreamSamples),
                    bandwidth=float(info.Bandwidth),
                    gain_parameter=int(info.GainParameter))
    if st != 0:
        return res

    if not pre_trigger:
        # no BusTriggerStart: the frame never starts, so the device must time out
        t0 = time.monotonic()
        stream = new_stream()
        status, _ = packet(dev, T, stream)
        res.elapsed_s = time.monotonic() - t0
        res.first_status = status
        res.statuses.append(status)
        return res

    packet_samples = max(1, int(info.PacketSamples))
    n_packets = max(1, int(info.PacketCount))
    t0 = time.monotonic()
    tstart = int(T.dll.IQS_BusTriggerStart(T.pointer(dev)))
    res.trigger_s = time.monotonic() - t0
    chunks = []
    stream = new_stream()
    try:
        for idx in range(n_packets):
            remaining = cfg.trigger_length - idx * packet_samples
            if remaining <= 0:
                break
            want = min(packet_samples, remaining)
            status, raw = packet(dev, T, stream)
            res.statuses.append(status)
            if status != 0:
                if status in TRANSIENT:
                    # re-issue while the frame is still open
                    status, raw = packet(dev, T, stream)
                    res.statuses.append(status)
                if status != 0:
                    res.packets_err += 1
                    res.first_status = status
                    break
            res.packets_ok += 1
            res.packet_samples.append(len(raw) // 2)
            res.scale_to_v = float(stream.IQS_ScaleToV)
            chunks.append(raw[:want * 2])
    finally:
        T.dll.IQS_BusTriggerStop(T.pointer(dev))
        res.elapsed_s = time.monotonic() - t0
    res.iq = (to_volts(np.concatenate(chunks), res.scale_to_v)
              if chunks else np.zeros(0, dtype=np.complex128))
    if tstart != 0:
        res.first_status = tstart
    return res


class StreamCapture:
    """Result of an Adaptive (continuous) capture."""

    def __init__(self):
        self.samples = 0
        self.packets_ok = 0
        self.packets_err = 0
        self.seconds = 0.0
        self.fs = 0.0
        self.iq = None
        self.packet_gap_ms = []
        self.error_statuses = []
        self.scale_to_v = 0.0
        self.discarded = 0

    def summary(self) -> dict:
        eff = self.samples / self.seconds if self.seconds else 0.0
        gaps = self.packet_gap_ms
        return dict(seconds=round(self.seconds, 3), packets_ok=self.packets_ok,
                    packets_err=self.packets_err, samples=self.samples,
                    fs=self.fs, effective_rate=eff,
                    rate_ratio=(eff / self.fs) if self.fs else 0.0,
                    loss_ppm=(1 - eff / self.fs) * 1e6 if self.fs else 0.0,
                    usb_mbytes_s=self.samples * 4 / self.seconds / 1e6 if self.seconds else 0.0,
                    gap_max_ms=round(max(gaps), 3) if gaps else 0.0,
                    gap_p99_ms=round(float(np.percentile(gaps, 99)), 3) if gaps else 0.0,
                    error_statuses=sorted(set(self.error_statuses)))


def stream_capture(dev, T, cfg: CaptureConfig, seconds: float = 3.0,
                   keep_samples: int = 0) -> StreamCapture:
    """Adaptive continuous capture; measures the sustained rate and gaps."""
    res = StreamCapture()
    st, out, info = configure(dev, T, cfg)
    res.fs = float(info.IQSampleRate)
    if st != 0:
        res.error_statuses.append(st)
        return res
    stream = new_stream()
    tstart = int(T.dll.IQS_BusTriggerStart(T.pointer(dev)))
    if tstart != 0:
        res.error_statuses.append(tstart)
        return res
    kept = []
    kept_n = 0
    discarded = drain(dev, T, stream, 0.25)
    t0 = time.monotonic()
    last = t0
    res.discarded = discarded
    try:
        while True:
            now = time.monotonic()
            if now - t0 >= seconds:
                break
            status, raw = packet(dev, T, stream)
            t1 = time.monotonic()
            if status != 0:
                res.packets_err += 1
                res.error_statuses.append(status)
                continue
            res.packets_ok += 1
            n = len(raw) // 2
            res.samples += n
            res.scale_to_v = float(stream.IQS_ScaleToV)
            res.packet_gap_ms.append((t1 - last) * 1e3)
            last = t1
            if kept_n < keep_samples:
                take = min(n, keep_samples - kept_n)
                kept.append(raw[:take * 2])
                kept_n += take
    finally:
        T.dll.IQS_BusTriggerStop(T.pointer(dev))
        res.seconds = last - t0
    if kept:
        res.iq = to_volts(np.concatenate(kept), res.scale_to_v)
    return res


def peak_dbfs(iq: np.ndarray) -> float:
    """Full-scale-relative level of a complex IQ block (int16 full scale = 1.0)."""
    if iq is None or not len(iq):
        return float('-inf')
    return float(20 * np.log10(max(1e-12, np.abs(iq).max())))


def iq_power_dbm(iq: np.ndarray) -> float:
    """Integrated power of a complex volt waveform as an absolute dBm (50 ohm)."""
    v = np.asarray(iq)
    if not len(v):
        return float('-inf')
    p_w = float(np.mean(np.abs(v) ** 2)) / 50.0
    return 10 * np.log10(max(1e-30, p_w)) + 30.0
