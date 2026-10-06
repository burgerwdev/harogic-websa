#!/usr/bin/env python3
"""Shared ADALM-PLUTO transmit layer for the bench tools.

Each bench tool has its own signal generator. This module owns the radio facts
once:

  * the AD9363 TX sample-rate floor (521 kHz),
  * the DAC scale, because `tx()` casts to int16 with no scale,
  * the measured reference-crystal error (1.98 ppm on this unit),
  * the cyclic-buffer transmit and the exit cleanup.

A tool builds one complex64 buffer and uses `Radio`. The class opens the radio,
applies the crystal trim, sets the LO, the rate and the gain, and restores the
crystal on exit. The signal generator stays in the tool.
"""
from __future__ import annotations

import argparse
import signal
import sys
import time

#: libiio context URI of the bench Pluto.
DEFAULT_URI = 'ip:192.168.2.1'
#: Nominal AD9361 reference crystal frequency.
XO_NOMINAL_HZ = 40_000_000
#: Measured crystal error of this unit, in ppm. A GNSS-locked SAN-90 measured it.
XO_PPM_LOW = 1.98
#: pyadi-iio `tx()` casts a complex float array to int16 with no scale. Scale at 2**14.
DAC_FULL_SCALE = 2 ** 14
#: The driver rejects a TX sample rate below 521 kHz, although the AD9363 reports 520.833 kHz.
MIN_TX_RATE_HZ = 521_000.0


def read_xo_correction(sdr) -> int | None:
    """Return the AD9361 `xo_correction` in Hz, or None when the driver hides it."""
    attrs = getattr(getattr(sdr, '_ctrl', None), 'attrs', None)
    if attrs is None or 'xo_correction' not in attrs:
        return None
    try:
        return int(float(attrs['xo_correction'].value))
    except (TypeError, ValueError):
        return None


def write_xo_correction(sdr, hz: int) -> bool:
    """Write the AD9361 `xo_correction`. Return False when the driver hides it."""
    attrs = getattr(getattr(sdr, '_ctrl', None), 'attrs', None)
    if attrs is None or 'xo_correction' not in attrs:
        return False
    attrs['xo_correction'].value = str(int(hz))
    return True


def trim_reference_clock(sdr, ppm: float, enabled: bool) -> int | None:
    """Point the AD9361 at the crystal's real frequency.

    Return the old value when the call changes it, or None. Run this before the
    LO and the sample rate, because the driver derives both from the crystal.
    """
    if not enabled:
        return None
    current = read_xo_correction(sdr)
    if current is None:
        print('note: xo_correction is not exposed by this driver; skipping the crystal trim',
              file=sys.stderr)
        return None
    target = round(XO_NOMINAL_HZ * (1.0 - ppm * 1e-6))
    if current == target:
        print(f'xo_correction already {current} Hz')
        return None
    if not write_xo_correction(sdr, target):
        return None
    print(f'xo_correction: {current} -> {target} Hz ({ppm:.2f} ppm crystal trim)')
    return current


def install_signal_handlers() -> None:
    """Turn SIGINT and SIGTERM into KeyboardInterrupt, so the cleanup runs.

    A background launch (`... &` from a non-interactive shell) starts with
    SIGINT set to SIG_IGN, and Python keeps an inherited ignore. Ctrl-C then
    never arrives, and the `finally` that restores `xo_correction` is skipped.
    SIGTERM skips it too. Convert both.
    """
    def _raise(signum, _frame):
        raise KeyboardInterrupt(signum)

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, _raise)
        except (ValueError, OSError):  # pragma: no cover - not the main thread
            pass


def seconds_to_next_boundary(period_s: float) -> float:
    """Return the delay to the next UTC-aligned `period_s` boundary."""
    now = time.time()
    return period_s - (now % period_s)


def add_radio_args(parser: argparse.ArgumentParser, *, default_lo: float,
                   default_gain: float, default_rate: float = MIN_TX_RATE_HZ,
                   default_scale: float = DAC_FULL_SCALE, dry_run_help: str) -> None:
    """Add the radio arguments every bench tool shares."""
    parser.add_argument('--lo', type=float, default=default_lo, help='TX LO in Hz')
    parser.add_argument('--rate', type=float, default=default_rate,
                        help=f'TX sample rate in Hz (driver floor {MIN_TX_RATE_HZ:.0f})')
    parser.add_argument('--gain', type=float, default=default_gain, help='TX hardware gain in dB (max 0)')
    parser.add_argument('--scale', type=float, default=default_scale,
                        help='baseband scale into the DAC int16 range (tx() does not scale)')
    parser.add_argument('--xo-ppm', type=float, default=XO_PPM_LOW,
                        help='reference crystal error in ppm (measured: 1.98 on this unit)')
    parser.add_argument('--no-xo-trim', action='store_true',
                        help='leave xo_correction alone (do not correct the crystal)')
    parser.add_argument('--uri', default=DEFAULT_URI, help='libiio context URI')
    parser.add_argument('--dry-run', action='store_true', help=dry_run_help)


class Radio:
    """The open radio, with the crystal trim and the cleanup around it.

    Use it as a context manager. The sample rate can differ from the requested
    rate: the driver rejects some values. Read `radio.rate` after entry and
    rebuild the signal when it differs.
    """

    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.sdr = None
        self.rate = int(args.rate)
        self.xo_restore = None

    def __enter__(self) -> Radio:
        import adi  # late, so --dry-run works without pyadi-iio

        install_signal_handlers()
        self.sdr = adi.Pluto(self.args.uri)
        self.xo_restore = trim_reference_clock(self.sdr, self.args.xo_ppm, not self.args.no_xo_trim)
        self.sdr.tx_lo = int(self.args.lo)
        try:
            self.sdr.sample_rate = int(self.args.rate)
        except ValueError as exc:
            print(f'note: radio rejected sample_rate={int(self.args.rate)} ({exc}); '
                  f'keeping {int(self.sdr.sample_rate)} Hz', file=sys.stderr)
        self.rate = int(self.sdr.sample_rate)
        self.sdr.tx_hardwaregain_chan0 = float(self.args.gain)
        return self

    def __exit__(self, *_exc) -> bool:
        if self.sdr is not None:
            try:
                self.sdr.tx_destroy_buffer()
            except Exception:  # pragma: no cover - driver teardown is best effort
                pass
            if self.xo_restore is not None:
                write_xo_correction(self.sdr, self.xo_restore)
                print(f'xo_correction restored to {self.xo_restore} Hz')
        print('TX stopped.')
        return False

    def push_cyclic(self, samples) -> None:
        """Push one buffer and let the Pluto repeat it, with no host-side gaps."""
        self.sdr.tx_cyclic_buffer = True
        self.sdr.tx(samples)

    def open_stream(self) -> None:
        """Switch to chunked one-shot pushes."""
        self.sdr.tx_cyclic_buffer = False

    def push_chunk(self, part) -> None:
        self.sdr.tx(part)

    def chunk_samples(self, chunk_s: float) -> int:
        return max(1, int(self.rate * chunk_s))


if __name__ == '__main__':  # pragma: no cover - a library module
    raise SystemExit('pluto_radio is a shared library; run a mode tool or tools/pluto_tx.py')
