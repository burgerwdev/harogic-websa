#!/usr/bin/env python3
"""Transmit a slow-scan television picture from a PlutoSDR, for end-to-end receiver testing.

Prefer the unified entry: `python3 tools/pluto_tx.py sstv [args]`.

This is a bench tool, not product code: it exists so the WebSA SDR path can be driven with a *known*
picture over the air, and so the decoder can be judged against something with visible structure
(colour bars, a grey ramp, a border, a callsign).

The signal generator is not here. The mode timing tables and the encoder are
`tools/fixtures/sstv_encode.py`, whose tables are ported from the vendored Rust tree the receiver
decodes with (`tools/fixtures/port_sstv_modes.py`), so the transmitter and the decoder cannot drift
apart. This file adds the radio side only.

Two things shape that side:

  1. **The audio is a sequence of tones, so the analytic signal is a sequence of complex
     exponentials.** SSTV carries a picture as 1500..2300 Hz tones (1200 Hz sync) on a single
     sideband, so synthesising `exp(j2πft)` per tone *is* the upper-sideband signal — no Hilbert
     transform, no image rejection to get wrong. The phase runs continuously across tones, so the
     spectrum stays clean at a pixel boundary.
  2. **A transmission is minutes long, so it streams.** PD-290 is 406 s of image; at the AD9363's
     521 kHz floor that is 212 M complex samples, about 1.7 GB. The buffer is therefore built in
     chunks and pushed one at a time, not handed to the driver as one cyclic buffer the way the FT8
     tool does.

Two hardware facts, measured on the bench ADALM-PLUTO (AD9363), apply here as they do to the other
tools: `tx()` casts the float array straight to int16 with no scaling (the baseband must be scaled
into the DAC's range, default `--scale 2**14`), and the TX sample-rate floor is 521 kHz.

    python3 tools/pluto/pluto_sstv_tx.py --dry-run                       # plan + head, no radio
    python3 tools/pluto/pluto_sstv_tx.py --mode 'Martin M1'              # one picture
    python3 tools/pluto/pluto_sstv_tx.py --image poster.png --repeat 3
    python3 tools/pluto/pluto_sstv_tx.py --list-modes

The picture defaults to a generated test pattern carrying `--callsign`; `--image` sends a PNG or PPM
file instead (resized, nearest neighbour, to the mode's own resolution).
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'fixtures'))

import pluto_radio as radio  # noqa: E402
import sstv_encode as sstv  # noqa: E402

#: Bench LO in Hz. The AD9363 transmits above ~325 MHz only.
DEFAULT_LO_HZ = 435e6
#: Default picture label, drawn into the generated pattern.
DEFAULT_CALLSIGN = 'CQ WE0BSA'
DEFAULT_LABEL = 'SSTV TEST PATTERN'
#: How long one push is. Long enough that the host is not in the driver's way, short enough that the
#: whole picture is never held in memory.
CHUNK_S = 0.25
#: Refuse to synthesize more than this in a dry run (the point of --dry-run is a quick check). The
#: default mode, Robot 36, is 36 s, so a plain `--dry-run` builds the whole transmission.
DRY_RUN_LIMIT_S = 40.0


def picture_for(args: argparse.Namespace, mode: dict) -> tuple[bytes, int, int]:
    """The picture to send: the operator's file, or the generated test pattern."""
    if args.image:
        source = sstv.read_image(Path(args.image))
        fitted = sstv.fit(source, mode)
        print(f'image          : {args.image} {source.width}x{source.height} '
              f'-> {fitted.width}x{fitted.height} ({mode["display"]})')
        return fitted.bytes(), fitted.width, fitted.height
    drawn = sstv.pattern_image(mode['width'], mode['height'], args.callsign, args.label)
    print(f'image          : generated pattern {drawn.width}x{drawn.height} '
          f'(callsign {args.callsign!r})')
    return drawn.bytes(), drawn.width, drawn.height


def stream_tones(tones: list[tuple[int, int]], rate: int, scale: float, chunk_samples: int,
                 phase: float = 0.0, carry: int = 0):
    """Yield complex64 baseband chunks for a tone sequence, `chunk_samples` at a time.

    The phase and the sample-count carry run across chunks, so a chunk boundary is inaudible: the
    tone is continuous, and a tone shorter than one chunk does not lose its fraction of a sample.
    Every yielded chunk is exactly `chunk_samples` long, the last one padded with silence, because the
    driver's one-shot `tx()` requires one length for the whole session.
    """
    buffer = np.empty(chunk_samples, dtype=np.complex64)
    filled = 0
    for hz, ns in tones:
        numerator = ns * rate + carry
        count = numerator // 1_000_000_000
        carry = numerator % 1_000_000_000
        while count > 0:
            take = min(count, chunk_samples - filled)
            step = 2.0 * np.pi * hz / rate
            buffer[filled:filled + take] = np.exp(
                1j * (phase + step * np.arange(take, dtype=np.float64))
            ).astype(np.complex64)
            phase = (phase + step * take) % (2.0 * np.pi)
            filled += take
            count -= take
            if filled == chunk_samples:
                yield (buffer * scale).astype(np.complex64)
                filled = 0
    if filled:
        # The tail is padded to a whole chunk: pyadi-iio allocates the TX buffer on the first push and
        # refuses a different length afterwards ("Cannot change buffer length on the fly"), so a short
        # final chunk ends the transmission with an exception. The padding is silence either way.
        buffer[filled:] = 0
        yield (buffer * scale).astype(np.complex64)


def describe(mode: dict, args: argparse.Namespace, duration_s: float) -> None:
    samples = int(duration_s * args.rate)
    print(f'mode           : {mode["display"]} (VIS {mode["vis"]}, '
          f'{mode["width"]}x{mode["height"]}, {mode["color"]})')
    print(f'transmission   : {duration_s:.2f}s -> {samples} complex samples at {args.rate:.0f} Hz '
          f'({samples * 8 / 1e6:.0f} MB if built whole; pushed in {CHUNK_S:.2f}s chunks)')
    print('tones          : 1200 Hz sync, 1500 Hz black .. 2300 Hz white, upper sideband')
    print(f'tx LO          : {args.lo / 1e6:.6f} MHz   (tune the analyzer here)')
    print(f'RF span        : {(args.lo + sstv.BLACK_HZ) / 1e6:.6f} .. '
          f'{(args.lo + sstv.WHITE_HZ) / 1e6:.6f} MHz, sync at {(args.lo + sstv.SYNC_HZ) / 1e6:.6f} MHz')
    print(f'scale          : {args.scale:.0f} (tx() does not scale; 2**14 is the DAC range)')


def transmit(args: argparse.Namespace) -> int:
    mode = sstv.find_mode(args.mode)
    rgb, width, height = picture_for(args, mode)
    tones = sstv.transmission_tones(mode, rgb, width, height)
    total_ns = sum(ns for _, ns in tones)
    duration_s = total_ns / 1e9
    if args.seconds:
        duration_s = min(duration_s, float(args.seconds))
        tones = _truncate(tones, int(duration_s * 1e9))
    describe(mode, args, duration_s)

    if args.dry_run:
        # Build a head of the baseband so the synthesis path is exercised, without spending minutes on
        # a 406 s mode. The limit covers the shortest modes whole, so a plain `--dry-run` really does
        # build Robot 36's transmission; a longer mode is planned in full and built part way, and says
        # so.
        limit = min(duration_s, args.dry_run_seconds)
        head = _truncate(tones, int(limit * 1e9))
        built = 0
        peak = 0.0
        for chunk in stream_tones(head, int(args.rate), args.scale, int(args.rate * CHUNK_S)):
            built += chunk.size
            peak = max(peak, float(np.max(np.abs(chunk))))
        print(f'\n--dry-run: built {built / args.rate:.2f}s of baseband '
              f'({built} samples, peak {peak:.0f} of {args.scale:.0f}); radio untouched.')
        if limit + 1e-9 < duration_s:
            print(f'           (the whole transmission is {duration_s:.2f}s; '
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
                print(f'  picture {sent}{"" if args.repeat == 0 else f"/{args.repeat}"}  '
                      f'{time.strftime("%H:%M:%S", time.gmtime(started))} UTC  '
                      f'{mode["display"]} {duration_s:.1f}s', flush=True)
                pushed = 0
                for chunk in stream_tones(tones, tx.rate, args.scale, chunk_samples):
                    tx.push_chunk(chunk)
                    pushed += 1
                elapsed = time.time() - started
                print(f'  done in {elapsed:.1f}s ({pushed} chunks)', flush=True)
                if args.pause > 0 and (args.repeat == 0 or sent < args.repeat):
                    time.sleep(args.pause)
        except KeyboardInterrupt:
            print('\ninterrupted')
    return 0


def _truncate(tones: list[tuple[int, int]], max_ns: int) -> list[tuple[int, int]]:
    """The first `max_ns` of a tone sequence (a tone is cut, not dropped)."""
    out: list[tuple[int, int]] = []
    used = 0
    for hz, ns in tones:
        if used >= max_ns:
            break
        take = min(ns, max_ns - used)
        out.append((hz, take))
        used += take
    return out


def list_modes() -> int:
    for mode in sstv.load_modes():
        tones = sstv.image_tones(mode, sstv.pattern_image(mode['width'], mode['height']).bytes(),
                                 mode['width'], mode['height'])
        ns = sum(duration for _, duration in sstv.header_tones(mode)) + sum(duration for _, duration in tones)
        print(f'{mode["index"]:2d}  {mode["display"]:<15} VIS {mode["vis"]:3d}  '
              f'{mode["width"]}x{mode["height"]:<4} {ns / 1e9:7.2f}s')
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--mode', default='Robot 36',
                        help='SSTV mode by name, table key, VIS code or index (see --list-modes)')
    parser.add_argument('--list-modes', action='store_true', help='list the modes and their lengths')
    parser.add_argument('--image', help='send this PNG or PPM (P6) file instead of the pattern')
    parser.add_argument('--callsign', default=DEFAULT_CALLSIGN,
                        help='text drawn into the generated pattern')
    parser.add_argument('--label', default=DEFAULT_LABEL, help='label drawn into the generated pattern')
    parser.add_argument('--seconds', type=float, default=0.0,
                        help='cut the transmission to this many seconds (0 = the whole picture)')
    parser.add_argument('--repeat', type=int, default=1,
                        help='how many pictures to send (0 = until interrupted)')
    parser.add_argument('--pause', type=float, default=5.0,
                        help='seconds of silence between pictures')
    parser.add_argument('--dry-run-seconds', type=float, default=DRY_RUN_LIMIT_S,
                        help='how much baseband a --dry-run synthesizes')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-10.0,
                         dry_run_help='encode and print the plan without touching the radio')
    args = parser.parse_args(argv)
    if args.list_modes:
        return list_modes()
    if args.repeat < 0:
        parser.error('--repeat cannot be negative')
    return transmit(args)


if __name__ == '__main__':
    raise SystemExit(main())
