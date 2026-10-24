#!/usr/bin/env python3
"""Compare native FT8 events with ft8_lib on the same capture's native listener PCM.

    python3 tools/bench/ft8_capture_check.py --frequency 7.074e6 --seconds 150
    python3 tools/bench/ft8_reference_compare.py /tmp/ft8_capture.json

The native side invokes sdr_decode and reads structured Receiver events. The
reference runs tools/bench/ft8_reference/build.sh at time_osr=4, freq_osr=4.
Station and message parity must both hold. An empty reference is inconclusive;
use --allow-empty-reference only when inspecting a known quiet-band capture.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from tools.bench.hil_reference_check import DEFAULT_NATIVE_BIN, run_native  # noqa: E402


def write_wav(samples: np.ndarray, rate: int, path: Path) -> None:
    peak = max(float(np.max(np.abs(samples))), 1e-20)
    pcm = (samples / peak * 32_000).astype('<i2')
    with wave.open(str(path), 'wb') as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(pcm.tobytes())


def run_reference(samples: np.ndarray, rate: int) -> list[str]:
    # ft8_lib's buffer cap is 20 s. Stepping 15 s windows by <= 15 - 12.64 s
    # covers a complete burst regardless of its phase against the capture grid.
    window = int(rate * 15)
    step = int(rate * 2.25)
    starts = list(range(0, max(1, len(samples) - window + 1), step))
    last = max(0, len(samples) - window)
    if starts[-1] != last:
        starts.append(last)
    texts = []
    with tempfile.TemporaryDirectory(prefix='websa-ft8-reference-') as directory:
        wav = Path(directory) / 'window.wav'
        for start in starts:
            write_wav(samples[start:start + window], rate, wav)
            proc = subprocess.run([
                'bash', str(ROOT / 'tools' / 'bench' / 'ft8_reference' / 'build.sh'),
                str(wav), '4', '4', str(rate),
            ], capture_output=True, text=True, check=True)
            for line in proc.stdout.splitlines():
                if 'DECODED:' in line:
                    texts.append(' '.join(line.split('DECODED:', 1)[1].split()))
    return texts


def ft8_texts(report: dict) -> list[str]:
    texts = []
    for event in report['events']:
        if event.get('type') == 'ft8':
            text = event['text']
            if not isinstance(text, str) or not text.strip():
                raise ValueError(f'invalid FT8 event: {event}')
            texts.append(' '.join(text.split()))
    return texts


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path, help='JSON written by ft8_capture_check.py')
    parser.add_argument('--native-bin', type=Path, default=DEFAULT_NATIVE_BIN)
    parser.add_argument('--allow-empty-reference', action='store_true')
    args = parser.parse_args()
    meta = json.loads(args.capture.read_text())
    if meta['mode'] != 'ft8':
        parser.error('capture mode must be ft8')
    pcm, rate, report = run_native(args.capture, args.native_bin)
    # The ft8_lib harness holds at most 960k samples (20 s at 48 kHz).
    if rate > 48_000:
        parser.error('ft8 reference requires native PCM at 48000 Hz or below')
    samples = pcm.mean(axis=1)
    reference = run_reference(samples, rate)
    ours = ft8_texts(report)

    def stations(texts: list[str]) -> set[str]:
        return {part for text in texts for part in text.split() if any(c.isdigit() for c in part)}

    ref_stations, our_stations = stations(reference), stations(ours)
    print(f'capture: {args.capture} ({len(samples) / rate:.1f} s of native PCM at {rate} Hz)')
    print(f'ft8_lib reference (time_osr 4, freq_osr 4): {len(reference)} decodes, stations {sorted(ref_stations)}')
    print(f'websa native receiver: {len(ours)} decodes, stations {sorted(our_stations)}')
    missed = sorted(set(reference) - set(ours))
    extra = sorted(set(ours) - set(reference))
    if missed:
        print(f'messages the reference decoded and we did not: {missed}')
    if extra:
        print(f'messages we decoded and the reference did not: {extra}')
    if not reference:
        print('inconclusive: the reference decoded no transmissions in this capture')
        return 0 if args.allow_empty_reference else 1
    if not missed:
        print('parity: every reference message was decoded by the native receiver')
        return 0
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
