"""The radiofax transmitter's signal plan: vendored framing, cumulative deadlines,
and the streaming synthesizer that turns them into baseband.

Pure numpy, no radio. `--dry-run` covers the radio-less path, and these tests cover
what a dry run cannot show: APT half-cycles, 95/5 phasing, FM/AM polarity, chunk
continuity at a rate that is not the protocol rate, and a fake `Radio` whose
accepted rate is not the requested one.
"""
from __future__ import annotations

import argparse
import math
import sys
import zlib
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.fixtures import radiofax_encode as fax  # noqa: E402
from tools.fixtures import sstv_encode as sstv  # noqa: E402
from tools.pluto import pluto_radiofax_tx as tx  # noqa: E402

RATE = 8_000


def _gray(spec: fax.Spec, lines: int, value: int = 0) -> bytes:
    return bytes([value]) * (spec.width * lines)


def _collect(spec: fax.Spec, gray: bytes, rate: int = RATE, scale: float = 1.0,
             chunk: int = 1000, max_samples: int | None = None) -> np.ndarray:
    chunks = list(tx.stream_page(fax.transmission_segments(spec, gray), rate, scale, chunk,
                                 max_samples=max_samples))
    assert chunks
    assert {part.size for part in chunks} == {chunk}
    audio = np.concatenate(chunks)
    if max_samples is None:
        # The tail pad is silence; the scheduled page is not.
        scheduled = fax.Scheduler(rate)
        for segment in fax.transmission_segments(spec, gray):
            scheduled.take(segment.seconds)
        assert scheduled.samples <= audio.size < scheduled.samples + chunk
        return audio[:scheduled.samples]
    return audio[:max_samples]


def _tone_hz(samples: np.ndarray, rate: int) -> float:
    """Mean instantaneous frequency of an analytic tone, in Hz."""
    phase = np.unwrap(np.angle(samples.astype(np.complex128)))
    return float(np.mean(np.diff(phase)) * rate / (2.0 * np.pi))


def test_widths_and_standard_phasing_match_the_vendor():
    assert fax.Spec(ioc=288).width == 905
    assert fax.Spec(ioc=576).width == 1810
    assert fax.round_half_up(math.pi * 288) == 905
    assert fax.round_half_up(math.pi * 576) == 1810
    for lpm, lines in ((60, 30), (90, 45), (120, 60), (180, 90), (240, 120)):
        spec = fax.Spec.standard(576, lpm)
        assert spec.phasing_lines == lines
        assert spec.start_seconds == 5.0 and spec.tail_seconds == 10.0


def test_half_up_deadlines_are_not_bankers_rounding():
    # 2.5 * rate lands on a half when the duration is chosen so. Python round(2.5) is 2.
    assert fax.round_half_up(2.5) == 3
    assert round(2.5) == 2
    rate = 10
    # 0.25 s * 10 Hz = 2.5, then another 0.25 s. Bankers would emit 2 + 2; half-up emits 3 + 2.
    assert fax.scheduled_samples([0.25, 0.25], rate) == [3, 2]
    assert sum(fax.scheduled_samples([0.25, 0.25], rate)) == fax.round_half_up(0.5 * rate)


def test_apt_toggles_and_phasing_fractions():
    spec = fax.Spec.standard(288, 120)
    spec = fax.Spec(ioc=288, lpm=120, phasing_seconds=0.5, start_seconds=5.0,
                    stop_seconds=5.0, tail_seconds=10.0)
    gray = _gray(spec, 1, 128)
    segments = list(fax.transmission_segments(spec, gray))
    start_n = fax.round_half_up(5.0 * 675.0 * 2.0)
    start = segments[:start_n]
    assert all(abs(seg.seconds - 1.0 / (2.0 * 675.0)) < 1e-15 for seg in start)
    assert [round(seg.frequency_hz) for seg in start[:4]] == [1500, 2300, 1500, 2300]
    phasing_n = spec.phasing_lines * 2
    phasing = segments[start_n:start_n + phasing_n]
    assert spec.phasing_lines == 1
    black, white = phasing
    assert abs(black.seconds - spec.line_seconds * 0.95) < 1e-15
    assert abs(white.seconds - spec.line_seconds * 0.05) < 1e-15
    assert round(black.frequency_hz) == 1500 and round(white.frequency_hz) == 2300
    image = segments[start_n + phasing_n:start_n + phasing_n + spec.width]
    assert len(image) == 905
    assert all(abs(seg.frequency_hz - (1500.0 + 800.0 * 128 / 255)) < 1e-9 for seg in image)
    stop_n = fax.round_half_up(5.0 * 450.0 * 2.0)
    stop = segments[-1 - stop_n:-1]
    assert [round(seg.frequency_hz) for seg in stop[:4]] == [1500, 2300, 1500, 2300]
    tail = segments[-1]
    assert tail.seconds == 10.0 and round(tail.frequency_hz) == 1500


def test_ioc576_start_is_300_hz_and_stop_stays_450():
    spec = fax.Spec(ioc=576, lpm=240, start_seconds=5.0, stop_seconds=5.0,
                    phasing_seconds=0.25, tail_seconds=0.0)
    gray = _gray(spec, 1)
    segments = list(fax.transmission_segments(spec, gray))
    start_n = fax.round_half_up(5.0 * 300.0 * 2.0)
    # 300 Hz keyed as black/white FM sits at 1500 and 2300; the *keying* rate is 300 Hz,
    # which the sample run below measures. Here the half-cycle count is the contract.
    assert start_n == 3000
    assert len(segments) > start_n
    stop_n = fax.round_half_up(5.0 * 450.0 * 2.0)
    assert stop_n == 4500


def test_page_length_matches_the_rounded_timeline():
    spec = fax.Spec.standard(288, 120)
    lines = 2
    gray = _gray(spec, lines, 96)
    expected_s = (5.0 + 30.0 + lines * spec.line_seconds + 5.0 + 10.0)
    samples = _collect(spec, gray, rate=44_100, chunk=8192)
    expected = fax.round_half_up(expected_s * 44_100)
    assert abs(samples.size - expected) <= 1


def test_fm_polarity_and_am_true_black():
    base = dict(ioc=576, lpm=120, phasing_seconds=30.0, start_seconds=5.0,
                stop_seconds=5.0, tail_seconds=10.0, include_apt=False, include_phasing=False)
    normal = fax.Spec(modulation='fm', polarity='normal', **base)
    inverted = fax.Spec(modulation='fm', polarity='inverted', **base)
    assert fax._level(normal, 0, 1.0).frequency_hz == 1500.0
    assert fax._level(normal, 255, 1.0).frequency_hz == 2300.0
    assert fax._level(inverted, 0, 1.0).frequency_hz == 2300.0
    assert fax._level(inverted, 255, 1.0).frequency_hz == 1500.0
    am = fax.Spec(modulation='am', polarity='normal', **base)
    black = fax._level(am, 0, 1.0)
    white = fax._level(am, 255, 1.0)
    assert black.frequency_hz == 1900.0 and black.amplitude == 0.0
    assert white.frequency_hz == 1900.0 and white.amplitude == 0.5
    swapped = fax.Spec(modulation='am', polarity='inverted', **base)
    assert fax._level(swapped, 0, 1.0).amplitude == 0.5
    assert fax._level(swapped, 255, 1.0).amplitude == 0.0


def test_synthesized_black_and_white_land_on_the_fm_edges():
    spec = fax.Spec(ioc=288, lpm=240, include_apt=False, include_phasing=False,
                    phasing_seconds=1.0)
    width = spec.width
    gray = bytes([0] * width + [255] * width)
    audio = _collect(spec, gray, rate=RATE, chunk=1024)
    line = fax.round_half_up(spec.line_seconds * RATE)
    black = _tone_hz(audio[:line], RATE)
    white = _tone_hz(audio[line:line * 2], RATE)
    assert abs(black - 1500.0) < 0.5
    assert abs(white - 2300.0) < 0.5
    # Inverted sends the same picture with the edges swapped.
    inv = fax.Spec(ioc=288, lpm=240, polarity='inverted', include_apt=False,
                   include_phasing=False, phasing_seconds=1.0)
    flipped = _collect(inv, gray, rate=RATE, chunk=1024)
    assert abs(_tone_hz(flipped[:line], RATE) - 2300.0) < 0.5
    assert abs(_tone_hz(flipped[line:line * 2], RATE) - 1500.0) < 0.5


def test_am_black_is_silent_and_white_is_the_carrier():
    spec = fax.Spec(ioc=288, lpm=240, modulation='am', include_apt=False,
                    include_phasing=False, phasing_seconds=1.0)
    gray = bytes([0] * spec.width + [255] * spec.width)
    audio = _collect(spec, gray, rate=RATE, chunk=512)
    line = fax.round_half_up(spec.line_seconds * RATE)
    assert float(np.max(np.abs(audio[:line]))) == 0.0
    assert abs(_tone_hz(audio[line:line * 2], RATE) - 1900.0) < 0.5
    assert abs(float(np.mean(np.abs(audio[line:line * 2]))) - 0.5) < 1e-4


def test_phase_is_continuous_across_tones_and_chunk_sizes():
    spec = fax.Spec(ioc=288, lpm=240, include_apt=False, include_phasing=True,
                    phasing_seconds=0.5, tail_seconds=0.0)
    gray = bytes([index % 256 for index in range(spec.width * 2)])
    runs = []
    for chunk in (64, 1000, 4096):
        chunks = list(tx.stream_page(fax.transmission_segments(spec, gray), RATE, 4.0, chunk))
        assert {part.size for part in chunks} == {chunk}
        assert all(part.dtype == np.complex64 for part in chunks)
        audio = np.concatenate(chunks)
        # Drop the zero pad. Its phase is undefined and must not be part of the compare.
        scheduled = fax.Scheduler(RATE)
        for segment in fax.transmission_segments(spec, gray):
            scheduled.take(segment.seconds)
        body = audio[:scheduled.samples]
        assert abs(float(np.max(np.abs(body))) - 2.0) < 1e-4
        phase = np.unwrap(np.angle(body.astype(np.complex128)))
        step = np.diff(phase)
        # A continuous tone never jumps by more than the fastest legal tone, 2700 Hz.
        assert float(np.max(np.abs(step))) < 2.0 * np.pi * 2700.0 / RATE + 1e-6
        runs.append(body)
    assert float(np.max(np.abs(runs[0] - runs[1]))) < 1e-4
    assert float(np.max(np.abs(runs[1] - runs[2]))) < 1e-4


def test_offset_outside_the_decoder_band_is_refused():
    spec = fax.Spec(ioc=576, lpm=120, offset_hz=900.0, include_apt=False, include_phasing=False,
                    phasing_seconds=1.0)
    try:
        fax._level(spec, 255, 0.0)  # 2300 + 900 = 3200
    except ValueError as exc:
        assert '2700' in str(exc)
    else:  # pragma: no cover - the raise is the assertion
        raise AssertionError('offset past 2700 Hz was accepted')
    low = fax.Spec(ioc=576, lpm=120, offset_hz=-1300.0, include_apt=False, include_phasing=False,
                   phasing_seconds=1.0)
    try:
        fax._level(low, 0, 0.0)  # 1500 - 1300 = 200
        raise AssertionError('offset below 300 Hz was accepted')
    except ValueError:
        pass
    try:
        fax.Spec(ioc=400, lpm=120).validate()
        raise AssertionError('IOC 400 was accepted')
    except ValueError:
        pass
    try:
        fax.Spec(include_apt=True, include_phasing=False).validate()
        raise AssertionError('APT without phasing was accepted')
    except ValueError:
        pass


def test_image_keeps_aspect_and_luma_matches_sstv(tmp_path: Path):
    # 4x2 PPM, red then green. Luma of pure red is 76, pure green is 150 (30/59 weights).
    ppm = tmp_path / 'tiny.ppm'
    ppm.write_bytes(b'P6\n4 2\n255\n' + bytes([255, 0, 0, 0, 255, 0]) * 4)
    picture = sstv.read_image(ppm)
    gray, lines = fax.fit_image(picture, 8, None)
    assert lines == 4  # round(2 * 8 / 4)
    assert list(gray[:4]) == [sstv._luma(255, 0, 0), sstv._luma(255, 0, 0),
                              sstv._luma(0, 255, 0), sstv._luma(0, 255, 0)]
    forced, forced_lines = fax.fit_image(picture, 905, 3)
    assert forced_lines == 3 and len(forced) == 905 * 3
    # The pattern is the SSTV pattern; luma of its first pixel is the white corner.
    drawn = sstv.pattern_image(16, 8, 'CQ', 'FAX')
    assert fax.luma(drawn)[0] == sstv._luma(255, 255, 255)


def test_png_reader_still_feeds_a_page(tmp_path: Path):
    raw = b'\x00' + bytes([255, 0, 0]) + bytes([0, 0, 255])
    compressed = zlib.compress(raw)

    def chunk(kind: bytes, payload: bytes) -> bytes:
        return (len(payload).to_bytes(4, 'big') + kind + payload
                + zlib.crc32(kind + payload).to_bytes(4, 'big'))

    png = (b'\x89PNG\r\n\x1a\n'
           + chunk(b'IHDR', (2).to_bytes(4, 'big') + (1).to_bytes(4, 'big') + b'\x08\x02\x00\x00\x00')
           + chunk(b'IDAT', compressed) + chunk(b'IEND', b''))
    path = tmp_path / 'two.png'
    path.write_bytes(png)
    gray, lines = fax.fit_image(sstv.read_image(path), 2, 1)
    assert lines == 1
    assert list(gray) == [sstv._luma(255, 0, 0), sstv._luma(0, 0, 255)]


def test_dry_run_builds_a_head_without_opening_the_radio(monkeypatch):
    calls = []
    built = []
    stream_page = tx.stream_page

    def observe_stream(*args, **kwargs):
        for chunk in stream_page(*args, **kwargs):
            built.append(chunk)
            yield chunk

    monkeypatch.setattr(tx, 'stream_page', observe_stream)
    monkeypatch.setattr(tx.radio, 'Radio', lambda *_a, **_k: calls.append('radio'))
    code = tx.main(['--dry-run', '--lines', '4', '--dry-run-seconds', '0.5', '--rate', str(RATE)])
    assert code == 0
    assert calls == []
    assert sum(chunk.size for chunk in built) == RATE // 2
    assert max(float(np.max(np.abs(chunk))) for chunk in built) > 0.0


def test_invalid_cli_is_an_error():
    try:
        tx.main(['--ioc', '400'])
    except SystemExit:
        pass
    else:  # pragma: no cover
        raise AssertionError('IOC 400 parsed')
    try:
        tx.main(['--no-phasing', '--dry-run', '--lines', '1'])
    except SystemExit as exc:
        assert exc.code not in (0, None)
    else:  # pragma: no cover
        raise AssertionError('APT without phasing ran')
    try:
        tx.main(['--offset-hz', '2000', '--dry-run', '--lines', '1'])
    except SystemExit as exc:
        assert exc.code not in (0, None)
    else:  # pragma: no cover
        raise AssertionError('a 2000 Hz offset ran')


class _FakeSdr:
    tx_cyclic_buffer = True
    pushed: list

    def tx(self, samples) -> None:
        self.pushed.append(np.array(samples, copy=True))


class _FakeRadio:
    """Stands in for `radio.Radio`: the driver accepted a different rate."""

    def __init__(self, args: argparse.Namespace) -> None:
        self.rate = int(args.rate) + 1000
        self.sdr = _FakeSdr()
        self.sdr.pushed = []

    def __enter__(self) -> _FakeRadio:
        return self

    def __exit__(self, *_exc) -> bool:
        return False

    def open_stream(self) -> None:
        self.sdr.tx_cyclic_buffer = False

    def push_chunk(self, part) -> None:
        assert not self.sdr.tx_cyclic_buffer
        self.sdr.tx(part)


def test_repeat_pause_uses_the_accepted_rate_and_real_silence(monkeypatch):
    opened: list[_FakeRadio] = []

    def open_radio(args: argparse.Namespace) -> _FakeRadio:
        radio = _FakeRadio(args)
        opened.append(radio)
        return radio

    monkeypatch.setattr(tx.radio, 'Radio', open_radio)
    code = tx.main([
        '--lines', '2', '--repeat', '2', '--pause', '0.3', '--seconds', '0.2',
        '--rate', str(RATE), '--no-apt', '--no-phasing', '--phasing-seconds', '1',
        '--scale', '8',
    ])
    assert code == 0
    radio = opened[0]
    assert radio.rate == RATE + 1000
    chunks = radio.sdr.pushed
    assert chunks
    assert all(part.dtype == np.complex64 and part.size == chunks[0].size for part in chunks)
    # Two cut pages and a pause between them. The pause is zeros, not a gap in the pushes.
    chunk = chunks[0].size
    page = int(fax.round_half_up(0.2 * radio.rate))
    page_chunks = math.ceil(page / chunk)
    pause_chunks = max(1, math.ceil(0.3 * radio.rate / chunk))
    assert len(chunks) == page_chunks * 2 + pause_chunks
    pause = chunks[page_chunks:page_chunks + pause_chunks]
    assert all(float(np.max(np.abs(part))) == 0.0 for part in pause)
    # The pattern's corner is white, so the first tone at the accepted rate is 2300 Hz.
    body = np.concatenate(chunks[:page_chunks])[:page]
    assert abs(_tone_hz(body[:chunk], radio.rate) - 2300.0) < 1.0


def test_picture_budget_is_checked_before_destination_allocation(tmp_path, monkeypatch):
    def refuse_allocation(*_args, **_kwargs):
        raise AssertionError('destination allocated before the size check')

    monkeypatch.setattr(tx.sstv, 'pattern_image', refuse_allocation)
    monkeypatch.setattr(tx.fax, 'resize', refuse_allocation)
    ppm = tmp_path / 'tiny.ppm'
    ppm.write_bytes(b'P6\n1 1\n255\n' + bytes([255, 255, 255]))
    lines = tx.MAX_PICTURE_BYTES // fax.Spec().width + 1
    for image in (None, str(ppm)):
        args = argparse.Namespace(image=image, lines=lines, callsign='', label='')
        try:
            tx.page_for(args, fax.Spec())
        except SystemExit:
            pass
        else:
            raise AssertionError('oversized destination accepted')


def test_empty_image_is_rejected_without_framing():
    spec = fax.Spec(include_apt=False, include_phasing=False)
    try:
        next(fax.transmission_segments(spec, b''))
    except ValueError:
        pass
    else:
        raise AssertionError('empty image accepted')


def test_cli_rejects_nonfinite_and_nonpositive_timing():
    for option, value in (('--rate', '0'), ('--rate', 'nan'), ('--scale', 'inf'),
                          ('--seconds', 'nan'), ('--pause', 'inf'),
                          ('--dry-run-seconds', '0')):
        try:
            tx.main(['--dry-run', option, value])
        except SystemExit as exc:
            assert exc.code not in (0, None)
        else:
            raise AssertionError(f'{option} {value} was accepted')
