#!/usr/bin/env python3
"""Capture the channelized baseband and ask two questions about it, slot by slot.

FT8 either decodes or it does not, and "it did not" has two very different causes: no transmission was
there to decode, or one was and the decoder missed it. This separates them.

  * the *spectrum* check looks for an FT8-shaped burst in each 15 s slot (a tone that stands well above
    the slot's own noise floor). A slot with no such tone cannot decode, whatever the decoder does;
  * the *decode* check replays exactly that IQ through the native production Receiver
    with sdr_decode, saves its structured events and float32 PCM, and requires the
    requested minimum number of decoded FT8 messages.

Usage:
    python3 tools/bench/ft8_capture_check.py --frequency 7.080e6 --seconds 60
    python3 tools/bench/ft8_reference_compare.py /tmp/ft8_capture.json

Capture a busy band; a second receiver decoding the same audio is useful ground truth.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import struct
import sys
import urllib.request
from pathlib import Path

import aiohttp
import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from tools.bench.ft8_reference_compare import ft8_texts  # noqa: E402
from tools.bench.hil_reference_check import DEFAULT_NATIVE_BIN, run_native  # noqa: E402

HDR = struct.Struct('<4sIIIddIIQd')  # IQ wire v2, including magic


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


async def capture(url: str, seconds: float) -> tuple[np.ndarray, float]:
    runs: list[tuple[list[np.ndarray], float]] = []
    blocks: list[np.ndarray] = []
    rate = 0.0
    identity = None
    last_seq = None
    async with (
        aiohttp.ClientSession() as session,
        session.ws_connect(f'{url}/ws?iq=1', max_msg_size=32 * 1024 * 1024) as ws,
    ):
        deadline = asyncio.get_running_loop().time() + seconds
        while asyncio.get_running_loop().time() < deadline:
            try:
                message = await asyncio.wait_for(ws.receive(), timeout=2)
            except asyncio.TimeoutError:
                continue
            if message.type != aiohttp.WSMsgType.BINARY:
                continue
            data = message.data
            if len(data) < HDR.size or data[:4] != b'IQBF':
                continue
            (_magic, version, seq, samples, frame_rate, centre, channel_id,
             epoch, _input_index, _group_delay) = HDR.unpack_from(data, 0)
            if version != 2 or len(data) != HDR.size + samples * 8:
                continue
            if not np.isfinite(frame_rate) or frame_rate <= 0:
                raise ValueError('invalid IQBF sample rate')
            stream = (channel_id, epoch, frame_rate, centre)
            if (seq == 0 or stream != identity
                    or (last_seq is not None and seq != (last_seq + 1) & 0xFFFFFFFF)):
                if blocks:
                    runs.append((blocks, rate))
                blocks = []
            identity, rate, last_seq = stream, frame_rate, seq
            if seq == 0:
                continue
            payload = np.frombuffer(data, dtype='<f4', count=samples * 2, offset=HDR.size)
            if samples and np.isfinite(payload).all():
                blocks.append(payload.copy())
            elif samples:
                raise ValueError('IQBF contains nonfinite samples')
    if blocks:
        runs.append((blocks, rate))
    if not runs:
        raise SystemExit('no baseband arrived: is the native service in SDR mode with channel 1 enabled?')
    longest, rate = max(runs, key=lambda run: sum(block.size for block in run[0]) / run[1])
    if len(runs) > 1:
        print(f'capture discontinuities: retained longest of {len(runs)} contiguous runs')
    return np.concatenate(longest), rate


def burst_per_slot(iq: np.ndarray, rate: float) -> list[dict]:
    """The strongest audio tone in each 15 s slot, relative to that slot's noise floor."""
    z = iq[0::2].astype(np.float64) + 1j * iq[1::2].astype(np.float64)
    window = int(rate * 0.16)                      # one FT8 symbol
    slot = int(rate * 15.0)
    out = []
    for index in range(len(z) // slot):
        segment = z[index * slot:(index + 1) * slot]
        peaks = []
        for start in range(0, len(segment) - window, window):
            spectrum = np.abs(np.fft.fft(segment[start:start + window] * np.hanning(window), 8192))[:4096]
            freqs = np.fft.fftfreq(8192, 1 / rate)[:4096]
            band = (freqs > 200) & (freqs < 3000)
            peaks.append((float(spectrum[band].max()), float(freqs[band][np.argmax(spectrum[band])])))
        floor = float(np.median([value for value, _ in peaks])) if peaks else 0.0
        best = max(peaks, default=(0.0, 0.0))
        out.append({
            'slot': index,
            'seconds': index * 15,
            # An FT8 tone stands far above the floor (measured: 10-100x); noise stays near 1.3x.
            'ratio': best[0] / floor if floor > 0 else 0.0,
            'hz': best[1],
        })
    return out


async def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--frequency', type=float, default=7.080e6)
    parser.add_argument('--ifbw', type=float, default=2400.0)
    parser.add_argument('--native-bin', type=Path, default=DEFAULT_NATIVE_BIN)
    parser.add_argument('--min-decodes', type=int, default=1,
                        help='minimum FT8 events required; use 0 for quiet-band diagnostics')
    parser.add_argument('--seconds', type=float, default=60.0)
    parser.add_argument('--out', default='/tmp/ft8_capture.json')
    args = parser.parse_args()

    post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(args.url, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                    'freq': args.frequency, 'mode': 'ft8', 'ifbw': args.ifbw,
                    'volume': 0.8, 'audio_rate': 48000})
    await asyncio.sleep(2.0)
    iq, rate = await capture(args.url, args.seconds)
    post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})

    path = Path(args.out).with_suffix('.iq')
    iq.astype(np.float32).tofile(path)
    Path(args.out).write_text(json.dumps({
        'iq_file': path.name, 'samples': int(iq.size // 2), 'fs_in': float(rate),
        'out_rate': 48000.0, 'center_hz': float(args.frequency), 'mode': 'ft8',
        'if_bw': float(args.ifbw), 'pitch': 700.0,
        'note': 'IQBFv2 float32 channelized IQ captured for the native production Receiver.',
    }, indent=1, sort_keys=True) + '\n')

    print(f'captured {iq.size // 2} complex samples ({iq.size / 2 / rate:.1f} s) at {rate:.1f} Hz -> {path}')
    print('slot-by-slot signal check (a burst is an FT8 tone; ~1.3x is noise):')
    for row in burst_per_slot(iq, rate):
        verdict = 'BURST' if row['ratio'] > 5.0 else 'noise'
        print(f"  slot {row['slot']:2d} t={row['seconds']:5.0f}s: {row['ratio']:6.2f}x @ {row['hz']:6.0f} Hz  {verdict}")
    pcm, audio_rate, report = run_native(Path(args.out), args.native_bin)
    pcm_path = Path(args.out).with_suffix('.pcm.f32')
    pcm.astype('<f4').tofile(pcm_path)
    report['output_pcm']['path'] = str(pcm_path.resolve())
    report_path = Path(args.out).with_suffix('.native.json')
    report_path.write_text(json.dumps(report, indent=1, sort_keys=True) + '\n')
    texts = ft8_texts(report)
    for text in texts:
        print(f'native FT8: {text}')
    print(f'native PCM: {pcm.shape[0]} frames, {pcm.shape[1]} channels at {audio_rate} Hz -> {pcm_path}')
    print(f'native decoder events: {report_path}')
    print(f'compare reference: python3 tools/bench/ft8_reference_compare.py {args.out} '
          f'--native-bin {args.native_bin}')
    if len(texts) < args.min_decodes:
        print(f'FAIL: {len(texts)} FT8 decodes; require at least {args.min_decodes}')
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(asyncio.run(main()))
