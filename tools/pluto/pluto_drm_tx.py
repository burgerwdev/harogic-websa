#!/usr/bin/env python3
"""Transmit a DRM signal from the PlutoSDR, for a controlled DRM bench loop.

Prefer the unified entry: `python3 tools/pluto_tx.py drm [args]`.

The point: a *known* DRM signal with known audio, so the SAN-90's DRM chain can be
tested at a comfortable SNR instead of waiting for shortwave propagation. It replaces
the HF path entirely:

  * the AD9363 TX cannot tune HF (>= ~325 MHz), but a DRM signal is just an OFDM
    waveform — the carrier is irrelevant to Dream. Transmit at UHF (400 MHz default)
    and tune the analyzer there.
  * prefer a **coax cable + attenuator** from the Pluto TX to the SAN-90 RF input:
    controlled level, no propagation, no interference. (A radiated test needs a UHF
    antenna on the analyzer; an HF antenna picks up almost nothing at 400 MHz.)
  * the DRM signal sits at `--base-hz` (default +100 kHz) from the LO so it is clear
    of the direct-conversion LO leakage: tune the analyzer to `LO + base_hz`.

The IQ input is an int16 WAV from the DecDRM transmitter:

    decdrm tx station.toml --output drm_iq.wav --duration 60

(a station with `format = "iq"` and the default `iq_swap` = I on the left, `-c 6` at
the analyzer). Dream's file input only decoded the int16 WAV, not the float32 one, so
keep `sample_format = "int16"` in the station output.

    python3 tools/pluto/pluto_drm_tx.py --iq drm_iq.wav --dry-run
    python3 tools/pluto/pluto_drm_tx.py --iq drm_iq.wav --lo 400e6 --gain -20 --seconds 120

Without `--iq`, the tool prints the plan and stops. The DRM samples come only from
the DecDRM transmitter.

Then: SDR mode, Center/Listen = `LO + base_hz` (400.1 MHz), Demod = DRM. Watch SNR/MER
and raise `--gain` (or drop attenuation) until MER is comfortable.
"""
from __future__ import annotations

import argparse
import sys
import time
import wave
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_radio as radio  # noqa: E402

#: Where the DRM baseband sits relative to the LO, clear of LO leakage.
DEFAULT_BASE_HZ = 100e3
#: The AD9363 TX sample-rate floor: below this the driver refuses.
DEFAULT_RATE = radio.MIN_TX_RATE_HZ
#: Bench LO in Hz. The AD9363 transmits above ~325 MHz only, so HF needs a different path.
DEFAULT_LO_HZ = 400e6


def load_iq(path: str) -> tuple[np.ndarray, int]:
    """Read an int16 stereo IQ WAV (I left, Q right) into complex64 at its own rate."""
    with wave.open(path, 'rb') as w:
        if w.getsampwidth() != 2:
            raise SystemExit(f'{path}: expected 16-bit PCM (DecDRM sample_format = "int16")')
        channels = w.getnchannels()
        rate = w.getframerate()
        raw = np.frombuffer(w.readframes(w.getnframes()), dtype='<i2').astype(np.float32)
    if channels < 2:
        raise SystemExit(f'{path}: expected 2 channels of I/Q, got {channels}')
    i = raw[0::channels] / 32768.0
    q = raw[1::channels] / 32768.0
    return (i + 1j * q).astype(np.complex64), rate


def resample_linear(z: np.ndarray, fs_in: float, fs_out: float) -> np.ndarray:
    """Linear-interpolation resampler: the input is a 10 kHz-wide DRM signal, so the
    interpolation error is far below the decoder's tolerance."""
    n_out = int(round(z.size * fs_out / fs_in))
    if n_out <= 1:
        return z
    pos = np.arange(n_out, dtype=np.float64) * (fs_in / fs_out)
    i0 = np.clip(pos.astype(np.int64), 0, z.size - 2)
    fr = (pos - i0).astype(np.float32)
    return (z[i0] * (1.0 - fr) + z[i0 + 1] * fr).astype(np.complex64)


def build_signal(path: str, rate: float, base_hz: float, scale: float,
                 conj: bool = False) -> np.ndarray:
    z, in_rate = load_iq(path)
    z = resample_linear(z, in_rate, rate)
    if conj:
        # The Pluto's direct-conversion I/Q convention can be opposite to the analyzer's DDC,
        # inverting the spectrum; Dream then sees a mirrored DRM signal and never locks.
        z = np.conj(z)
    if base_hz:
        # Shift the DRM baseband off DC (direct-conversion LO leakage lands on DC).
        n = np.arange(z.size, dtype=np.float64)
        z = z * np.exp(2j * np.pi * base_hz * n / rate).astype(np.complex64)
    return (z * scale).astype(np.complex64)


def describe(args, in_rate: int, sig: np.ndarray) -> None:
    peak = float(np.abs(sig).max()) if sig.size else 0.0
    print(f'input           : {args.iq} ({in_rate} Hz -> {args.rate:.0f} Hz)')
    print(f'samples         : {sig.size} ({sig.size / args.rate:.1f} s complex64, '
          f'{sig.nbytes / 1e6:.1f} MB), scale {args.scale:.0f}, peak {peak:.0f}/32767')
    print(f'TX LO           : {args.lo / 1e6:.6f} MHz')
    print(f'DRM carrier     : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer here, Demod = DRM)')


def describe_plan(args: argparse.Namespace) -> None:
    """Print the transmit plan when the caller gives no IQ file."""
    print('input           : (none; pass --iq <int16 stereo WAV>)')
    print(f'TX LO           : {args.lo / 1e6:.6f} MHz')
    print(f'DRM carrier     : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer here, Demod = DRM)')
    print(f'TX rate         : {args.rate:.0f} Hz, scale {args.scale:.0f}')


def transmit(args: argparse.Namespace) -> int:
    if args.iq is None:
        if not args.dry_run:
            raise SystemExit(
                'no IQ source: pass --iq <int16 stereo WAV>. Build one with\n'
                '  decdrm tx tools/bench/drm_bench/station_iq.toml --output drm_iq.wav --duration 60')
        describe_plan(args)
        print('\n--dry-run: planned from the parameters; pass --iq to build the samples.')
        return 0

    sig = build_signal(args.iq, args.rate, args.base_hz, args.scale, args.conj)
    _z, in_rate = load_iq(args.iq)
    describe(args, in_rate, sig)
    if args.dry_run:
        print('\n--dry-run: signal built, radio untouched.')
        return 0

    with radio.Radio(args) as tx:
        if tx.rate != int(args.rate):
            sig = build_signal(args.iq, tx.rate, args.base_hz, args.scale, args.conj)

        deadline = time.time() + args.seconds if args.seconds > 0 else None
        if not args.stream:
            # Cyclic DMA buffer: the Pluto repeats it on its own, so there are no host-side
            # gaps (chunked one-shot tx() underruns between buffers and DRM never locks).
            print(f'TX cyclic: lo={tx.sdr.tx_lo} rate={tx.rate} gain={args.gain} '
                  f'buffer={sig.size / tx.rate:.1f}s', flush=True)
            tx.push_cyclic(sig)
            try:
                while deadline is None or time.time() < deadline:
                    time.sleep(1.0)
            except KeyboardInterrupt:
                print('\ninterrupted')
            return 0

        tx.open_stream()
        chunk_n = tx.chunk_samples(args.chunk)
        print(f'TX on: lo={tx.sdr.tx_lo} rate={tx.rate} gain={args.gain} '
              f'chunk={args.chunk:.2f}s', flush=True)
        sent = 0
        idx = 0
        try:
            while deadline is None or time.time() < deadline:
                part = sig[idx:idx + chunk_n]
                if part.size < chunk_n:
                    part = np.concatenate([part, sig[:chunk_n - part.size]])
                tx.push_chunk(part)
                sent += 1
                idx = (idx + chunk_n) % sig.size
                if sent % 40 == 0:
                    print(f'  {sent} chunks ({sent * args.chunk:.0f}s)', flush=True)
        except KeyboardInterrupt:
            print('\ninterrupted')
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--iq', default=None,
                        help='int16 stereo IQ WAV (I left, Q right). Required to transmit; '
                             'without it the tool prints the plan only')
    parser.add_argument('--base-hz', type=float, default=DEFAULT_BASE_HZ,
                        help='DRM baseband offset from the LO (0 = on the LO, at LO leakage)')
    parser.add_argument('--chunk', type=float, default=0.25, help='streaming chunk in seconds')
    parser.add_argument('--stream', action='store_true',
                        help='chunked one-shot streaming instead of a cyclic buffer')
    parser.add_argument('--seconds', type=float, default=180.0, help='transmit seconds (0 = forever)')
    parser.add_argument('--conj', action='store_true',
                        help='conjugate the baseband (invert the spectrum) to match the TX/RX I/Q'
                             'convention')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-20.0, default_rate=DEFAULT_RATE,
                         dry_run_help='build and print, do not key the radio')
    return transmit(parser.parse_args(argv))


if __name__ == '__main__':
    sys.exit(main())
