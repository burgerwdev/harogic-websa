"""Synthetic IQ for the vector/demod tests: known answers, no hardware.

Test-only on purpose: it builds signals with a *known* symbol rate, carrier offset,
timing phase, level and burst duty, so a measurement can be checked against the truth
rather than against another implementation. The pulse shape and the nominal grids come
from `web_sa.demod.digital` (the production code), so the tests cannot drift away from
the conventions the chain itself uses.

Power convention, same as the production modules: complex volts, 50 ohm, so a signal of
amplitude ``A`` carries ``A^2/50`` W and ``10*log10(A^2/50)+30`` dBm.
"""
from __future__ import annotations

import numpy as np

from web_sa.demod import digital as D


def dbm_of_amplitude(amplitude: float) -> float:
    """dBm of a complex signal whose samples have magnitude ``amplitude``."""
    return 10 * np.log10(amplitude ** 2 / 50.0) + 30.0


def tone(n: int, fs: float, freq_hz: float = 0.0, amplitude: float = 1e-3) -> np.ndarray:
    """Complex tone of a known amplitude (absolute volts)."""
    t = np.arange(n) / fs
    return amplitude * np.exp(2j * np.pi * freq_hz * t)


def noise(n: int, *, amplitude: float = 1.0, seed: int = 0) -> np.ndarray:
    """Circular complex Gaussian noise with the given per-sample RMS."""
    rng = np.random.default_rng(seed)
    x = rng.normal(size=n) + 1j * rng.normal(size=n)
    return x * (amplitude / np.sqrt(2.0))


def modulate(kind: str = 'qpsk', n_sym: int = 4096, sps: int = 16, rolloff: float = 0.35,
             *, amplitude: float | None = None, seed: int = 0) -> tuple:
    """RRC-shaped PSK/QAM baseband (one RRC, the receiver applies the matched one).

    Returns ``(iq, symbols)``. The nominal grid is unit RMS, so ``amplitude=None`` gives
    a unit-RMS signal; pass an amplitude in volts to place it at a known level.

    Symbols are **zero-inserted** before the pulse shape (holding each symbol for ``sps``
    samples instead adds a rectangular envelope and destroys the constellation); the full
    convolution is kept so no pulse is truncated at the edges, and the power is normalised
    on the steady-state core.
    """
    pts = D.nominal_points(kind)
    rng = np.random.default_rng(seed)
    sym = rng.choice(pts, size=int(n_sym))
    up = np.zeros(len(sym) * sps, dtype=complex)
    up[::sps] = sym
    iq = np.convolve(up, D.rrc(rolloff, sps))
    core = iq[D.SPAN * sps:len(iq) - D.SPAN * sps]
    iq = iq * (1.0 / np.sqrt(np.mean(np.abs(core) ** 2)))
    if amplitude is not None:
        iq = iq * amplitude
    return iq, sym


def add_awgn(iq: np.ndarray, snr_db: float, *, seed: int = 1) -> np.ndarray:
    """Add noise at a per-sample SNR (the convention the probes use)."""
    n = noise(len(iq), seed=seed)
    p_sig = float(np.mean(np.abs(iq) ** 2))
    p_noise = float(np.mean(np.abs(n) ** 2))
    return iq + n * np.sqrt(p_sig / (p_noise * 10 ** (snr_db / 10.0)))


def add_cfo(iq: np.ndarray, cfo_hz: float, fs: float) -> np.ndarray:
    """Rotate the block by a constant carrier offset."""
    return iq * np.exp(2j * np.pi * cfo_hz * np.arange(len(iq)) / fs)


def burst(n: int, block: int, *, duty: float = 0.5, on_amplitude: float = 1e-3,
          seed: int | None = None) -> np.ndarray:
    """Block-wise burst with a known duty cycle and an off-state at the noise floor.

    ``seed=None`` (the default) uses a *periodic* pattern, so the duty cycle is exact and
    a duty test measures the measurement, not the random draw. A seed gives the random
    on/off pattern a real capture would have, for tests that want that instead.
    """
    n_blocks = n // block
    if seed is None:
        period = max(2, int(round(1.0 / duty)))
        pattern = (np.arange(n_blocks) % period == 0).astype(float)
    else:
        rng = np.random.default_rng(seed)
        pattern = (rng.random(n_blocks) < duty).astype(float)
    env = np.repeat(pattern, block)[:n]
    return (env * on_amplitude) + noise(n, amplitude=on_amplitude * 1e-3, seed=1)
