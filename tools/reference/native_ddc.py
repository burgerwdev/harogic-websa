"""Native channelizer ABI adapter for standalone reference comparisons."""
from __future__ import annotations

import ctypes as C

import numpy as np

from .ddc import DdcChannel


class NativeDdcChannel(DdcChannel):
    def __init__(self, dsp, path):
        super().__init__(dsp)
        self.lib = C.CDLL(path)
        self.lib.channel_open.argtypes = [C.c_double, C.c_double, C.c_int]
        self.lib.channel_open.restype = C.c_void_p
        self.lib.channel_open_ratio.argtypes = [C.c_double, C.c_double, C.c_int,
                                                C.c_uint32, C.c_uint32, C.c_double]
        self.lib.channel_open_ratio.restype = C.c_void_p
        self.lib.channel_process.argtypes = [C.c_void_p, C.POINTER(C.c_int16), C.c_int,
                                             C.POINTER(C.c_float), C.c_int]
        self.lib.channel_process.restype = C.c_int
        self.lib.channel_close.argtypes = [C.c_void_p]
        self.lib.channel_quantum.argtypes = [C.c_double]
        self.lib.channel_quantum.restype = C.c_int
        self.lib.channel_select_decimate.argtypes = [C.c_double, C.c_int]
        self.lib.channel_select_decimate.restype = C.c_int
        self.lib.channel_delay.argtypes = [C.c_void_p]
        self.lib.channel_delay.restype = C.c_double
        self.lib.channel_output_rate.argtypes = [C.c_void_p]
        self.lib.channel_output_rate.restype = C.c_double
        self.lib.channel_clock.argtypes = [C.c_void_p, C.POINTER(C.c_uint64), C.POINTER(C.c_uint64)]
        self.lib.channel_clock.restype = C.c_int
        self.handle = None
        self.ratio_up = self.ratio_down = 1
        self.passband_hz = 0.0

    def quantum(self, fs_in):
        return int(self.lib.channel_quantum(fs_in))

    def select_decimate(self, fs_in, ceiling):
        return int(self.lib.channel_select_decimate(fs_in, ceiling))

    def clock(self):
        if not self.handle:
            raise RuntimeError('Native DDC not configured')
        input_samples, output_samples = C.c_uint64(), C.c_uint64()
        if self.lib.channel_clock(self.handle, C.byref(input_samples), C.byref(output_samples)):
            raise RuntimeError('Native DDC clock failed')
        return input_samples.value, output_samples.value

    def close(self):
        if self.handle:
            self.lib.channel_close(self.handle)
            self.handle = None
        self._ready = False

    def configure(self, fs_in, offset_hz, decimate, sample_points,
                  ratio_up=1, ratio_down=1, passband_hz=0.0):
        self.close()
        self._ready = False
        if fs_in < 31250000.0:
            self.ratio_up = self.ratio_down = 1
            self.passband_hz = 0.0
            return super().configure(fs_in, offset_hz, decimate, sample_points)
        if ratio_up == ratio_down:
            handle = self.lib.channel_open(fs_in, offset_hz, decimate)
        else:
            handle = self.lib.channel_open_ratio(fs_in, offset_hz, decimate,
                                                 ratio_up, ratio_down, passband_hz)
        if not handle:
            raise RuntimeError(f'Native DDC unsupported: fs={fs_in}, decimate={decimate}')
        self.handle = handle
        self.ratio_up, self.ratio_down = int(ratio_up), int(ratio_down)
        self.passband_hz = float(passband_hz)
        self.fs_in = float(fs_in)
        self.offset_hz = float(offset_hz)
        self.decimate = self.requested_decimate = int(decimate)
        self.sample_points = int(sample_points)
        self.fs_out = self.lib.channel_output_rate(handle)
        delay = self.lib.channel_delay(handle)
        if self.fs_out <= 0 or delay < 0:
            self.close()
            raise RuntimeError('Native DDC returned invalid geometry')
        self.out_points = sample_points // decimate + 2
        self.delay = float(delay)
        self._drop_pending = round(delay)
        self._output = np.empty(self.out_points * 2, np.float32)
        self._ready = True

    def process(self, int16_buf, n=None):
        if not self.handle:
            return super().process(int16_buf, n)
        if not self._ready:
            raise RuntimeError('Native DDC not configured')
        n = self.sample_points if n is None else int(n)
        if n <= 0 or n > self.sample_points:
            raise ValueError(f'DDC input samples out of range: {n}')
        if isinstance(int16_buf, np.ndarray):
            if int16_buf.dtype != np.int16 or not int16_buf.flags.c_contiguous or int16_buf.size < n*2:
                raise ValueError('Native DDC requires contiguous int16 IQ')
            ptr = int16_buf.ctypes.data_as(C.POINTER(C.c_int16))
        else:
            ptr = C.cast(int16_buf, C.POINTER(C.c_int16))
            if not ptr:
                raise ValueError('Native DDC input pointer is null')
        consumed, produced = self.clock()
        count = self.lib.channel_process(self.handle, ptr, n,
            self._output.ctypes.data_as(C.POINTER(C.c_float)), self.out_points)
        if count < 0 or count > self.out_points:
            raise RuntimeError(f'Native DDC invalid output count={count}')
        skip = min(self._drop_pending, count)
        self._drop_pending -= skip
        samples = self._output[2*skip:2*count].reshape(-1, 2)
        if samples.size:
            output_index = produced + skip
            if self.ratio_up == self.ratio_down:
                causal_index = (output_index + 1) * self.decimate - 1
            else:
                # Rational::process emits at ceil(output*down/up + 95/2).
                up, down = self.ratio_up, self.ratio_down
                coarse_index = (2 * output_index * down + 95 * up + 2 * up - 1) // (2 * up)
                causal_index = (coarse_index + 1) * self.decimate - 1
            self.output_input_offset = causal_index - consumed
        else:
            self.output_input_offset = None
        # Borrowed until the next process() call; the session copies into IQBF before then.
        return samples[:, 0], samples[:, 1]
