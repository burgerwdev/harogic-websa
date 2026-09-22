#!/usr/bin/env python3
"""Transmit a looping FT8 signal from a PlutoSDR, for end-to-end receiver testing.

This is a bench tool, not product code: it exists so the WebSA SDR path can be driven with a
*known* over-the-air signal. It reuses the protocol encoder in `tools/gen_ft8_fixtures.py`
(payload -> CRC -> LDPC -> 79 tones) and only adds the transmission side: baseband synthesis at
the Pluto's own sample rate, 15 s slot alignment, and a cyclic DMA stream.

Two hardware facts measured on the ADALM-PLUTO (AD9363) that shape this script:

  1. **`tx()` casts the float array straight to int16.** pyadi-iio's Pluto does
     `i.astype(self._tx_data_type)` with no scaling, so a baseband in [-1, 1] truncates to
     zero and the radio transmits *silence*, ~70 dB down. The baseband must be scaled into the
     DAC's int16 range (`--scale`, default 2**14).
  2. **The TX sample rate floor is 521 kHz** (pyadi-iio rejects anything lower, even though
     the AD9363 reports 520.833 kHz). One 15 s slot at that rate is ~7.8 M complex samples,
     so the cyclic buffer is large; `--rate` exists to trade buffer size for headroom.

The TX spectrum is **not** inverted: a baseband tone at `+f` was measured at `LO + f`
(+200 kHz baseband -> 435.199270 MHz with LO 435 MHz), so the tone order reaches the air
unchanged and no conjugation is applied.

The audio sits at **1500 Hz**, not at a low offset, and that is a deliberate operating choice.
An SDR's reference crystal is a few ppm off and the error grows with RF: this Pluto measures
1.98 ppm low, i.e. -809 Hz at 411 MHz and -864 Hz at 435 MHz. Against a 1 kHz base that lands the
signal at ~191 Hz, under the decoder's lower edge, and nothing decodes; against 1500 Hz the same
error lands at ~691 Hz, mid-band. 1500 Hz is also the offset WSJT-X uses by default and it keeps
the signal clear of the receiver filter's roll-off. `--base-hz` overrides it.

That ppm error is corrected at the source, too: the AD9361 is told the real crystal frequency via
`xo_correction`, which fixes the LO *and* the sample clock at the clock root. On this unit that
took the error from 1.98 ppm to under 0.02 ppm, so the RF lands where `--lo` says and the decoded
frequency is the true one (worth having: a spot uploaded 800 Hz off sends people to the wrong
place). The attribute is volatile -- a Pluto power cycle restores the default -- so this script
re-applies it on every run and puts the previous value back on the way out.

    python3 tools/pluto_ft8_tx.py --dry-run                    # encode + plan, no radio
    python3 tools/pluto_ft8_tx.py --slots 4                    # transmit 4 slots
    python3 tools/pluto_ft8_tx.py --message 'CQ WE0BSA PM95'
"""
from __future__ import annotations

import argparse
import importlib.util
import signal
import sys
import time
from pathlib import Path

import numpy as np

ENCODER_PATH = Path(__file__).resolve().with_name('gen_ft8_fixtures.py')

#: FT8 timing, from the protocol: 79 symbols of 0.160 s = 12.64 s inside a 15 s slot.
SYMBOL_SECONDS = 0.160
TONE_SPACING_HZ = 6.25
SLOT_SECONDS = 15.0
DEFAULT_MESSAGE = 'CQ WE0BSA PM95'
DEFAULT_URI = 'ip:192.168.2.1'
#: Where the FT8 tones sit in the audio passband. Mid-band, matching WSJT-X's default TX offset,
#: so a few hundred Hz of clock error cannot push the signal out of the decoder's search window.
DEFAULT_BASE_HZ = 1500.0

#: Nominal AD9361 reference crystal, and how far off this unit's is. Measured against a
#: GNSS-locked SAN-90 with a CW at 411 and 435 MHz: -1.97 and -1.99 ppm. Telling the AD9361 the
#: real frequency (`xo_correction`) corrects the LO and the sample clock together.
XO_NOMINAL_HZ = 40_000_000
XO_PPM_LOW = 1.98

#: pyadi-iio's Pluto `tx()` casts complex float straight to the DAC's int16 format, so the
#: baseband has to be scaled into that range itself. 2**14 is the conventional level (the
#: library's own examples use it); 2**15 would clip on the peaks.
DAC_FULL_SCALE = 2 ** 14


def load_encoder():
    """Import `gen_ft8_fixtures` by path so `tools/` needs no `__init__.py`."""
    spec = importlib.util.spec_from_file_location('gen_ft8_fixtures', ENCODER_PATH)
    if spec is None or spec.loader is None:  # pragma: no cover - packaging error
        raise SystemExit(f'cannot load encoder at {ENCODER_PATH}')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def tones_for_message(encoder, message: str) -> list[int]:
    """The 79-tone sequence the decoder must recover, via the shared encoder."""
    payload, payload_bits77 = encoder.standard_payload(message)
    del payload
    bits91 = payload_bits77 + [(encoder.crc14(payload_bits77) >> (13 - i)) & 1 for i in range(14)]
    return encoder.tones_for(encoder.ldpc_encode(bits91))


def synth_baseband(tones: list[int], rate: float, base_hz: float) -> np.ndarray:
    """Phase-continuous 8-FSK at `rate` Hz, tones at `base_hz + k * 6.25`.

    Synthesised directly at the transmitter's rate rather than resampled from the fixture's
    48 kHz: the tone sequence is what the encoder owns, the sample rate is a radio detail.
    """
    symbol_samples = int(round(rate * SYMBOL_SECONDS))
    out = np.empty(len(tones) * symbol_samples, dtype=np.complex64)
    phase = 0.0
    cursor = 0
    for tone in tones:
        frequency = base_hz + tone * TONE_SPACING_HZ
        step = 2.0 * np.pi * frequency / rate
        out[cursor:cursor + symbol_samples] = np.exp(1j * (phase + step * np.arange(symbol_samples)))
        phase = (phase + step * symbol_samples) % (2.0 * np.pi)
        cursor += symbol_samples
    return out


def build_slot(tones: list[int], rate: float, base_hz: float, scale: float,
               ramp_ms: float = 5.0) -> np.ndarray:
    """One 15 s slot: the 12.64 s FT8 burst followed by silence, raised-cosine ramped."""
    burst = synth_baseband(tones, rate, base_hz)
    ramp = int(round(rate * ramp_ms / 1000.0))
    if ramp > 1:
        edge = 0.5 * (1.0 - np.cos(np.pi * np.arange(ramp) / ramp))
        burst[:ramp] *= edge
        burst[-ramp:] *= edge[::-1]
    slot = np.zeros(int(round(rate * SLOT_SECONDS)), dtype=np.complex64)
    if burst.size > slot.size:
        raise SystemExit(f'FT8 burst ({burst.size}) does not fit in a {SLOT_SECONDS}s slot at {rate} Hz')
    slot[:burst.size] = burst
    # Scale last, and keep the result complex64: `tx()` casts to int16 and would otherwise
    # throw the whole waveform away.
    return (slot * scale).astype(np.complex64)


def seconds_to_next_slot(slot: float = SLOT_SECONDS) -> float:
    """Delay until the next UTC-aligned FT8 slot boundary."""
    now = time.time()
    return slot - (now % slot)


def describe(message: str, tones: list[int], rate: float, base_hz: float, lo: float,
             scale: float, slot: np.ndarray) -> None:
    print(f'message        : {message}')
    print(f'tones (79)     : {" ".join(str(t) for t in tones)}')
    print(f'tone span      : {base_hz:.2f} .. {base_hz + 7 * TONE_SPACING_HZ:.2f} Hz '
          f'({TONE_SPACING_HZ} Hz spacing, 8-FSK)')
    print(f'burst          : {len(tones)} symbols x {SYMBOL_SECONDS}s = '
          f'{len(tones) * SYMBOL_SECONDS:.2f}s  (+{SLOT_SECONDS - len(tones) * SYMBOL_SECONDS:.2f}s silence)')
    print(f'rate           : {rate:.0f} Hz   slot samples: {slot.size} '
          f'({slot.nbytes / 1e6:.1f} MB complex64), scale {scale:.0f}')
    print(f'tx LO          : {lo / 1e6:.6f} MHz')
    print(f'RF tones       : {(lo + base_hz) / 1e6:.6f} .. '
          f'{(lo + base_hz + 7 * TONE_SPACING_HZ) / 1e6:.6f} MHz')


def read_xo_correction(sdr) -> int | None:
    """The AD9361's `xo_correction` in Hz, or None when this driver does not expose it."""
    attrs = getattr(getattr(sdr, '_ctrl', None), 'attrs', None)
    if attrs is None or 'xo_correction' not in attrs:
        return None
    try:
        return int(float(attrs['xo_correction'].value))
    except (TypeError, ValueError):
        return None


def write_xo_correction(sdr, hz: int) -> bool:
    attrs = getattr(getattr(sdr, '_ctrl', None), 'attrs', None)
    if attrs is None or 'xo_correction' not in attrs:
        return False
    attrs['xo_correction'].value = str(int(hz))
    return True


def trim_reference_clock(sdr, ppm: float, enabled: bool) -> int | None:
    """Point the AD9361 at the crystal's real frequency; return the old value if we changed it.

    Must run *before* the LO and sample rate are set, since the driver derives both from it.
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
    """Turn SIGINT and SIGTERM into KeyboardInterrupt so the cleanup actually runs.

    A background launch (``... &`` from a non-interactive shell) starts with SIGINT already set to
    SIG_IGN, and Python deliberately keeps an inherited ignore -- so Ctrl-C never arrives and the
    `finally` that restores `xo_correction` would be skipped. SIGTERM's default action skips it
    too. Both are converted, which is what `--slots N` and a Ctrl-C in the foreground already do.
    """
    def _raise(signum, _frame):
        raise KeyboardInterrupt(signum)

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, _raise)
        except (ValueError, OSError):  # pragma: no cover - not the main thread
            pass


def transmit(args: argparse.Namespace) -> int:
    import adi  # imported late so --dry-run works without pyadi-iio

    encoder = load_encoder()
    tones = tones_for_message(encoder, args.message)
    slot = build_slot(tones, args.rate, args.base_hz, args.scale)
    describe(args.message, tones, args.rate, args.base_hz, args.lo, args.scale, slot)

    if args.dry_run:
        print('\n--dry-run: encoded and planned, radio untouched.')
        return 0

    install_signal_handlers()
    sdr = adi.Pluto(args.uri)
    xo_restore = trim_reference_clock(sdr, args.xo_ppm, not args.no_xo_trim)
    sdr.tx_lo = int(args.lo)
    try:
        sdr.sample_rate = int(args.rate)
    except ValueError as exc:
        print(f'note: radio rejected sample_rate={int(args.rate)} ({exc}); '
              f'keeping {int(sdr.sample_rate)} Hz', file=sys.stderr)
    sdr.tx_hardwaregain_chan0 = float(args.gain)
    actual_rate = int(sdr.sample_rate)
    if actual_rate != int(args.rate):
        print(f'note: rebuilding the slot for the actual sample_rate={actual_rate} Hz', file=sys.stderr)
        slot = build_slot(tones, actual_rate, args.base_hz, args.scale)
    sdr.tx_cyclic_buffer = True

    delay = seconds_to_next_slot()
    print(f'\nwaiting {delay:.2f}s for the next 15s slot boundary...')
    time.sleep(delay)

    print(f'pushing {slot.size} samples ({slot.nbytes / 1e6:.1f} MB)...')
    sdr.tx(slot)
    print(f'TX on: lo={sdr.tx_lo} rate={int(sdr.sample_rate)} gain={args.gain} dBm-scale '
          f'cyclic slot={SLOT_SECONDS}s', flush=True)

    try:
        sent = 0
        while args.slots == 0 or sent < args.slots:
            start = time.time()
            stop = start + SLOT_SECONDS - len(tones) * SYMBOL_SECONDS
            begin = time.strftime('%H:%M:%S', time.gmtime(start))
            finish = time.strftime('%H:%M:%S', time.gmtime(start + len(tones) * SYMBOL_SECONDS))
            print(f'  slot {sent + 1:3d}  burst {begin} -> {finish} UTC '
                  f'(silent until {time.strftime("%H:%M:%S", time.gmtime(stop))})', flush=True)
            sent += 1
            time.sleep(SLOT_SECONDS - ((time.time() - start) % SLOT_SECONDS) if args.slots == 0 else SLOT_SECONDS)
    except KeyboardInterrupt:
        print('\ninterrupted')
    finally:
        sdr.tx_destroy_buffer()
        if xo_restore is not None:
            write_xo_correction(sdr, xo_restore)
            print(f'xo_correction restored to {xo_restore} Hz')
        print('TX stopped.')
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--message', default=DEFAULT_MESSAGE, help='FT8 text to send')
    parser.add_argument('--lo', type=float, default=435e6, help='TX LO in Hz')
    parser.add_argument('--rate', type=float, default=521000.0, help='TX sample rate in Hz')
    parser.add_argument('--base-hz', type=float, default=DEFAULT_BASE_HZ,
                        help='lowest FT8 tone in Hz (1500 = mid-band, the WSJT-X convention)')
    parser.add_argument('--xo-ppm', type=float, default=XO_PPM_LOW,
                        help='reference crystal error in ppm (measured: -1.98 on this unit)')
    parser.add_argument('--no-xo-trim', action='store_true',
                        help='leave xo_correction alone (do not correct the crystal)')
    parser.add_argument('--gain', type=float, default=-10.0, help='TX hardware gain in dB (max 0)')
    parser.add_argument('--uri', default=DEFAULT_URI, help='libiio context URI')
    parser.add_argument('--slots', type=int, default=0, help='slots to send (0 = forever)')
    parser.add_argument('--scale', type=float, default=DAC_FULL_SCALE,
                        help='baseband scale into the DAC int16 range (tx() does not scale)')
    parser.add_argument('--dry-run', action='store_true',
                        help='encode and print the plan without touching the radio')
    return transmit(parser.parse_args())


if __name__ == '__main__':
    raise SystemExit(main())
