#!/usr/bin/env python3
"""Transmit a CW (Morse) test signal from a PlutoSDR, for checking the CW decoder on the bench.

The point is the same as `pluto_ft8_tx.py`: a *known* over-the-air signal, so what the analyzer
shows can be compared against what the decoder reports. The waveform is the simplest one there is -
the carrier, keyed on and off with Morse timing - and the receiver's CW demodulator turns it into
the beat note the operator hears and the decoder reads.

Two properties decide whether the decode works, and both are choices here:

  * **the carrier sits one Pitch above the dial** (`+700 Hz` by default, exactly like the FT8
    tool's `+1500`): the CW demodulator selects a narrow band *at the Pitch* above zero IF, so the
    operator tunes to `--lo` and hears the sidetone at their Pitch setting. A carrier at the dial
    itself (zero beat) is deliberately not demodulated - that is where the receiver's DC
    cancellation and LO leakage live. Tune the analyzer to `--lo` and leave its Pitch at the tool's
    `--pitch`.
  * **the silence between repeats is long enough to close a line** (10 dots): the decoder ends a
    line on a long gap, so the window shows one timestamped line per transmission instead of every
    repeat running together.

Morse timing is the ITU one (a dash is 3 dots, 1 dot between elements, 3 between characters, 7
between words), keyed with a 5 ms raised-cosine edge so the keying has no clicks. The message
repeats every `--cycle` seconds (15 s by default), UTC-aligned like the FT8 script, and the
hardware facts (DAC scaling, the AD9361 crystal trim, the TX sample-rate floor) are shared with it.

    python3 tools/pluto_cw_tx.py --dry-run                  # key + plan, no radio
    python3 tools/pluto_cw_tx.py 'CQ CQ DE N0CALL'          # the message is the argument
    python3 tools/pluto_cw_tx.py --message 'TEST DE N0CALL' --wpm 25 --gain -20
    python3 tools/pluto_cw_tx.py --pitch 1000               # match the analyzer's Pitch control

On the analyzer: SDR mode, Demod = CW, Pitch = 700 (the default), tune to `--lo`, and open the
decode window (Decode -> On). Raise `--gain` until the beat note stands well out of the noise:
the same link budget as the FT8 script applies (-70 dB is a marginal link on a bench setup, -40 dB
a comfortable one).
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

import numpy as np

# The FT8 transmitter owns the hardware lessons (DAC scaling, the crystal trim, the sample-rate
# floor, the signal handling that restores the crystal on exit); this tool reuses them.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_ft8_tx as ft8tx  # noqa: E402

#: The message the fake backend keys too, so the bench and the e2e expect the same text.
DEFAULT_MESSAGE = 'TEST DE N0CALL'
DEFAULT_WPM = 20.0
DEFAULT_CYCLE_S = 15.0
#: Silence before the message (the decoder's level rails settle on the noise) and after it (the
#: line-closing gap, in dots).
LEAD_IN_S = 0.6
TAIL_DOTS = 10
EDGE_MS = 5.0

#: ITU Morse. Letters, digits and the punctuation an operator actually sends.
MORSE: dict[str, str] = {
    'A': '.-', 'B': '-...', 'C': '-.-.', 'D': '-..', 'E': '.', 'F': '..-.', 'G': '--.',
    'H': '....', 'I': '..', 'J': '.---', 'K': '-.-', 'L': '.-..', 'M': '--', 'N': '-.',
    'O': '---', 'P': '.--.', 'Q': '--.-', 'R': '.-.', 'S': '...', 'T': '-', 'U': '..-',
    'V': '...-', 'W': '.--', 'X': '-..-', 'Y': '-.--', 'Z': '--..',
    '0': '-----', '1': '.----', '2': '..---', '3': '...--', '4': '....-',
    '5': '.....', '6': '-....', '7': '--...', '8': '---..', '9': '----.',
    '.': '.-.-.-', ',': '--..--', '?': '..--..', '/': '-..-.', '=': '-...-',
    '+': '.-.-.', '-': '-....-', ':': '---...', "'": '.----.', '!': '-.-.--', '@': '.--.-.',
}


def keying(message: str, wpm: float, rate: float) -> np.ndarray:
    """A 0/1 keying envelope for `message`: lead-in, the message, then the line-closing gap."""
    dot = max(1, int(round(rate * 1.2 / max(1.0, wpm))))
    runs: list[tuple[int, int]] = [(0, int(round(rate * LEAD_IN_S)))]
    words = message.split()
    for wi, word in enumerate(words):
        for ci, ch in enumerate(word):
            code = MORSE.get(ch.upper())
            if code is None:
                raise SystemExit(f'no Morse for {ch!r} (message: {message!r})')
            for si, sym in enumerate(code):
                runs.append((1, dot * (3 if sym == '-' else 1)))
                if si < len(code) - 1:
                    runs.append((0, dot))
            if ci < len(word) - 1:
                runs.append((0, dot * 3))
        if wi < len(words) - 1:
            runs.append((0, dot * 7))
    runs.append((0, dot * TAIL_DOTS))
    return np.concatenate([np.full(n, k, dtype=np.float32) for k, n in runs])


def build_cycle(key: np.ndarray, rate: float, cycle_s: float, base_hz: float,
                scale: float) -> np.ndarray:
    """One cycle: the keyed carrier at `base_hz` followed by silence, ramped and scaled.

    The carrier is the baseband itself (I = keying, Q = 0) rotated to `base_hz`. The default
    offset is negative (-2 x pitch), which the CW demod's Pitch then maps onto the decoded
    beat note while the RF carrier stays clear of DC.
    """
    n = key.size
    if base_hz:
        t = np.arange(n) / rate
        baseband = key * np.exp(2j * np.pi * base_hz * t)
    else:
        baseband = key.astype(np.complex64)
    ramp = int(round(rate * EDGE_MS / 1000.0))
    if ramp > 1:
        edge = 0.5 * (1.0 - np.cos(np.pi * np.arange(ramp) / ramp))
        # Only the key-down edges: ramp each mark's first and last samples, not the whole cycle.
        rises = np.flatnonzero((key[1:] > 0) & (key[:-1] <= 0)) + 1
        falls = np.flatnonzero((key[1:] <= 0) & (key[:-1] > 0)) + 1
        def apply_edges(indexes, rising):
            for at in indexes[:max(0, (baseband.size - ramp) // max(1, ramp))]:
                end = min(at + ramp, baseband.size)
                w = edge[:end - at] if rising else edge[:end - at][::-1]
                baseband[at:end] *= w
        apply_edges(rises, True)
        apply_edges(falls, False)
    cycle = np.zeros(int(round(rate * cycle_s)), dtype=np.complex64)
    if key.size > cycle.size:
        raise SystemExit(f'the keyed message ({key.size / rate:.2f}s) does not fit in a '
                         f'{cycle_s:.1f}s cycle at {wpm_rate_hint(rate, key)} - raise --cycle')
    cycle[:key.size] = baseband
    # Scale last, and keep complex64: `tx()` casts to int16 without scaling.
    return (cycle * scale).astype(np.complex64)


def wpm_rate_hint(rate: float, key: np.ndarray) -> str:
    return f'{rate:.0f} Hz'


def describe(args: argparse.Namespace, key: np.ndarray, cycle: np.ndarray) -> None:
    dot_ms = 1200.0 / max(1.0, args.wpm)
    marks = int(np.count_nonzero(np.diff((key > 0).astype(np.int8)) > 0))
    print(f'message        : {args.message}')
    print(f'speed          : {args.wpm:.0f} WPM (dot {dot_ms:.0f} ms, dash {3 * dot_ms:.0f} ms), '
          f'{marks} marks')
    print(f'keying         : {key.size / args.rate:.2f}s (incl. {LEAD_IN_S:.1f}s lead-in and '
          f'{TAIL_DOTS} dots tail), silence to {args.cycle:.1f}s')
    print(f'rate           : {args.rate:.0f} Hz   cycle samples: {cycle.size} '
          f'({cycle.nbytes / 1e6:.1f} MB complex64), scale {args.scale:.0f}')
    print(f'tx LO          : {args.lo / 1e6:.6f} MHz')
    print(f'RF carrier     : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer to --lo, Pitch = {args.pitch:.0f} Hz)')
    print(f'sidetone       : {abs(args.base_hz):.0f} Hz '
          f'({args.base_hz:+.0f} Hz from the dial; the demod band sits at the Pitch)')


def transmit(args: argparse.Namespace) -> int:
    key = keying(args.message, args.wpm, args.rate)
    cycle = build_cycle(key, args.rate, args.cycle, args.base_hz, args.scale)
    describe(args, key, cycle)
    if args.dry_run:
        print('\n--dry-run: keyed and planned, radio untouched.')
        return 0

    import adi  # imported late so --dry-run works without pyadi-iio

    ft8tx.install_signal_handlers()
    sdr = adi.Pluto(args.uri)
    xo_restore = ft8tx.trim_reference_clock(sdr, args.xo_ppm, not args.no_xo_trim)
    sdr.tx_lo = int(args.lo)
    try:
        sdr.sample_rate = int(args.rate)
    except ValueError as exc:
        print(f'note: radio rejected sample_rate={int(args.rate)} ({exc}); '
              f'keeping {int(sdr.sample_rate)} Hz', file=sys.stderr)
    sdr.tx_hardwaregain_chan0 = float(args.gain)
    actual_rate = int(sdr.sample_rate)
    if actual_rate != int(args.rate):
        print(f'note: rebuilding the cycle for the actual sample_rate={actual_rate} Hz', file=sys.stderr)
        key = keying(args.message, args.wpm, actual_rate)
        cycle = build_cycle(key, actual_rate, args.cycle, args.base_hz, args.scale)
    sdr.tx_cyclic_buffer = True

    delay = args.cycle - (time.time() % args.cycle)
    print(f'\nwaiting {delay:.2f}s for the next {args.cycle:.0f}s cycle boundary...')
    time.sleep(delay)
    print(f'pushing {cycle.size} samples ({cycle.nbytes / 1e6:.1f} MB)...')
    sdr.tx(cycle)
    print(f'TX on: lo={sdr.tx_lo} rate={int(sdr.sample_rate)} gain={args.gain} dBm-scale '
          f'cyclic cycle={args.cycle:.0f}s', flush=True)
    try:
        sent = 0
        while args.slots == 0 or sent < args.slots:
            start = time.time()
            sent += 1
            begin = time.strftime('%H:%M:%S', time.gmtime(start))
            finish = time.strftime('%H:%M:%S', time.gmtime(start + key.size / actual_rate))
            print(f'  cycle {sent:3d}  {args.message!r} {begin} -> {finish} UTC '
                  f'(silent until {time.strftime("%H:%M:%S", time.gmtime(start + args.cycle))})',
                  flush=True)
            time.sleep(args.cycle)
    except KeyboardInterrupt:
        print('\ninterrupted')
    finally:
        sdr.tx_destroy_buffer()
        if xo_restore is not None:
            ft8tx.write_xo_correction(sdr, xo_restore)
            print(f'xo_correction restored to {xo_restore} Hz')
        print('TX stopped.')
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    # The message can be a positional argument (the common case at the bench) or --message;
    # the positional wins when both are given.
    parser.add_argument('message_pos', nargs='?', default=None,
                        help=f'CW text to send (default: {DEFAULT_MESSAGE!r})')
    parser.add_argument('--message', dest='message_opt', default=None,
                        help='CW text to send (same as the positional argument)')
    parser.set_defaults(base_hz=None)
    parser.add_argument('--wpm', type=float, default=DEFAULT_WPM, help='keying speed in WPM')
    parser.add_argument('--lo', type=float, default=411e6, help='TX LO (= what the analyzer tunes to)')
    parser.add_argument('--pitch', type=float, default=700.0,
                        help='the analyzer Pitch control this transmission targets (its beat note)')
    parser.add_argument('--base-hz', type=float, default=None,
                        help='carrier offset from the LO (default: +pitch - the CW demod selects '
                             'its band at the Pitch above the dial; a zero offset is not decoded)')
    parser.add_argument('--cycle', type=float, default=DEFAULT_CYCLE_S,
                        help='seconds per repeat cycle (message + silence)')
    parser.add_argument('--rate', type=float, default=521000.0, help='TX sample rate in Hz')
    parser.add_argument('--xo-ppm', type=float, default=ft8tx.XO_PPM_LOW,
                        help='reference crystal error in ppm (measured: -1.98 on this unit)')
    parser.add_argument('--no-xo-trim', action='store_true',
                        help='leave xo_correction alone (do not correct the crystal)')
    parser.add_argument('--gain', type=float, default=-10.0, help='TX hardware gain in dB (max 0)')
    parser.add_argument('--uri', default=ft8tx.DEFAULT_URI, help='libiio context URI')
    parser.add_argument('--slots', type=int, default=0, help='cycles to send (0 = forever)')
    parser.add_argument('--scale', type=float, default=ft8tx.DAC_FULL_SCALE,
                        help='baseband scale into the DAC int16 range (tx() does not scale)')
    parser.add_argument('--dry-run', action='store_true',
                        help='key and print the plan without touching the radio')
    args = parser.parse_args(argv)
    args.message = args.message_pos or args.message_opt or DEFAULT_MESSAGE
    if args.base_hz is None:
        args.base_hz = args.pitch
    return transmit(args)


def main() -> int:
    return parse_args()


if __name__ == '__main__':
    raise SystemExit(main())
