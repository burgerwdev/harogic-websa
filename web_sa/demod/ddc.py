"""
demod/ddc.py -- DSP_DDC channelizer wrapper (verified on SAN-90).

Verified behaviour (see tools/sdr_probe/FINDINGS.md):
  * input  : interleaved int16 IQ at fs_in
  * output : interleaved *complex float32*
  * offset : f_out = f_in_rel + DDCOffsetFrequency  -> to receive absolute `ft`,
             set offset = center_hz - ft
  * output rate = fs_in / decimate; proper low-pass at +/- rate_out/2
  * filter delay can be read (in output samples) and should be skipped
"""
from __future__ import annotations

from ctypes import cast as _cast

import numpy as np

from ..hardware import sdk_bindings as sb


class DdcChannel:
    def __init__(self, dsp):
        self.dsp = dsp                 # c_void_p DSP handle (dev.dsp)
        self.fs_in = 0.0
        self.offset_hz = 0.0
        self.decimate = 0
        self.sample_points = 0
        self.fs_out = 0.0
        self.out_points = 0
        self.delay = 0
        self._ready = False

    def configure(self, fs_in: float, offset_hz: float, decimate: int, sample_points: int) -> None:
        T = sb
        decimate = max(1, int(decimate))
        sample_points = max(1, int(sample_points))
        din = T.DSP_DDC_TypeDef()
        dout = T.DSP_DDC_TypeDef()
        T.dll.DSP_DDC_DeInit(T.pointer(din))
        din.DDCOffsetFrequency = float(offset_hz)
        din.SampleRate = float(fs_in)          # INPUT rate
        din.DecimateFactor = float(decimate)
        din.SamplePoints = sample_points
        st = T.dll.DSP_DDC_Configuration(T.pointer(self.dsp), T.pointer(din), T.pointer(dout))
        if st != 0:
            raise RuntimeError(f'DSP_DDC_Configuration status={st}')
        delay = sb.c_uint32(0)
        T.dll.DSP_DDC_GetDelay(T.pointer(self.dsp), T.pointer(delay))
        T.dll.DSP_DDC_Reset(T.pointer(self.dsp))   # fresh stream
        self.fs_in = float(fs_in)
        self.offset_hz = float(dout.DDCOffsetFrequency)
        self.decimate = max(1, int(round(float(dout.DecimateFactor))))
        self.sample_points = sample_points
        self.fs_out = float(dout.SampleRate)
        self.out_points = int(dout.SamplePoints)
        if self.fs_out <= 0 or self.out_points <= 0:
            raise RuntimeError(
                f'DSP_DDC_Configuration returned invalid output '
                f'rate={self.fs_out} points={self.out_points}')
        self.delay = int(delay.value)
        self._drop_pending = self.delay   # skip the filter transient ONCE, not per packet
        self._ready = True

    def process(self, int16_buf, n: int | None = None):
        """Run one DDC block. Returns (i, q) float64 arrays (delay already skipped)."""
        if not self._ready:
            raise RuntimeError('DdcChannel not configured')
        T = sb
        if n is None:
            n = self.sample_points
        n = int(n)
        if n <= 0 or n > self.sample_points:
            raise ValueError(f'DDC input samples out of range: {n} (configured {self.sample_points})')
        ins = T.IQStream_TypeDef()
        if isinstance(int16_buf, np.ndarray):
            if int16_buf.dtype != np.int16 or not int16_buf.flags.c_contiguous:
                raise ValueError('DDC numpy input must be contiguous int16')
            if int16_buf.size < n * 2:
                raise ValueError(f'DDC input buffer too small: {int16_buf.size} values for {n} IQ')
            addr = int(int16_buf.ctypes.data)
        else:
            addr = _cast(int16_buf, T.c_void_p).value
        if not addr:
            raise ValueError('DDC input pointer is null')
        ins.AlternIQStream = _cast(T.c_void_p(addr), T.POINTER(T.c_void_p))
        ins.IQS_StreamInfo.PacketSamples = n
        ins.IQS_StreamInfo.IQSampleRate = self.fs_in
        outs = T.IQStream_TypeDef()
        st = T.dll.DSP_DDC_Execute(T.pointer(self.dsp), T.pointer(ins), T.pointer(outs))
        if st != 0:
            raise RuntimeError(f'DSP_DDC_Execute status={st}')
        pts = int(outs.IQS_StreamInfo.PacketSamples) or self.out_points
        if pts == 0:
            return np.zeros(0), np.zeros(0)
        max_points = self.out_points + max(self.delay, 8)
        if pts < 0 or pts > max_points:
            raise RuntimeError(f'DSP_DDC_Execute returned invalid points={pts}, max={max_points}')
        if not outs.AlternIQStream:
            raise RuntimeError('DSP_DDC_Execute returned a null output pointer')
        data_format = int(getattr(outs.IQS_Profile.DataFormat, 'value', -1))
        if data_format not in (0, int(T.DataFormat_TypeDef.Complexfloat)):
            raise RuntimeError(f'DSP_DDC_Execute returned unexpected format={data_format}')
        packet_bytes = int(outs.IQS_StreamInfo.PacketDataSize)
        required_bytes = pts * 2 * np.dtype(np.float32).itemsize
        if packet_bytes < required_bytes:
            raise RuntimeError(
                f'DSP_DDC_Execute returned short buffer={packet_bytes}, need={required_bytes}')
        arr = np.ctypeslib.as_array(
            _cast(outs.AlternIQStream, T.POINTER(T.c_float * (pts * 2))).contents).copy()
        i = arr[0::2].astype(np.float64)
        q = arr[1::2].astype(np.float64)
        # Drop the DDC filter transient only on the first block after configure();
        # dropping it from every packet would discard `delay` samples per packet and
        # make the audio run at < realtime (verified: ~0.85x with delay=90/955).
        d = self._drop_pending
        if d > 0:
            if d >= len(i):
                self._drop_pending = d - len(i)
                return np.zeros(0), np.zeros(0)
            i, q = i[d:], q[d:]
            self._drop_pending = 0
        return i, q
