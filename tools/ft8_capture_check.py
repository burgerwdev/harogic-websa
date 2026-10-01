#!/usr/bin/env python3
"""Capture the channelized baseband and ask two questions about it, slot by slot.

FT8 either decodes or it does not, and "it did not" has two very different causes: no transmission was
there to decode, or one was and the decoder missed it. This separates them.

  * the *spectrum* check looks for an FT8-shaped burst in each 15 s slot (a tone that stands well above
    the slot's own noise floor). A slot with no such tone cannot decode, whatever the decoder does;
  * the *decode* check runs the capture through the committed `dsp.wasm` with the same wrapper the
    browser worker uses (via `frontend/modern/src/__tests__/ft8Capture.test.ts`), so what is measured is
    the artifact that ships.

Usage:
    python3 tools/ft8_capture_check.py --frequency 7.080e6 --seconds 60
    # then, as the tool prints:
    WEBSA_FT8_IQ=/tmp/ft8_capture.json npx vitest run src/__tests__/ft8Capture.test.ts

Capture the window while the band is busy (a phone app decoding our audio is the ideal ground truth:
it proves a transmission is present in the same stream).
"""
from __future__ import annotations

import argparse
import asyncio
import json
import struct
import urllib.request
from pathlib import Path

import aiohttp
import numpy as np

HDR = struct.Struct('<4sIIIdd')     # magic, ver, seq, samples, rate, centre


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


async def capture(url: str, seconds: float) -> tuple[np.ndarray, float]:
    blocks: list[np.ndarray] = []
    rate = 0.0
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
            _magic, _ver, seq, samples, rate, _centre = HDR.unpack_from(data, 0)
            if seq == 0:
                blocks = []                       # the stream restarted: keep the newest capture
                continue
            blocks.append(np.frombuffer(data, dtype='<f4', count=samples * 2,
                                        offset=HDR.size).copy())
    if not blocks:
        raise SystemExit('no baseband arrived: is the service in SDR mode with the FT8 demodulator?')
    return np.concatenate(blocks), rate


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
    parser.add_argument('--decimate', type=int, default=32)
    parser.add_argument('--seconds', type=float, default=60.0)
    parser.add_argument('--out', default='/tmp/ft8_capture.json')
    args = parser.parse_args()

    post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(args.url, {'cmd': 'SET_SDR', 'center': args.frequency, 'decimate': args.decimate})
    post(args.url, {'cmd': 'SET_SDR_TUNE', 'listen': args.frequency})
    post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'ft8', 'ifbw': args.ifbw, 'pitch': 700,
                    'volume': 0.8, 'squelch': -140.0, 'agc': True})
    await asyncio.sleep(2.0)
    iq, rate = await capture(args.url, args.seconds)
    post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})

    path = Path(args.out).with_suffix('.iq')
    iq.astype(np.float32).tofile(path)
    Path(args.out).write_text(json.dumps({
        'iq_file': path.name, 'samples': int(iq.size // 2), 'fs_in': float(rate),
        'out_rate': 48000.0, 'center_hz': float(args.frequency), 'mode': 'ft8',
        'if_bw': float(args.ifbw), 'pitch': 700.0,
        'note': 'IQBF capture from tools/ft8_capture_check.py: the channelized baseband the browser '
                'decoder receives.',
    }, indent=1, sort_keys=True) + '\n')

    print(f'captured {iq.size // 2} complex samples ({iq.size / 2 / rate:.1f} s) at {rate:.1f} Hz -> {path}')
    print('slot-by-slot signal check (a burst is an FT8 tone; ~1.3x is noise):')
    for row in burst_per_slot(iq, rate):
        verdict = 'BURST' if row['ratio'] > 5.0 else 'noise'
        print(f"  slot {row['slot']:2d} t={row['seconds']:5.0f}s: {row['ratio']:6.2f}x @ {row['hz']:6.0f} Hz  {verdict}")
    print('decode it with the shipped artifact:')
    print(f'  WEBSA_FT8_IQ={args.out} npx vitest run src/__tests__/ft8Capture.test.ts')
    return 0


if __name__ == '__main__':
    raise SystemExit(asyncio.run(main()))
