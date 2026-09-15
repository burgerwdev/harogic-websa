"""demod/digital.py -- symbol-level DSP for PSK/QAM (NumPy only, no vendor library).

The vendor digital demodulation library is not installed on this bench
(``Demod_Check() == -1``), so every symbol-level step has to be ours. This module
owns the *estimators and corrections* a constellation needs:

1. :func:`fft_resample`          -- band-limited resampling to a fixed sps
2. :func:`estimate_symbol_rate`  -- Oerder-Meyr: the |x|^2 spectral line at 1/T
3. :func:`estimate_timing`       -- the phase of that same line, plus
   :func:`resolve_timing` for its half-symbol ambiguity
4. :func:`estimate_cfo_mth`      -- M-th power non-data-aided carrier estimate
5. :func:`matched_filter_symbols`-- RRC matched filter at symbol instants

The decision-directed stages (slicing, EVM/SER, the symbol table and the 4-fold
ambiguity handling) land on top of these with VSA_ROADMAP section 3; the nominal
grids are already here because a Tier 1 cloud is drawn against them.

Everything is array-based (FFTs and vector arithmetic), so no stage needs a
per-symbol Python loop. Nothing assumes the reference symbols: the estimators run
on a real capture.
"""
from __future__ import annotations

import numpy as np

#: Nominal, unit-RMS constellations. Gray-coded bit mappings arrive with the
#: symbol table (roadmap 3.x); the grids are what a cloud is compared against.
NOMINAL: dict[str, np.ndarray] = {
    'qpsk': np.array([1 + 1j, -1 + 1j, -1 - 1j, 1 - 1j], dtype=complex) / np.sqrt(2.0),
    '16qam': np.array([complex(i, q) for i in (-3, -1, 1, 3) for q in (-3, -1, 1, 3)],
                      dtype=complex) / np.sqrt(10.0),
}

#: Matched-filter span in symbols (probe-verified: 10 is enough for every beta here).
SPAN = 10


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
    p = np.abs(x) ** 2
    p = p - p.mean()
    w = np.blackman(n)
    spec = np.fft.fft(p * w)
    band = np.zeros(n, dtype=bool)
    band[int(min_rel * n):int(max_rel * n) + 1] = True      # positive side only
    mag = np.where(band, np.abs(spec), 0.0)
    k = int(np.argmax(mag))
    if k <= 0 or k >= n - 1:
        return 0.0, 0.0
    a, b, c = (np.log(max(1e-30, mag[k - 1])), np.log(max(1e-30, mag[k])),
               np.log(max(1e-30, mag[k + 1])))
    den = a - 2 * b + c
    d = 0.0 if abs(den) < 1e-12 else float(np.clip(0.5 * (a - c) / den, -0.5, 0.5))
    return (k + d) * fs / n, float(np.angle(spec[k]) / (2 * np.pi))


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
    lo, hi = max(0, k - 1), min(n - 1, k + 1)   # two bins: a non-integer sps still works
    v = spec[lo:hi + 1].sum()
    return float(((np.pi - np.angle(v)) * sps / (2 * np.pi)) % sps)


def resolve_timing(x: np.ndarray, sps: int, rolloff: float, tau: float) -> float:
    """Pick between ``tau`` and ``tau + sps/2`` by matched-filter output power.

    Sampling half a symbol away from the symbol instant puts the matched filter
    output near a zero crossing, so the two candidates separate easily. Measured:
    the winner is always the true instant across the probe cases (injected timing
    recovered to <0.05 sample).
    """
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
