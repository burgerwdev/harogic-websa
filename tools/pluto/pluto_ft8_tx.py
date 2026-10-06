#!/usr/bin/env python3
"""Transmit a looping FT8 signal from a PlutoSDR, for end-to-end receiver testing.

Prefer the unified entry: `python3 tools/pluto_tx.py ft8 [args]`.

This is a bench tool, not product code: it exists so the WebSA SDR path can be driven with a
*known* over-the-air signal. It reuses the protocol encoder in `tools/fixtures/gen_ft8_fixtures.py`
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

    python3 tools/pluto/pluto_ft8_tx.py --dry-run                    # encode + plan, no radio
    python3 tools/pluto/pluto_ft8_tx.py --slots 4                    # transmit 4 slots
    python3 tools/pluto/pluto_ft8_tx.py --message 'CQ WE0BSA PM95'

**`--gain -70` is too low to decode reliably** (measured on the bench, SAN-90 listening on a
411 MHz antenna a few metres away). The received tone, in the channelized baseband, against the
noise floor beside it:

| `--gain` | tone | tone / floor | product decodes |
|---------|------|--------------|-----------------|
| -70 dB | +29 dB | +6 dB | 1..6 of 6 slots, `snr` 10-14 dB - a coin flip |
| -40 dB | +60 dB | +35 dB | every slot in the middle of a run (5 of 6; the two ends are the 28 s window pre-roll and the truncated tail) |

30 dB of TX gain bought exactly 30 dB of SNR with the floor unchanged (`status_warning` 0,
attenuator 15 dB, IF gain 2), so there is no compression to trade against - and the decoder's
miss rate at -70 dB is a link-budget symptom, not a decoder one. Raise the gain until the tone
stands ~30 dB over the floor, and check the analyzer for IF overflow (-12) once.
"""
from __future__ import annotations

import argparse
import importlib.util
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_radio as radio  # noqa: E402

ENCODER_PATH = (Path(__file__).resolve().parent.parent / 'fixtures' / 'gen_ft8_fixtures.py')

#: FT8 timing, from the protocol: 79 symbols of 0.160 s = 12.64 s inside a 15 s slot.
SYMBOL_SECONDS = 0.160
TONE_SPACING_HZ = 6.25
SLOT_SECONDS = 15.0
DEFAULT_MESSAGE = 'CQ WE0BSA PM95'
#: Where the FT8 tones sit in the audio passband. Mid-band, matching WSJT-X's default TX offset,
#: so a few hundred Hz of clock error cannot push the signal out of the decoder's search window.
DEFAULT_BASE_HZ = 1500.0
#: Bench LO in Hz. The AD9363 transmits above ~325 MHz only. Tune the analyzer here.
DEFAULT_LO_HZ = 435e6


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


def transmit(args: argparse.Namespace) -> int:
    encoder = load_encoder()
    tones = tones_for_message(encoder, args.message)
    slot = build_slot(tones, args.rate, args.base_hz, args.scale)
    describe(args.message, tones, args.rate, args.base_hz, args.lo, args.scale, slot)

    if args.dry_run:
        print('\n--dry-run: encoded and planned, radio untouched.')
        return 0

    with radio.Radio(args) as tx:
        if tx.rate != int(args.rate):
            print(f'note: rebuilding the slot for the actual sample_rate={tx.rate} Hz',
                  file=sys.stderr)
            slot = build_slot(tones, tx.rate, args.base_hz, args.scale)

        delay = seconds_to_next_slot()
        print(f'\nwaiting {delay:.2f}s for the next {SLOT_SECONDS:.0f}s slot boundary...')
        time.sleep(delay)

        print(f'pushing {slot.size} samples ({slot.nbytes / 1e6:.1f} MB)...')
        tx.push_cyclic(slot)
        print(f'TX on: lo={tx.sdr.tx_lo} rate={tx.rate} gain={args.gain} '
              f'cyclic slot={SLOT_SECONDS:.0f}s', flush=True)

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
                time.sleep(SLOT_SECONDS - ((time.time() - start) % SLOT_SECONDS)
                           if args.slots == 0 else SLOT_SECONDS)
        except KeyboardInterrupt:
            print('\ninterrupted')
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--message', default=DEFAULT_MESSAGE, help='FT8 text to send')
    parser.add_argument('--base-hz', type=float, default=DEFAULT_BASE_HZ,
                        help='lowest FT8 tone in Hz (1500 = mid-band, the WSJT-X convention)')
    parser.add_argument('--slots', type=int, default=0, help='slots to send (0 = forever)')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-10.0,
                         dry_run_help='encode and print the plan without touching the radio')
    return transmit(parser.parse_args(argv))


if __name__ == '__main__':
    raise SystemExit(main())
