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
  * `wfm`  - FM, wideband (`--deviation-hz`, default 75 kHz). `--rds-rt TEXT`
             adds RDS RadioText on the 57 kHz subcarrier of the composite
  * `pm`   - phase modulation (`--pm-index` radians)

The built-in tone runs for a whole number of audio cycles, so the cyclic buffer
joins without a click. `--wav FILE` sends a real audio file instead. This is the
easy way to judge the decoded audio quality: speech or music, not a single tone.
A WAV defaults to its whole length, is peak-normalised to 0.9, and loops when
`--duration` is set. It reads 8/16/24/32-bit PCM.

`--voice` applies the real-transmitter SSB chain to a `--wav`: a 300-2700 Hz
band-pass and a peak compressor. It matches what a voice SSB station sends, and
keeps the signal from pumping a receiver's AGC. Use it for USB/LSB voice tests.

`--rds-rt TEXT` turns a `wfm` transmission into a broadcast station: RDS Group 2A
RadioText, Group 0A PS name and an optional Group 4A clock group, in biphase BPSK on
a 57 kHz subcarrier. The pilot and the data share the 75 kHz deviation budget with
the audio. `--rds-ps` sets the 8-character station name; `--rds-pi`, `--rds-pty` and
`--rds-deviation-hz` set the code, the type and the injection (2 kHz by default).
Repeat `--rds-rt`, or separate the texts with commas, to rotate messages. A message
stays up for whole passes, at least two, because a receiver wants the repetition of a
segment; `--rds-rt-cycle-seconds` sets that hold. RDS is for `wfm` only, because
57 kHz does not fit a narrowband channel. Open Data, EON and the alternative
frequencies are not implemented; see `rds_encode.py`.

A pass is the 16 RadioText segments as one burst of 1.4 s, then four PS groups, so a
pass takes 1.75 s. A burst lets a receiver collect the whole text before a marginal
signal drops a segment. The 19 kHz pilot is part of the transmission: EN 50067
section 1.6 locks the subcarrier to its third harmonic, so a receiver that recovers
its RDS clock from the pilot needs it (`--pilot-deviation-hz`, 0 to leave it out).
The tool encodes the transmission as it goes, so a long hold costs no memory.
`--rds-buffer` builds one rotation and lets the device repeat it instead, which
cannot gap, for a host that is slow to feed the link.

    python3 tools/pluto_tx.py wfm --rds-rt 'CQ DE N0CALL, bench RDS test' --rds-ps 'BENCH 1'
    python3 tools/pluto_tx.py wfm --rds-rt 'RadioText' --rds-pi 0x1234 --rds-pty 10

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
from datetime import datetime, timedelta, timezone
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_radio as radio  # noqa: E402
import rds_encode as rds  # noqa: E402

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
#: RDS defaults: a recognisable bench code, PTY 0 (undefined), and the
#: conventional subcarrier injection, about 2 kHz of the 75 kHz WFM deviation.
DEFAULT_RDS_PI = 0x1234
DEFAULT_RDS_PTY = 0
#: The bench station name: neutral, and exactly the 8 characters Group 0A carries.
DEFAULT_RDS_PS = 'RDS TEST'
DEFAULT_RDS_DEVIATION_HZ = rds.DEFAULT_DEVIATION_HZ
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


def station_stream(args: argparse.Namespace) -> rds.StationStream:
    """The group stream of the station, from the operator settings.

    The clock group carries real time, so the stream needs a start. The local half-hour
    offset goes with it, as a station sends UTC plus its offset and the radio shows
    local time.
    """
    clock = None
    if args.rds_ct:
        offset = datetime.now().astimezone().utcoffset() or timedelta()
        clock = rds.Clock(datetime.now(timezone.utc),
                          rds.pass_seconds(args.rds_ps, args.rds_ps_groups),
                          int(round(offset.total_seconds() / 1800)))
    return rds.StationStream(args.rds_pi, args.rds_pty, args.rds_ps, args.rds_rt,
                             args.rds_rt_cycle_seconds, tp=args.rds_tp,
                             ab_toggle=not args.rds_ab_fixed, clock=clock,
                             ps_groups=args.rds_ps_groups)


def broadcast_composite(audio: np.ndarray, t: np.ndarray, args: argparse.Namespace,
                        deviation_hz: float, stream=None, peak: float | None = None
                        ) -> np.ndarray:
    """The WFM composite: audio, the 19 kHz pilot and the RDS subcarrier.

    The pilot and RDS do not modulate the carrier on their own. They are components
    of the composite, so they share the deviation budget with the audio, and the sum
    is scaled back to `deviation_hz` peak. `--rds-deviation-hz` and
    `--pilot-deviation-hz` are components of that budget; the audio takes the rest.
    `stream` carries the group sequence across the chunks of a live transmission.
    `peak` is the budget of the whole transmission: a value per chunk would step the
    deviation at every chunk boundary.
    """
    if not args.rds_rt and not args.pilot_deviation_hz:
        return audio
    if args.mode != 'wfm':
        raise SystemExit(f'--rds-rt needs wfm: RDS rides the 57 kHz subcarrier of the FM '
                         f'composite, and {args.mode} has no room for it')
    if getattr(args, 'no_audio', False):
        mpx = np.zeros(audio.shape, dtype=np.float64)
    else:
        mpx = audio.astype(np.float64)
    if args.pilot_deviation_hz:
        mpx = mpx + rds.pilot_wave(t, args.pilot_deviation_hz / deviation_hz)
    if args.rds_rt:
        if stream is None:
            stream = station_stream(args)
        mpx = mpx + rds.station_wave(t, stream, args.rds_deviation_hz / deviation_hz)
    if peak is None:
        peak = float(np.max(np.abs(mpx)))       # what fills the budget before scaling
        args.composite_peak = peak
    # The budget is a ceiling. A quieter composite (RDS alone, or a quiet wav) stays
    # as it is, and describe reports what really goes out.
    scale = 1.0 / peak if peak > 1.0 else 1.0
    args.composite_scale = scale
    return mpx * scale


def build_baseband(args: argparse.Namespace, rate: float) -> np.ndarray:
    if args.rds_rt and args.rds_buffer:
        # The device repeats the buffer, so it must hold the whole rotation. A shorter
        # buffer can air the first messages only.
        args.duration = max(args.duration or DEFAULT_DURATION_S,
                            len(args.rds_rt) * rds.hold_seconds(args.rds_rt_cycle_seconds,
                                                                args.rds_ps,
                                                                args.rds_ps_groups))
    if args.wav:
        audio = load_audio(args.wav, rate, args.channel)
        audio = fit_length(audio, rate, args.duration, not args.no_loop)
        if args.voice:
            audio = voice_band_pass(audio, rate, args.voice_hp, args.voice_lp)
            audio = voice_compress(audio)
        audio = apply_level(audio, args.normalize_peak, args.audio_gain_db,
                            not args.no_normalize)
    else:
        duration = args.duration if args.duration else DEFAULT_DURATION_S
        audio, _n = tone_audio(rate, args.tone_hz, duration)
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
        integral = np.cumsum(broadcast_composite(audio, t, args, deviation)) / rate
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
    print('audio          : ' + ('none: the pilot and RDS alone, the RDS test signal'
                                 if getattr(args, 'no_audio', False) else source))
    scale = getattr(args, 'composite_scale', 1.0)
    if args.rds_rt or args.pilot_deviation_hz:
        print(f'pilot          : {rds.PILOT_HZ / 1e3:.3f} kHz, deviation '
              f'{args.pilot_deviation_hz * scale:.0f} Hz '
              f'({args.pilot_deviation_hz * scale / args.deviation_hz * 100.0:.1f}% of '
              f'{args.deviation_hz:.0f} Hz)'
              + ('' if args.pilot_deviation_hz else ' - none: a RDS radio that derives its '
                                                   'clock from the pilot may not lock'))
    if args.rds_rt:
        hold = rds.hold_seconds(args.rds_rt_cycle_seconds, args.rds_ps, args.rds_ps_groups)
        more = f' + {len(args.rds_rt) - 1} more' if len(args.rds_rt) > 1 else ''
        names = rds.ps_names(args.rds_ps)
        named = any(name.strip() for name in names)
        shown_groups = (f'{args.rds_ps_groups} Group 0A group(s) per pass'
                        if args.rds_ps_groups != rds.PS_GROUPS_PER_PASS else '4 x Group 0A')
        print('ps             : ' + (', '.join(f'"{name}"' for name in names)
                                     + f' in {shown_groups}, no AF, the station-name '
                                       'cadence a receiver expects'
                                     + (f', rotating every {hold:.1f} s' if len(names) > 1 else '')
                                     if named else 'none: RadioText groups only'))
        print(f'rds            : PI {args.rds_pi:04X}, PTY {args.rds_pty}, RadioText '
              f'"{args.rds_rt[0]}"{more}, each held {hold:.1f} s, rotation '
              f'{len(args.rds_rt) * hold:.1f} s')
        print(f'flags          : TP={int(args.rds_tp)}'
              + (', Group 4A clock time once a minute' if args.rds_ct else ', no clock group')
              + (', A/B flag fixed at 0' if args.rds_ab_fixed
                 else ', A/B flag moves with each message (the standard)'))
        pattern = rds.group_pattern(args.rds_ps, args.rds_ps_groups)
        sequence = ' + '.join(f'{pattern.count(kind)} x {"0A" if kind == 0 else "2A"}'
                             for kind in dict.fromkeys(pattern))
        print(f'rds groups     : {sequence}, one RadioText pass (16 segments) '
              f'{rds.pass_seconds(args.rds_ps, args.rds_ps_groups):.1f} s, '
              + (f'one cyclic buffer of {len(args.rds_rt) * hold:.1f} s, '
                 f'{int(len(args.rds_rt) * hold * args.rate) * 4 / 1e6:.0f} MB, '
                 f'repeated by the device' if args.rds_buffer
                 else 'encoded as it goes, no buffer'))
        print(f'rds subcarrier : {rds.SUBCARRIER_HZ / 1e3:.3f} kHz, deviation '
              f'{args.rds_deviation_hz * scale:.0f} Hz '
              f'({args.rds_deviation_hz * scale / args.deviation_hz * 100.0:.1f}% of the '
              f'{args.deviation_hz / 1e3:.0f} kHz budget), 90 deg to the pilot third harmonic')
        audio_dev = 0.0 if getattr(args, 'no_audio', False) else args.deviation_hz * scale
        print(f'deviation      : audio {audio_dev / 1e3:.2f} kHz + pilot '
              f'{args.pilot_deviation_hz * scale / 1e3:.2f} kHz + rds '
              f'{args.rds_deviation_hz * scale / 1e3:.2f} kHz of a '
              f'{args.deviation_hz / 1e3:.0f} kHz budget')
    print(f'baseband       : {sig.size} samples ({sig.size / args.rate:.2f} s), '
          f'scale {args.scale:.0f}, peak {np.abs(sig).max():.0f}/32767')
    print(f'TX LO          : {args.lo / 1e6:.6f} MHz')
    print(f'RF signal      : {(args.lo + args.base_hz) / 1e6:.6f} MHz '
          f'(tune the analyzer here, Demod = {args.mode.upper()})')


#: Seconds of composite per push in the live broadcast path.
STREAM_CHUNK_S = 1.0


def broadcast_chunk(args: argparse.Namespace, rate: float, start: int, samples: int,
                    stream, phase: float, audio: np.ndarray | None = None,
                    peak: float | None = None) -> tuple[np.ndarray, float]:
    """One chunk of the live composite, with the FM carrier phase carried over.

    The group sequence continues across the chunks. The carrier phase passes from one
    chunk to the next, so the FM signal stays continuous at the joins.
    """
    index = np.arange(start, start + samples)
    t = index / rate
    if audio is None:
        chunk_audio = np.cos(2.0 * np.pi * args.tone_hz * t).astype(np.float32)
        if args.audio_gain_db:
            chunk_audio = apply_level(chunk_audio, 1.0, args.audio_gain_db, False)
    else:
        chunk_audio = audio[index % audio.size]      # a --wav, looped
    mpx = broadcast_composite(chunk_audio, t, args, args.deviation_hz, stream, peak)
    carrier = phase + 2.0 * np.pi * args.deviation_hz * np.cumsum(mpx) / rate
    base = (np.exp(1j * carrier) * args.scale).astype(np.complex64)
    if args.base_hz:
        base = base * np.exp(2j * np.pi * args.base_hz * t).astype(np.complex64)
    return base, float(carrier[-1] % (2.0 * np.pi))


def transmit_broadcast(args: argparse.Namespace) -> int:
    """Send the broadcast as it is generated, the way a station does."""
    stream = station_stream(args)
    peak = getattr(args, 'composite_peak', None)     # the plan's budget, for every chunk
    audio = None
    if args.wav:
        audio = load_audio(args.wav, args.rate, args.channel)
        if args.voice:
            audio = voice_band_pass(audio, args.rate, args.voice_hp, args.voice_lp)
            audio = voice_compress(audio)
        audio = apply_level(audio, args.normalize_peak, args.audio_gain_db,
                            not args.no_normalize)

    with radio.Radio(args) as tx:
        rate = tx.rate
        samples = max(1, int(round(rate * STREAM_CHUNK_S)))
        tx.open_stream()
        start, phase, slowest, wall0 = 0, 0.0, 0.0, time.time()
        deadline = time.time() + args.seconds if args.seconds > 0 else None
        print(f'streaming: lo={tx.sdr.tx_lo} rate={rate} gain={args.gain} mode=wfm, '
              f'{STREAM_CHUNK_S:.1f} s chunks', flush=True)
        try:
            while deadline is None or time.time() < deadline:
                begun = time.time()
                chunk, phase = broadcast_chunk(args, rate, start, samples, stream, phase, audio,
                                               peak)
                tx.push_chunk(chunk)
                slowest = max(slowest, time.time() - begun)
                start += samples
                # How far this host is ahead of the radio: the number that shows whether
                # the link keeps up. A push blocks when the device queue is full, which
                # means ahead, not a fault.
                ahead = start / rate - (time.time() - wall0)
                if ahead < 0.0:
                    print(f'note: {ahead:+.1f} s ahead of the radio: the host is not feeding the '
                          f'link fast enough, so the carrier drops out. Run with --rds-buffer '
                          f'(and a rotation that fits one) for a transmission that cannot gap, '
                          f'or stop whatever else is loading this machine', file=sys.stderr)
                if start % (5 * samples) == 0:
                    print(f'  {start / rate:.0f} s sent, {ahead:+.1f} s ahead, slowest chunk '
                          f'{slowest:.2f} s', flush=True)
        except KeyboardInterrupt:
            print('\ninterrupted')
    return 0


def transmit(args: argparse.Namespace) -> int:
    mode = args.mode.lower()
    if mode not in MODES:
        raise SystemExit(f'unknown mode {args.mode!r}; choose from {", ".join(MODES)}')
    args.mode = mode
    print(f'building the {mode} signal ...', flush=True)
    if mode == 'wfm' and args.rds_rt and not args.rds_buffer:
        # A station sends a continuous stream, so the plan is its first seconds and
        # the transmission generates the rest of it as it goes.
        describe(args, build_baseband(args, args.rate))
        if args.dry_run:
            print('\n--dry-run: synthesised and planned, radio untouched.')
            return 0
        return transmit_broadcast(args)
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

        print(f'pushing {sig.size} samples ({sig.nbytes / 1e6:.1f} MB, '
              f'{sig.size * 4 / 1e6:.1f} MB as the radio counts it)...')
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
    parser.add_argument('--rds-rt', action='append', metavar='TEXT', default=None,
                        help='wfm only: add RDS Group 2A RadioText, 64 characters maximum. '
                             'Repeat the option, or separate the texts with commas, to rotate '
                             'messages (see --rds-rt-cycle-seconds)')
    parser.add_argument('--rds-pi', type=lambda v: int(v, 0), default=DEFAULT_RDS_PI,
                        help='RDS PI code, decimal or 0x hex (default 0x1234)')
    parser.add_argument('--rds-ps', action='append', metavar='NAME', default=None,
                        help='PS station name, 8 characters, space-padded (default "%s"), '
                             'sent as four Group 0A groups after each RadioText burst. Repeat '
                             'the option, or separate names with commas, to rotate them on the '
                             'RadioText hold; a real station keeps its name fixed'
                             % DEFAULT_RDS_PS)
    parser.add_argument('--rds-ps-groups', type=int, default=rds.PS_GROUPS_PER_PASS,
                        help='PS groups per pass, 0 to 4 (default %d). Four keeps the name '
                             'current in one pass; fewer make the pass shorter and the name '
                             'completes over more passes' % rds.PS_GROUPS_PER_PASS)
    parser.add_argument('--rds-pty', type=int, default=DEFAULT_RDS_PTY,
                        help='RDS program type, 0..31 (default 0, undefined)')
    parser.add_argument('--rds-rt-cycle-seconds', type=float, default=None,
                        help='seconds each RadioText stays up, rounded up to whole passes '
                             f'(a pass is {rds.pass_seconds(DEFAULT_RDS_PS):.1f} s with a PS '
                             f'name). The minimum is {rds.DEFAULT_HOLD_PASSES} passes, the '
                             f'repetition a receiver wants; the default is '
                             f'{rds.DEFAULT_HOLD_S:.0f} s')
    parser.add_argument('--rds-deviation-hz', type=float, default=DEFAULT_RDS_DEVIATION_HZ,
                        help='RDS injection level as FM deviation in Hz (default 2000)')
    parser.add_argument('--pilot-deviation-hz', type=float, default=None,
                        help='19 kHz pilot injection in Hz (default: 9%% of the FM '
                             'deviation, 6.75 kHz at 75 kHz; 0 = no pilot). EN 50067 locks '
                             'the RDS subcarrier to the third harmonic of the pilot, so a '
                             'receiver that recovers its RDS clock from the pilot needs one')
    parser.add_argument('--rds-buffer', action='store_true',
                        help='wfm: build the rotation once and let the device repeat it, '
                             'instead of encoding as it goes. A repeated buffer cannot gap, but '
                             'it must hold the whole rotation: this unit takes about 28 s of '
                             'baseband (58 MB as the radio counts it)')
    parser.add_argument('--rds-ab-fixed', action='store_true',
                        help='keep the Group 2A A/B flag at 0 instead of moving it with each '
                             'message. The standard asks for the move, and a receiver uses it '
                             'to accept the new text. PiFmRds keeps it fixed')
    parser.add_argument('--rds-tp', action='store_true',
                        help='set the Traffic Program bit (default off). Some radios show '
                             'their program-info screen only for a TP station')
    parser.add_argument('--rds-ct', action='store_true',
                        help='send the Group 4A clock time once a minute, UTC plus the local '
                             'offset, so a radio can set its clock (default off)')
    parser.add_argument('--no-audio', action='store_true',
                        help='transmit the pilot and RDS only, no tone. This is the best RDS '
                             'test signal: a tone at full deviation spends the modulation the '
                             'radio has to carry on an empty 1 kHz, and the RDS subcarrier then '
                             'sits at 2%% of it, so segments drop and a receiver shows a '
                             'half-updated text. Without audio the RDS level is the same, but '
                             'nothing else is modulating')
    parser.add_argument('--seconds', type=float, default=0.0,
                        help='transmit seconds (0 = until interrupted)')
    radio.add_radio_args(parser, default_lo=DEFAULT_LO_HZ, default_gain=-10.0,
                         dry_run_help='synthesise and print the plan without touching the radio')
    args = parser.parse_args(argv)
    if args.rds_ps is not None:
        # An empty name is a request for no Group 0A at all, not a mistake.
        args.rds_ps = [part.strip() for value in args.rds_ps for part in value.split(',')]
    else:
        args.rds_ps = [DEFAULT_RDS_PS]
    if args.rds_rt is not None:
        args.rds_rt = [part.strip() for value in args.rds_rt for part in value.split(',')]
        args.rds_rt = [text for text in args.rds_rt if text]
        if not args.rds_rt:
            parser.error('--rds-rt needs a non-empty text')
        if not 0 <= args.rds_pty <= 31:
            parser.error('--rds-pty must be 0..31')
        if args.rds_rt_cycle_seconds is not None and args.rds_rt_cycle_seconds <= 0:
            parser.error('--rds-rt-cycle-seconds must be positive')
        if not 0 <= args.rds_pi <= 0xFFFF:
            parser.error('--rds-pi must be a 16-bit code')
    if not 0 <= args.rds_ps_groups <= rds.PS_CHARS // 2:
        parser.error('--rds-ps-groups must be 0..%d' % (rds.PS_CHARS // 2))
    if args.deviation_hz is None:
        args.deviation_hz = (DEFAULT_WFM_DEVIATION_HZ if args.mode == 'wfm'
                             else DEFAULT_NFM_DEVIATION_HZ)
    if args.pilot_deviation_hz is None:
        # Every real broadcast carries the pilot when it carries RDS.
        args.pilot_deviation_hz = (rds.DEFAULT_PILOT_FRACTION * args.deviation_hz
                                   if args.rds_rt else 0.0)
    if args.pilot_deviation_hz < 0:
        parser.error('--pilot-deviation-hz must not be negative')
    return transmit(args)


if __name__ == '__main__':
    raise SystemExit(main())
