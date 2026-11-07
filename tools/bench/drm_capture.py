#!/usr/bin/env python3
"""Capture one continuous channelized baseband stream for offline DRM diagnostics.

The app's compatibility ``?iq=1`` WebSocket carries IQBF after fine tuning and DDC.
Production receivers run in Rust. This capture is an offline reference input;
it is not a recording of every native receiver or its processing state.

The tool does not open the device. It subscribes to the running SDR session, so the
service and the browser keep their settings. Set the app up first (SDR mode, demodulator,
center, listen, decimate), then capture.

Usage:
    python3 tools/bench/drm_capture.py --seconds 12 \\
        --out /tmp/drm_live.f32 --json /tmp/drm_live.json

Exit status is non-zero when no frame arrives, so the command can gate a script.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import math
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

import aiohttp
import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tools.reference.framer import BASEBAND_HEADER, MAGIC_BASEBAND  # noqa: E402

#: The WebSocket path that carries IQ only (the display connection carries no IQ).
IQ_PATH = '/ws?iq=1&noaudio=1'


@dataclass(frozen=True)
class BasebandFrame:
    seq: int
    rate: float
    center_hz: float
    channel_id: int
    epoch: int
    input_index: int
    delay_samples: float
    iq: np.ndarray


def decode_baseband(buf: bytes) -> BasebandFrame | None:
    """Decode and validate one IQBF frame, preserving its identity and timing.

    Layout (see `tools/reference/framer.py`): 56-byte IQ wire v2 header,
    followed by float32[2 * samples].
    """
    if buf[:4] != MAGIC_BASEBAND:
        return None
    payload = 4 + BASEBAND_HEADER.size
    if len(buf) < payload:
        raise ValueError('truncated IQBF header')
    (version, seq, samples, rate, center_hz, channel_id, epoch,
     input_index, group_delay) = BASEBAND_HEADER.unpack_from(buf, 4)
    if version != 2 or len(buf) != payload + samples * 8 or (seq == 0) != (samples == 0):
        raise ValueError('invalid IQBF version, sample count or boundary')
    if (not all(math.isfinite(v) for v in (rate, center_hz, group_delay))
            or rate < 0 or (samples and rate == 0) or group_delay < 0):
        raise ValueError('invalid IQBF sample rate, frequency or delay')
    iq = np.frombuffer(buf, dtype='<f4', offset=payload, count=samples * 2)
    if not np.isfinite(iq).all():
        raise ValueError('nonfinite IQBF samples')
    return BasebandFrame(seq, rate, center_hz, channel_id, epoch, input_index, group_delay, iq)


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

    Drain startup traffic, then reject any stream change or sequence discontinuity.
    Wall duration measures receipt time; signal duration comes from sample count.
    """
    if (not math.isfinite(seconds) or seconds <= 0 or not math.isfinite(settle)
            or settle < 0 or not isinstance(max_bytes, int) or max_bytes < 8):
        raise ValueError('capture requires positive finite duration and byte budget')
    chunks: list[np.ndarray] = []
    first: BasebandFrame | None = None
    previous: BasebandFrame | None = None
    samples = 0
    drained = 0
    ws_url = url.replace('http://', 'ws://').replace('https://', 'wss://').rstrip('/') + IQ_PATH
    timeout = aiohttp.ClientTimeout(total=None, sock_connect=10)
    async with (
        aiohttp.ClientSession(timeout=timeout) as session,
        session.ws_connect(ws_url, heartbeat=None) as ws,
    ):
        drain_until = time.monotonic() + settle
        deadline = drain_until + seconds
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
                if time.monotonic() < drain_until:
                    drained += 1
                    continue
                if frame.seq == 0:
                    if first is not None:
                        raise ValueError('IQBF reset during capture')
                    continue
                if previous is not None:
                    geometry = ('channel_id', 'epoch', 'rate', 'center_hz', 'delay_samples')
                    if any(getattr(frame, key) != getattr(previous, key) for key in geometry):
                        raise ValueError('IQBF identity or geometry changed during capture')
                    if frame.seq != previous.seq + 1:
                        raise ValueError('IQBF sequence gap or duplicate during capture')
                    unknown = (1 << 64) - 1
                    if (frame.input_index != unknown and previous.input_index != unknown
                            and frame.input_index <= previous.input_index):
                        raise ValueError('IQBF input clock moved backwards during capture')
                if samples * 8 + frame.iq.nbytes > max_bytes:
                    break
                chunks.append(frame.iq)
                samples += frame.iq.size // 2
                if first is None:
                    first = frame
                previous = frame
                if samples * 8 == max_bytes:
                    break
            elif message.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                break
    stopped = time.monotonic()
    wall = stopped - drain_until
    if first is None or previous is None:
        raise SystemExit(
            f'no IQ frame arrived in {seconds:.1f} s from {ws_url}\n'
            'The app publishes baseband only in SDR mode with a running session. '
            'Check `./status.sh` (mode must be `sdr`) and retry.'
        )
    iq = np.concatenate(chunks)
    rate = first.rate
    result = {
        'source': ws_url,
        'captured_at': datetime.now(timezone.utc).isoformat(timespec='seconds'),
        'frames': len(chunks),
        'drained_frames': drained,
        'samples': samples,
        'seconds_of_signal': round(samples / rate, 3) if rate else 0.0,
        'wall_seconds': round(wall, 2),
        'rate_hz_mean': round(rate, 4),
        'rate_hz_min': round(rate, 4),
        'rate_hz_max': round(rate, 4),
        'center_hz': first.center_hz,
        'channel_id': first.channel_id,
        'epoch': first.epoch,
        'input_index_first': str(first.input_index),
        'delay_samples': first.delay_samples,
        'seq_first': first.seq,
        'seq_last': previous.seq,
        'seq_gaps': 0,
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
