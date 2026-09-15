#!/usr/bin/env python3
"""Reproducible synthetic IQ generator for VSA probes (offline, no hardware).

The tinySA Ultra can only produce CW / AM / FM, so every PSK/QAM claim is
validated on synthetic IQ injected into the DSP chain. These helpers are the
single source of those signals: ideal constellations, RRC pulse shaping, AWGN,
carrier-frequency offset and fractional timing offset, plus the EVM reference
maths used to check a measurement against theory.

Conventions
-----------
* samples/symbol ``sps``, symbol values normalised to unit average power
* ``modulate()`` output is rescaled to unit average power (so SNR is exact)
* the receiver side is a matched filter with the same RRC, sampled at symbol
  instants: the cascade is a raised cosine with zero ISI

Usage:  from siggen import modulate, add_awgn, evm_rms_percent, ...
"""
from __future__ import annotations

import numpy as np

#: ideal unit-average-power constellations
#: Default RRC truncation span (symbols each side) shared by tx and receiver.
#: It sets the noiseless EVM floor of the synthetic reference, measured in
#: probe_siggen_selfcheck.py: 1.6% at span 4, 0.29% at span 10, 0.013% at
#: span 20, 0.007% at span 64.
SPAN = 20

_POINTS = {
    'bpsk': np.array([-1.0, 1.0]),
    'qpsk': np.array([1 + 1j, -1 + 1j, -1 - 1j, 1 - 1j]) / np.sqrt(2),
    '8psk': np.exp(2j * np.pi * np.arange(8) / 8),
    '16qam': np.array([(i - 1.5) + 1j * (q - 1.5)
                       for i in range(4) for q in range(4)]) / np.sqrt(2.5),
    '64qam': np.array([(i - 3.5) + 1j * (q - 3.5)
                       for i in range(8) for q in range(8)]) / np.sqrt(10.5),
}


def constellation(kind: str) -> np.ndarray:
    """Ideal symbol points, unit average power."""
    try:
        return _POINTS[kind.lower()]
    except KeyError:
        raise ValueError(f'unknown modulation {kind!r}; have {sorted(_POINTS)}') from None


def bits_per_symbol(kind: str) -> int:
    n = len(constellation(kind))
    return int(np.log2(n))


def rrc(rolloff: float, sps: int, span: int = 10) -> np.ndarray:
    """Root-raised-cosine impulse response, unit DC gain (sum(h) == 1).

    The two removable singularities of the closed form (t = 0 and
    t = +-1/(4*beta)) are avoided by nudging those grid points, which keeps the
    sampled response correct to ~1e-9 for every beta the probes use.
    """
    b = float(rolloff)
    n = np.arange(-span * sps, span * sps + 1, dtype=float)
    t = n / sps
    h = np.empty_like(t)
    with np.errstate(divide='ignore', invalid='ignore'):
        near_sing = np.abs(np.abs(4 * b * t) - 1) < 1e-9
        t = np.where(near_sing, t + 1e-9, t)
        num = np.sin(np.pi * t * (1 - b)) + 4 * b * t * np.cos(np.pi * t * (1 + b))
        den = np.pi * t * (1 - (4 * b * t) ** 2)
        h = num / den
    h[n == 0] = 1 - b + 4 * b / np.pi
    return h / h.sum()


def symbols(kind: str, n: int, seed: int = 0) -> np.ndarray:
    rng = np.random.default_rng(seed)
    return rng.choice(constellation(kind), size=int(n))


class SynthSignal:
    """A generated waveform plus everything needed to reference it.

    The receiver matched-filters and samples at symbol instants, which recovers
    ``symbols[:len(rx)]`` exactly (the cascade RRC+RRC is a Nyquist raised
    cosine): use :meth:`ref_from`. The tx pulse-shaping delay and the matched
    filter delay are the same ``span*sps``, so no symbol is lost at the start.
    """

    def __init__(self, iq, symbols, sps, rolloff, kind, seed, span=SPAN):
        self.iq = iq
        self.symbols = symbols
        self.sps = sps
        self.rolloff = rolloff
        self.kind = kind
        self.seed = seed
        self.span = span
        self.n_sym = len(symbols)
        self.impairments: dict = {}

    def __repr__(self):
        return (f'SynthSignal({self.kind}, nsym={self.n_sym}, sps={self.sps}, '
                f'rolloff={self.rolloff}, power={float(np.mean(np.abs(self.iq)**2)):.6f})')

    def ref_from(self, rx: np.ndarray) -> np.ndarray:
        """Reference symbols that line up with matched-filter output ``rx``."""
        return self.symbols[:len(rx)]

    def copy_with(self, iq) -> SynthSignal:
        s = SynthSignal(iq, self.symbols, self.sps, self.rolloff, self.kind,
                        self.seed, self.span)
        s.impairments = dict(self.impairments)
        return s


def modulate(kind: str = 'qpsk', n_sym: int = 1024, sps: int = 8,
             rolloff: float = 0.35, seed: int = 0, span: int = SPAN) -> SynthSignal:
    """Pulse-shaped (single RRC) unit-power waveform; receiver uses the MF.

    The full convolution is kept, so the first and last symbol pulses are
    complete (nothing is truncated at the edges). Power is normalised on the
    steady-state part so that ``mean(|iq|^2) == 1`` holds for the payload.
    """
    sym = symbols(kind, n_sym, seed)
    up = np.zeros(len(sym) * sps, dtype=complex)
    up[::sps] = sym
    h = rrc(rolloff, sps, span)
    iq = np.convolve(up, h)
    core = iq[span * sps:len(iq) - span * sps]
    iq = iq * np.sqrt(1.0 / np.mean(np.abs(core) ** 2))
    return SynthSignal(iq, sym, sps, rolloff, kind, seed, span)


def add_awgn(iq: np.ndarray, snr_db: float, seed: int = 1) -> np.ndarray:
    """Complex AWGN at the requested SNR relative to the *signal* power."""
    rng = np.random.default_rng(seed)
    p_sig = float(np.mean(np.abs(iq) ** 2))
    p_noise = p_sig / 10 ** (snr_db / 10.0)
    n = (rng.standard_normal(len(iq)) + 1j * rng.standard_normal(len(iq))) / np.sqrt(2)
    return iq + n * np.sqrt(p_noise)


def add_cfo(iq: np.ndarray, cfo_hz: float, fs: float) -> np.ndarray:
    t = np.arange(len(iq)) / fs
    return iq * np.exp(2j * np.pi * cfo_hz * t)


def add_timing(iq: np.ndarray, delay_samples: float) -> np.ndarray:
    """Exact band-limited fractional delay (FFT linear phase)."""
    n = len(iq)
    k = np.fft.fftfreq(n) * n
    return np.fft.ifft(np.fft.fft(iq) * np.exp(-2j * np.pi * k * delay_samples / n))


def matched_filter_symbols(iq: np.ndarray, sps: int, rolloff: float,
                           timing: float = 0.0, span: int = SPAN) -> np.ndarray:
    """RRC matched filter -> symbol-rate sampling at the given timing phase.

    ``out[m]`` aligns with ``SynthSignal.symbols[m]`` (see
    ``SynthSignal.ref_from``).
    """
    h = rrc(rolloff, sps, span)
    mf = np.convolve(iq, h)
    start = 2 * span * sps + int(round(timing))
    n_sym = (len(mf) - start) // sps
    return mf[start:start + n_sym * sps:sps]


def nearest_symbols(rx: np.ndarray, kind: str) -> np.ndarray:
    pts = constellation(kind)
    idx = np.abs(rx[:, None] - pts[None, :]).argmin(axis=1)
    return pts[idx]


def evm_rms_percent(rx: np.ndarray, ref: np.ndarray) -> float:
    """RMS EVM in percent, both arrays symbol-spaced and gain-aligned."""
    rx = np.asarray(rx)
    ref = np.asarray(ref)
    n = min(len(rx), len(ref))
    rx, ref = rx[:n], ref[:n]
    err = rx - ref
    return float(np.sqrt(np.sum(np.abs(err) ** 2) / np.sum(np.abs(ref) ** 2)) * 100.0)


def align_gain(rx: np.ndarray, ref: np.ndarray) -> np.ndarray:
    """Least-squares complex gain that maps ref onto rx (measurement AGC)."""
    n = min(len(rx), len(ref))
    rx, ref = rx[:n], ref[:n]
    return rx * (np.vdot(rx, ref) / np.vdot(rx, rx))


def theoretical_evm_percent(snr_db: float, sps: int = 1) -> float:
    """Coherent-detection RMS EVM floor for a unit-power signal.

    ``snr_db`` is the per-sample SNR of the AWGN channel (the convention
    :func:`add_awgn` uses). With a matched filter the symbol-energy-to-noise
    ratio is ``sps`` times the sample SNR, so the ideal EVM is
    ``sqrt(N0/Es) = 1/sqrt(SNR_lin * sps)``. Verified against measurement in
    probe_siggen_selfcheck.py (ratio 0.99-1.00 over 10-30 dB at sps=8).
    """
    return 100.0 / np.sqrt(10 ** (snr_db / 10.0) * sps)


def realized_snr_db(clean: np.ndarray, noisy: np.ndarray) -> float:
    p_noise = float(np.mean(np.abs(noisy - clean) ** 2))
    p_sig = float(np.mean(np.abs(clean) ** 2))
    return 10 * np.log10(p_sig / p_noise)


def symbol_errors(rx: np.ndarray, ref: np.ndarray, kind: str) -> float:
    n = min(len(rx), len(ref))
    return float(np.mean(nearest_symbols(rx[:n], kind) != ref[:n]))
