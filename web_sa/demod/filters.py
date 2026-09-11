"""
demod/filters.py -- streaming DSP primitives for the SDR mode (numpy only).

Everything here is stateful across blocks so a continuous IQ packet stream can be
filtered / resampled without per-packet discontinuities (clicks).
"""
from __future__ import annotations

import numpy as np


def design_lowpass(fs: float, cutoff_hz: float, ntaps: int = 129) -> np.ndarray:
    """Windowed-sinc real low-pass with unit DC gain."""
    cutoff = min(abs(float(cutoff_hz)), 0.499 * fs)
    if cutoff <= 0:
        return np.ones(1, dtype=np.float32)
    m = ntaps | 1
    n = np.arange(m) - (m - 1) / 2.0
    h = (2.0 * cutoff / fs) * np.sinc(2.0 * cutoff / fs * n) * np.hamming(m)
    s = np.sum(h)
    if s != 0:
        h = h / s
    return h.astype(np.float32)


def design_complex_bandpass(fs: float, f_lo: float, f_hi: float, ntaps: int = 257) -> np.ndarray:
    """Complex FIR passing [f_lo, f_hi] Hz (signed, baseband) and rejecting the rest.

    A real low-pass of width (f_hi-f_lo) is shifted to the band centre; the result is
    complex so it can select an asymmetric band (USB positive, LSB negative, CW around
    the sidetone pitch).
    """
    if f_hi < f_lo:
        f_lo, f_hi = f_hi, f_lo
    bw = max(1.0, float(f_hi - f_lo))
    f0 = (float(f_hi) + float(f_lo)) / 2.0
    m = ntaps | 1
    n = np.arange(m) - (m - 1) / 2.0
    h_lp = (bw / fs) * np.sinc((bw / fs) * n) * np.hamming(m)
    h = h_lp * np.exp(1j * 2.0 * np.pi * f0 / fs * n)
    mag = np.sum(np.abs(h))
    if mag > 0:
        h = h / mag
    return h.astype(np.complex64)


class StreamFilter:
    """Stateful FIR (real or complex) with an overlap tail between blocks."""

    def __init__(self, h: np.ndarray):
        self.h = np.asarray(h)
        self.reset()

    def reset(self) -> None:
        self.tail = np.zeros(max(0, len(self.h) - 1), dtype=self.h.dtype)

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x)
        if len(self.h) <= 1 or x.size == 0:
            return x
        buf = np.concatenate([self.tail, x]) if self.tail.size else x
        y = np.convolve(buf, self.h, mode='valid')
        self.tail = buf[-(len(self.h) - 1):]
        return y


class LinearResampler:
    """Streaming linear-interpolation resampler (fs_in -> fs_out), phase continuous."""

    def __init__(self, fs_in: float, fs_out: float):
        self.step = float(fs_in) / float(fs_out)
        self.t = 0.0
        self.prev = None

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x, dtype=np.float64)
        if x.size == 0:
            return np.zeros(0, dtype=np.float32)
        if self.prev is not None:
            x = np.concatenate([[self.prev], x])
        n = int(np.floor((len(x) - 1 - self.t) / self.step)) + 1
        if n > 0:
            pos = self.t + np.arange(n) * self.step
            i0 = np.minimum(pos.astype(np.int64), len(x) - 2)
            fr = np.clip(pos - i0, 0.0, 1.0)
            y = x[i0] * (1.0 - fr) + x[i0 + 1] * fr
            self.t = pos[-1] + self.step
        else:
            y = np.zeros(0, dtype=np.float64)
        self.t -= (len(x) - 1)
        self.prev = x[-1]
        return y.astype(np.float32)


class Agc:
    """Simple RMS AGC with independent attack/release."""

    def __init__(self, target=0.2, attack=0.02, release=0.002, max_gain=1e4):
        self.target = float(target)
        self.attack = float(attack)
        self.release = float(release)
        self.max_gain = float(max_gain)
        self.gain = 1.0

    def reset(self) -> None:
        self.gain = 1.0

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x, dtype=np.float32)
        if x.size == 0:
            return x
        rms = float(np.sqrt(np.mean(x.astype(np.float64) ** 2) + 1e-20))
        desired = min(self.max_gain, self.target / max(rms, 1e-9))
        coef = self.attack if desired < self.gain else self.release
        self.gain += (desired - self.gain) * coef
        return x * self.gain
