#!/usr/bin/env python3
"""Transmit a radiofax (WEFAX) page from a PlutoSDR, for end-to-end receiver testing.

Prefer the unified entry: `python3 tools/pluto_tx.py radiofax [args]`
(`wefax` is the same tool).

The schedule follows the vendored `FaxEncoder` in
`dsp/core/src/digital/radiofax/vendor/fax.rs`: APT start, 95/5 phasing,
image pixels, APT stop and a black tail. The shared SSTV picture helpers
read PNG/PPM files and draw the default pattern.

Tone segments are pulled lazily and synthesized as upper-sideband complex
exponentials at the radio's accepted sample rate. At 60 LPM, 100 image
lines take 100 seconds before the 50 seconds of standard framing.
Every push has the same length; the final chunk is zero-padded. Pauses
between pages are explicit zero chunks. `--seconds 0` sends the whole page.
`--dry-run` builds at most two seconds by default, without opening a radio.

    python3 tools/pluto/pluto_radiofax_tx.py --dry-run
    python3 tools/pluto/pluto_radiofax_tx.py --ioc 576 --lpm 120
    python3 tools/pluto/pluto_radiofax_tx.py --image chart.png --lines 800
    python3 tools/pluto/pluto_radiofax_tx.py --list-modes
"""
from __future__ import annotations

import argparse
import math
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import pluto_radio as radio  # noqa: E402

from tools.fixtures import radiofax_encode as fax  # noqa: E402
from tools.fixtures import sstv_encode as sstv  # noqa: E402

#: Bench LO in Hz. The AD9363 transmits above ~325 MHz only.
DEFAULT_LO_HZ = 435e6
DEFAULT_CALLSIGN = 'CQ WE0BSA'
DEFAULT_LABEL = 'RADIOFAX TEST PATTERN'
#: How long one push is. Long enough that the host is not in the driver's way,
#: short enough that a page is never held whole.
CHUNK_S = 0.25
#: A dry run only has to prove the path. A default page is minutes long.
DRY_RUN_LIMIT_S = 2.0
#: Pattern height when the operator names neither a file nor `--lines`.
PATTERN_LINES = 128
#: Gray bytes, not pixels: 40 MiB is already far past a weather chart.
MAX_PICTURE_BYTES = 40 * 1024 * 1024
#: Helpful combinations, not every IOC/LPM pair. `--list-modes` prints these.
LISTED_MODES = (
    (576, 120, 'fm', 'WMO IOC 576 / 120 LPM, the usual HF weather chart'),
    (576, 60, 'fm', 'WMO IOC 576 / 60 LPM, twice as long'),
    (288, 120, 'fm', 'WMO IOC 288 / 120 LPM, half the width'),
    (576, 90, 'fm', 'WMO 90 LPM'),
    (576, 240, 'fm', 'WMO 240 LPM'),
    (576, 180, 'fm', '180 LPM, manual on this receiver'),
    (576, 120, 'am', 'AM subcarrier, black true zero'),
)


def page_for(args: argparse.Namespace, spec: fax.Spec) -> tuple[bytes, int]:
    """Fit a file to the IOC width, preserving aspect unless `--lines` is given."""
    source = sstv.read_image(Path(args.image)) if args.image else None
    lines = args.lines
    if lines is None:
        lines = (max(1, fax.round_half_up(source.height * spec.width / source.width))
                 if source is not None else PATTERN_LINES)
    if lines < 1:
        raise SystemExit('--lines must be at least 1')
    # Bound the destination before resize or pattern generation allocates it.
    picture_bytes = spec.width * lines
    if picture_bytes > MAX_PICTURE_BYTES:
        raise SystemExit(f'page is {picture_bytes / 1e6:.1f} MB of gray; '
                         f'the limit is {MAX_PICTURE_BYTES / 1e6:.0f} MB '
                         f'({spec.width} x {lines})')
    if source is not None:
        gray, lines = fax.fit_image(source, spec.width, lines)
        note = (f'{source.width}x{source.height} -> {spec.width}x{lines}'
                + (' (aspect)' if args.lines is None else ''))
        print(f'image          : {args.image} {note}')
    else:
        drawn = sstv.pattern_image(spec.width, lines, args.callsign, args.label)
        gray = fax.luma(drawn)
        print(f'image          : generated pattern {spec.width}x{lines} '
              f'(callsign {args.callsign!r})')
    return gray, lines


def stream_page(segments, rate: int, scale: float, chunk_samples: int,
                phase: float = 0.0, max_samples: int | None = None):
    """Yield complex64 chunks for a lazy segment iterator.

    Synthesis is at `rate` (the rate the radio actually accepted, not the rate
    asked for). Each segment's length comes from the cumulative rounded deadline,
    and the oscillator phase is the phase the previous sample left behind, so a
    chunk boundary is continuous. Every chunk has length `chunk_samples`; the
    last is zero-padded because `open_stream` refuses a new length. `max_samples`
    stops a dry run without pulling the rest of the page.
    """
    tau = 2.0 * np.pi
    scheduler = fax.Scheduler(rate)
    buffer = np.empty(chunk_samples, dtype=np.complex64)
    filled = 0
    produced = 0
    stop = max_samples if max_samples is not None else sys.maxsize
    for segment in segments:
        count = scheduler.take(segment.seconds)
        if produced + count > stop:
            count = stop - produced
        produced += count
        hz = segment.frequency_hz
        amp = segment.amplitude
        while count > 0:
            take = min(count, chunk_samples - filled)
            step = tau * hz / rate
            theta = phase + step * np.arange(take, dtype=np.float64)
            tone = np.exp(1j * theta) * amp
            buffer[filled:filled + take] = tone
            phase = (phase + step * take) % tau
            filled += take
            count -= take
            if filled == chunk_samples:
                yield (buffer * scale).astype(np.complex64, copy=False)
                filled = 0
        if produced >= stop:
            break
    if filled:
        buffer[filled:] = 0
        yield (buffer * scale).astype(np.complex64, copy=False)


def silence_chunks(rate: int, chunk_samples: int, seconds: float):
    """Yield fixed-size zero chunks covering at least `seconds` between pages."""
    if seconds <= 0.0:
        return
    need = max(1, int(np.ceil(seconds * rate / chunk_samples)))
    zero = np.zeros(chunk_samples, dtype=np.complex64)
    for _index in range(need):
        yield zero


def describe(spec: fax.Spec, args: argparse.Namespace, lines: int, duration_s: float) -> None:
    black, white = _extremes(spec)
    span = f'{min(black, white):.0f}..{max(black, white):.0f} Hz'
    if spec.modulation == 'am':
        span = f'{fax.AM_CARRIER_HZ + spec.offset_hz:.0f} Hz, amplitude 0..1'
    framing = []
    if spec.include_apt:
        framing.append(f'APT {spec.start_seconds:.3g}s/{spec.stop_seconds:.3g}s')
    if spec.include_phasing:
        framing.append(f'phasing {spec.phasing_lines} lines ({spec.phasing_seconds:.3g}s)')
    if spec.include_apt and spec.tail_seconds:
        framing.append(f'black tail {spec.tail_seconds:.3g}s')
    if not framing:
        framing.append('image only')
    print(f'mode           : IOC {spec.ioc} ({spec.width} px), {spec.lpm} LPM, '
          f'{spec.modulation.upper()} {spec.polarity}, {lines} image lines')
    print(f'framing        : {", ".join(framing)}')
    print(f'transmission   : {duration_s:.2f}s at {args.rate:.0f} Hz '
          f'(pushed in {CHUNK_S:.2f}s chunks; rebuilt at the radio rate)')
    print(f'tones          : {span}, offset {spec.offset_hz:+.1f} Hz, upper sideband')
    print(f'tx LO          : {args.lo / 1e6:.6f} MHz   (tune the analyzer here)')
    print(f'scale          : {args.scale:.0f} (tx() does not scale; 2**14 is the DAC range)')


def _extremes(spec: fax.Spec) -> tuple[float, float]:
    black = fax._level(spec, 0, 0.0).frequency_hz
    white = fax._level(spec, 255, 0.0).frequency_hz
    return black, white


def transmit(args: argparse.Namespace) -> int:
    spec = _spec(args)
    gray, lines = page_for(args, spec)
    duration_s = fax.page_seconds(spec, lines)
    limit_s = duration_s
    if args.seconds:
        limit_s = min(duration_s, float(args.seconds))
    describe(spec, args, lines, duration_s)
    if args.seconds:
        print(f'cut            : {limit_s:.2f}s of {duration_s:.2f}s')

    if args.dry_run:
        head_s = min(limit_s, args.dry_run_seconds)
        head_samples = int(fax.round_half_up(head_s * args.rate)) if head_s > 0 else 0
        built = 0
        peak = 0.0
        chunk_samples = max(1, int(args.rate * CHUNK_S))
        for chunk in stream_page(fax.transmission_segments(spec, gray), int(args.rate),
                                 args.scale, chunk_samples, max_samples=head_samples):
            built += int(chunk.size)
            peak = max(peak, float(np.max(np.abs(chunk))))
        print(f'\n--dry-run: built {built / args.rate:.2f}s of baseband '
              f'({built} samples, peak {peak:.0f} of {args.scale:.0f}); radio untouched.')
        if head_s + 1e-9 < duration_s:
            print(f'           (the whole page is {duration_s:.2f}s; '
                  f'--dry-run-seconds raises the build limit)')
        return 0

    sent = 0
    with radio.Radio(args) as tx:
        chunk_samples = max(1024, int(tx.rate * CHUNK_S))
        print(f'\nactual rate    : {tx.rate} Hz (driver may differ from --rate)')
        tx.open_stream()
        try:
            while args.repeat == 0 or sent < args.repeat:
                sent += 1
                started = time.time()
                which = '' if args.repeat == 0 else f'/{args.repeat}'
                print(f'  page {sent}{which}  '
                      f'{time.strftime("%H:%M:%S", time.gmtime(started))} UTC  '
                      f'IOC {spec.ioc}/{spec.lpm} LPM {limit_s:.1f}s', flush=True)
                pushed = 0
                budget = None if limit_s + 1e-12 >= duration_s else int(
                    fax.round_half_up(limit_s * tx.rate))
                for chunk in stream_page(fax.transmission_segments(spec, gray), tx.rate,
                                         args.scale, chunk_samples, max_samples=budget):
                    tx.push_chunk(chunk)
                    pushed += 1
                elapsed = time.time() - started
                print(f'  done in {elapsed:.1f}s ({pushed} chunks)', flush=True)
                if args.pause > 0 and (args.repeat == 0 or sent < args.repeat):
                    for chunk in silence_chunks(tx.rate, chunk_samples, args.pause):
                        tx.push_chunk(chunk)
                        pushed += 1
                    print(f'  pause {args.pause:.1f}s ({pushed} chunks total)', flush=True)
        except KeyboardInterrupt:
            print('\ninterrupted')
    return 0


def _spec(args: argparse.Namespace) -> fax.Spec:
    spec = fax.Spec(
        ioc=args.ioc, lpm=args.lpm, modulation=args.modulation, polarity=args.polarity,
        phasing_seconds=args.phasing_seconds, start_seconds=args.start_seconds,
        stop_seconds=args.stop_seconds, tail_seconds=args.tail_seconds,
        include_apt=not args.no_apt, include_phasing=not args.no_phasing,
        offset_hz=args.offset_hz,
    )
    try:
        spec.validate()
    except ValueError as exc:
        raise SystemExit(str(exc)) from None
    # The band check is per tone, so a bad offset fails here, before any synthesis.
    try:
        fax._level(spec, 0, 0.0)
        fax._level(spec, 255, 0.0)
    except ValueError as exc:
        raise SystemExit(str(exc)) from None
    return spec


def list_modes() -> int:
    print(f'{"IOC":>4}  {"LPM":>4}  {"mod":<3}  {"px":>4}  lines  note')
    for ioc, lpm, modulation, note in LISTED_MODES:
        spec = fax.Spec(ioc=ioc, lpm=lpm, modulation=modulation)
        duration = fax.page_seconds(spec, PATTERN_LINES)
        print(f'{ioc:4d}  {lpm:4d}  {modulation:<3}  {spec.width:4d}  '
              f'{PATTERN_LINES:5d}  {duration:7.1f}s  {note}')
    print('default pattern is '
          f'{PATTERN_LINES} lines; --lines or --image set the page. '
          'APT 5 s, phasing 30 s, stop 5 s, black tail 10 s.')
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--ioc', type=int, default=576, choices=fax.VALID_IOC,
                        help='index of cooperation: 576 (1810 px) or 288 (905 px)')
    parser.add_argument('--lpm', type=int, default=120, choices=fax.VALID_LPM,
                        help='lines per minute (180 is manual on this receiver)')
    parser.add_argument('--modulation', default='fm', choices=('fm', 'am'),
                        help='FM 1900+/-400 Hz, or AM at 1900 Hz (black amplitude 0)')
    parser.add_argument('--polarity', default='normal', choices=('normal', 'inverted'),
                        help='normal: black low / white high; inverted swaps them')
    parser.add_argument('--offset-hz', type=float, default=0.0,
                        help='shift every tone, still inside 300..2700 Hz')
    parser.add_argument('--phasing-seconds', type=float, default=30.0,
                        help='phasing duration; the line count is rounded')
    parser.add_argument('--start-seconds', type=float, default=5.0, help='APT start duration')
    parser.add_argument('--stop-seconds', type=float, default=5.0, help='APT stop duration')
    parser.add_argument('--tail-seconds', type=float, default=10.0,
                        help='black tail after APT stop (omitted without APT)')
    parser.add_argument('--no-apt', action='store_true', help='omit APT start, stop and the tail')
    parser.add_argument('--no-phasing', action='store_true',
                        help='omit phasing (APT then requires it, so this also needs --no-apt)')
    parser.add_argument('--list-modes', action='store_true', help='print the usual IOC/LPM combinations')
    parser.add_argument('--image', help='send this PNG or PPM (P6) instead of the pattern')
    parser.add_argument('--lines', type=int, default=None,
                        help='image lines; a file keeps its aspect when this is omitted')
    parser.add_argument('--callsign', default=DEFAULT_CALLSIGN, help='text drawn into the pattern')
    parser.add_argument('--label', default=DEFAULT_LABEL, help='label drawn into the pattern')
    parser.add_argument('--seconds', type=float, default=0.0,
                        help='cut the page to this many seconds (0 = the whole page)')
    parser.add_argument('--repeat', type=int, default=1,
                        help='how many pages to send (0 = until interrupted)')
    parser.add_argument('--pause', type=float, default=5.0,
                        help='seconds of pushed silence between pages')
    parser.add_argument('--dry-run-seconds', type=float, default=DRY_RUN_LIMIT_S,
                        help='how much baseband a --dry-run synthesizes (default 2)')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-10.0,
                         dry_run_help='encode and print the plan without touching the radio')
    args = parser.parse_args(argv)
    if args.list_modes:
        return list_modes()
    if args.repeat < 0:
        parser.error('--repeat cannot be negative')
    if args.lines is not None and args.lines < 1:
        parser.error('--lines must be at least 1')
    for name in ('seconds', 'pause'):
        value = getattr(args, name)
        if not math.isfinite(value) or value < 0:
            parser.error(f'--{name} must be finite and nonnegative')
    for name in ('rate', 'scale', 'lo', 'dry_run_seconds'):
        value = getattr(args, name)
        if not math.isfinite(value) or value <= 0:
            parser.error(f'--{name.replace("_", "-")} must be finite and positive')
    if int(args.rate) < 1:
        parser.error('--rate must be at least 1 Hz')
    for name in ('gain', 'xo_ppm'):
        if not math.isfinite(getattr(args, name)):
            parser.error(f'--{name.replace("_", "-")} must be finite')
    return transmit(args)


if __name__ == '__main__':
    raise SystemExit(main())
