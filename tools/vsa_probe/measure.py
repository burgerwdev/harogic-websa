#!/usr/bin/env python3
"""Tier1 vector measurements: what a VSA can show without deciding symbols.

* ``spectrum_dbm``     — Welch-averaged spectrum in absolute dBm
* ``power_vs_time``    — envelope power trace (bursts, ramps, duty cycle)
* ``ccdf``             — complementary CDF of the envelope, plus the Rayleigh
                         reference for a noise-only signal
* ``spectrogram``      — STFT matrix for a waterfall
* ``constellation``    — carrier-corrected symbol cloud (Tier1: no slicing)

Cost is reported by the caller; see ``probe_dsp_loopback.py`` for measured
microseconds per 100 k samples.
"""
from __future__ import annotations

import analysis as A
import demod as D
import numpy as np
import siggen as S


def spectrum_dbm(iq: np.ndarray, fs: float, nfft: int = 4096,
                 overlap: float = 0.5, window: str = 'blackman-harris') -> tuple:
    """Welch-averaged power spectrum in absolute dBm (50 ohm)."""
    x = np.asarray(iq)
    n = len(x)
    nfft = int(min(nfft, n))
    step = max(1, int(nfft * (1 - overlap)))
    w = A._window(window, nfft)
    norm = np.sum(w ** 2) * nfft
    acc = np.zeros(nfft)
    count = 0
    for start in range(0, n - nfft + 1, step):
        seg = x[start:start + nfft]
        acc += np.abs(np.fft.fft(seg * w)) ** 2
        count += 1
    if not count:
        seg = np.zeros(nfft, dtype=complex)
        seg[:n] = x
        acc = np.abs(np.fft.fft(seg * w)) ** 2
        count = 1
    p = acc / (count * norm)
    freqs = np.fft.fftshift(np.fft.fftfreq(nfft, d=1 / fs))
    dbm = 10 * np.log10(np.maximum(np.fft.fftshift(p), 1e-30) / 50.0) + 30.0
    return freqs, dbm


def power_vs_time(iq: np.ndarray, fs: float, block: int = 256) -> tuple:
    """Envelope power trace in dBm plus the measured duty cycle above the median."""
    x = np.asarray(iq)
    n = (len(x) // block) * block
    if n == 0:
        return np.zeros(0), np.zeros(0), 0.0
    p = np.mean(np.abs(x[:n].reshape(-1, block)) ** 2, axis=1)
    dbm = 10 * np.log10(np.maximum(p, 1e-30) / 50.0) + 30.0
    t = (np.arange(len(p)) + 0.5) * block / fs
    thr = np.median(dbm) + 3.0
    return t, dbm, float(np.mean(dbm > thr))


def ccdf(iq: np.ndarray, block: int = 64) -> tuple:
    """CCDF of the envelope relative to its mean power, in dB."""
    x = np.asarray(iq)
    n = (len(x) // block) * block
    if n == 0:
        return np.zeros(0), np.zeros(0)
    p = np.mean(np.abs(x[:n].reshape(-1, block)) ** 2, axis=1)
    p = p / p.mean()
    xdb = 10 * np.log10(np.maximum(p, 1e-12))
    order = np.sort(xdb)[::-1]
    prob = np.arange(1, len(order) + 1) / len(order)
    return order, prob


def ccdf_rayleigh(xdb: np.ndarray) -> np.ndarray:
    """CCDF a complex-Gaussian signal must follow: exp(-P/Pavg).

    Valid for single samples (block = 1). Block-averaging lowers the variance,
    so the CCDF of a block-averaged trace is *steeper* than Rayleigh and must
    not be compared against this curve.
    """
    return np.exp(-(10 ** (xdb / 10.0)))


def spectral_centroid(freqs: np.ndarray, dbm: np.ndarray,
                      band_hz: float | None = None) -> float:
    """Power-weighted mean frequency: the carrier offset of a symmetric spectrum.

    Restrict to ``band_hz`` when the capture also contains wideband noise: over
    the full band a single noise realisation tilts the estimate by ~1 kHz
    (measured at 25 dB SNR, 3.9 MSPS), while inside the occupied band the signal
    dominates.
    """
    f = np.asarray(freqs)
    p = 10 ** (np.asarray(dbm) / 10.0)
    if band_hz is not None:
        m = np.abs(f) <= band_hz
        f, p = f[m], p[m]
    return float(np.sum(f * p) / max(1e-30, float(p.sum())))


def spectrogram(iq: np.ndarray, fs: float, nfft: int = 256, hop: int = 128) -> tuple:
    """STFT magnitude in dB, newest row last (time x frequency)."""
    x = np.asarray(iq)
    w = A._window('blackman-harris', nfft)
    rows = []
    for start in range(0, len(x) - nfft + 1, hop):
        rows.append(np.abs(np.fft.fftshift(np.fft.fft(x[start:start + nfft] * w))) ** 2)
    if not rows:
        return np.zeros((0, nfft)), np.zeros(nfft)
    m = np.array(rows)
    freqs = np.fft.fftshift(np.fft.fftfreq(nfft, d=1 / fs))
    return 10 * np.log10(np.maximum(m, 1e-30) / m.max()), freqs


def constellation(iq: np.ndarray, fs: float, rolloff: float = 0.35,
                  symbol_rate: float | None = None, sps: int = 8,
                  compensate_cfo: bool = True) -> dict:
    """Tier1 symbol cloud: timing + (optional) carrier correction, no slicing.

    This is what a VSA shows in constellation mode before any decision is made.
    """
    x = np.asarray(iq)
    est_rate, _ = D.estimate_symbol_rate(x, fs)
    rate = symbol_rate or est_rate
    out = dict(symbol_rate_est=est_rate, symbol_rate_used=rate, cfo_hz=0.0,
               timing_samples=0.0, symbols=None)
    if rate <= 0:
        return out
    n_out = int(round(len(x) * sps * rate / fs))
    xs = D.fft_resample(x, n_out)
    fs_out = sps * rate
    if compensate_cfo:
        # Tier1 has no decisions, so the M-th power estimate runs on the matched
        # filter output (M-th power needs symbol-spaced samples)
        mf = S.matched_filter_symbols(xs, sps, rolloff)
        if len(mf) > 16:
            out['cfo_hz'] = D.estimate_cfo_mth(mf, 'qpsk', rate)
            del mf
    out['timing_samples'] = D.estimate_timing(xs, sps)
    if out['timing_samples']:
        xs = S.add_timing(xs, -out['timing_samples'])
    xc = D.remove_cfo(xs, out['cfo_hz'], fs_out) if out['cfo_hz'] else xs
    out['symbols'] = S.matched_filter_symbols(xc, sps, rolloff)
    return out
