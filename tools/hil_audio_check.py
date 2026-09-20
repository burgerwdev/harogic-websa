#!/usr/bin/env python3
"""Hardware-in-the-loop audio check: real IQ from the analyzer, measured through the browser DSP.

The DSP moved into the browser, so the verification that matters is on real hardware: point the
analyzer at a signal from the TinySA, capture the raw IQ the browser receives, run it through the
*committed* WASM artifact, and measure the demodulated audio (SINAD and THD at the mode's expected
tone). The measurement runs in Node through the same TypeScript wrapper the worker uses, so what is
measured is the artifact that ships, not a re-implementation.

Usage:
    python3 tools/hil_audio_check.py --mode cw --frequency 100.2e6 --seconds 2
    # then: WEBSA_HIL_IQ=/tmp/hil_iq.json npx vitest run src/__tests__/hil.test.ts

The capture file is JSON (parameters) plus a raw int16 IQ sidecar, so it can be re-measured without
the bench.
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

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.hardware_smoke import TinySaSource, safe_tinysa_level  # noqa: E402

IQ_HEADER = struct.Struct('<4sIIIdd')     # magic, ver, seq, samples, rate, center


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
    """Collect IQDF frames from the `?iq=1` socket for `seconds`.

    Only a *contiguous* run is usable for a measurement: IQ is a continuous signal, so a dropped
    frame is a phase discontinuity, and a demodulator fed one produces broadband splatter that
    looks like terrible SINAD while the tone is perfectly fine. Frames are therefore tracked by
    sequence number, split into runs at every gap, and the longest run is returned (with the gap
    count reported) instead of blindly concatenating whatever arrived.
    """
    runs: list[list[np.ndarray]] = []
    current: list[np.ndarray] = []
    rate = 0.0
    center = 0.0
    last_seq: int | None = None
    gaps = 0
    frames = 0
    deadline = time.monotonic() + seconds
    async with aiohttp.ClientSession() as session:
        async with session.ws_connect(f'{url}/ws?iq=1', max_msg_size=32 * 1024 * 1024) as ws:
            while time.monotonic() < deadline:
                try:
                    message = await asyncio.wait_for(
                        ws.receive(), timeout=max(0.2, deadline - time.monotonic()))
                except asyncio.TimeoutError:
                    break
                if message.type != aiohttp.WSMsgType.BINARY:
                    continue
                data = message.data
                if len(data) < IQ_HEADER.size or data[:4] != b'IQDF':
                    continue
                _magic, _ver, seq, samples, rate, center = IQ_HEADER.unpack_from(data, 0)
                payload = np.frombuffer(data, dtype='<i2', count=samples * 2, offset=IQ_HEADER.size)
                frames += 1
                if seq == 0 or (last_seq is not None and seq != (last_seq + 1) & 0xFFFFFFFF):
                    # seq == 0 is the stream's own flush marker; anything else is a gap.
                    if current:
                        runs.append(current)
                    current = []
                    if seq != 0 and last_seq is not None:
                        gaps += 1
                last_seq = seq
                current.append(payload.astype(np.int16))
    if current:
        runs.append(current)
    if not runs:
        raise SystemExit('no IQ arrived: is the service in SDR mode with the DSP socket open?')
    longest = max(runs, key=len)
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
        await command(session, url, {'cmd': 'SET_SDR', 'center': args.frequency, 'decimate': args.decimate})
        await command(session, url, {'cmd': 'SET_SDR_TUNE', 'listen': args.frequency})
        await command(session, url, {
            'cmd': 'SET_SDR_DEMOD', 'mode': demod_mode, 'ifbw': args.ifbw,
            'pitch': args.pitch, 'volume': 1.0, 'squelch': -140.0, 'agc': True,
        })
        await asyncio.sleep(2.0)                     # let the stream and the settle window pass
        iq, rate, center, stats = await capture(url, args.seconds)
        await command(session, url, {'cmd': 'SET_MODE', 'mode': 'std'})

    iq_path = Path(args.out).with_suffix('.iq')
    iq_path.write_bytes(iq.tobytes())
    meta = {
        'note': 'Captured by tools/hil_audio_check.py from the SAN-90 IQ stream; measured by '
                'frontend/modern/src/__tests__/hil.test.ts through the committed dsp.wasm.',
        'iq_file': iq_path.name,
        'samples': int(iq.size // 2),
        'fs_in': float(rate),
        'center_hz': float(center),
        'mode': demod_mode,
        'if_bw': float(args.ifbw),
        'pitch': float(args.pitch),
        'modulation': args.modulation,
        'mod_freq_hz': float(args.mod_freq),
        'expected_tone_hz': float(args.mod_freq) if args.modulation in ('am', 'fm') else float(args.pitch),
        'out_rate': 48_000.0,
        'decimate': max(1, int(rate // (48_000 * 4))) if rate > 0 else 1,
        'tiny_sa_hz': float(args.frequency),
        'capture': stats,
    }
    Path(args.out).write_text(json.dumps(meta, indent=1, sort_keys=True) + '\n')
    print(f'captured {meta["samples"]} complex samples at {rate/1e6:.4f} MSps -> {args.out}')
    print(f'capture integrity: {stats["frames"]} frames, {stats["gaps"]} gap(s), '
          f'longest contiguous run {stats["run_frames"]} frames / {stats["run_samples"]} samples')
    print(f'measure with: WEBSA_HIL_IQ={args.out} npx vitest run src/__tests__/hil.test.ts')
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--tinysa-port', default='/dev/ttyACM0')
    parser.add_argument('--frequency', type=float, default=100.2e6)
    parser.add_argument('--decimate', type=int, default=32)
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
    return asyncio.run(run(parser.parse_args()))


if __name__ == '__main__':
    raise SystemExit(main())
