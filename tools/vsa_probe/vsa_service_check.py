#!/usr/bin/env python3
"""Drive the *real* device through VSA mode and check the session contract.

Task-3 acceptance, hardware-in-the-loop (the service must be running and connected):

    python3 tools/vsa_probe/vsa_service_check.py                      # capture + stream
    python3 tools/vsa_probe/vsa_service_check.py --depth 262144
    python3 tools/vsa_probe/vsa_service_check.py --expect-dbm -25     # with a known source

It checks, in order:
  1. `SET_MODE {mode:'vsa'}` makes `STATUS.mode` `vsa` and publishes a `vsa` request block.
  2. A complete capture frame arrives with **no packet shortfall**: the frame delivered at
     least the requested depth and ``health.err`` stayed 0. A capture view keeps arming the
     next frame, so ``busy`` stays true; ``progress`` reports the in-flight frame.
  3. The streaming view keeps producing frames (the panadapter refreshes at ~20 Hz).
  4. Leaving VSA restores the swept configuration (centre/span before the run).
  5. Optionally, the measured peak matches ``--expect-dbm`` within ``--tolerance`` dB.

Writes a JSON record next to this file (untracked, like the other probe outputs).
"""
from __future__ import annotations

import argparse
import asyncio
import json
import math
import pathlib
import sys
import time

import aiohttp

OUT = pathlib.Path(__file__).with_name('vsa_service_check.json')


async def post(session, base, payload):
    async with session.post(f'{base}/api/config', json=payload) as response:
        return response.status, await response.json()


async def state(session, base):
    async with session.get(f'{base}/api/state') as response:
        return await response.json()


async def set_mode(session, base, mode):
    status, body = await post(session, base, {'cmd': 'SET_MODE', 'mode': mode})
    if status != 200:
        raise RuntimeError(f'SET_MODE {mode} -> {status} {body}')
    return body


async def wait_for_frame(session, base, *, timeout, depth=0, previous=None):
    """Poll STATUS until a complete frame is published; return (state, samples_seen)."""
    deadline = time.monotonic() + timeout
    seen: list[dict] = []
    while time.monotonic() < deadline:
        now = await state(session, base)
        vsa = now.get('vsa', {})
        last = vsa.get('last') or {}
        seen.append({'progress': round(float(vsa.get('progress', 0.0)), 3),
                     'busy': bool(vsa.get('busy')),
                     'frames': (vsa.get('health') or {}).get('frames'),
                     'samples': last.get('samples')})
        if last and last != previous and (not depth or int(last.get('samples') or 0) >= depth):
            return now, seen
        await asyncio.sleep(0.05)
    raise TimeoutError(f'no frame within {timeout:.1f}s '
                       f'(last vsa={json.dumps((await state(session, base)).get("vsa", {}))[:300]})')


def check(condition, message, problems):
    print(('ok   ' if condition else 'FAIL ') + message)
    if not condition:
        problems.append(message)


async def run(args) -> int:
    headers = {'Authorization': f'Bearer {args.token}'} if args.token else {}
    problems: list[str] = []
    record: dict = {'depth': args.depth, 'view': None, 'started': time.time()}

    async with aiohttp.ClientSession(headers=headers) as session:
        before = await state(session, args.http_url)
        if not before.get('connected'):
            print('SAN device is not connected', file=sys.stderr)
            return 2
        kept = {'center': before['center'], 'span': before['span'], 'mode': before['mode']}

        # 1. mode switch
        await set_mode(session, args.http_url, 'vsa')
        now = await state(session, args.http_url)
        check(now.get('mode') == 'vsa', f"STATUS.mode == 'vsa' (got {now.get('mode')!r})", problems)
        vsa = now.get('vsa', {})
        check(bool(vsa), 'STATUS carries the vsa request block', problems)
        # The view is mode-private state: it keeps the user's last choice across mode
        # switches (MODE_STATE_FLOW), so only its domain is asserted here; the run forces
        # `capture` below and checks that it took.
        check(vsa.get('view') in ('capture', 'stream'),
              f"view is a known view (got {vsa.get('view')!r})", problems)
        record['entry_view'] = vsa.get('view')

        # 2. capture one frame, no shortfall
        status, body = await post(session, args.http_url,
                                  {'cmd': 'SET_VSA', 'center': args.center,
                                   'decimate': args.decimate, 'view': 'capture',
                                   'depth': args.depth, 'measure': args.measure})
        check(status == 200, f'SET_VSA accepted ({status})', problems)
        t0 = time.monotonic()
        now, progress = await wait_for_frame(session, args.http_url, timeout=args.timeout,
                                            depth=args.depth)
        elapsed = time.monotonic() - t0
        vsa = now.get('vsa', {})
        actual = dict(vsa.get('actual') or {})
        health = dict(vsa.get('health') or {})
        packet_samples = int(actual.get('packet_samples') or 0)
        expected = math.ceil(args.depth / packet_samples) if packet_samples else 0
        depth_read = int((vsa.get('last') or {}).get('samples') or 0)
        check(packet_samples > 0, f"device reported packet_samples={packet_samples}", problems)
        check(actual.get('depth') == args.depth,
              f"actual depth == requested ({actual.get('depth')} vs {args.depth})", problems)
        # "No shortfall": the frame delivered at least the requested depth (a packet is the
        # device's granularity, so a little more is expected, never less).
        check(depth_read >= args.depth,
              f'captured {depth_read} of {args.depth} requested samples (no shortfall)', problems)
        if depth_read > args.depth:
            print(f'     frame over-delivered {depth_read - args.depth} samples '
                  f'(< one packet = {packet_samples})')
        check(int(health.get('err', -1)) == 0, f"no packet errors (health.err={health.get('err')})",
              problems)
        check(int(health.get('ok', 0)) >= expected,
              f"packets ok {health.get('ok')} >= expected {expected}", problems)
        check(int(actual.get('packets') or 0) == expected,
              f"packets planned {actual.get('packets')} == expected {expected}", problems)
        advance = [p['progress'] for p in progress]
        check(any(p > 0 for p in advance),
              f'progress advanced while the frame transferred ({advance[:8]})', problems)
        check(bool(vsa.get('busy')),
              'a capture view keeps arming the next frame (busy stays true)', problems)
        check(int(health.get('frames', 0)) >= 1, f"frames published: {health.get('frames')}",
              problems)
        ratio = elapsed / (args.depth / float(actual.get('iq_rate') or 1.0))
        print(f'     elapsed {elapsed:.2f}s vs signal {args.depth / float(actual.get("iq_rate") or 1.0):.2f}s '
              f'(x{ratio:.2f})')
        check(vsa.get('view') == 'capture', 'SET_VSA put the session in the capture view',
              problems)
        peak = float((vsa.get('last') or {}).get('peak_dbm', float('nan')))
        print(f'     frame in {elapsed:.2f}s, peak {peak:.1f} dBm, '
              f'floor {(vsa.get("last") or {}).get("floor_dbm")}')
        record['capture'] = {'elapsed_s': elapsed, 'actual': actual, 'health': health,
                             'last': vsa.get('last'), 'progress': progress[:40],
                             'measure': args.measure}
        print(f'     measure {args.measure}: ' + json.dumps(
            {k: v for k, v in (vsa.get('last') or {}).items()
             if k not in ('peak_dbm', 'floor_dbm', 'peak_hz')})[:220])

        # 3. streaming view
        status, _ = await post(session, args.http_url,
                               {'cmd': 'SET_VSA', 'view': 'stream',
                                'measure': 'spectrum'})
        check(status == 200, f'SET_VSA view=stream accepted ({status})', problems)
        first = (await state(session, args.http_url)).get('vsa', {}).get('last')
        now, _ = await wait_for_frame(session, args.http_url, timeout=args.timeout,
                                      previous=first)
        vsa = now.get('vsa', {})
        check(not vsa.get('busy'), 'streaming does not report busy', problems)
        check(bool(vsa.get('last')) and vsa.get('last') != first,
              'the streaming view keeps refreshing its result', problems)
        record['stream'] = {'actual': vsa.get('actual'), 'health': vsa.get('health'),
                            'last': vsa.get('last')}

        # 4. leaving restores the swept configuration
        await set_mode(session, args.http_url, 'std')
        after = await state(session, args.http_url)
        check(after.get('mode') == 'std', f"STATUS.mode back to std (got {after.get('mode')!r})",
              problems)
        check(abs(after['center'] - kept['center']) < 1.0 and abs(after['span'] - kept['span']) < 1.0,
              f"swept centre/span restored ({after['center']:.3f}/{after['span']:.3f} vs "
              f"{kept['center']:.3f}/{kept['span']:.3f})", problems)

        # 5. optional level check against a known source
        if args.expect_dbm is not None:
            check(abs(peak - args.expect_dbm) <= args.tolerance,
                  f'peak {peak:.1f} dBm within {args.tolerance} dB of {args.expect_dbm} dBm',
                  problems)

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
    parser.add_argument('--token', default='')
    parser.add_argument('--center', type=float, default=100.2e6)
    parser.add_argument('--decimate', type=int, default=16)
    parser.add_argument('--measure', default='spectrum',
                        help='Tier 1 measurement to request (demod/vector.py)')
    parser.add_argument('--depth', type=int, default=1 << 17)
    parser.add_argument('--timeout', type=float, default=60.0)
    parser.add_argument('--expect-dbm', type=float, default=None)
    parser.add_argument('--tolerance', type=float, default=3.0)
    return parser.parse_args()


if __name__ == '__main__':
    raise SystemExit(asyncio.run(run(parse_args())))
