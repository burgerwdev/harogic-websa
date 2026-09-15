#!/usr/bin/env python3
"""Spectral helpers for the VSA probes.

Two normalisations are used deliberately:

* **coherent** (a CW tone): the peak bin maps to a tone amplitude
  ``2|X_k| / sum(w)``, which converts to an absolute dBm through 50 ohm.
* **incoherent** (noise / integrated band): ``|X_k|^2 / (N * sum(w^2))`` so that
  summing bins returns the mean power of the block.

Getting this wrong is the classic way to report a 3 dB-off power, so the two
are separate, named functions and the probes state which one they used.
"""
from __future__ import annotations

import numpy as np


def _window(name: str, n: int) -> np.ndarray:
    if name == 'rect':
        return np.ones(n)
    if name == 'hann':
        return np.hanning(n)
    if name == 'blackman-harris':
        k = np.arange(n)
        return (0.35875 - 0.48829 * np.cos(2 * np.pi * k / n)
                + 0.14128 * np.cos(4 * np.pi * k / n)
                - 0.01168 * np.cos(6 * np.pi * k / n))
    raise ValueError(f'unknown window {name!r}')


def fft_relative_db(iq: np.ndarray, window: str = 'blackman-harris') -> tuple:
    """Shifted spectrum of a complex block, dB relative to its own peak."""
    x = np.asarray(iq)
    n = len(x)
    w = _window(window, n)
    spec = np.fft.fftshift(np.fft.fft(x * w))
    p = np.abs(spec) ** 2 / (n * np.sum(w ** 2))
    freqs = np.fft.fftshift(np.fft.fftfreq(n))
    return freqs, 10 * np.log10(np.maximum(p, 1e-30) / p.max())


def _peak_interp_db(mag: np.ndarray, k: int) -> tuple[float, float]:
    """Parabolic interpolation of a log-magnitude peak.

    Returns ``(bin offset, dB gain over the peak bin)``. Hann main lobes are
    close enough to parabolic that both the frequency and the amplitude error
    drop below ~0.1 dB / ~0.01 bin.
    """
    if k <= 0 or k >= len(mag) - 1:
        return 0.0, 0.0
    a, b, c = (20 * np.log10(max(1e-30, mag[k - 1])),
               20 * np.log10(max(1e-30, mag[k])),
               20 * np.log10(max(1e-30, mag[k + 1])))
    den = a - 2 * b + c
    d = 0.0 if abs(den) < 1e-12 else float(np.clip(0.5 * (a - c) / den, -0.5, 0.5))
    return d, b - 0.25 * (a - c) * d - b


def tone_dbm(iq: np.ndarray, fs: float, f_rel: float, window: str = 'hann',
             search_hz: float | None = None) -> tuple:
    """Absolute level of the strongest tone near ``f_rel`` (Hz, relative to DC).

    Returns ``(frequency_hz, dbm)``. The input is complex baseband (analytic),
    so the coherent bin value maps to the tone's own complex amplitude
    ``|X_k| / sum(w)`` and the power is ``|A|^2 / 50``. Measured against the
    vendor swept path on the same CW tone this agrees within 0.3 dB with the
    block mean power (see probe_iq_level.py), i.e. no extra 3 dB bandpass
    conversion is needed with the vendor's ``IQS_ScaleToV``.
    """
    x = np.asarray(iq)
    n = len(x)
    w = _window(window, n)
    spec = np.fft.fft(x * w)
    amp = np.abs(spec) / np.sum(w)
    freqs = np.fft.fftfreq(n, d=1.0 / fs)
    band = (np.abs(((freqs - f_rel + fs / 2) % fs) - fs / 2)
            <= (search_hz if search_hz is not None else fs / n * 4))
    if not band.any():
        return float('nan'), float('-inf')
    k = int(np.argmax(np.where(band, amp, 0)))
    d, gain_db = _peak_interp_db(amp, k)
    a = amp[k] * 10 ** (gain_db / 20.0)
    dbm = 10 * np.log10(max(1e-30, a ** 2 / 50.0)) + 30.0
    return float(freqs[k] + d * fs / n), float(dbm)


def band_power_dbm(iq: np.ndarray, fs: float, f_rel: float, bw_hz: float,
                   window: str = 'hann') -> float:
    """Integrated noise-like power in a band (incoherent normalisation)."""
    x = np.asarray(iq)
    n = len(x)
    w = _window(window, n)
    spec = np.fft.fft(x * w)
    p = np.abs(spec) ** 2 / (n * np.sum(w ** 2))
    freqs = np.fft.fftfreq(n, d=1.0 / fs)
    band = np.abs(((freqs - f_rel + fs / 2) % fs) - fs / 2) <= bw_hz / 2
    tot = float(p[band].sum())          # p sums to the mean power over all bins
    return 10 * np.log10(max(1e-30, tot / 50.0)) + 30.0


def mean_dbm(iq: np.ndarray) -> float:
    """Mean power of the whole block in absolute dBm (50 ohm)."""
    x = np.asarray(iq)
    if not len(x):
        return float('-inf')
    return 10 * np.log10(max(1e-30, float(np.mean(np.abs(x) ** 2)) / 50.0)) + 30.0


def noise_floor_dbm(iq: np.ndarray, fs: float, window: str = 'blackman-harris',
                    percentile: float = 50.0) -> float:
    """Spectral noise density floor expressed in a 1 Hz band."""
    x = np.asarray(iq)
    n = len(x)
    w = _window(window, n)
    p = np.abs(np.fft.fft(x * w)) ** 2 / (n * np.sum(w ** 2))
    floor = float(np.percentile(p, percentile))
    return 10 * np.log10(max(1e-30, floor / (fs / n) / 50.0)) + 30.0
