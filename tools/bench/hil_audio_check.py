#!/usr/bin/env python3
"""Capture hardware IQ and measure the production native receiver's float32 PCM.

    python3 tools/bench/hil_audio_check.py --mode cw --frequency 100.2e6 --seconds 2
    python3 tools/bench/hil_reference_check.py /tmp/hil_iq.json

The IQBFv2 capture remains reusable offline. sdr_decode processes that exact IQ;
its report supplies the actual PCM rate and channels for tone, SINAD and THD.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import struct
import sys
import time
from pathlib import Path

import aiohttp
import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from tools.bench.hardware_smoke import TinySaSource, safe_tinysa_level  # noqa: E402
from tools.bench.hil_reference_check import (  # noqa: E402
    DEFAULT_NATIVE_BIN,
    measure_audio,
    run_native,
)

BASEBAND_HEADER = struct.Struct('<4sIIIddIIQd')  # IQ wire v2, including magic


async def command(session: aiohttp.ClientSession, url: str, payload: dict) -> dict:
    async with session.post(f'{url}/api/config', json=payload, timeout=20) as response:
        return await response.json()


def configure_tinysa(
    source: TinySaSource,
    frequency_hz: float,
    level_dbm: float,
    modulation: str = 'cw',
    mod_freq_hz: float = 1_000.0,
    depth_percent: int = 50,
    deviation_hz: int = 3_000,
) -> None:
    """Bench-verified generator sequence (see the tinySA skill).

    The order and the single `output on` are the whole point: entering generator mode with
    `output off` first is what makes a later `output on` radiate, a second `output on` toggles the
    RF back off, and the level must be set *before* the output is enabled. The earlier version of
    this tool sent `output <level>`/`frequency`/`cw`, which the Ultra+ answers with usage text and
    ignores, so the analyzer captured a noise floor and the measurement was meaningless.

    `modulation` selects what the measurement can show: an unmodulated carrier (cw) exercises the CW
    demodulator, whose audio noise floor is set by the *combined phase noise* of the generator and
    the analyzer, while `am`/`fm` carry a tone that survives that noise.
    """
    for cmd in ('modulation off', 'output off', 'mode low output'):
        source.command(cmd)
    source.command(f'sweep cw {int(frequency_hz)}')
    source.command('wait')
    source.command(f'level {level_dbm:g}')
    if modulation == 'am':
        source.command('modulation am')
        source.command(f'modulation freq {int(mod_freq_hz)}')
        source.command(f'modulation depth {int(depth_percent)}')
    elif modulation == 'fm':
        source.command('modulation fm')
        source.command(f'modulation freq {int(mod_freq_hz)}')
        source.command(f'modulation deviation {int(deviation_hz)}')
    source.command('output on')
    source.command('resume')
    # Read the state back: a wedged generator accepts the commands and radiates nothing.
    print('tinySA state:', ' '.join(source.command('info').splitlines()[:2]))


async def capture(url: str, seconds: float) -> tuple[np.ndarray, float, float, dict]:
    """Collect IQBF frames (the channelized baseband) from the `?iq=1` socket for `seconds`.

    Only a *contiguous* run is usable for a measurement: a dropped frame is a phase discontinuity,
    and a demodulator fed one produces broadband splatter that looks like terrible SINAD while the
    tone is perfectly fine. Frames are therefore tracked by sequence number, split into runs at
    every gap, and the longest run is returned (with the gap count reported) instead of blindly
    concatenating whatever arrived.

    The DDC output is replayed through the production native Receiver offline.
    """
    runs: list[tuple[list[np.ndarray], float, float]] = []
    current: list[np.ndarray] = []
    rate = 0.0
    center = 0.0
    identity = None
    last_seq: int | None = None
    gaps = 0
    frames = 0
    deadline = time.monotonic() + seconds
    async with (
        aiohttp.ClientSession() as session,
        session.ws_connect(f'{url}/ws?iq=1', max_msg_size=32 * 1024 * 1024) as ws,
    ):
            while time.monotonic() < deadline:
                try:
                    message = await asyncio.wait_for(
                        ws.receive(), timeout=max(0.2, deadline - time.monotonic()))
                except asyncio.TimeoutError:
                    break
                if message.type != aiohttp.WSMsgType.BINARY:
                    continue
                data = message.data
                if len(data) < BASEBAND_HEADER.size or data[:4] != b'IQBF':
                    continue
                (_magic, version, seq, samples, frame_rate, frame_center, channel_id,
                 epoch, _input_index, _group_delay) = BASEBAND_HEADER.unpack_from(data, 0)
                if version != 2 or len(data) != BASEBAND_HEADER.size + samples * 8:
                    continue
                if not np.isfinite(frame_rate) or frame_rate <= 0:
                    raise ValueError('invalid IQBF sample rate')
                stream = (channel_id, epoch, frame_rate, frame_center)
                discontinuity = last_seq is not None and seq != (last_seq + 1) & 0xFFFFFFFF
                if seq == 0 or stream != identity or discontinuity:
                    if current:
                        runs.append((current, rate, center))
                    current = []
                    if seq != 0 and discontinuity:
                        gaps += 1
                identity, rate, center, last_seq = stream, frame_rate, frame_center, seq
                if seq == 0:
                    continue
                payload = np.frombuffer(data, dtype='<f4', count=samples * 2,
                                        offset=BASEBAND_HEADER.size)
                if not np.isfinite(payload).all():
                    raise ValueError('IQBF contains nonfinite samples')
                if samples:
                    frames += 1
                    current.append(payload.copy())
    if current:
        runs.append((current, rate, center))
    if not runs:
        raise SystemExit('no baseband arrived: is the service in SDR mode with the DSP socket open?')
    longest, rate, center = max(runs, key=lambda run: sum(block.size for block in run[0]) / run[1])
    stats = {
        'frames': frames,
        'gaps': gaps,
        'runs': len(runs),
        'run_frames': len(longest),
        'run_samples': int(sum(block.size for block in longest) // 2),
    }
    return np.concatenate(longest), rate, center, stats


async def run(args) -> int:
    url = args.url
    async with aiohttp.ClientSession() as session:
        # The generator first: a known tone at a level safe for the analyzer's input.
        if args.tinysa_port:
            source = TinySaSource(args.tinysa_port)
            level = safe_tinysa_level(args.frequency)
            configure_tinysa(source, args.frequency, level, modulation=args.modulation,
                             mod_freq_hz=args.mod_freq, deviation_hz=args.deviation)
            print(f'tinySA: {args.modulation.upper()} at {args.frequency/1e6:.4f} MHz, {level} dBm')

        # The demodulator has to match what the generator emits, or the measurement compares a tone
        # against the wrong detector.
        demod_mode = args.mode
        if args.modulation == 'am' and args.mode == 'auto':
            demod_mode = 'am'
        elif args.modulation == 'fm' and args.mode == 'auto':
            demod_mode = 'nfm'
        if demod_mode == 'auto':
            demod_mode = 'cw'

        state = await command(session, url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        print(f"mode: {state.get('mode')}")
        listen = args.frequency - args.pitch if demod_mode == 'cw' else args.frequency
        await command(session, url, {
            'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
            'freq': listen, 'mode': demod_mode, 'ifbw': args.ifbw,
            'volume': 1.0, 'audio_rate': 48_000, 'pitch': args.pitch,
        })
        await asyncio.sleep(2.0)                     # let the stream and the settle window pass
        baseband, rate, center, stats = await capture(url, args.seconds)
        await command(session, url, {'cmd': 'SET_MODE', 'mode': 'std'})

    iq_path = Path(args.out).with_suffix('.iq')
    iq_path.write_bytes(baseband.tobytes())
    meta = {
        'note': 'Captured IQBFv2 channelized float32 IQ; measured through the native sdr_decode receiver.',
        'iq_file': iq_path.name,
        'samples': int(baseband.size // 2),
        'fs_in': float(rate),
        'center_hz': float(center),
        'mode': demod_mode,
        'if_bw': float(args.ifbw),
        'pitch': float(args.pitch),
        'modulation': args.modulation,
        'mod_freq_hz': float(args.mod_freq),
        'expected_tone_hz': float(args.mod_freq) if args.modulation in ('am', 'fm') else float(args.pitch),
        'out_rate': 48_000.0,
        'tiny_sa_hz': float(args.frequency),
        'capture': stats,
    }
    Path(args.out).write_text(json.dumps(meta, indent=1, sort_keys=True) + '\n')
    print(f'captured {meta["samples"]} baseband samples at {rate/1e3:.2f} kSps -> {args.out}')
    print(f'capture integrity: {stats["frames"]} frames, {stats["gaps"]} gap(s), '
          f'longest contiguous run {stats["run_frames"]} frames / {stats["run_samples"]} samples')
    pcm, audio_rate, report = run_native(Path(args.out), args.native_bin)
    measurement = measure_audio('native receiver', pcm, audio_rate, meta['expected_tone_hz'])
    pcm_path = Path(args.out).with_suffix('.pcm.f32')
    pcm.astype('<f4').tofile(pcm_path)
    report['output_pcm']['path'] = str(pcm_path.resolve())
    print(f'native PCM: {pcm_path} ({pcm.shape[1]} channels at {audio_rate} Hz)')
    report_path = Path(args.out).with_suffix('.native.json')
    report_path.write_text(json.dumps(report, indent=1, sort_keys=True) + '\n')
    print(f'native decoder report: {report_path}')
    print(f'compare Python reference: python3 tools/bench/hil_reference_check.py {args.out} '
          f'--native-bin {args.native_bin}')
    failures = []
    for channel, result in enumerate(measurement['channels'], 1):
        if abs(result['tone_hz'] - meta['expected_tone_hz']) > args.tone_tolerance:
            failures.append(f'channel {channel}: tone outside {args.tone_tolerance:g} Hz tolerance')
        if result['sinad_db'] < args.min_sinad:
            failures.append(f'channel {channel}: SINAD below {args.min_sinad:g} dB')
        if result['thd_db'] > args.max_thd:
            failures.append(f'channel {channel}: THD above {args.max_thd:g} dB')
    for failure in failures:
        print(f'FAIL: {failure}')
    if failures:
        return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--tinysa-port', default='/dev/ttyACM0')
    parser.add_argument('--frequency', type=float, default=100.2e6)
    parser.add_argument('--mode', default='auto',
                        choices=['auto', 'cw', 'am', 'nfm', 'wfm', 'usb', 'lsb'])
    parser.add_argument('--modulation', default='cw', choices=['cw', 'am', 'fm'],
                        help='what the generator emits: an unmodulated carrier, or a 1 kHz tone')
    parser.add_argument('--mod-freq', type=float, default=1_000.0)
    parser.add_argument('--deviation', type=int, default=3_000)
    parser.add_argument('--ifbw', type=float, default=500.0)
    parser.add_argument('--pitch', type=float, default=700.0)
    parser.add_argument('--seconds', type=float, default=2.0)
    parser.add_argument('--out', default='/tmp/hil_iq.json')
    parser.add_argument('--native-bin', type=Path, default=DEFAULT_NATIVE_BIN)
    parser.add_argument('--min-sinad', type=float, default=10.0, help='minimum native SINAD in dB')
    parser.add_argument('--max-thd', type=float, default=-15.0, help='maximum native THD in dB')
    parser.add_argument('--tone-tolerance', type=float, default=10.0, help='maximum tone error in Hz')
    return asyncio.run(run(parser.parse_args()))


if __name__ == '__main__':
    raise SystemExit(main())
