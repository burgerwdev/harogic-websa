#!/usr/bin/env python3
"""Transmit a DRM signal from the PlutoSDR, for a controlled DRM bench loop.

Prefer the unified entry: `python3 tools/pluto_tx.py drm [args]` for DRM30 or
`python3 tools/pluto_tx.py drmplus [args]` for DRM+ mode E.

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

IQ inputs:
  * `--iq PATH` — legacy DRM30 path: int16 stereo WAV (I left, Q right) from the
    DecDRM transmitter:

        decdrm tx station.toml --output drm_iq.wav --duration 60

    (a station with `format = "iq"` and the default `iq_swap` = I on the left, `-c 6`
    at the analyzer). Dream's file input only decoded the int16 WAV, not the float32
    one, so keep `sample_format = "int16"` in the station output.

  * `--iq-f32 PATH --iq-rate RATE` — committed interleaved little-endian complex
    float32 I/Q. This is the DRM+ source format, and also accepts the committed
    DRM30 end-to-end AAC fixture.

Committed sources (no external generator needed):
  * DRM30: `tests/fixtures/drm/drm_modeB_so3_48k_aac.f32`, 48 kHz, 6.0 s, mode B /
    SO3, 10 kHz, AAC-LC 24 kHz mono 1 kHz sine (README provenance).
  * DRM+: `tests/fixtures/drm/drm_modeE_so0_96k_aac.f32`, 96 kHz, 740 mode E symbols,
    AAC-LC 24 kHz mono 1 kHz sine. `drmplus` uses this file by default.

DRM+ seam policy:
  The committed Mode E fixture is 740 symbols long, not a whole number of 160-symbol
  superframes. The default takes the first 640 symbols (four complete superframes) and
  appends `--gap` seconds of silence (default 1.0 s). The Pluto repeats that buffer:
  each repetition is a finite, valid burst followed by clean silence, so the receiver
  must reacquire at the next burst. This is deliberately not a continuous station and
  does not claim continuous superframe/interleaver continuity across the seam.

Legacy DRM30 examples:

    python3 tools/pluto/pluto_drm_tx.py --iq drm_iq.wav --dry-run
    python3 tools/pluto/pluto_drm_tx.py --iq drm_iq.wav --lo 400e6 --gain -20 --seconds 120

Without any IQ source, the DRM30 tool prints the plan and stops. The DRM+ mode always
has its committed fixture, so it never runs without a source.

Then: SDR mode, Center/Listen = `LO + base_hz` (400.1 MHz), Demod = DRM (DRM+ for mode
E). Watch SNR/MER and raise `--gain` (or drop attenuation) until MER is comfortable.
"""
from __future__ import annotations

import argparse
import sys
import time
import wave
from fractions import Fraction
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_radio as radio  # noqa: E402

try:  # The fallback below keeps the fixture usable on hosts without SciPy.
    from scipy.signal import resample_poly as _scipy_resample_poly
except ImportError:  # pragma: no cover - depends on the host
    _scipy_resample_poly = None

#: Where the DRM baseband sits relative to the LO, clear of LO leakage.
DEFAULT_BASE_HZ = 100e3
#: The AD9363 TX sample-rate floor: below this the driver refuses.
DEFAULT_RATE = radio.MIN_TX_RATE_HZ
#: Bench LO in Hz. The AD9363 transmits above ~325 MHz only, so HF needs a different path.
DEFAULT_LO_HZ = 400e6

#: Committed fixtures and their rates. The README proves both formats and rates.
FIXTURES = Path(__file__).resolve().parents[2] / 'tests/fixtures/drm'
DRM30_F32_FIXTURE = FIXTURES / 'drm_modeB_so3_48k_aac.f32'
DRM30_F32_RATE = 48_000
DRMPLUS_F32_FIXTURE = FIXTURES / 'drm_modeE_so0_96k_aac.f32'
DRMPLUS_RATE = 96_000

#: Mode E geometry (ES 201 980): 216 FFT + 24 guard = 240 samples/symbol at 96 kHz.
MODE_E_SYMBOL_SAMPLES = 240
MODE_E_SYMBOLS_PER_FRAME = 40
MODE_E_FRAMES_PER_SUPERFRAME = 4
MODE_E_SUPERFRAME_SYMBOLS = 160
#: The committed fixture's tail is an incomplete superframe, so burst repeats are
#: separated by silence instead of being spliced together.
DRMPLUS_GAP_S = 1.0


class Source:
    """One loaded IQ source, already cut to the burst that will be repeated."""

    __slots__ = ('z', 'rate', 'path', 'kind', 'total_samples', 'total_symbols',
                 'burst_symbols', 'superframes', 'gap_s')

    def __init__(self, z: np.ndarray, rate: float, path: str, kind: str,
                 total_samples: int | None = None, total_symbols: int | None = None,
                 burst_symbols: int | None = None, superframes: int | None = None,
                 gap_s: float = 0.0):
        self.z = z
        self.rate = rate
        self.path = path
        self.kind = kind
        self.total_samples = total_samples if total_samples is not None else z.size
        self.total_symbols = total_symbols
        self.burst_symbols = burst_symbols
        self.superframes = superframes
        self.gap_s = gap_s


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


def load_iq_f32(path: str, rate: float) -> tuple[np.ndarray, float]:
    """Read interleaved little-endian float32 I/Q and normalise its peak to 1.0."""
    raw = np.fromfile(path, dtype='<f4')
    if raw.size == 0:
        raise SystemExit(f'{path}: empty float32 IQ file')
    if raw.size % 2:
        raise SystemExit(f'{path}: odd float32 value count; expected interleaved I/Q')
    if not np.isfinite(raw).all():
        raise SystemExit(f'{path}: non-finite sample in float32 IQ file')
    z = raw[0::2].astype(np.float64) + 1j * raw[1::2].astype(np.float64)
    peak = float(np.abs(z).max())
    if peak <= 0.0:
        raise SystemExit(f'{path}: all-zero float32 IQ file')
    # The committed fixtures are not scaled like the int16 WAV path: scale by peak so
    # --scale keeps its DAC meaning instead of overflowing int16.
    return (z / peak).astype(np.complex64), rate


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


def resample_fft(z: np.ndarray, fs_in: float, fs_out: float) -> np.ndarray:
    """Bandlimited complex resampling with an oversampled DFT.

    This is the fallback when `scipy.signal.resample_poly` is unavailable. It is a
    true bandlimited interpolation, not linear interpolation, which matters for the
    96 kHz mode E fixture: its carriers reach about 47 kHz, so dropped images and
    interpolation tilt would land in the DRM+ channel. The input is zero-padded so
    the periodic DFT does not wrap one burst edge into the other.
    """
    z = np.asarray(z, dtype=np.complex128)
    n = z.size
    n_out = max(1, int(round(n * fs_out / fs_in)))
    if n_out == n:
        return z.astype(np.complex64)
    pad = max(32, int(round(0.002 * fs_in)))
    zz = np.concatenate((np.zeros(pad, dtype=np.complex128), z,
                         np.zeros(pad, dtype=np.complex128)))
    n_pad = zz.size
    n_out_pad = max(1, int(round(n_pad * fs_out / fs_in)))
    spectrum = np.fft.fft(zz)
    out_spectrum = np.zeros(n_out_pad, dtype=np.complex128)
    keep = min(n_pad, n_out_pad)
    out_spectrum[:keep // 2 + 1] = spectrum[:keep // 2 + 1]
    if keep > 1:
        out_spectrum[-(keep - 1) // 2:] = spectrum[-(keep - 1) // 2:]
    out = np.fft.ifft(out_spectrum) * (n_out_pad / n_pad)
    start = int(round(pad * fs_out / fs_in))
    out = out[start:start + n_out]
    if out.size < n_out:
        out = np.concatenate((out, np.zeros(n_out - out.size, dtype=np.complex128)))
    return out.astype(np.complex64)


def resample_f32(z: np.ndarray, fs_in: float, fs_out: float) -> np.ndarray:
    """Resample committed complex-f32 IQ, preferring SciPy's rational polyphase filter."""
    if float(fs_in) == float(fs_out):
        return z.astype(np.complex64, copy=False)
    if (_scipy_resample_poly is not None and float(fs_in).is_integer()
            and float(fs_out).is_integer()):
        ratio = Fraction(int(fs_out), int(fs_in)).limit_denominator(100_000)
        return np.asarray(_scipy_resample_poly(z, ratio.numerator, ratio.denominator),
                          dtype=np.complex64)
    return resample_fft(z, fs_in, fs_out)


def prepare_source(args: argparse.Namespace) -> Source | None:
    """Load the selected source and apply the DRM+ burst policy."""
    if args.iq and args.iq_f32:
        raise SystemExit('give either --iq (int16 stereo WAV) or --iq-f32 (complex float32)')
    if args.iq and args.iq_rate is not None:
        raise SystemExit('--iq-rate is for --iq-f32; an int16 WAV carries its own rate')
    if args.iq and args.burst_symbols is not None:
        raise SystemExit('--burst-symbols is a DRM+ mode E option')
    if args.mode == 'drmplus' and args.iq:
        raise SystemExit('DRM+ mode E needs --iq-f32 complex baseband; --iq is the legacy '
                         'DRM30 WAV path')

    if args.iq:
        z, rate = load_iq(args.iq)
        gap = 0.0 if args.gap is None else args.gap
        return Source(z=z, rate=rate, path=args.iq, kind='int16 WAV',
                      total_samples=z.size, gap_s=gap)

    path = args.iq_f32
    rate = args.iq_rate
    if path is None and args.mode == 'drmplus':
        path = str(DRMPLUS_F32_FIXTURE)
        rate = DRMPLUS_RATE
    if path is None:
        return None
    if rate is None:
        raise SystemExit('--iq-f32 needs --iq-rate <source rate in Hz>')
    if rate <= 0:
        raise SystemExit(f'--iq-rate must be positive, got {rate}')

    z, rate = load_iq_f32(path, rate)
    total_samples = z.size
    total_symbols = None
    burst_symbols = None
    superframes = None
    if args.mode == 'drmplus':
        if int(rate) != DRMPLUS_RATE:
            raise SystemExit(f'DRM+ mode E is defined at {DRMPLUS_RATE} Hz complex baseband; '
                             f'{path} declares {rate:.0f} Hz')
        if z.size % MODE_E_SYMBOL_SAMPLES:
            raise SystemExit(f'{path}: {z.size} complex samples is not a whole number of '
                             f'{MODE_E_SYMBOL_SAMPLES}-sample mode E symbols')
        total_symbols = z.size // MODE_E_SYMBOL_SAMPLES
        complete = (total_symbols // MODE_E_SUPERFRAME_SYMBOLS) * MODE_E_SUPERFRAME_SYMBOLS
        burst_symbols = args.burst_symbols if args.burst_symbols is not None else complete
        if burst_symbols <= 0:
            raise SystemExit(f'{path}: no complete {MODE_E_SUPERFRAME_SYMBOLS}-symbol mode E '
                             f'superframe in {total_symbols} symbols')
        if burst_symbols % MODE_E_SUPERFRAME_SYMBOLS:
            raise SystemExit(f'--burst-symbols must be a multiple of '
                             f'{MODE_E_SUPERFRAME_SYMBOLS}, got {burst_symbols}')
        if burst_symbols > total_symbols:
            raise SystemExit(f'--burst-symbols {burst_symbols} exceeds the source '
                             f'({total_symbols} symbols)')
        z = z[:burst_symbols * MODE_E_SYMBOL_SAMPLES].copy()
        superframes = burst_symbols // MODE_E_SUPERFRAME_SYMBOLS
    elif args.burst_symbols is not None:
        raise SystemExit('--burst-symbols is a DRM+ mode E option')

    default_gap = DRMPLUS_GAP_S if args.mode == 'drmplus' else 0.0
    gap = args.gap if args.gap is not None else default_gap
    if gap < 0:
        raise SystemExit(f'--gap must be non-negative, got {gap}')
    if args.mode == 'drmplus' and gap <= 0:
        raise SystemExit('DRM+ mode E needs a positive --gap: the committed fixture is not '
                         'cyclic-superframe aligned, so repeats need silent separation')
    return Source(z=z, rate=rate, path=str(path), kind='interleaved float32',
                  total_samples=total_samples, total_symbols=total_symbols,
                  burst_symbols=burst_symbols, superframes=superframes, gap_s=gap)


def build_signal(source: Source, rate: float, base_hz: float, scale: float,
                 conj: bool = False) -> np.ndarray:
    if source.kind == 'int16 WAV':
        z = resample_linear(source.z, source.rate, rate)
    else:
        z = resample_f32(source.z, source.rate, rate)
    if conj:
        # The Pluto's direct-conversion I/Q convention can be opposite to the analyzer's DDC,
        # inverting the spectrum; Dream then sees a mirrored DRM signal and never locks.
        z = np.conj(z)
    if base_hz:
        # Shift the DRM baseband off DC (direct-conversion LO leakage lands on DC).
        n = np.arange(z.size, dtype=np.float64)
        z = z * np.exp(2j * np.pi * base_hz * n / rate)
    z = z * scale
    if source.gap_s:
        z = np.concatenate((z, np.zeros(int(round(source.gap_s * rate)), dtype=np.complex64)))
    return np.asarray(z, dtype=np.complex64)


def resampler_name(source: Source, rate: float) -> str:
    if float(source.rate) == float(rate):
        return 'none (source rate equals TX rate)'
    if source.kind == 'int16 WAV':
        return 'linear interpolation (legacy DRM30 path)'
    if _scipy_resample_poly is not None:
        return 'scipy.signal.resample_poly'
    return 'bandlimited FFT (scipy.signal unavailable)'


def describe(args: argparse.Namespace, source: Source, sig: np.ndarray, rate: float) -> None:
    peak = float(np.abs(sig).max()) if sig.size else 0.0
    source_seconds = source.z.size / source.rate
    total_seconds = source.total_samples / source.rate
    print(f'source          : {source.path} ({source.kind})')
    print(f'                  {source.rate:.0f} Hz, {source.total_samples} complex samples, '
          f'{total_seconds:.3f} s')
    if source.total_symbols is not None:
        print(f'mode E symbols  : {source.total_symbols} in source at '
              f'{MODE_E_SYMBOL_SAMPLES} samples/symbol '
              f'({MODE_E_SYMBOLS_PER_FRAME} symbols/frame, '
              f'{MODE_E_FRAMES_PER_SUPERFRAME} frames/superframe)')
    if source.burst_symbols is not None:
        print(f'burst           : {source.burst_symbols} symbols = {source.superframes} complete '
              f'superframes ({source.z.size} samples, {source_seconds:.3f} s)')
    else:
        print(f'burst           : complete source ({source.z.size} samples, '
              f'{source_seconds:.3f} s)')
    print(f'gap             : {source.gap_s:.3f} s silence after each burst')
    if args.mode == 'drmplus':
        print('bandwidth       : 100 kHz nominal (mode E, SO0)')
    elif Path(source.path).name == DRM30_F32_FIXTURE.name:
        print('bandwidth       : 10 kHz (committed mode B / SO3 AAC fixture)')
    else:
        print('bandwidth       : DRM30 source-defined (legacy int16 WAV path)')
    print(f'resampler       : {resampler_name(source, rate)}')
    print(f'TX rate         : requested {args.rate:.0f} Hz -> buffer {rate:.0f} Hz')
    print(f'TX baseband     : {sig.size} samples ({sig.size / rate:.3f} s), '
          f'{sig.nbytes / 1e6:.1f} MB, scale {args.scale:.0f}, '
          f'peak {peak:.0f}/32767')
    print(f'TX LO           : {args.lo / 1e6:.6f} MHz')
    print(f'DRM carrier     : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer here, Demod = {"DRM+" if args.mode == "drmplus" else "DRM"})')
    if args.mode == 'drmplus':
        print('reacquisition   : yes; every burst restarts at the fixture FAC frame 0 '
              'after the silent gap')
        print('station mode    : finite valid bursts (loop-safe, not a continuous station)')


def describe_plan(args: argparse.Namespace) -> None:
    """Print the transmit plan when the caller gives no IQ file."""
    print('input           : (none; pass --iq <int16 stereo WAV> or '
          '--iq-f32 <complex float32> --iq-rate <Hz>)')
    print(f'DRM30 fixture   : {DRM30_F32_FIXTURE} ({DRM30_F32_RATE} Hz)')
    print(f'DRM+ fixture    : {DRMPLUS_F32_FIXTURE} ({DRMPLUS_RATE} Hz, default for drmplus)')
    print(f'TX LO           : {args.lo / 1e6:.6f} MHz')
    print(f'DRM carrier     : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer here, Demod = DRM)')
    print(f'TX rate         : {args.rate:.0f} Hz, scale {args.scale:.0f}')


def transmit(args: argparse.Namespace) -> int:
    source = prepare_source(args)
    if source is None:
        if not args.dry_run:
            raise SystemExit(
                'no IQ source: pass --iq <int16 stereo WAV> or\n'
                '  --iq-f32 tests/fixtures/drm/drm_modeB_so3_48k_aac.f32 --iq-rate 48000\n'
                'The DRM30 WAV path expects a DecDRM file:\n'
                '  decdrm tx tools/bench/drm_bench/station_iq.toml --output drm_iq.wav '
                '--duration 60')
        describe_plan(args)
        print('\n--dry-run: planned from the parameters; pass an IQ source to build the samples.')
        return 0

    sig = build_signal(source, args.rate, args.base_hz, args.scale, args.conj)
    describe(args, source, sig, args.rate)
    if args.dry_run:
        print('\n--dry-run: signal built, radio untouched.')
        return 0

    with radio.Radio(args) as tx:
        if tx.rate != int(args.rate):
            sig = build_signal(source, tx.rate, args.base_hz, args.scale, args.conj)
            print(f'note: rebuilt at the radio rate {tx.rate} Hz '
                  f'({sig.size} samples/{sig.size / tx.rate:.3f} s)', flush=True)

        deadline = time.time() + args.seconds if args.seconds > 0 else None
        if not args.stream:
            # Cyclic DMA buffer: the Pluto repeats it on its own, so there are no host-side
            # gaps (chunked one-shot tx() underruns between buffers and DRM never locks).
            print(f'TX cyclic: lo={tx.sdr.tx_lo} rate={tx.rate} gain={args.gain} '
                  f'buffer={sig.size / tx.rate:.3f}s ({sig.size} samples)', flush=True)
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
    parser.add_argument('--mode', choices=('drm30', 'drmplus'), default='drm30',
                        help='DRM system; drmplus selects mode E and its committed fixture')
    parser.add_argument('--iq', default=None,
                        help='int16 stereo IQ WAV (I left, Q right). Legacy DRM30 path; '
                             'without a source the tool prints the plan only')
    parser.add_argument('--iq-f32', default=None, metavar='PATH',
                        help='interleaved little-endian complex float32 I/Q (DRM+ and '
                             'committed fixtures). Needs --iq-rate')
    parser.add_argument('--iq-rate', type=float, default=None, metavar='RATE',
                        help='source sample rate in Hz for --iq-f32')
    parser.add_argument('--burst-symbols', type=int, default=None,
                        help='DRM+ mode E only: complete superframe count to repeat '
                             '(default: all complete superframes in the source)')
    parser.add_argument('--gap', type=float, default=None,
                        help='silence after each burst in seconds (DRM+ default '
                             f'{DRMPLUS_GAP_S:.1f} and must be positive; DRM30 default 0)')
    parser.add_argument('--base-hz', type=float, default=DEFAULT_BASE_HZ,
                        help='DRM baseband offset from the LO (0 = on the LO, at LO leakage)')
    parser.add_argument('--chunk', type=float, default=0.25, help='streaming chunk in seconds')
    parser.add_argument('--stream', action='store_true',
                        help='chunked one-shot streaming instead of a cyclic buffer')
    parser.add_argument('--seconds', type=float, default=180.0, help='transmit seconds (0 = forever)')
    parser.add_argument('--conj', action='store_true',
                        help='conjugate the baseband (invert the spectrum) to match the TX/RX '
                             'I/Q convention')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-20.0,
                         default_rate=DEFAULT_RATE,
                         dry_run_help='build and print, do not key the radio')
    return transmit(parser.parse_args(argv))


if __name__ == '__main__':
    sys.exit(main())
