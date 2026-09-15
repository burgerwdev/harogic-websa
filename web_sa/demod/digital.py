"""demod/digital.py -- symbol-level DSP for PSK/QAM (NumPy only, no vendor library).

The vendor digital demodulation library is not installed on this bench
(``Demod_Check() == -1``), so every symbol-level step has to be ours. This module is
the whole chain, in the order it runs:

1. :func:`estimate_symbol_rate`  -- Oerder-Meyr: the |x|^2 spectral line at 1/T
2. :func:`fft_resample`          -- band-limited resampling to a fixed sps
3. :func:`estimate_timing`       -- the phase of that same line, plus
   :func:`resolve_timing` for its half-symbol ambiguity
4. :func:`matched_filter_symbols`-- RRC matched filter at the symbol instants
5. :func:`estimate_cfo_mth`      -- M-th power non-data-aided carrier estimate
6. :func:`track_carrier`         -- decision-directed residual phase/frequency fit
7. :func:`decide` / :func:`symbol_table` -- slicing and the bit table
8. :func:`evm_percent` / :func:`mer_db` / :func:`symbol_errors` -- the metrics
9. :func:`demodulate`            -- the chain, its cost and its ambiguity report

Two measured rules are baked in because they are the difference between a right and a
wrong EVM:

* **The matched-filter transient is dropped.** The first ``SPAN`` symbols ride a
  partially filled filter and the tail past the signal is zeros; leaving them in added
  9.9 % to the sliced EVM in the probe loopback (a near-zero symbol slices to a
  full-magnitude point and dominates RMS EVM).
* **The carrier phase is resolved externally.** The M-th power estimator cannot tell a
  rotation by a multiple of 360/M degrees (90 deg for QPSK and square QAM): every
  candidate gives identical EVM, so no internal metric can choose one. A known preamble
  does it automatically; otherwise the user's rotation is applied and the candidates are
  reported (`resolve_ambiguity`). Never silently rotate.

Nothing here assumes the reference symbols: the estimators and the tracker run on a real
capture, and the reference is only used to *measure* SER/BER and to resolve the ambiguity.

**Measured** (`tests/test_digital.py`, RRC-shaped QPSK/16-QAM at 3.906 MSPS, sps 16, 4096
symbols, 2.5 kHz carrier offset, 0.4 samples of timing):

=========================================  ==========================================
EVM against the matched-filter bound       median ratio 0.99 over 8-30 dB (no bias); one
                                           8 dB realisation in five reads 15 % high,
                                           which is the estimator's own variance there
SER / BER at 8/12/20/30 dB                 0 with the preamble resolving the rotation
timing error                               <= 0.026 sample over 8-30 dB, 0.001 at 30 dB.
                                           The blind |x|^2 stage alone needs 30 dB to
                                           reach 0.05 (0.31 samples at 8 dB), which is
                                           why the decision-aided stage exists
carrier offset (total, M-th + tracked)     <= 0.03 Hz
cost, 2^17-sample capture (33.6 ms)        169 ms (503 % of real time) on this machine;
                                           a 2^20 capture costs 3.8 s (1402 %)
tracker share of that cost                 2.3 ms for 4000 symbols, 10x faster than the
                                           probe's per-symbol PLL (25 ms)
=========================================  ==========================================
"""
from __future__ import annotations

import time
from dataclasses import dataclass, field

import numpy as np

#: Nominal, unit-RMS constellations. Gray-coded bit mappings arrive with the
#: symbol table (roadmap 3.x); the grids are what a cloud is compared against.
NOMINAL: dict[str, np.ndarray] = {
    'qpsk': np.array([1 + 1j, -1 + 1j, -1 - 1j, 1 - 1j], dtype=complex) / np.sqrt(2.0),
    '16qam': np.array([complex(i, q) for i in (-3, -1, 1, 3) for q in (-3, -1, 1, 3)],
                      dtype=complex) / np.sqrt(10.0),
}

#: Matched-filter span in symbols. Measured noiseless EVM floor of the truncation: 1.6 % at
#: 4, 0.29 % at 10, 0.013 % at 20, 0.007 % at 64. 20 is the measurement-grade choice: the
#: floor stops mattering above ~25 dB SNR, and the matcher is still one `np.convolve`.
SPAN = 20

#: Blind symbol-rate estimates need at least this many samples per symbol: at sps 2 the
#: |x|^2 line collapses (measured), so the chain refuses the capture instead of returning a
#: confident-looking cloud.
MIN_SPS = 4

#: Gray-coded bit patterns per axis level (-3, -1, +1, +3 for 16-QAM).
_GRAY4 = ((0, 0), (0, 1), (1, 1), (1, 0))


def symbol_table(kind: str) -> tuple:
    """``(bits_per_symbol, table)`` for a modulation.

    ``table[i]`` is the bit tuple of ``nominal_points(kind)[i]``: the nominal grid order is
    the table order (QPSK: 00, 01, 11, 10 in the I/Q plane, i.e. Gray-coded; 16-QAM:
    I-major, each axis Gray-coded from -3 to +3). This is what a VSA's symbol table shows
    and what turns decisions into bits.
    """
    if kind == 'qpsk':
        return 2, tuple(_GRAY4)
    if kind == '16qam':
        return 4, tuple(_GRAY4[i] + _GRAY4[q] for i in range(4) for q in range(4))
    raise ValueError(f'unknown modulation {kind!r}')


def nominal_points(kind: str) -> np.ndarray:
    """Unit-RMS nominal constellation for ``kind`` (raises on an unknown name)."""
    try:
        return NOMINAL[kind].copy()
    except KeyError:
        raise ValueError(f'unknown modulation {kind!r}') from None


def rrc(rolloff: float, sps: int, span: int = SPAN) -> np.ndarray:
    """Root-raised-cosine impulse response, unit DC gain (``sum(h) == 1``).

    The two removable singularities of the closed form (``t = 0`` and
    ``t = +-1/(4*beta)``) are avoided by nudging those grid points, which keeps the
    sampled response correct to ~1e-9 for every beta used here.
    """
    b = float(rolloff)
    n = np.arange(-span * sps, span * sps + 1, dtype=float)
    t = n / sps
    with np.errstate(divide='ignore', invalid='ignore'):
        near_sing = np.abs(np.abs(4 * b * t) - 1) < 1e-9
        t = np.where(near_sing, t + 1e-9, t)
        num = np.sin(np.pi * t * (1 - b)) + 4 * b * t * np.cos(np.pi * t * (1 + b))
        den = np.pi * t * (1 - (4 * b * t) ** 2)
        h = num / den
    h[n == 0] = 1 - b + 4 * b / np.pi
    return h / h.sum()


def add_timing(iq: np.ndarray, delay_samples: float) -> np.ndarray:
    """Exact band-limited fractional delay (FFT linear phase)."""
    n = len(iq)
    k = np.fft.fftfreq(n) * n
    return np.fft.ifft(np.fft.fft(iq) * np.exp(-2j * np.pi * k * delay_samples / n))


def fft_resample(x: np.ndarray, n_out: int) -> np.ndarray:
    """Band-limited resampling to ``n_out`` samples (exact for a band-limited ``x``).

    The spectrum is shifted so DC sits at the centre and the same physical
    frequencies are copied across; an index-arithmetic version dropped the
    Nyquist bin and shifted every negative frequency by one bin, which quietly
    added ~9.6 % EVM to the demodulated constellation (measured with the probes).
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


def matched_filter_symbols(iq: np.ndarray, sps: int, rolloff: float,
                           timing: float = 0.0, span: int = SPAN) -> np.ndarray:
    """RRC matched filter -> symbol-rate sampling at the given timing phase."""
    h = rrc(rolloff, sps, span)
    mf = np.convolve(iq, h)
    start = 2 * span * sps + int(round(timing))
    n_sym = (len(mf) - start) // sps
    return mf[start:start + n_sym * sps:sps]


def _symbol_line(iq: np.ndarray, min_rel: float = 0.002, max_rel: float = 0.45) -> tuple:
    """The |x|^2 spectrum both the rate and the timing estimate come from.

    One transform serves both (they read the same line at 1/T), which is why they are
    computed together instead of each paying for its own FFT of the whole capture.
    Returns ``(spec, band_lo, band_hi)``.
    """
    x = np.asarray(iq)
    n = len(x)
    p = np.abs(x) ** 2
    p = p - p.mean()
    w = np.blackman(n)
    return np.fft.fft(p * w), int(min_rel * n), int(max_rel * n)


def estimate_symbol_rate(iq: np.ndarray, fs: float,
                         min_rel: float = 0.002, max_rel: float = 0.45) -> tuple:
    """Oerder-Meyr symbol-rate estimate from the |x|^2 spectral line.

    Returns ``(symbol_rate_hz, line_phase_turns)``; the phase is the timing offset
    in turns (1 turn = 1 symbol). The search band is deliberately blind
    (0.2 %..45 % of the sample rate): a symbol rate above half the sample rate
    cannot be represented, and below 4 samples/symbol the line is too weak to
    find (measured: sps 2 collapses, so callers must reject sps < 4).
    """
    x = np.asarray(iq)
    n = len(x)
    spec, lo, hi = _symbol_line(x, min_rel, max_rel)
    band = np.zeros(n, dtype=bool)
    band[lo:hi + 1] = True                                  # positive side only
    mag = np.where(band, np.abs(spec), 0.0)
    k = int(np.argmax(mag))
    if k <= 0 or k >= n - 1:
        return 0.0, 0.0
    a, b, c = (np.log(max(1e-30, mag[k - 1])), np.log(max(1e-30, mag[k])),
               np.log(max(1e-30, mag[k + 1])))
    den = a - 2 * b + c
    d = 0.0 if abs(den) < 1e-12 else float(np.clip(0.5 * (a - c) / den, -0.5, 0.5))
    return (k + d) * fs / n, float(np.angle(spec[k]) / (2 * np.pi))


def estimate_timing(iq: np.ndarray, sps: float, *, _spec: np.ndarray | None = None) -> float:
    """Timing offset in samples from the |x|^2 line phase at 1/T.

    The line's own coefficient has a phase of 0 or pi depending on the pulse shape, so the
    result is ambiguous by half a symbol: pair it with :func:`resolve_timing` before using
    it. ``_spec`` lets the caller pass the spectrum it already computed (see
    :func:`_symbol_line`).
    """
    x = np.asarray(iq)
    n = len(x)
    spec = _symbol_line(x)[0] if _spec is None else _spec
    k = int(round(n / sps))
    if not 0 < k < n // 2:
        return 0.0
    lo, hi = max(0, k - 1), min(n - 1, k + 1)   # two bins: a non-integer sps still works
    v = spec[lo:hi + 1].sum()
    return float(((np.pi - np.angle(v)) * sps / (2 * np.pi)) % sps)


def resolve_timing(x: np.ndarray, sps: int, rolloff: float, tau: float,
                   *, limit: int = 1 << 15) -> float:
    """Pick between ``tau`` and ``tau + sps/2`` by matched-filter output power.

    Sampling half a symbol away from the symbol instant puts the matched filter output
    near a zero crossing, so the two candidates separate easily: the winner is always the
    true instant (measured: injected timing recovered to 0.016 sample at 8192 symbols).

    Only the first ``limit`` samples are compared. The offset is a property of the whole
    block, and the full-capture version costs five times as much for no extra accuracy
    (measured on 2^17 samples: 87 ms against 18 ms at 2^15).
    """
    x = np.asarray(x)
    if limit and len(x) > limit:
        x = x[:limit]
    best = None
    for cand in (tau % sps, (tau + sps / 2.0) % sps):
        shifted = add_timing(x, -cand) if cand else x
        mf = matched_filter_symbols(shifted, int(sps), rolloff)
        power = float(np.mean(np.abs(mf) ** 2)) if len(mf) else -1.0
        if best is None or power > best[0]:
            best = (power, cand)
    return best[1]


def estimate_cfo_mth(symbols: np.ndarray, symbol_rate: float,
                     nfft_mult: int = 8, smooth: int = 3) -> float:
    """M-th power non-data-aided carrier estimate (M = 4: QPSK and square QAM).

    ``symbols`` must be symbol-spaced (the M-th power of a pulse-shaped
    oversampled signal has no line). The 4th-power spectrum carries the carrier
    offset multiplied by 4, so the peak bin gives ``cfo = m / (4 * nfft)`` cycles
    per symbol -- unambiguous within +-1/8 of the symbol rate, which is the range
    this chain is used over. One FFT, not a grid search: 1024 symbols cost ~50 us.
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
    """De-rotate a block by ``cfo_hz`` (a pure frequency shift, no phase tracking)."""
    t = np.arange(len(x)) / fs
    return x * np.exp(-2j * np.pi * cfo_hz * t)


# ---------------- decisions, metrics and the demodulation chain ----------------

def decide(symbols: np.ndarray, kind: str) -> tuple:
    """Slice to the nearest nominal point; returns ``(ideal, indices)``.

    The input is RMS-normalised first, so any AGC scale (or none at all) is accepted; the
    indices index :func:`symbol_table`'s table, which is what turns them into bits.
    """
    pts = nominal_points(kind)
    s = np.asarray(symbols, dtype=complex)
    if not len(s):
        return np.zeros(0, dtype=complex), np.zeros(0, dtype=int)
    rms = float(np.sqrt(np.mean(np.abs(s) ** 2)))
    if rms > 0:
        s = s / rms
    idx = np.abs(s[:, None] - pts[None, :]).argmin(axis=1)
    return pts[idx], idx


def align_gain(ideal: np.ndarray, rx: np.ndarray) -> complex:
    """Least-squares complex gain ``g`` with ``g * ideal ~= rx`` (measurement AGC).

    ``np.vdot(a, b)`` conjugates its first argument, so the argument order matters: using
    ``vdot(rx, ideal)`` returns the conjugate of the wanted gain and turned every EVM
    reading into 200 % (found in the probe loopback).
    """
    return np.vdot(ideal, rx) / np.vdot(ideal, ideal)


def evm_percent(rx: np.ndarray, ideal: np.ndarray, *, drop: int = 0,
                normalise: bool = False) -> float:
    """RMS EVM in percent after a least-squares gain alignment.

    ``normalise`` divides by the reference's RMS, which makes the number comparable across
    captures; otherwise the reference's own scale is used (a reference that is already
    unit-RMS gives the same answer either way).
    """
    rx = np.asarray(rx, dtype=complex)
    ideal = np.asarray(ideal, dtype=complex)
    n = min(len(rx), len(ideal))
    if drop:
        rx, ideal = rx[drop:n - drop], ideal[drop:n - drop]
        n = len(rx)
    if n == 0:
        return float('nan')
    rx, ideal = rx[:n], ideal[:n]
    g = align_gain(ideal, rx)
    err = rx - g * ideal
    ref = np.sum(np.abs(g * ideal) ** 2)
    if ref <= 0:
        return float('nan')
    return float(np.sqrt(np.sum(np.abs(err) ** 2) / ref) * 100.0)


def evm_to_mer_db(evm_frac: float) -> float:
    """MER from a *fractional* RMS EVM: ``-20*log10(EVM)``."""
    if not np.isfinite(evm_frac) or evm_frac <= 0:
        return float('nan')
    return float(-20.0 * np.log10(evm_frac))


def evm_to_snr_db(evm_frac: float) -> float:
    """Per-symbol SNR implied by a fractional RMS EVM (``1/EVM**2`` for unit-RMS points)."""
    if not np.isfinite(evm_frac) or evm_frac <= 0:
        return float('nan')
    return float(-20.0 * np.log10(evm_frac))


def symbol_errors(indices: np.ndarray, reference: np.ndarray) -> float:
    """Symbol error rate between decided indices and a reference index sequence."""
    n = min(len(indices), len(reference))
    if n == 0:
        return float('nan')
    return float(np.mean(np.asarray(indices)[:n] != np.asarray(reference)[:n]))


def _line_fit(values: np.ndarray, index: np.ndarray | None = None) -> tuple:
    """Least-squares ``(a, b)`` for ``values ~= a + b * index`` (index defaults to 0..n-1)."""
    y = np.asarray(values, dtype=float)
    x = np.arange(len(y), dtype=float) if index is None else np.asarray(index, dtype=float)
    n = len(y)
    if n < 2:
        return (float(y[0]) if n else 0.0), 0.0
    sx, sy = float(x.sum()), float(y.sum())
    sxx = float(np.dot(x, x))
    sxy = float(np.dot(x, y))
    den = n * sxx - sx * sx
    b = 0.0 if abs(den) < 1e-12 else (n * sxy - sx * sy) / den
    return (sy - b * sx) / n, b


def track_carrier(symbols: np.ndarray, kind: str, symbol_rate_hz: float,
                  *, iterations: int = 3, reject_deg: float = 30.0) -> tuple:
    """Decision-directed carrier tracking with no per-symbol loop.

    Each pass slices its own output, takes every symbol's phase error
    (``angle(y * conj(decision))``), fits one straight line through those errors and
    de-rotates by that phase-plus-frequency. A per-symbol PLL applies the same correction
    with a recursive filter; the block fit reaches it in two or three vectorised passes and,
    because it fits a line through the whole block, it tolerates the decision errors a
    low-SNR capture inevitably contains (errors beyond ``reject_deg`` are dropped from the
    fit and the pass is repeated).

    Returns ``(corrected, phase_rad_trace, freq_hz, residual_deg)``; the residual is the
    scatter of the phase errors that remain, i.e. the tracker's own quality number.
    """
    symbols = np.asarray(symbols, dtype=complex)
    n = len(symbols)
    if n < 8:
        return symbols, np.zeros(n), 0.0, float('nan')
    rms = float(np.sqrt(np.mean(np.abs(symbols) ** 2)))
    # Work on a unit-RMS copy (decisions need a scale-free cloud) but return the *original*
    # scale: a VSA shows volts, and rms_v/mean_dbm describe the capture, not the slicer.
    y = symbols / rms if rms > 0 else symbols
    index = np.arange(n, dtype=float)
    phase = np.zeros(n)
    a = b = 0.0
    for _ in range(max(1, iterations)):
        z = y * np.exp(-1j * phase)
        ideal, _idx = decide(z, kind)
        err = np.angle(z * np.conj(ideal))
        a, b = _line_fit(err)
        fit = a + b * index
        keep = np.abs(err - fit) < np.deg2rad(reject_deg)
        if 8 <= int(keep.sum()) < n:
            a, b = _line_fit(err[keep], index[keep])
        phase = phase + a + b * index
    z = y * np.exp(-1j * phase)
    ideal, _idx = decide(z, kind)
    err = np.angle(z * np.conj(ideal))
    residual = float(np.rad2deg(np.std(err - (a + b * index))))
    freq_hz = float(b * symbol_rate_hz / (2 * np.pi))
    return symbols * np.exp(-1j * phase), phase, freq_hz, residual



def _cubic_complex(values: np.ndarray, index: np.ndarray) -> np.ndarray:
    """Cubic (Catmull-Rom) interpolation of a complex array at fractional indices."""
    n = len(values)
    i = np.floor(index).astype(int)
    t = index - i
    i0 = np.clip(i - 1, 0, n - 1)
    i1 = np.clip(i, 0, n - 1)
    i2 = np.clip(i + 1, 0, n - 1)
    i3 = np.clip(i + 2, 0, n - 1)
    y0, y1, y2, y3 = values[i0], values[i1], values[i2], values[i3]
    a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3
    b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3
    c = -0.5 * y0 + 0.5 * y2
    return ((a * t + b) * t + c) * t + y1


def refine_timing(iq: np.ndarray, sps: int, rolloff: float, kind: str,
                  *, step: float = 0.25, limit: int = 1 << 16,
                  iterations: int = 2) -> tuple:
    """Decision-aided sampling instant, plus the symbols sampled there.

    The |x|^2 line gives a blind estimate whose error grows as the SNR falls (measured: 0.31
    samples mean at 8 dB over 4096 symbols, 0.04 at 30 dB). This second stage minimises what
    actually matters: the decision-directed EVM as a function of the sampling phase,
    ``J(d) = |y(d) - g*decision(y(d))|**2 / |g*decision|**2`` with the least-squares gain
    ``g`` re-solved per phase. J is sharply curved around the right instant (measured: the
    EVM of a sub-sample error at sps 8 is 4 % at 0.25 and 8 % at 0.5 samples), so a parabola
    through three evaluations pins the minimum in one or two passes -- no loop over symbols,
    just one convolution of a bounded prefix plus a few cubic interpolations.

    **It must run after the carrier is removed**: with a few kHz of offset the constellation
    rotates tens of times across a block and the decisions stop meaning anything.

    Returns the extra sampling offset. The caller re-samples through ``add_timing`` and
    ``matched_filter_symbols`` rather than taking interpolated values here: one code path
    makes the symbols, so a cloud cannot differ by how it was sampled.
    Measured over 8-30 dB with 4096 symbols: worst error 0.028 samples, where the blind
    estimate alone needed 30 dB to reach 0.05.
    """
    x = np.asarray(iq)
    if limit and len(x) > limit:
        x = x[:limit]
    mf = np.convolve(x, rrc(rolloff, sps))
    start = 2 * SPAN * sps
    n = (len(mf) - start - sps) // sps
    if n < 64:
        return 0.0
    grid = start + np.arange(n) * sps
    pts = nominal_points(kind)

    def evm_at(offset: float) -> float:
        y = _cubic_complex(mf, grid + offset)
        rms = float(np.sqrt(np.mean(np.abs(y) ** 2)))
        if rms <= 0:
            return float('inf')
        z = y / rms
        d = pts[np.abs(z[:, None] - pts[None, :]).argmin(axis=1)]
        g = np.vdot(d, z) / np.vdot(d, d)
        ref = np.sum(np.abs(g * d) ** 2)
        if ref <= 0:
            return float('inf')
        return float(np.sum(np.abs(z - g * d) ** 2) / ref)

    base = evm_at(0.0)
    tau = 0.0
    for _ in range(max(1, iterations)):
        scores = [evm_at(tau - step), base, evm_at(tau + step)]
        den = scores[0] - 2.0 * scores[1] + scores[2]
        if abs(den) < 1e-30:
            break
        vertex = float(np.clip(0.5 * (scores[0] - scores[2]) / den * step, -step, step))
        tau += vertex
        if abs(vertex) < 0.003:
            break
        base = evm_at(tau)
    # Never hand back a phase the data says is worse than where we started.
    return tau if tau and evm_at(tau) < evm_at(0.0) else 0.0


def resolve_ambiguity(symbols: np.ndarray, kind: str, *,
                      reference: np.ndarray | None = None,
                      phase_rot_deg: float = 0.0) -> dict:
    """Rotate a cloud out of the M-th power ambiguity and report how it was decided.

    The M-th power estimator cannot distinguish rotations that differ by a multiple of
    360/M degrees (90 for QPSK and square QAM): every candidate has identical EVM, so no
    internal metric can choose. Resolution is therefore external, and this function never
    rotates silently:

    * ``reference`` -- the known preamble/pilot symbols as *indices* into the nominal grid
      (``decide`` gives them). The rotation whose decisions match it best wins and the SER of
      every candidate is reported.
    * otherwise the user's ``phase_rot_deg`` is applied (``resolved_by='user'``), or the
      cloud is left as recovered (``resolved_by='none'``) with the candidate list.
    """
    s = np.asarray(symbols, dtype=complex)
    steps = max(1, int(round(360.0 / 90.0))) if kind in ('qpsk', '16qam') else 1
    candidates = []
    for k in range(steps):
        deg = 360.0 * k / steps
        rotated = s * np.exp(1j * np.deg2rad(deg))
        ser = float('nan')
        if reference is not None and len(reference):
            _ideal, idx = decide(rotated, kind)
            ser = symbol_errors(idx, reference)
        candidates.append({'rotation_deg': deg, 'ser': ser})
    if reference is not None and len(reference):
        best = min(candidates, key=lambda c: (c['ser'] if c['ser'] == c['ser'] else 1.0))
        return {'symbols': s * np.exp(1j * np.deg2rad(best['rotation_deg'])),
                'rotation_deg': best['rotation_deg'], 'resolved_by': 'reference',
                'candidates': candidates}
    applied = float(phase_rot_deg) % 360.0
    return {'symbols': s * np.exp(1j * np.deg2rad(applied)) if applied else s,
            'rotation_deg': applied, 'resolved_by': 'user' if applied else 'none',
            'candidates': candidates}


@dataclass
class DemodResult:
    """Everything the chain measured about one capture (all of it JSON-safe in summary)."""

    kind: str = 'qpsk'
    error: str = ''
    symbol_rate_hz: float = 0.0        # blind estimate
    symbol_rate_used: float = 0.0      # what the correction used (given or estimated)
    #: Total carrier offset removed: the M-th power estimate plus what the tracker fitted.
    cfo_hz: float = 0.0
    #: The non-data-aided part of it (what a Tier 1 view knows on its own).
    cfo_mth_hz: float = 0.0
    #: The decision-directed part (the tracker's residual).
    cfo_tracked_hz: float = 0.0
    timing_samples: float = 0.0
    sps: int = 0
    n_symbols: int = 0
    rms_v: float = 0.0
    mean_dbm: float = float('nan')
    evm_percent: float = float('nan')
    mer_db: float = float('nan')
    snr_db: float = float('nan')
    ser: float = float('nan')
    ber: float = float('nan')
    #: EVM against the known reference symbols (only when one was given).
    evm_reference_percent: float = float('nan')
    phase_residual_deg: float = float('nan')
    rotation_deg: float = 0.0
    resolved_by: str = 'none'
    candidates: list = field(default_factory=list)
    symbols: np.ndarray = field(default_factory=lambda: np.zeros(0, dtype=complex))
    ideal: np.ndarray = field(default_factory=lambda: np.zeros(0, dtype=complex))
    indices: np.ndarray = field(default_factory=lambda: np.zeros(0, dtype=int))
    phase_trace_rad: np.ndarray = field(default_factory=lambda: np.zeros(0))
    cpu_s: float = 0.0
    signal_s: float = 0.0

    def summary(self) -> dict:
        """The scalars that ride in STATUS and in a VSAD measurement block."""
        # `modulation`, not `kind`: the caller's own `kind` is the *measurement* name
        # (a measurement result carries both), so the two must not collide.
        out = {'modulation': self.kind}
        out.update({k: getattr(self, k) for k in
               ('error', 'symbol_rate_hz', 'symbol_rate_used', 'cfo_hz',
                'cfo_mth_hz', 'cfo_tracked_hz', 'timing_samples', 'sps', 'n_symbols', 'rms_v',
                'mean_dbm', 'evm_percent', 'mer_db', 'snr_db', 'ser', 'ber',
                'evm_reference_percent', 'phase_residual_deg', 'rotation_deg',
                'resolved_by')})
        out['symbols_n'] = self.n_symbols
        out['cpu_s'] = self.cpu_s
        out['signal_s'] = self.signal_s
        out['cpu_realtime_pct'] = (100.0 * self.cpu_s / self.signal_s) if self.signal_s else float('nan')
        return out


def tier1_cloud(iq: np.ndarray, fs: float, *, modulation: str = 'qpsk', rolloff: float = 0.35,
                symbol_rate: float | None = None, sps: int = 8,
                compensate_cfo: bool = True, trim: bool = True,
                refine_timing_offset: bool = False) -> dict:
    """Symbol cloud with rate/timing/carrier correction and no decisions.

    What a Tier 1 constellation shows. The matched-filter transient is dropped
    (``trim``): those symbols ride a partially filled filter, and leaving them in inflates
    every metric computed from the cloud. Returns the cloud, the nominal grid on the
    cloud's own scale, its RMS, and what was corrected (rate, CFO, timing) plus
    ``sps_too_low`` when the blind rate cannot be trusted.
    """
    x = np.asarray(iq)
    spec = _symbol_line(x)[0]
    est_rate, _ = estimate_symbol_rate(x, fs)
    rate = float(symbol_rate or est_rate)
    out = {'symbols': np.zeros(0, dtype=complex), 'nominal': np.zeros(0, dtype=complex),
           'rms_v': 0.0, 'mean_dbm': float('-inf'), 'symbol_rate_est': float(est_rate),
           'symbol_rate_used': rate, 'cfo_hz': 0.0, 'timing_samples': 0.0,
           'sps_too_low': False}
    if rate <= 0 or not len(x):
        return out
    if fs / rate < MIN_SPS:
        out['sps_too_low'] = True
        return out
    n_out = int(round(len(x) * sps * rate / fs))
    xs = fft_resample(x, n_out)
    fs_out = sps * rate
    if compensate_cfo:
        # Tier 1 has no decisions, so the M-th power estimate runs on the matched filter
        # output (the M-th power needs symbol-spaced samples).
        mf = matched_filter_symbols(xs, sps, rolloff)
        if len(mf) > 16:
            out['cfo_hz'] = estimate_cfo_mth(mf, rate)
        else:
            out['cfo_hz'] = 0.0
    # The |x|^2 line of the resampled block sits at the same Fourier index and carries the
    # same phase (a resample keeps the block's duration, so a delay in symbols is
    # unchanged), so the cached spectrum is reused when the lengths match -- and recomputed
    # otherwise, where the bin grid no longer lines up.
    cached = spec if len(xs) == len(x) else None
    tau = resolve_timing(xs, sps, rolloff, estimate_timing(xs, sps, _spec=cached))
    if tau:
        xs = add_timing(xs, -tau)
    if out['cfo_hz']:
        xs = remove_cfo(xs, out['cfo_hz'], fs_out)
    if refine_timing_offset:
        # Decision-aided second stage, after the carrier is gone (see refine_timing).
        extra = refine_timing(xs, sps, rolloff, modulation)
        if extra:
            tau += extra
            xs = add_timing(xs, -extra)
    sym = matched_filter_symbols(xs, sps, rolloff)
    out['timing_samples'] = tau
    if trim:
        sym = _trim_symbols(sym)
    out['symbols'] = sym
    if len(sym):
        rms = float(np.sqrt(np.mean(np.abs(sym) ** 2)))
        out['rms_v'] = rms
        out['mean_dbm'] = float(10 * np.log10(max(rms ** 2, 1e-30) / 50.0) + 30.0)
        out['nominal'] = nominal_points(modulation) * rms
    return out


def _trim_symbols(symbols: np.ndarray) -> np.ndarray:
    """Drop the matched-filter transient and the zero tail (see SPAN's note)."""
    s = np.asarray(symbols)
    if len(s) <= SPAN + 8:
        return s[:0]
    s = s[SPAN:]
    mag = np.abs(s)
    thr = 0.2 * float(np.sqrt(np.mean(mag ** 2)))
    above = np.nonzero(mag > thr)[0]
    if len(above):
        s = s[:int(above[-1]) + 1]
    return s if len(s) >= 8 else s[:0]


def demodulate(iq: np.ndarray, fs: float, kind: str = 'qpsk', *, rolloff: float = 0.35,
               symbol_rate: float | None = None, sps: int = 8,
               reference: np.ndarray | None = None, phase_rot_deg: float = 0.0,
               track: bool = True) -> DemodResult:
    """Whole capture -> symbols -> decisions chain, with its own cost measurement.

    ``reference`` (the known ideal symbols of a preamble, complex) is optional: it resolves
    the 4-fold phase ambiguity and is the only way SER/BER and an EVM against the truth can
    be measured. A capture the chain cannot work on comes back with ``error`` set
    (``sps_too_low``, ``no_symbol_line``, ``too_short``) instead of raising, because it is a
    property of the signal, not a bug. Without a reference the cloud is rotated by
    ``phase_rot_deg`` (or left as recovered) and ``resolved_by`` says which.
    """
    res = DemodResult(kind=kind)
    t0 = time.perf_counter()
    x = np.asarray(iq)
    res.signal_s = len(x) / fs if fs else 0.0
    try:
        est_rate, _ = estimate_symbol_rate(x, fs)
        res.symbol_rate_hz = float(est_rate)
        rate = float(symbol_rate or est_rate)
        res.symbol_rate_used = rate
        if rate <= 0:
            res.error = 'no_symbol_line'
            return res
        res.sps = int(fs // rate)
        if fs / rate < MIN_SPS:
            res.error = 'sps_too_low'
            return res
        cloud = tier1_cloud(x, fs, modulation=kind, rolloff=rolloff, symbol_rate=rate,
                            sps=sps, refine_timing_offset=True)
        res.timing_samples = cloud['timing_samples']
        res.cfo_mth_hz = cloud['cfo_hz']
        syms = cloud['symbols']
        if not len(syms):
            res.error = 'too_short'
            return res
        if track:
            syms, phase, tracked, residual = track_carrier(syms, kind, rate)
            res.cfo_tracked_hz = tracked
            res.phase_residual_deg = residual
            res.phase_trace_rad = phase
        # Reported as one number because that is the offset a caller has to undo to get
        # back to baseband (measured: within 0.03 Hz of the truth over 8-30 dB).
        res.cfo_hz = res.cfo_mth_hz + res.cfo_tracked_hz
        ref_indices = None
        if reference is not None and len(reference):
            _ideal, ref_indices = decide(reference, kind)
        resolved = resolve_ambiguity(syms, kind, reference=ref_indices,
                                     phase_rot_deg=phase_rot_deg)
        syms = resolved['symbols']
        res.rotation_deg = resolved['rotation_deg']
        res.resolved_by = resolved['resolved_by']
        res.candidates = resolved['candidates']
        rms = float(np.sqrt(np.mean(np.abs(syms) ** 2)))
        res.rms_v = rms
        if rms <= 0:
            res.error = 'silent'
            return res
        res.mean_dbm = float(10 * np.log10(max(rms ** 2, 1e-30) / 50.0) + 30.0)
        res.symbols = syms
        res.n_symbols = len(syms)
        res.ideal = nominal_points(kind) * rms
        ideal_norm, res.indices = decide(syms, kind)
        res.evm_percent = evm_percent(syms, ideal_norm)
        res.mer_db = evm_to_mer_db(res.evm_percent / 100.0)
        res.snr_db = evm_to_snr_db(res.evm_percent / 100.0)
        if ref_indices is not None:
            res.ser = symbol_errors(res.indices, ref_indices)
            _bits_per_symbol, table = symbol_table(kind)
            n = min(len(res.indices), len(ref_indices))
            got = np.asarray([table[i] for i in res.indices[:n]], dtype=int)
            want = np.asarray([table[int(i) % len(table)] for i in ref_indices[:n]], dtype=int)
            res.ber = float(np.mean(got != want))
            res.evm_reference_percent = evm_percent(syms, np.asarray(reference, dtype=complex))
        return res
    finally:
        res.cpu_s = time.perf_counter() - t0
