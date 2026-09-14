#!/usr/bin/env python3
"""Exercise every command in the table against a running service (hardware-in-the-loop).

A registry refactor can silently drop or mis-wire one command; the UI state regression only
touches the ones the UI uses. This sweep walks the whole table - including the mode-gated
ones - and also checks that the guard rails still reject what they must.

    python3 tools/command_sweep.py                 # all commands except CAL_REFCLK
    python3 tools/command_sweep.py --with-cal      # include the GNSS calibration (slow)

Exit status is non-zero if any command fails or any expected rejection is accepted.
"""
from __future__ import annotations

import argparse
import asyncio
import sys

import aiohttp

# (cmd, payload, mode-to-enter-first). The order matters: sessions are entered once and
# the commands that belong to them run while that session is active.
SWEEP: list[tuple[str, dict, str | None]] = [
    ('SET_FREQ', {'center': 100.2e6, 'span': 10e6}, None),
    ('SET_REF', {'mode': 'manual', 'ref': -30}, None),
    ('SET_RBW', {'mode': 'manual', 'rbw': 100e3}, None),
    ('SET_VBW', {'mode': 'manual', 'vbw': 100e3}, None),
    ('SET_SWEEP', {'mode': 0}, None),
    ('SET_POINTS', {'points': 1000}, None),
    ('SET_SPUR', {'mode': 'bypass'}, None),
    ('SET_WINDOW', {'window': 1}, None),
    ('SET_DETECTOR', {'mode': 'auto'}, None),
    ('SET_AMP', {'atten': -1, 'preamp': 0, 'ifgain': 2, 'gain_strategy': 0}, None),
    ('SET_REFCK', {'mode': 'internal'}, None),
    ('SET_REFCKOUT', {'on': False}, None),
    ('SET_PRESET', {}, None),
    ('SET_FREQ', {'center': 100.2e6, 'span': 10e6}, None),
    ('SET_MODE', {'mode': 'harmonic'}, None),
    ('SET_HARM', {'f0': 100e6, 'count': 3, 'span': 5e6}, 'harmonic'),
    ('SET_MODE', {'mode': 'pnm'}, None),
    ('SET_PNM', {'center': 100e6, 'threshold': -50, 'traceavg': 4, 'start': 1e3, 'stop': 1e6}, 'pnm'),
    ('SET_MODE', {'mode': 'rta'}, None),
    ('SET_RTA', {'center': 100.2e6, 'span': 10e6}, 'rta'),
    ('SET_RBW', {'mode': 'auto'}, 'rta'),
    ('SET_VBW', {'mode': 'equal'}, 'rta'),
    ('SET_SWEEP', {'mode': 2}, 'rta'),
    ('SET_TRIGGER', {'source': 'bus', 'edge': 'rising', 'level': -40}, 'rta'),
    ('SET_MODE', {'mode': 'sdr'}, None),
    ('SET_SDR', {'center': 100.2e6, 'decimate': 16}, 'sdr'),
    ('SET_SDR_TUNE', {'listen': 100.2e6}, 'sdr'),
    ('SET_SDR_DEMOD', {'mode': 'am', 'ifbw': 12000, 'squelch': -110, 'volume': 0.8,
                       'agc': True, 'pitch': 700, 'deemph_us': -1}, 'sdr'),
    ('SET_REF', {'mode': 'auto'}, 'sdr'),
    ('SET_MODE', {'mode': 'std'}, None),
]

# (cmd, payload, mode, expected error code)
REJECTIONS = [
    ('NOT_A_COMMAND', {}, 'std', 'unknown_command'),
    ('SET_RBW', {'mode': 'manual', 'rbw': 99e6}, 'std', 'above_max'),
    ('SET_FREQ', {}, 'std', 'freq_requires_pair'),
    ('SET_WINDOW', {'window': 1}, 'rta', 'swp_only'),
    ('SET_FREQ', {'center': 1e9, 'span': 1e6}, 'sdr', 'sdr_unsupported'),
    ('SET_RBW', {'mode': 'manual', 'rbw': 1e6}, 'harmonic', 'cmd_unavailable_measurement'),
]


async def post(session, base, payload):
    async with session.post(f'{base}/api/config', json=payload) as response:
        body = await response.json()
        return response.status, body


async def set_mode(session, base, mode):
    status, body = await post(session, base, {'cmd': 'SET_MODE', 'mode': mode})
    if status != 200:
        raise RuntimeError(f'SET_MODE {mode} failed: {status} {body}')


async def run(args) -> int:
    headers = {'Authorization': f'Bearer {args.token}'} if args.token else {}
    problems: list[str] = []
    active_mode = 'std'
    async with aiohttp.ClientSession(headers=headers) as session:
        async with session.get(f'{args.http_url}/api/state') as response:
            state = await response.json()
        if not state.get('connected'):
            print('SAN device is not connected', file=sys.stderr)
            return 2
        await set_mode(session, args.http_url, 'std')
        active_mode = 'std'

        if args.with_cal:
            status, body = await post(session, args.http_url,
                                      {'cmd': 'CAL_REFCLK', 'count': 3})
            if status != 200:
                problems.append(f'CAL_REFCLK -> {status} {body}')
            else:
                print('ok   CAL_REFCLK')

        swept = 0
        for cmd, payload, mode in SWEEP:
            if mode is not None and mode != active_mode:
                await set_mode(session, args.http_url, mode)
                active_mode = mode
            status, body = await post(session, args.http_url, {'cmd': cmd, **payload})
            if status != 200 or body.get('response_to') != cmd:
                problems.append(f'{cmd} ({active_mode}) -> {status} {body}')
                print(f'FAIL {cmd} ({active_mode}) -> {status} {body}')
            else:
                print(f'ok   {cmd} ({active_mode})')
                swept += 1
            if cmd == 'SET_MODE':
                active_mode = payload.get('mode', active_mode)

        for cmd, payload, mode, code in REJECTIONS:
            await set_mode(session, args.http_url, mode)
            active_mode = mode
            status, body = await post(session, args.http_url, {'cmd': cmd, **payload})
            if status != 400 or body.get('code') != code:
                problems.append(f'{cmd} in {mode}: expected {code}, got {status} {body}')
                print(f'FAIL {cmd} in {mode}: expected {code}, got {status} {body}')
            else:
                print(f'ok   rejected {cmd} in {mode} ({code})')

        await set_mode(session, args.http_url, 'std')
        await post(session, args.http_url,
                   {'cmd': 'SET_FREQ', 'center': state['center'], 'span': state['span']})

    print(f'\n{len(SWEEP)} commands swept ({len(problems)} problems)')
    if problems:
        for problem in problems:
            print('  ' + problem, file=sys.stderr)
        return 1
    print('all commands executed and all guard rails held')
    return 0


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--http-url', default='http://127.0.0.1:8080')
    parser.add_argument('--token', default='')
    parser.add_argument('--with-cal', action='store_true')
    return parser.parse_args()


if __name__ == '__main__':
    raise SystemExit(asyncio.run(run(parse_args())))
