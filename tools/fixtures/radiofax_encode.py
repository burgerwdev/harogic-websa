#!/usr/bin/env python3
"""Radiofax/WEFAX baseband schedule for the Pluto bench transmitter.

The schedule follows `FaxEncoder` in
`dsp/core/src/digital/radiofax/vendor/fax.rs`: APT start, 95/5 phasing,
image pixels, APT stop and a black tail. IOC 288 starts with 675 Hz
black/white toggles, IOC 576 with 300 Hz; stop toggles are 450 Hz.

FM maps black..white onto 1500..2300 Hz. AM uses a fixed 1900 Hz carrier
with amplitude 0..1, preserving true black. Polarity inverts either map.
Segments are lazy; the cumulative scheduler rounds positive deadlines
half up, matching Rust rather than Python's bankers' rounding. Picture
reading, resizing and pattern generation reuse the SSTV helpers.
"""
from __future__ import annotations

import math
from collections.abc import Iterator
from dataclasses import dataclass

from tools.fixtures.sstv_encode import Picture, resize

#: APT start frequency per IOC. The decoder recognises these two alternations.
START_HZ = {288: 675.0, 576: 300.0}
STOP_HZ = 450.0
FM_CENTER_HZ = 1900.0
FM_DEVIATION_HZ = 400.0
#: The receiver's radiofax audio window. A tone outside it never reaches the decoder.
AUDIO_LOW_HZ = 300.0
AUDIO_HIGH_HZ = 2700.0
AM_CARRIER_HZ = 1900.0
#: IOC values the vendored encoder knows, and the LPM values the product exposes.
VALID_IOC = (288, 576)
VALID_LPM = (60, 90, 120, 180, 240)
#: Width is `round(pi * IOC)`: 905 and 1810. Kept as literals so pi drift cannot move them.
IOC_WIDTH = {288: 905, 576: 1810}
#: Vendored bounds: phasing in 0.001..=600 s, the other framing times in 0..=600 s.
MAX_FRAMING_S = 600.0
MIN_PHASING_S = 0.001


def round_half_up(value: float) -> int:
    """Rust's `f64::round` for a non-negative deadline: half away from zero.

    `round(n + 0.5)` in Python returns the even integer, so 2.5 becomes 2.
    The fax scheduler rounds that to 3. Deadlines here only increase.
    """
    return math.floor(value + 0.5)


@dataclass(frozen=True)
class Spec:
    """One page's transmission parameters. `standard()` is the WMO framing."""

    ioc: int = 576
    lpm: int = 120
    modulation: str = 'fm'
    polarity: str = 'normal'
    phasing_seconds: float = 30.0
    start_seconds: float = 5.0
    stop_seconds: float = 5.0
    tail_seconds: float = 10.0
    include_apt: bool = True
    include_phasing: bool = True
    amplitude: float = 0.5
    offset_hz: float = 0.0

    @classmethod
    def standard(cls, ioc: int = 576, lpm: int = 120) -> Spec:
        """WMO framing: 5 s start, 30 s phasing, 5 s stop, 10 s black tail, FM."""
        return cls(ioc=ioc, lpm=lpm)

    def validate(self) -> None:
        if self.ioc not in VALID_IOC or self.lpm not in VALID_LPM:
            raise ValueError(f'radiofax IOC must be {VALID_IOC} and LPM {VALID_LPM}')
        if self.modulation not in ('fm', 'am') or self.polarity not in ('normal', 'inverted'):
            raise ValueError('radiofax modulation must be fm or am, polarity normal or inverted')
        if self.include_apt and not self.include_phasing:
            raise ValueError('APT framing requires phasing')
        if (not math.isfinite(self.phasing_seconds)
                or not MIN_PHASING_S <= self.phasing_seconds <= MAX_FRAMING_S):
            raise ValueError('radiofax phasing must be finite and in 0.001..=600 s')
        for name, value in (('start', self.start_seconds), ('stop', self.stop_seconds),
                            ('tail', self.tail_seconds)):
            if not math.isfinite(value) or not 0.0 <= value <= MAX_FRAMING_S:
                raise ValueError(f'radiofax {name} duration must be finite and in 0..=600 s')
        if not math.isfinite(self.amplitude) or not 0.0 <= self.amplitude <= 1.0:
            raise ValueError('radiofax amplitude must be in 0..=1')
        if not math.isfinite(self.offset_hz):
            raise ValueError('radiofax offset must be finite')

    @property
    def width(self) -> int:
        return IOC_WIDTH[self.ioc]

    @property
    def line_seconds(self) -> float:
        return 60.0 / self.lpm

    @property
    def phasing_lines(self) -> int:
        """`round(LPM * phasing_seconds / 60)`, the vendored `phasing_line_count`."""
        return round_half_up(self.lpm * self.phasing_seconds / 60.0)

    def apt_start_hz(self) -> float:
        return START_HZ[self.ioc]


@dataclass(frozen=True)
class Segment:
    """One constant tone. `seconds` is the ideal duration; the scheduler quantises it."""

    frequency_hz: float
    amplitude: float
    seconds: float


def luma(picture: Picture) -> bytes:
    """The integer luma SSTV uses for a gray scan: `(30R + 59G + 11B) / 100`."""
    rgb = picture.rgb
    return bytes((30 * rgb[index] + 59 * rgb[index + 1] + 11 * rgb[index + 2]) // 100
                 for index in range(0, len(rgb), 3))


def fit_image(picture: Picture, width: int, lines: int | None = None) -> tuple[bytes, int]:
    """Gray bytes at the IOC width.

    Without `lines`, the height keeps the source aspect (`round(height * width / source_width)`,
    at least one line). With `lines`, nearest-neighbour resize uses that height.
    """
    if lines is None:
        lines = max(1, round_half_up(picture.height * width / picture.width))
    if lines < 1:
        raise ValueError('radiofax line count must be positive')
    return luma(resize(picture, width, lines)), lines


def transmission_segments(spec: Spec, gray: bytes) -> Iterator[Segment]:
    """Yield a nonempty whole-line Gray8 page in protocol order."""
    spec.validate()
    width = spec.width
    if len(gray) % width != 0:
        raise ValueError('radiofax gray length is not a whole number of IOC lines')
    lines = len(gray) // width
    if lines < 1:
        raise ValueError('radiofax page needs at least one image line')
    if spec.include_apt:
        yield from _toggle(spec, spec.apt_start_hz(), spec.start_seconds)
    if spec.include_phasing:
        black = spec.line_seconds * 0.95
        white = spec.line_seconds * 0.05
        for _line in range(spec.phasing_lines):
            yield _level(spec, 0, black)
            yield _level(spec, 255, white)
    pixel_seconds = spec.line_seconds / width
    for value in gray:
        yield _level(spec, value, pixel_seconds)
    if spec.include_apt:
        yield from _toggle(spec, STOP_HZ, spec.stop_seconds)
        if spec.tail_seconds > 0.0:
            yield _level(spec, 0, spec.tail_seconds)


def _toggle(spec: Spec, frequency_hz: float, seconds: float) -> Iterator[Segment]:
    """Black/white half-cycles of a keyed tone. The count is `round(seconds * 2 * f)`."""
    total = round_half_up(seconds * frequency_hz * 2.0)
    if total == 0:
        return
    half = 1.0 / (2.0 * frequency_hz)
    for index in range(total):
        yield _level(spec, 0 if index % 2 == 0 else 255, half)


def _level(spec: Spec, value: int, seconds: float) -> Segment:
    normalized = value / 255.0
    if spec.modulation == 'fm':
        video = (1.0 - normalized) if spec.polarity == 'inverted' else normalized
        frequency = FM_CENTER_HZ - FM_DEVIATION_HZ + 2.0 * FM_DEVIATION_HZ * video
        amplitude = spec.amplitude
    else:
        video = (1.0 - normalized) if spec.polarity == 'inverted' else normalized
        frequency = AM_CARRIER_HZ
        amplitude = spec.amplitude * video
    frequency += spec.offset_hz
    if not AUDIO_LOW_HZ <= frequency <= AUDIO_HIGH_HZ:
        raise ValueError(f'radiofax tone {frequency:.1f} Hz is outside 300..2700 Hz')
    return Segment(frequency, amplitude, seconds)


class Scheduler:
    """Running sample deadlines for one page at one sample rate.

    `take` returns how many samples the next segment occupies. The sum of a page
    equals `round(total_seconds * rate)`, within the one-sample bound the vendored
    timeline test allows when the ideal seconds themselves do not add exactly.
    """

    def __init__(self, rate: int) -> None:
        if rate < 1:
            raise ValueError('radiofax sample rate must be positive')
        self.rate = rate
        self.deadline = 0.0
        self.scheduled = 0

    def take(self, seconds: float) -> int:
        self.deadline += seconds * self.rate
        rounded = round_half_up(self.deadline)
        count = rounded - self.scheduled
        if count < 0:
            raise ValueError('radiofax scheduler deadline moved backwards')
        self.scheduled = rounded
        return count

    @property
    def samples(self) -> int:
        return self.scheduled


def page_seconds(spec: Spec, lines: int) -> float:
    """Ideal duration of one page, before sample quantisation.

    APT half-cycles are counted with the same round the encoder uses, then turned
    back into seconds, so a start that is not an integer number of cycles has the
    length the toggles actually occupy.
    """
    spec.validate()
    if lines < 1:
        raise ValueError('radiofax line count must be positive')
    total = lines * spec.line_seconds
    if spec.include_phasing:
        total += spec.phasing_lines * spec.line_seconds
    if spec.include_apt:
        total += _toggle_seconds(spec.apt_start_hz(), spec.start_seconds)
        total += _toggle_seconds(STOP_HZ, spec.stop_seconds)
        total += spec.tail_seconds
    return total


def _toggle_seconds(frequency_hz: float, seconds: float) -> float:
    return round_half_up(seconds * frequency_hz * 2.0) / (2.0 * frequency_hz)


def scheduled_samples(seconds: list[float], rate: int) -> list[int]:
    """Sample count of each ideal duration, sharing one cumulative deadline."""
    scheduler = Scheduler(rate)
    return [scheduler.take(duration) for duration in seconds]
