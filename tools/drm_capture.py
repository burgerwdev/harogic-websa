#!/usr/bin/env python3
"""Capture the channelized baseband that the browser DSP worker receives.

Why this exists: the DRM receiver runs in the browser over the app's ``?iq=1`` WebSocket.
That stream carries the channelized baseband (the DSP_DDC output after the fine-tuning
NCO). This is the worker's exact input. A capture of it makes a live failure reproducible
offline, where a Rust test can iterate in seconds.

The tool does not open the device. It subscribes to the running SDR session, so the
service and the browser keep their settings. Set the app up first (SDR mode, demodulator,
center, listen, decimate), then capture.

Usage:
    python3 tools/drm_capture.py --seconds 12 \\
        --out tests/fixtures/drm/drm_live.f32 --json /tmp/drm_live.json

Exit status is non-zero when no frame arrives, so the command can gate a script.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import aiohttp
import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from web_sa.measurements.framer import BASEBAND_HEADER, MAGIC_BASEBAND  # noqa: E402

#: The WebSocket path that carries IQ only (the display connection carries no IQ).
IQ_PATH = '/ws?iq=1&noaudio=1'


def decode_baseband(buf: bytes) -> tuple[int, int, float, float, np.ndarray] | None:
    """Decode one IQBF frame into (version, seq, rate Hz, center Hz, interleaved IQ).

    Layout (see `web_sa/measurements/framer.py`): magic + version(u32) + seq(u32) +
    samples(u32) + rate(f64) + center_hz(f64) + float32[2 * samples].
    """
    if len(buf) < 4 or buf[:4] != MAGIC_BASEBAND or len(buf) < 4 + BASEBAND_HEADER.size:
        return None
    version, seq, samples, rate, center_hz = BASEBAND_HEADER.unpack_from(buf, 4)
    payload = 4 + BASEBAND_HEADER.size
    if len(buf) != payload + samples * 8:
        return None
    iq = np.frombuffer(buf, dtype='<f4', offset=payload, count=samples * 2)
    return int(version), int(seq), float(rate), float(center_hz), iq


def level_db(iq: np.ndarray) -> dict[str, float]:
    """Report the baseband level. Full scale is 1.0, so dBFS needs no calibration."""
    if iq.size == 0:
        return {'rms_dbfs': -200.0, 'peak_dbfs': -200.0, 'crest_db': 0.0}
    # Samples are interleaved I/Q, so both channels belong to the same complex vector.
    rms = float(np.sqrt(np.mean(np.square(iq, dtype=np.float64))))
    peak = float(np.max(np.abs(iq)))
    rms_db = 20.0 * np.log10(rms) if rms > 0 else -200.0
    peak_db = 20.0 * np.log10(peak) if peak > 0 else -200.0
    return {'rms_dbfs': round(rms_db, 2), 'peak_dbfs': round(peak_db, 2),
            'crest_db': round(peak_db - rms_db, 2)}


async def capture(url: str, seconds: float, max_bytes: int, settle: float = 1.0) -> dict:
    """Capture `seconds` of baseband after a `settle` drain.

    The drain matters: the backend sends the frames it queued while the socket was
    opening, so a capture that starts counting at once counts a burst as signal. The
    burst inflates the sample count by about 1.8x and makes the rate look wrong.
    """
    chunks: list[np.ndarray] = []
    rates: list[float] = []
    centers: list[float] = []
    seqs: list[int] = []
    samples = 0
    started = time.monotonic()
    drain_until = started + settle
    deadline = drain_until + seconds
    drained = 0
    ws_url = url.replace('http://', 'ws://').replace('https://', 'wss://').rstrip('/') + IQ_PATH
    timeout = aiohttp.ClientTimeout(total=None, sock_connect=10)
    async with (
        aiohttp.ClientSession(timeout=timeout) as session,
        session.ws_connect(ws_url, heartbeat=None) as ws,
    ):
        while time.monotonic() < deadline:
            try:
                message = await asyncio.wait_for(ws.receive(), timeout=max(
                    0.5, deadline - time.monotonic()))
            except asyncio.TimeoutError:
                break
            if message.type == aiohttp.WSMsgType.BINARY:
                frame = decode_baseband(message.data)
                if frame is None:
                    continue
                _version, seq, rate, center_hz, iq = frame
                if time.monotonic() < drain_until:
                    drained += 1
                    continue
                chunks.append(iq)
                rates.append(rate)
                centers.append(center_hz)
                seqs.append(seq)
                samples += iq.size // 2
                if samples * 8 >= max_bytes:
                    break
            elif message.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                break
    stopped = time.monotonic()
    wall = stopped - drain_until
    if not chunks:
        raise SystemExit(
            f'no IQ frame arrived in {seconds:.1f} s from {ws_url}\n'
            'The app publishes baseband only in SDR mode with a running session. '
            'Check `./status.sh` (mode must be `sdr`) and retry.'
        )
    iq = np.concatenate(chunks)
    rate = float(np.mean(rates))
    result = {
        'source': ws_url,
        'captured_at': datetime.now(timezone.utc).isoformat(timespec='seconds'),
        'frames': len(chunks),
        'drained_frames': drained,
        'samples': samples,
        'seconds_of_signal': round(samples / rate, 3) if rate else 0.0,
        'wall_seconds': round(wall, 2),
        'rate_hz_mean': round(rate, 4),
        'rate_hz_min': round(min(rates), 4),
        'rate_hz_max': round(max(rates), 4),
        'center_hz': round(float(np.mean(centers)), 3),
        'seq_first': seqs[0],
        'seq_last': seqs[-1],
        'seq_gaps': sum(1 for a, b in zip(seqs, seqs[1:], strict=False) if b > a + 1),
        'format': 'interleaved float32 I,Q (little-endian)',
    }
    result.update(level_db(iq))
    return {'iq': iq, 'meta': result}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--url', default='http://127.0.0.1:8080', help='app base URL')
    parser.add_argument('--seconds', type=float, default=12.0, help='capture seconds')
    parser.add_argument('--out', default='/tmp/drm_live.f32', help='output IQ file (raw f32)')
    parser.add_argument('--json', default='', help='metadata output path (default: <out>.json)')
    parser.add_argument('--max-bytes', type=int, default=512 * 1024 * 1024,
                        help='stop after this many payload bytes')
    parser.add_argument('--settle', type=float, default=1.0,
                        help='seconds to drain queued frames before the capture')
    args = parser.parse_args()

    captured = asyncio.run(capture(args.url, args.seconds, args.max_bytes, args.settle))
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    captured['iq'].tofile(out)
    meta_path = Path(args.json) if args.json else out.with_suffix(out.suffix + '.json')
    meta = captured['meta']
    meta['file'] = out.name
    meta_path.write_text(json.dumps(meta, indent=2) + '\n')
    print(json.dumps(meta, indent=2))
    print(f'\nwrote {out} ({out.stat().st_size / 1e6:.1f} MB) and {meta_path}', flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
