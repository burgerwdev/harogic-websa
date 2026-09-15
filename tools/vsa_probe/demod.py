#!/usr/bin/env python3
"""Tier2 in-house PSK/QAM demodulation chain (NumPy only, no vendor library).

The vendor digital demodulation library is not installed
(``Demod_Check() == -1``), so every symbol-level step has to be ours. This
module is the whole chain a VSA would need, written so each stage can be
measured independently:

1. ``estimate_symbol_rate``  — Oerder-Meyr: the |x|^2 spectral line at 1/T
2. ``fft_resample``          — band-limited resampling to a fixed sps
3. ``estimate_cfo_mth``      — M-th power non-data-aided carrier estimate
4. ``estimate_timing``       — the phase of the same |x|^2 line
5. ``matched_filter_symbols``— RRC matched filter (from siggen)
6. ``carrier_pll``           — decision-directed second-order phase tracking
7. ``decide`` / ``evm_percent`` — slicing and error-vector magnitude
8. ``demodulate``            — the chain, returning symbols and diagnostics

Nothing here assumes the reference symbols: the PLL slices its own output, so
the same code runs on hardware captures.
"""
from __future__ import annotations

import time

import numpy as np
import siggen as S


def fft_resample(x: np.ndarray, n_out: int) -> np.ndarray:
    """Band-limited resampling to ``n_out`` samples (exact for a band-limited x).

    The spectrum is shifted so DC sits at the centre and the same physical
    frequencies are copied across; an earlier index-arithmetic version dropped
    the Nyquist bin and shifted every negative frequency by one bin, which
    quietly added ~9.6 % EVM to the demodulated constellation (measured in
    probe_dsp_loopback.py).
    """
    n_in = len(x)
    if n_out <= 0:
        return np.zeros(0, dtype=complex)
    if n_in == n_out:
        return np.asarray(x).copy()
    X = np.fft.fftshift(np.fft.fft(x))
    c_in, c_out = n_in // 2, n_out // 2
    half = min(c_in, c_out)
    left = min(c_in, c_out, half)
    right = min(n_in - 1 - c_in, n_out - 1 - c_out, half)
    Y = np.zeros(n_out, dtype=complex)
    Y[c_out - left:c_out + right + 1] = X[c_in - left:c_in + right + 1]
    return np.fft.ifft(np.fft.ifftshift(Y)) * (n_out / n_in)


def estimate_symbol_rate(iq: np.ndarray, fs: float,
                         min_rel: float = 0.002, max_rel: float = 0.45) -> tuple:
    """Oerder-Meyr symbol-rate estimate from the |x|^2 spectral line.

    Returns ``(symbol_rate_hz, line_phase_turns)``. The phase is the timing
    offset in turns (1 turn = 1 symbol).
    """
    x = np.asarray(iq)
    n = len(x)
    p = np.abs(x) ** 2
    p = p - p.mean()
    w = np.blackman(n)
    spec = np.fft.fft(p * w)
    lo = int(min_rel * n)
    hi = int(max_rel * n)
    band = np.zeros(n, dtype=bool)
    band[lo:hi + 1] = True                        # positive side only
    mag = np.where(band, np.abs(spec), 0.0)
    k = int(np.argmax(mag))
    if k <= 0 or k >= n - 1:
        return 0.0, 0.0
    a, b, c = (np.log(max(1e-30, mag[k - 1])), np.log(max(1e-30, mag[k])),
               np.log(max(1e-30, mag[k + 1])))
    den = a - 2 * b + c
    d = 0.0 if abs(den) < 1e-12 else float(np.clip(0.5 * (a - c) / den, -0.5, 0.5))
    rate = (k + d) * fs / n
    phase = float(np.angle(spec[k]) / (2 * np.pi))
    return rate, phase


def estimate_timing(iq: np.ndarray, sps: float) -> float:
    """Timing offset in samples from the |x|^2 line phase at 1/T.

    The line's own coefficient has a phase of 0 or pi depending on the pulse
    shape, so the result is ambiguous by half a symbol: pair it with
    :func:`resolve_timing` before using it.
    """
    x = np.asarray(iq)
    n = len(x)
    p = np.abs(x) ** 2
    p = p - p.mean()
    spec = np.fft.fft(p * np.blackman(n))
    k = int(round(n / sps))
    if not 0 < k < n // 2:
        return 0.0
    # sum the two bins around the line so a non-integer sps still works
    lo, hi = max(0, k - 1), min(n - 1, k + 1)
    v = spec[lo:hi + 1].sum()
    return float(((np.pi - np.angle(v)) * sps / (2 * np.pi)) % sps)


def resolve_timing(x: np.ndarray, sps: float, rolloff: float,
                   tau: float) -> float:
    """Pick between ``tau`` and ``tau + sps/2`` by matched-filter output power.

    Sampling half a symbol away from the symbol instant puts the matched filter
    output near a zero crossing, so the two candidates are easy to separate.
    Measured: the winner is always the true instant for every case in
    ``probe_dsp_loopback.py`` (injected timing recovered to <0.05 sample).
    """
    best = None
    for cand in (tau, (tau + sps / 2.0) % sps):
        shifted = S.add_timing(x, -cand) if cand else x
        mf = S.matched_filter_symbols(shifted, int(sps), rolloff)
        power = float(np.mean(np.abs(mf) ** 2)) if len(mf) else -1.0
        if best is None or power > best[0]:
            best = (power, cand)
    return best[1]


def estimate_cfo_mth(symbols: np.ndarray, kind: str, symbol_rate: float,
                     nfft_mult: int = 8, smooth: int = 3) -> float:
    """M-th power non-data-aided carrier estimate (M = 4: QPSK and square QAM).

    ``symbols`` must be symbol-spaced (M-th power of a pulse-shaped oversampled
    signal has no line). The 4th-power spectrum carries the carrier offset
    multiplied by 4, so the peak bin gives ``cfo = m / (4 * nfft)`` cycles per
    symbol; unambiguously within +-1/8 of the symbol rate, which is the range
    the probes test. An FFT, not a grid search: 1024 symbols cost ~50 us.
    """
    z = np.asarray(symbols) ** 4
    n = len(z)
    if n < 16:
        return 0.0
    nfft = max(1024, int(n * nfft_mult))
    Z = np.fft.fft(z, nfft)
    k = int(np.argmax(np.abs(Z)))
    if k > nfft // 2:
        k -= nfft
    if smooth > 1 and 0 < abs(k) < nfft // 2 - 1:
        a, b, c = (np.log(max(1e-30, abs(Z[k - 1]))), np.log(max(1e-30, abs(Z[k]))),
                   np.log(max(1e-30, abs(Z[k + 1]))))
        den = a - 2 * b + c
        if abs(den) > 1e-12:
            k = k + float(np.clip(0.5 * (a - c) / den, -0.5, 0.5))
    return float(k / (4.0 * nfft) * symbol_rate)


def remove_cfo(x: np.ndarray, cfo_hz: float, fs: float) -> np.ndarray:
    t = np.arange(len(x)) / fs
    return x * np.exp(-2j * np.pi * cfo_hz * t)


def carrier_pll(symbols: np.ndarray, kind: str, symbol_rate: float,
                loop_bw: float = 0.02) -> tuple:
    """Decision-directed second-order phase/frequency tracking.

    Returns ``(corrected_symbols, phase_trace_rad, freq_trace_hz)``. The loop
    slices its own output, so it needs no reference sequence.
    """
    s = np.asarray(symbols)
    pts = S.constellation(kind)
    n = len(s)
    out = np.empty(n, dtype=complex)
    phase = 0.0
    freq = 0.0
    zeta = 0.707
    theta = loop_bw / (zeta + 1 / (4 * zeta))
    kp = 4 * zeta * theta / (1 + 2 * zeta * theta + theta ** 2)
    ki = 4 * theta ** 2 / (1 + 2 * zeta * theta + theta ** 2)
    phases = np.empty(n)
    freqs = np.empty(n)
    for i in range(n):
        y = s[i] * np.exp(-1j * phase)
        out[i] = y
        idx = int(np.argmin(np.abs(pts - y)))
        err = np.angle(y * np.conj(pts[idx]))
        freq += ki * err
        phase += freq + kp * err
        phases[i] = phase
        freqs[i] = freq
    return out, phases, freqs * symbol_rate / (2 * np.pi)


def decide(symbols: np.ndarray, kind: str) -> tuple:
    """Slice to the nearest ideal point; returns (ideal, indices).

    The input is RMS-normalised first, so any AGC scale is accepted.
    """
    pts = S.constellation(kind)
    s = np.asarray(symbols)
    rms = float(np.sqrt(np.mean(np.abs(s) ** 2))) if len(s) else 1.0
    if rms > 0:
        s = s / rms
    idx = np.abs(s[:, None] - pts[None, :]).argmin(axis=1)
    return pts[idx], idx


def ideal_gain(rx: np.ndarray, ideal: np.ndarray) -> complex:
    """LS complex gain g with ``g * ideal ~= rx`` (measurement AGC).

    ``np.vdot(a, b)`` conjugates its first argument, so the order here matters:
    using ``vdot(rx, ideal)`` returns the conjugate of the wanted gain and
    turned every EVM reading into 200 % (found in probe_dsp_loopback.py).
    """
    return np.vdot(ideal, rx) / np.vdot(ideal, ideal)


def evm_percent(rx: np.ndarray, ideal: np.ndarray, drop: int = 0) -> float:
    """RMS EVM in percent after LS gain alignment."""
    rx = np.asarray(rx)
    ideal = np.asarray(ideal)
    if drop:
        rx, ideal = rx[drop:-drop], ideal[drop:-drop]
    g = ideal_gain(rx, ideal)
    return S.evm_rms_percent(rx, g * ideal)


class DemodResult:
    def __init__(self):
        self.symbols = None          # recovered symbols (carrier/timing corrected)
        self.sliced = None           # nearest ideal points (VSA-style EVM)
        self.ideal = None            # ground truth, when the probe has it
        self.evm_vs_sliced = float('nan')
        self.evm_vs_truth = float('nan')
        self.ser_vs_truth = float('nan')
        self.phase_ambiguity_deg = float('nan')
        self.symbol_rate = 0.0
        self.symbol_rate_used = 0.0
        self.cfo_hz = 0.0
        self.timing_samples = 0.0
        self.sps_out = 0
        self.n_symbols = 0
        self.cpu_s = 0.0
        self.signal_s = 0.0

    def summary(self) -> dict:
        return dict(n_symbols=self.n_symbols, symbol_rate=self.symbol_rate,
                    cfo_hz=self.cfo_hz, timing_samples=self.timing_samples,
                    evm_vs_sliced=float(self.evm_vs_sliced),
                    evm_vs_truth=float(self.evm_vs_truth),
                    ser_vs_truth=float(self.ser_vs_truth),
                    phase_ambiguity_deg=float(self.phase_ambiguity_deg),
                    cpu_s=self.cpu_s, signal_s=self.signal_s,
                    cpu_realtime_pct=100 * self.cpu_s / self.signal_s
                    if self.signal_s else float('nan'))


def demodulate(iq: np.ndarray, fs: float, kind: str = 'qpsk', rolloff: float = 0.35,
               symbol_rate: float | None = None, sps: int = 8,
               truth: np.ndarray | None = None, pll_bw: float = 0.02) -> DemodResult:
    """Full capture -> symbols chain. ``truth`` is optional ground truth."""
    res = DemodResult()
    t0 = time.perf_counter()
    x = np.asarray(iq)
    res.signal_s = len(x) / fs

    est_rate, _ = estimate_symbol_rate(x, fs)
    res.symbol_rate = est_rate
    rate = symbol_rate if symbol_rate else est_rate
    res.symbol_rate_used = rate
    if rate <= 0:
        res.cpu_s = time.perf_counter() - t0
        return res

    n_out = int(round(len(x) * sps * rate / fs))
    xs = fft_resample(x, n_out)
    res.sps_out = sps

    # Timing comes first: |x|^2 carries the symbol-rate line and is immune to a
    # constant carrier phase. The M-th power estimator needs symbol-spaced
    # samples, so the carrier estimate happens after the matched filter.
    tau = resolve_timing(xs, sps, rolloff, estimate_timing(xs, sps))
    # The timing offset is only meaningful modulo one symbol; wrapping keeps the
    # sampled symbols aligned with the start of the capture (a +1 symbol offset
    # would otherwise shift every recovered symbol by one index).
    tau = (tau + sps / 2.0) % sps - sps / 2.0
    res.timing_samples = tau
    if tau:
        xs = S.add_timing(xs, -tau)

    mf_full = S.matched_filter_symbols(xs, sps, rolloff)
    # Drop the matched-filter transient at the start (a partially filled filter)
    # and the zero tail at the end (symbols beyond the end of the signal).
    # Leaving either in added 9.9 % to the sliced EVM in the first loopback run:
    # a near-zero symbol slices to a full-magnitude point and dominates RMS EVM.
    span = S.SPAN
    if len(mf_full) <= span + 8:
        res.cpu_s = time.perf_counter() - t0
        return res
    mf = mf_full[span:]
    mag = np.abs(mf)
    thr = 0.2 * float(np.sqrt(np.mean(mag ** 2)))
    above = np.nonzero(mag > thr)[0]
    if len(above):
        mf = mf[:int(above[-1]) + 1]
    if len(mf) < 8:
        res.cpu_s = time.perf_counter() - t0
        return res
    res.cfo_hz = estimate_cfo_mth(mf, kind, rate)
    mf = remove_cfo(mf, res.cfo_hz, rate)
    syms, _, _ = carrier_pll(mf, kind, rate, loop_bw=pll_bw)
    rms = float(np.sqrt(np.mean(np.abs(syms) ** 2)))
    syms = syms / rms if rms > 0 else syms          # a VSA displays a normalised cloud
    res.symbols = syms
    res.n_symbols = len(syms)
    sliced, _ = decide(syms, kind)
    res.sliced = sliced
    res.evm_vs_sliced = evm_percent(syms, sliced)
    if truth is not None:
        t = truth[span:span + len(syms)]
        n = min(len(t), len(syms))
        res.ideal = t[:n]
        res.evm_vs_truth = evm_percent(syms[:n], t[:n])
        # The M-th power estimator cannot resolve the absolute phase: the
        # constellation may be rotated by k*90 degrees. A real VSA needs a
        # preamble/pilot for that; the loopback probe resolves it against the
        # reference and reports which rotation it was.
        best = None
        for k in range(4):
            rot = np.exp(1j * np.pi * k / 2)
            ser = float(np.mean(decide(syms[:n] * rot, kind)[0] != t[:n]))
            if best is None or ser < best[0]:
                best = (ser, k * 90)
        res.ser_vs_truth = best[0]
        res.phase_ambiguity_deg = float(best[1])
    res.cpu_s = time.perf_counter() - t0
    return res
