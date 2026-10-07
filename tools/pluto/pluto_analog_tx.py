#!/usr/bin/env python3
"""Transmit an analog test signal from a PlutoSDR, for the SDR demodulators.

Prefer the unified entry: `python3 tools/pluto_tx.py <mode> [args]`.

The tool synthesises a known signal and transmits it. The operator tunes the
analyzer to `--lo` and selects the matching demodulator. The default audio is a
1 kHz tone, the bench convention, so the decoded audio is easy to check.

Modes and the receiver they exercise:

  * `am`   - full-carrier amplitude modulation (envelope detector)
  * `dsb`  - double sideband with a residual carrier. The app detects DSB with
             the envelope detector, so the carrier must stay.
  * `usb`  - upper sideband: the tone sits above the dial
  * `lsb`  - lower sideband: the tone sits below the dial
  * `nfm`  - FM, narrowband (`--deviation-hz`, default 3 kHz)
  * `wfm`  - FM, wideband (`--deviation-hz`, default 75 kHz)
  * `pm`   - phase modulation (`--pm-index` radians)

The built-in tone runs for a whole number of audio cycles, so the cyclic buffer
joins without a click. `--wav FILE` sends a real audio file instead. This is the
easy way to judge the decoded audio quality: speech or music, not a single tone.
A WAV defaults to its whole length, is peak-normalised to 0.9, and loops when
`--duration` is set. It reads 8/16/24/32-bit PCM.

`--voice` applies the real-transmitter SSB chain to a `--wav`: a 300-2700 Hz
band-pass and a peak compressor. It matches what a voice SSB station sends, and
keeps the signal from pumping a receiver's AGC. Use it for USB/LSB voice tests.

    python3 tools/pluto_tx.py am  --dry-run
    python3 tools/pluto_tx.py nfm --lo 411e6 --gain -10
    python3 tools/pluto_tx.py usb --wav tools/bench/audio/speech.wav --seconds 60
    python3 tools/pluto_tx.py wfm --wav music.wav --channel left

On the analyzer: SDR mode, tune to `--lo`, set Demod to the mode, and check the
audio. For FM, select `NFM` or `WFM` to match `nfm` or `wfm`. Raise `--gain` until
the signal stands well out of the noise.
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

#: The modes this tool synthesises. `cw` has its own tool (keyed carrier).
MODES = ('am', 'dsb', 'usb', 'lsb', 'nfm', 'wfm', 'pm')
DEFAULT_TONE_HZ = 1000.0
DEFAULT_DURATION_S = 2.0
#: A --wav is normalised to this peak, so any file gives the right modulation.
DEFAULT_NORMALIZE_PEAK = 0.9
#: The SSB/voice band, the real-transmitter convention.
DEFAULT_VOICE_HP = 300.0
DEFAULT_VOICE_LP = 2700.0
#: Bench LO in Hz. Tune the analyzer here. The AD9363 transmits above ~325 MHz only.
DEFAULT_LO_HZ = 411e6
DEFAULT_AM_DEPTH = 0.5
#: Residual carrier for DSB. The envelope detector needs it.
DEFAULT_DSB_CARRIER = 0.5
DEFAULT_NFM_DEVIATION_HZ = 3000.0
DEFAULT_WFM_DEVIATION_HZ = 75000.0
DEFAULT_PM_INDEX = 1.0
#: A short ramp at the buffer edges. It stops a click when the file is not periodic.
EDGE_MS = 2.0


def load_audio(path: str, rate: float, channel: str = 'sum') -> np.ndarray:
    """Read a PCM WAV at the radio rate as one mono channel.

    Support 8/16/24/32-bit PCM. Mix to mono with `channel` (sum, left or right).
    """
    with wave.open(path, 'rb') as w:
        channels = w.getnchannels()
        in_rate = w.getframerate()
        width = w.getsampwidth()
        raw = w.readframes(w.getnframes())
    if width == 3:
        # 24-bit PCM: rebuild each little-endian signed sample from 3 bytes.
        b = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
        packed = b[:, 0] | (b[:, 1] << 8) | (b[:, 2] << 16)
        data = ((packed ^ 0x800000) - 0x800000).astype(np.float64) / float(1 << 23)
    else:
        dtype = {1: np.uint8, 2: '<i2', 4: '<i4'}.get(width)
        if dtype is None:
            raise SystemExit(f'{path}: unsupported PCM width {width * 8} bits '
                             f'(need 8/16/24/32-bit)')
        data = np.frombuffer(raw, dtype=dtype).astype(np.float64)
        if width == 1:
            data = data - 128.0
        data = data / float(1 << (8 * width - 1))
    data = data.reshape(-1, channels)
    if channels == 1 or channel == 'sum':
        data = data.mean(axis=1)
    elif channel == 'left':
        data = data[:, 0]
    else:  # right
        data = data[:, min(1, channels - 1)]
    n_out = int(round(data.size * rate / in_rate))
    pos = np.arange(n_out, dtype=np.float64) * (in_rate / rate)
    i0 = np.clip(pos.astype(np.int64), 0, max(0, data.size - 2))
    fr = (pos - i0).astype(np.float32)
    return (data[i0] * (1.0 - fr) + data[i0 + 1] * fr).astype(np.float32)


def fit_length(audio: np.ndarray, rate: float, duration_s: float | None,
               loop: bool) -> np.ndarray:
    """Match the audio to the cyclic-buffer length.

    `duration_s` None keeps the whole file. Otherwise loop the file to the
    length, or truncate/pad it when `loop` is False.
    """
    if duration_s is None:
        return audio
    n = max(1, int(round(rate * duration_s)))
    if audio.size >= n:
        return audio[:n]
    if not loop:
        return np.pad(audio, (0, n - audio.size))
    reps = int(np.ceil(n / max(1, audio.size)))
    return np.tile(audio, reps)[:n]


def apply_level(audio: np.ndarray, peak: float, gain_db: float,
                normalize: bool) -> np.ndarray:
    """Apply the audio gain and (optionally) normalise the peak to `peak`."""
    a = audio * (10.0 ** (gain_db / 20.0))
    if normalize and a.size:
        m = float(np.max(np.abs(a)))
        if m > 1e-9:
            a = a * (peak / m)
    return a.astype(np.float32)


def voice_band_pass(audio: np.ndarray, rate: float, lo: float, hi: float) -> np.ndarray:
    """Band-limit speech to `lo`..`hi` Hz, the SSB/voice convention.

    Real transmitters high-pass at ~300 Hz (the low rumble wastes power and makes
    the receiver's AGC pump) and low-pass at ~2700 Hz. The transform is a plain
    brick-wall on the spectrum; the audio is offline, so the edges do not matter.
    """
    n = audio.size
    m = _fft_len(n)
    spectrum = np.fft.rfft(audio, n=m)
    freqs = np.fft.rfftfreq(m, 1.0 / rate)
    spectrum[(freqs < lo) | (freqs > hi)] = 0.0
    return np.fft.irfft(spectrum, n=m)[:n].astype(np.float32)


def voice_compress(audio: np.ndarray, peak: float = 0.7, ratio: float = 4.0) -> np.ndarray:
    """Limit peaks above `peak` at `ratio`, so the voice keeps a steadier level.

    This is a simple peak limiter, not an AGC: it only acts on the loud samples,
    which is what keeps an SSB signal from pumping the receiver's AGC.
    """
    magnitude = np.abs(audio)
    over = magnitude > peak
    if not np.any(over):
        return audio
    out = audio.copy()
    out[over] = np.sign(audio[over]) * (peak + (magnitude[over] - peak) / ratio)
    return out


def _fft_len(n: int) -> int:
    """The next power of two >= n.

    numpy runs the radix-2 path only on smooth lengths. 6158220 = 2**2*3*5*197*521
    carries two large primes and falls back to Bluestein, about 4x slower; padding
    to a power of two (and truncating the result) removes that cost.
    """
    return 1 << max(0, (n - 1).bit_length())


def analytic(x: np.ndarray) -> np.ndarray:
    """Return the analytic signal of a real signal (a Hilbert transform by FFT)."""
    n = x.size
    m = _fft_len(n)
    spectrum = np.fft.fft(x, n=m)
    weights = np.zeros(m)
    weights[0] = 1.0
    if m % 2 == 0:
        weights[1:m // 2] = 2.0
        weights[m // 2] = 1.0
    else:
        weights[1:(m + 1) // 2] = 2.0
    return np.fft.ifft(spectrum * weights, n=m)[:n].astype(np.complex64)


def tone_audio(rate: float, tone_hz: float, duration_s: float) -> tuple[np.ndarray, int]:
    """An integer number of tone cycles, so the cyclic buffer joins cleanly."""
    cycles = max(1, int(round(duration_s * tone_hz)))
    n = int(round(cycles * rate / tone_hz))
    t = np.arange(n) / rate
    return np.cos(2.0 * np.pi * tone_hz * t).astype(np.float32), n


def apply_edge_ramp(sig: np.ndarray, rate: float) -> np.ndarray:
    ramp = int(round(rate * EDGE_MS / 1000.0))
    if ramp <= 1 or sig.size < 2 * ramp:
        return sig
    edge = 0.5 * (1.0 - np.cos(np.pi * np.arange(ramp) / ramp))
    sig[:ramp] *= edge
    sig[-ramp:] *= edge[::-1]
    return sig


def build_baseband(args: argparse.Namespace, rate: float) -> np.ndarray:
    if args.wav:
        audio = load_audio(args.wav, rate, args.channel)
        audio = fit_length(audio, rate, args.duration, not args.no_loop)
        if args.voice:
            audio = voice_band_pass(audio, rate, args.voice_hp, args.voice_lp)
            audio = voice_compress(audio)
        audio = apply_level(audio, args.normalize_peak, args.audio_gain_db,
                            not args.no_normalize)
    else:
        audio, _n = tone_audio(rate, args.tone_hz,
                               args.duration if args.duration else DEFAULT_DURATION_S)
        if args.audio_gain_db:
            audio = apply_level(audio, 1.0, args.audio_gain_db, False)
    n = audio.size
    t = np.arange(n) / rate

    if args.mode == 'am':
        base = (1.0 + args.am_depth * audio).astype(np.complex64)
    elif args.mode == 'dsb':
        base = (args.dsb_carrier + audio).astype(np.complex64)
    elif args.mode == 'usb':
        base = analytic(audio)
    elif args.mode == 'lsb':
        base = np.conj(analytic(audio))
    elif args.mode == 'pm':
        base = np.exp(1j * args.pm_index * audio).astype(np.complex64)
    else:  # nfm, wfm
        deviation = args.deviation_hz
        integral = np.cumsum(audio) / rate
        base = np.exp(1j * 2.0 * np.pi * deviation * integral).astype(np.complex64)

    if args.base_hz:
        base = base * np.exp(2j * np.pi * args.base_hz * t).astype(np.complex64)
    if args.wav:
        # The built-in tone already joins at the buffer wrap. A real file may not.
        base = apply_edge_ramp(base, rate)
    return (base * args.scale).astype(np.complex64)


def describe(args: argparse.Namespace, sig: np.ndarray) -> None:
    detail = ''
    if args.mode in ('am',):
        detail = f', depth {args.am_depth:.2f}'
    elif args.mode == 'dsb':
        detail = f', carrier {args.dsb_carrier:.2f}'
    elif args.mode in ('nfm', 'wfm'):
        detail = f', deviation {args.deviation_hz:.0f} Hz'
    elif args.mode == 'pm':
        detail = f', index {args.pm_index:.2f} rad'
    if args.wav:
        note = (f', peak-normalised to {args.normalize_peak:.2f}'
                if not args.no_normalize else '')
        if args.audio_gain_db:
            note += f', gain {args.audio_gain_db:+.1f} dB'
        source = f'{args.wav} ({args.channel}{note})'
    else:
        source = f'{args.tone_hz:.0f} Hz tone'
    print(f'mode           : {args.mode}{detail}')
    print(f'audio          : {source}')
    print(f'baseband       : {sig.size} samples ({sig.size / args.rate:.2f} s), '
          f'scale {args.scale:.0f}, peak {np.abs(sig).max():.0f}/32767')
    print(f'TX LO          : {args.lo / 1e6:.6f} MHz')
    print(f'RF signal      : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer here, Demod = {args.mode.upper()})')


def transmit(args: argparse.Namespace) -> int:
    mode = args.mode.lower()
    if mode not in MODES:
        raise SystemExit(f'unknown mode {args.mode!r}; choose from {", ".join(MODES)}')
    args.mode = mode
    print(f'building the {mode} signal ...', flush=True)
    sig = build_baseband(args, args.rate)
    describe(args, sig)
    if args.dry_run:
        print('\n--dry-run: synthesised and planned, radio untouched.')
        return 0

    with radio.Radio(args) as tx:
        if tx.rate != int(args.rate):
            print(f'note: rebuilding the signal for the actual sample_rate={tx.rate} Hz',
                  file=sys.stderr)
            sig = build_baseband(args, tx.rate)

        print(f'pushing {sig.size} samples ({sig.nbytes / 1e6:.1f} MB)...')
        tx.push_cyclic(sig)
        print(f'TX on: lo={tx.sdr.tx_lo} rate={tx.rate} gain={args.gain} '
              f'mode={mode} cyclic={sig.size / tx.rate:.2f}s', flush=True)
        deadline = time.time() + args.seconds if args.seconds > 0 else None
        try:
            while deadline is None or time.time() < deadline:
                time.sleep(1.0)
        except KeyboardInterrupt:
            print('\ninterrupted')
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('mode', choices=MODES, help='the modulation to synthesise')
    parser.add_argument('--tone-hz', type=float, default=DEFAULT_TONE_HZ,
                        help='audio tone frequency in Hz (ignored with --wav)')
    parser.add_argument('--wav', default=None,
                        help='send a PCM WAV instead of a tone (8/16/24/32-bit)')
    parser.add_argument('--channel', choices=('sum', 'left', 'right'), default='sum',
                        help='channel to take from a stereo WAV (default: sum)')
    parser.add_argument('--duration', type=float, default=None,
                        help='cyclic buffer seconds (default: the whole --wav, else 2 s)')
    parser.add_argument('--no-loop', action='store_true',
                        help='do not loop a --wav to fill --duration (truncate or pad)')
    parser.add_argument('--normalize-peak', type=float, default=DEFAULT_NORMALIZE_PEAK,
                        help='peak a --wav is normalised to (default 0.9)')
    parser.add_argument('--no-normalize', action='store_true',
                        help='keep the --wav level as-is (no peak normalisation)')
    parser.add_argument('--audio-gain-db', type=float, default=0.0,
                        help='extra audio gain in dB (applied after normalisation)')
    parser.add_argument('--voice', action='store_true',
                        help='SSB/voice processing on a --wav: 300-2700 Hz band-pass + peak '
                             'compression, the real-transmitter chain')
    parser.add_argument('--voice-hp', type=float, default=DEFAULT_VOICE_HP,
                        help='voice band-pass high-pass corner in Hz (default 300)')
    parser.add_argument('--voice-lp', type=float, default=DEFAULT_VOICE_LP,
                        help='voice band-pass low-pass corner in Hz (default 2700)')
    parser.add_argument('--base-hz', type=float, default=0.0,
                        help='extra offset of the whole signal from the LO (Hz)')
    parser.add_argument('--am-depth', type=float, default=DEFAULT_AM_DEPTH,
                        help='AM modulation depth (0..1)')
    parser.add_argument('--dsb-carrier', type=float, default=DEFAULT_DSB_CARRIER,
                        help='DSB residual carrier level (the envelope detector needs it)')
    parser.add_argument('--deviation-hz', type=float, default=None,
                        help='FM deviation peak in Hz (default: 3 kHz NFM, 75 kHz WFM)')
    parser.add_argument('--pm-index', type=float, default=DEFAULT_PM_INDEX,
                        help='PM modulation index in radians')
    parser.add_argument('--seconds', type=float, default=0.0,
                        help='transmit seconds (0 = until interrupted)')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-10.0,
                         dry_run_help='synthesise and print the plan without touching the radio')
    args = parser.parse_args(argv)
    if args.deviation_hz is None:
        args.deviation_hz = (DEFAULT_WFM_DEVIATION_HZ if args.mode == 'wfm'
                             else DEFAULT_NFM_DEVIATION_HZ)
    return transmit(args)


if __name__ == '__main__':
    raise SystemExit(main())
