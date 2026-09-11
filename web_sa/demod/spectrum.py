"""
demod/spectrum.py -- panadapter FFT + waterfall row for the SDR mode.

Converts complex baseband (int16/IQS domain, with `scale_to_v`) to a power spectrum
in dBm using the equivalent-sinusoid amplitude of a windowed FFT. This is not a
fully calibrated measurement, but it tracks the true level closely (a -25 dBm CW
tone measured ~-25 dBm).
"""
from __future__ import annotations

import numpy as np

_TWO_PI = 2.0 * np.pi


class Panadapter:
    def __init__(self, fft_size: int = 2048, ref_ohms: float = 50.0,
                 wf_lo_dbm: float = -120.0, wf_hi_dbm: float = -20.0):
        self.fft_size = int(fft_size)
        self.ref_ohms = float(ref_ohms)
        self.wf_lo = float(wf_lo_dbm)
        self.wf_hi = float(wf_hi_dbm)
        self.window = np.hanning(self.fft_size).astype(np.float64)
        self.coherent = float(self.window.sum())
        self._buf_i = np.zeros(0, dtype=np.float64)
        self._buf_q = np.zeros(0, dtype=np.float64)

    def reset(self) -> None:
        self._buf_i = np.zeros(0, dtype=np.float64)
        self._buf_q = np.zeros(0, dtype=np.float64)

    def process(self, i, q, fs: float, center_hz: float, scale_to_v: float,
                bandwidth: float | None = None):
        """Return (freq_hz, power_dbm, wf_row_uint16) or None if not enough data."""
        i = np.asarray(i, dtype=np.float64)
        q = np.asarray(q, dtype=np.float64)
        if i.size:
            self._buf_i = np.concatenate([self._buf_i, i])[-self.fft_size:]
            self._buf_q = np.concatenate([self._buf_q, q])[-self.fft_size:]
        if self._buf_i.size < self.fft_size:
            return None
        x = (self._buf_i + 1j * self._buf_q) * self.window
        spec = np.fft.fftshift(np.fft.fft(x, self.fft_size))
        amp = 2.0 * np.abs(spec) / self.coherent          # equivalent sinusoid amplitude (LSB)
        vrms = amp / np.sqrt(2.0) * float(scale_to_v)
        power = vrms ** 2 / self.ref_ohms
        power_dbm = 10.0 * np.log10(power + 1e-30) + 30.0  # W -> dBm
        freq = center_hz + np.linspace(-fs / 2.0, fs / 2.0, self.fft_size, endpoint=False)
        if bandwidth is not None and 0 < bandwidth < fs:
            keep = np.abs(freq - center_hz) <= float(bandwidth) / 2.0
            freq = freq[keep]
            power_dbm = power_dbm[keep]
        row = self.waterfall_row(power_dbm)
        return freq, power_dbm.astype(np.float32), row

    def waterfall_row(self, power_dbm: np.ndarray) -> np.ndarray:
        span = max(1.0, self.wf_hi - self.wf_lo)
        v = (np.asarray(power_dbm, dtype=np.float64) - self.wf_lo) / span
        v = np.clip(v, 0.0, 1.0) * 65535.0
        return v.astype(np.uint16)
