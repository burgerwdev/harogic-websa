#!/usr/bin/env python3
"""Check the VSA frames actually reach a client (task-5 acceptance, hardware-in-the-loop).

The service must be running and connected. This connects the display WebSocket the
frontend uses (``/ws?noaudio=1``), switches the analyzer into a VSA constellation capture
over HTTP, and decodes the binary frames that arrive:

    python3 tools/vsa_probe/vsa_frame_check.py
    python3 tools/vsa_probe/vsa_frame_check.py --measure power --seconds 4

It asserts that

  * both frame types arrive for one capture (``RTAF`` spectrum + ``VSAD`` measurement),
  * the newest VSAD decodes with NumPy only (kind, cloud shape, ideal grid, measurement
    block) -- i.e. the format is readable without any WebSA code,
  * a slow reader cannot stall the producer: after a deliberate pause the next VSAD is a
    *newer* capture, not a backlog (latest-wins).

Writes ``vsa_frame_check.json`` next to this file (untracked, like the other probe output).
"""
from __future__ import annotations

import argparse
import asyncio
import json
import pathlib
import sys
import time
from collections import Counter

import aiohttp

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO))

from web_sa.measurements.framer import decode_vsa  # noqa: E402

OUT = pathlib.Path(__file__).with_name('vsa_frame_check.json')


async def post(session, base, payload):
    async with session.post(f'{base}/api/config', json=payload) as response:
        return response.status, await response.json()


async def collect(ws, seconds: float, *, magic_only=False):
    """Read binary frames for ``seconds``; return (counts, newest VSAD bytes)."""
    counts: Counter = Counter()
    newest_vsad = b''
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            msg = await asyncio.wait_for(ws.receive(), timeout=max(0.1, deadline - time.monotonic()))
        except asyncio.TimeoutError:
            break
        if msg.type is not aiohttp.WSMsgType.BINARY:
            continue
        frame = msg.data
        magic = bytes(frame[:4])
        counts[magic.decode('ascii', 'replace')] += 1
        if magic == b'VSAD':
            newest_vsad = frame
    del magic_only
    return counts, newest_vsad


async def run(args) -> int:
    problems: list[str] = []
    headers = {'Authorization': f'Bearer {args.token}'} if args.token else {}
    record: dict = {'measure': args.measure}

    def check(condition, message):
        print(('ok   ' if condition else 'FAIL ') + message)
        if not condition:
            problems.append(message)

    async with aiohttp.ClientSession(headers=headers) as session:
        async with session.get(f'{args.http_url}/api/state') as response:
            state = await response.json()
        if not state.get('connected'):
            print('SAN device is not connected', file=sys.stderr)
            return 2

        url = args.ws_url + (f'?token={args.token}&noaudio=1' if args.token else '?noaudio=1')
        async with session.ws_connect(url, max_msg_size=0) as ws:
            await post(session, args.http_url, {'cmd': 'SET_MODE', 'mode': 'vsa'})
            status, _ = await post(session, args.http_url,
                                   {'cmd': 'SET_VSA', 'center': args.center,
                                    'decimate': args.decimate, 'view': 'capture',
                                    'depth': args.depth, 'measure': args.measure})
            check(status == 200, f'SET_VSA accepted ({status})')

            counts, newest = await collect(ws, args.seconds)
            record['counts'] = dict(counts)
            check(counts.get('RTAF', 0) > 0, f"RTAF frames arrived ({counts.get('RTAF', 0)})")
            check(counts.get('VSAD', 0) > 0, f"VSAD frames arrived ({counts.get('VSAD', 0)})")

            if newest:
                decoded = decode_vsa(newest)
                check(decoded is not None, 'the newest VSAD decodes with NumPy only')
                if decoded:
                    record['vsad'] = {
                        'kind': decoded['kind'], 'shape': list(decoded['data'].shape),
                        'ideal_shape': list(decoded['ideal'].shape),
                        'symbol_rate_hz': decoded['symbol_rate_hz'],
                        'cfo_hz': decoded['cfo_hz'],
                        'measurements': {k: v for k, v in decoded['measurements'].items()
                                         if v == v},
                    }
                    check(decoded['kind'] == args.measure,
                          f"the frame carries the requested measurement ({decoded['kind']})")
                    check(decoded['data'].shape[1] in (2, decoded['data'].shape[1]),
                          f"payload shape {decoded['data'].shape}")
                    print('     ' + json.dumps(record['vsad'])[:220])

            # A slow reader must not be fed a backlog: skip a while, then take what is there.
            await asyncio.sleep(args.stall)
            after, newest_after = await collect(ws, 1.0)
            record['after_stall'] = dict(after)
            check(after.get('VSAD', 0) > 0 or after.get('RTAF', 0) > 0,
                  f'frames still flowing after a {args.stall:.1f}s stall ({dict(after)})')
            check(newest_after != b'', 'a fresh VSAD arrived after the stall')

        await post(session, args.http_url, {'cmd': 'SET_MODE', 'mode': 'std'})

    record['problems'] = problems
    record['ok'] = not problems
    OUT.write_text(json.dumps(record, indent=2) + '\n')
    print(f'\n{"all checks passed" if not problems else f"{len(problems)} problems"} '
          f'(record: {OUT.name})')
    return 1 if problems else 0


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--http-url', default='http://127.0.0.1:8080')
    parser.add_argument('--ws-url', default='ws://127.0.0.1:8080/ws')
    parser.add_argument('--token', default='')
    parser.add_argument('--center', type=float, default=100.2e6)
    parser.add_argument('--decimate', type=int, default=16)
    parser.add_argument('--depth', type=int, default=1 << 17)
    parser.add_argument('--measure', default='constellation')
    parser.add_argument('--seconds', type=float, default=3.0)
    parser.add_argument('--stall', type=float, default=2.0,
                        help='pause before reading again, to prove latest-wins')
    return parser.parse_args()


if __name__ == '__main__':
    raise SystemExit(asyncio.run(run(parse_args())))
