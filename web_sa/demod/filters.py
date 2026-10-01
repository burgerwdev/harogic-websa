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


def one_pole_iir(x: np.ndarray, alpha: float, prev: float,
                 chunk: int = 256) -> tuple[np.ndarray, float]:
    """Streaming one-pole recursion ``y[n] = alpha*y[n-1] + x[n]``.

    Vectorised with a chunked cumulative sum. The chunk length is bounded so the
    per-chunk weight ratio cannot overflow: a fast filter (e.g. 50 us de-emphasis)
    would otherwise blow up over a whole block. Replaces per-sample Python loops,
    which dominated the SDR demodulator cost.
    """
    x = np.asarray(x, dtype=np.float64)
    if x.size == 0:
        return x, float(prev)
    alpha = float(alpha)
    if not (0.0 < alpha < 1.0):
        return x, float(x[-1])
    decay = -np.log(alpha)
    chunk = max(1, min(int(chunk), int(max(1.0, 2.0 / decay))))
    out = np.empty_like(x)
    y = float(prev)
    for start in range(0, x.size, chunk):
        seg = x[start:start + chunk]
        w = alpha ** np.arange(seg.size, dtype=np.float64)
        seg_out = w * (y + np.cumsum(seg / w))
        out[start:start + seg.size] = seg_out
        y = float(seg_out[-1])
    return out, y


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
    """RMS AGC with independent attack/release, a silence gate and a hold input.

    ``hold=True`` applies the current gain without adapting. The SDR path holds the AGC
    while it is discarding a reconfiguration transient, so the gain cannot wind up on
    audio that is never published (that wound-up gain used to be applied to the first real
    audio block, producing an audible burst that then decayed). Near-silence below
    ``silence_floor`` likewise never raises the gain.
    """

    def __init__(self, target=0.1, attack=0.02, release=0.002, max_gain=1e4,
                 silence_floor=1e-4, ceiling=0.95):
        self.target = float(target)
        self.attack = float(attack)
        self.release = float(release)
        self.max_gain = float(max_gain)
        self.silence_floor = float(silence_floor)
        # Peak ceiling. An RMS target alone cannot bound the peak: FM broadcast audio has a
        # crest factor of 4-5, so a 0.2 RMS target still clipped on loud passages (measured
        # on a real station: 18 clipped frames in 2 s in WFM, 5 in AM).
        #
        # It is applied as a limiter *gain* (instant down, slow up) rather than by scaling each
        # block by its own peak. Scaling per block is what a peaky signal hears as a level: on a
        # noise floor the crest factor varies block to block, so the level was modulated by up to
        # 2.2x (measured on AM noise) with a step at every block boundary - the periodic, level-
        # dependent sound a listener hears on a quiet frequency and loses as soon as a station
        # arrives. The limiter's gain holds a steady value on steady input, so the level is steady.
        self.ceiling = float(ceiling)
        self.limit = 1.0
        self.gain = 1.0
        self._primed = False

    def reset(self) -> None:
        self.gain = 1.0
        self.limit = 1.0
        self._primed = False

    def process(self, x: np.ndarray, hold: bool = False) -> np.ndarray:
        x = np.asarray(x, dtype=np.float32)
        if x.size == 0:
            return x
        gain_before = self.gain
        if not hold:
            rms = float(np.sqrt(np.mean(x.astype(np.float64) ** 2) + 1e-20))
            if rms >= self.silence_floor:
                desired = min(self.max_gain, self.target / max(rms, 1e-9))
                # Jump straight to the safe gain when the current one would push this block
                # past full scale, and initialise from the first block after a (re)configure.
                # Ramping instead (attack is only 20% per block) left the signal at unity gain
                # for tens of blocks, so the FM discriminator output - which is in Hz, up to
                # +/-fs/2 - came out tens of times over full scale and was hard-clipped: the
                # burst heard after every reconfigure/tune. Scaling is deployment-dependent
                # (Hz for FM, arbitrary DDC units for AM), so the AGC must not assume gain 1
                # is anywhere near correct.
                if (not self._primed) or rms * self.gain > 1.0:
                    self.gain = desired
                    # A jump is not ramped: the first block after a configure must already be at the
                    # right level (ramping it left FM clipped for tens of blocks), and the clip guard
                    # must apply now, not at the end of the block.
                    gain_before = desired
                    self._primed = True
                else:
                    coef = self.attack if desired < self.gain else self.release
                    self.gain += (desired - self.gain) * coef
        # The gain is ramped across the block instead of stepping at its boundary. The AGC adapts once
        # per block and a block is 19.65 ms at the DDC rate - 50.9 Hz - so a stepped gain
        # amplitude-modulates everything at ~51 Hz and its harmonics. On a noise floor that is a
        # pulsing, buzzy tone (the ear integrates a narrowband component, so it survives the level
        # being turned right down), it moves as the DDC rate drifts, and a station masks it: the
        # "periodic sound, period not fixed, worst on a quiet frequency" a listener reports.
        y = x * np.linspace(gain_before, self.gain, x.size, dtype=np.float64).astype(np.float32)
        if self.ceiling > 0.0 and y.size:
            peak = float(np.max(np.abs(y)))
            wanted = self.ceiling / peak if peak > self.ceiling else 1.0
            before = self.limit
            # Instant down, slow up: the release is the one the gain already uses, so the level
            # recovers in a few hundred milliseconds instead of tracking each block's crest factor.
            if wanted < self.limit:
                self.limit = wanted
            else:
                self.limit += (wanted - self.limit) * self.release
            if self.limit < 1.0 or before < 1.0:
                # Ramped for the same reason the AGC's gain is: a constant gain per block is amplitude
                # modulation at the block rate, and a block is 960 output samples = 20.00 ms = exactly
                # 50 Hz (with a 100 Hz harmonic). The limiter engages constantly on a noise floor, so
                # that step was a 50 Hz modulation of the noise - the periodic sound heard on a quiet
                # frequency. Measured: +27 dB envelope component at 50.0 Hz before this ramp.
                y = y * np.linspace(before, self.limit, y.size, dtype=np.float64).astype(np.float32)
                peak_after = float(np.max(np.abs(y)))
                if peak_after > self.ceiling:
                    # The ramp starts at the previous block's gain; a block needing more reduction than
                    # that applies it to the whole block at once (rare, and better than a clip).
                    y = y * np.float32(self.ceiling / peak_after)
        return y

    def level(self) -> float:
        """The gain the last block came out with (the AGC gain times the limiter's), for tests."""
        return self.gain * self.limit
