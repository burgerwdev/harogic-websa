"""ITA2 asynchronous FSK for the bench (ITU-T S.1 alphabet, US figures).

One start bit, five data bits least-significant first, and 1.5 stop bits.
Symbol edges use one cumulative sample clock; phase remains continuous.
https://www.itu.int/rec/T-REC-S.1/en
"""
from __future__ import annotations

import numpy as np

LETTERS = '\0E\nA SIU\rDRJNFCKTZLWHYPQOBG\0MXV\0'
FIGURES = '\0' + '3\n- ' + "'87\r$4" + '\0' + ',!:(5")2#6019?&\0./;\0'
LTRS, FIGS = 31, 27


def codes(text: str) -> list[int]:
    if not text or len(text) > 256:
        raise ValueError('RTTY message must contain 1..256 characters')
    result = [LTRS, LTRS]
    figures = False
    for character in text.upper():
        if character == '\0':
            raise ValueError('RTTY message cannot contain NUL')
        current, other = (FIGURES, LETTERS) if figures else (LETTERS, FIGURES)
        if character not in current:
            if character not in other:
                raise ValueError(f'RTTY cannot encode {character!r}; use letters, digits or ITA2 punctuation')
            figures = not figures
            result.append(FIGS if figures else LTRS)
            current = FIGURES if figures else LETTERS
        result.append(current.index(character))
    return result


def source(text: str, baud: float = 45.45, shift: float = 170, low: float = 2125, reverse: bool = False):
    if not np.isfinite(baud) or baud not in (45.45, 50, 75, 100):
        raise ValueError('RTTY baud must be 45.45, 50, 75 or 100')
    if not np.isfinite(shift) or shift not in (170, 200, 425, 450, 850) or not 300 <= low <= 3300 - shift:
        raise ValueError('RTTY tones must fit 300..3300 Hz with a supported shift')
    rate = 48000
    bits = [(True, 30)]  # 15 idle mark bits allow tone/timing acquisition.
    for code in codes(text):
        bits.append((False, 2))
        bits.extend((bool(code & (1 << bit)), 2) for bit in range(5))
        bits.append((True, 3))
    bits.append((True, 30))
    half_bits = np.cumsum([0, *(length for _, length in bits)])
    edges = np.rint(half_bits * rate / (2 * baud)).astype(np.int64)
    mark, space = (low + shift, low) if reverse else (low, low + shift)
    tones = np.empty(int(edges[-1]), dtype=np.float64)
    for index, (is_mark, _) in enumerate(bits):
        tones[edges[index]:edges[index + 1]] = mark if is_mark else space
    phase = (2 * np.pi / rate) * np.cumsum(tones)
    samples = np.exp(1j * phase).astype(np.complex64)
    return samples, rate, len(samples) / rate, text.upper(), float(phase[-1])
