#!/usr/bin/env python3
"""Performance baseline for the running service (report gap G-1).

Measures what the review previously only asserted: delivered frame rates per mode, the
mode-switch latency, the SDR audio frame rate and the worker process CPU. Results are
printed as JSON and can be compared against a recorded baseline:

    # record / update the baseline (bench machine, service running)
    python3 tools/bench.py --write-baseline tools/bench_baseline.json

    # regression check (CI-on-bench or before a release)
    python3 tools/bench.py --check tools/bench_baseline.json

Thresholds are deliberately loose (fps may not fall below 70 % of the baseline, the
switch may not take longer than 2x, CPU may not exceed 1.5x of it): the point is to catch
a structural regression, not to police machine noise. Absolute numbers depend on the host;
the baseline file records the host it was taken on.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import platform
import socket
import struct
import subprocess
import sys
import time
from contextlib import suppress
from pathlib import Path

import aiohttp

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.hardware_smoke import collect_rta, collect_swp, wait_status  # noqa: E402

FPS_FLOOR = 0.70          # delivered fps must stay above this fraction of the baseline
SWITCH_CEIL = 2.0         # mode switch may not take longer than this multiple
CPU_CEIL = 1.5            # worker CPU may not exceed this multiple


def worker_pid() -> int | None:
    try:
        out = subprocess.run(['pgrep', '-f', 'web_sa.main'],
                             capture_output=True, text=True).stdout.split()
    except Exception:
        return None
    return int(out[0]) if out else None


def cpu_seconds(pid: int | None) -> float | None:
    if pid is None:
        return None
    try:
        stat = Path(f'/proc/{pid}/stat').read_text().split()
        ticks = os.sysconf('SC_CLK_TCK')
        return (int(stat[13]) + int(stat[14])) / ticks
    except Exception:
        return None


class CpuMeter:
    """CPU seconds consumed by the worker between two marks."""

    def __init__(self) -> None:
        self.pid = worker_pid()
        self.start = cpu_seconds(self.pid)

    def since(self) -> float | None:
        now = cpu_seconds(self.pid)
        if self.start is None or now is None:
            return None
        return round(max(0.0, now - self.start), 3)


async def measure_audio(ws, seconds: float) -> dict:
    frames = 0
    samples = 0
    bad = 0
    started = time.monotonic()
    while time.monotonic() - started < seconds:
        try:
            message = await asyncio.wait_for(ws.receive(), timeout=3)
        except asyncio.TimeoutError:
            break
        if message.type != aiohttp.WSMsgType.BINARY:
            continue
        data = message.data
        if data[:4] != b'AUDF':
            continue
        if len(data) < 16:
            bad += 1
            continue
        _seq, _rate, count = struct.unpack_from('<III', data, 4)
        if len(data) != 16 + count * 2:
            bad += 1
            continue
        frames += 1
        samples += count
    elapsed = max(1e-6, time.monotonic() - started)
    return {'frames': frames, 'fps': round(frames / elapsed, 2), 'samples': samples, 'bad': bad}


async def measure_switch(ws, mode_payload: dict, timeout: float = 10.0) -> float | None:
    """Milliseconds from sending a mode command to the first data frame of that mode."""
    started = time.monotonic()
    await ws.send_json(mode_payload)
    deadline = started + timeout
    while time.monotonic() < deadline:
        message = await asyncio.wait_for(ws.receive(), timeout=3)
        if message.type == aiohttp.WSMsgType.BINARY and len(message.data) >= 4:
            return round((time.monotonic() - started) * 1000, 1)
    return None


async def post_config(session, base, payload) -> tuple[int, dict]:
    async with session.post(f'{base}/api/config', json=payload) as response:
        return response.status, await response.json()


async def configure_for_bench(session, base: str, args) -> dict:
    """Put the device into the fixed configuration the baseline was recorded with.

    Without this the measured frame rate depends on whatever the previous test left behind
    (points/span/RBW change the sweep time), which produced a false 2.5x "regression" the
    first time this comparison ran.
    """
    setup = [
        {'cmd': 'SET_MODE', 'mode': 'std'},
        {'cmd': 'SET_POINTS', 'points': args.points},
        {'cmd': 'SET_RBW', 'mode': 'auto'},
        {'cmd': 'SET_VBW', 'mode': 'equal'},
        {'cmd': 'SET_SWEEP', 'mode': 0},
        {'cmd': 'SET_SPUR', 'mode': 'bypass'},
        {'cmd': 'SET_WINDOW', 'window': 1},
        {'cmd': 'SET_DETECTOR', 'mode': 'auto'},
        {'cmd': 'SET_AMP', 'atten': -1, 'preamp': 0, 'ifgain': 2, 'gain_strategy': 0},
        {'cmd': 'SET_REF', 'mode': 'manual', 'ref': -30},
        {'cmd': 'SET_FREQ', 'center': args.frequency, 'span': args.span},
    ]
    for payload in setup:
        status, body = await post_config(session, base, payload)
        if status != 200:
            raise RuntimeError(f'bench setup {payload["cmd"]} failed: {status} {body}')
    async with session.get(f'{base}/api/state') as response:
        state = await response.json()
    return {
        'points_requested': args.points,
        'points_device': state.get('points'),
        'rbw_mode': state.get('rbw_mode'),
        'rbw_hz': state.get('rbw'),
        'ref_dbm': state.get('ref'),
        'atten': (state.get('amp') or {}).get('atten'),
        'spur': state.get('spur'),
    }


async def run(args) -> dict:
    result: dict = {
        'host': socket.gethostname(),
        'platform': platform.platform(),
        'python': platform.python_version(),
        'center_hz': args.frequency,
        'span_hz': args.span,
        'duration_s': args.duration,
    }
    token = f'?token={args.token}' if args.token else ''
    headers = {'Authorization': f'Bearer {args.token}'} if args.token else {}
    original = None
    try:
        async with aiohttp.ClientSession(headers=headers) as session:
            async with session.get(f'{args.http_url}/api/state') as response:
                original = await response.json()
                if not original.get('connected'):
                    raise RuntimeError('SAN device is not connected')
            mode = original.get('mode', 'std')
            if mode != 'std':
                await session.post(f'{args.http_url}/api/config',
                                   json={'cmd': 'SET_MODE', 'mode': 'std'})
                await asyncio.sleep(1.0)

            result['configuration'] = await configure_for_bench(session, args.http_url, args)
            await asyncio.sleep(0.5)

            async with session.ws_connect(f'{args.ws_url}/ws{token}',
                                          max_msg_size=32 * 1024 * 1024) as ws:
                await ws.send_json({'cmd': 'SET_FREQ', 'center': args.frequency, 'span': args.span})
                await wait_status(ws, lambda s: abs(s['span'] - args.span) < 1)

                meter = CpuMeter()
                swp = await collect_swp(ws, args.duration)
                swp['cpu_s'] = meter.since()
                result['swp'] = swp

                # RTA: time the mode switch itself, then set the window and measure.
                result['switch_to_rta_ms'] = await measure_switch(
                    ws, {'cmd': 'SET_MODE', 'mode': 'rta'})
                await ws.send_json({'cmd': 'SET_RTA', 'center': args.frequency,
                                    'span': min(args.span, 50.78125e6)})
                await wait_status(ws, lambda s: s.get('mode') == 'rta')
                meter = CpuMeter()
                result['rta'] = await collect_rta(ws, args.duration, time.monotonic())
                result['rta']['cpu_s'] = meter.since()

                # SDR: panadapter frames + audio frames on the same socket.
                result['switch_to_sdr_ms'] = await measure_switch(
                    ws, {'cmd': 'SET_MODE', 'mode': 'sdr'})
                await ws.send_json({'cmd': 'SET_SDR', 'center': args.frequency, 'decimate': 16})
                await wait_status(ws, lambda s: s.get('mode') == 'sdr')
                meter = CpuMeter()
                spec = await collect_rta(ws, args.duration, time.monotonic())
                audio = await measure_audio(ws, max(1.0, args.duration - 2))
                spec['cpu_s'] = meter.since()
                result['sdr'] = {
                    'frames': spec.get('frames', 0), 'fps': spec.get('fps', 0),
                    'bad_frames': spec.get('bad_frames', 0), 'points': spec.get('points', []),
                    'audio': audio, 'cpu_s': spec['cpu_s'],
                }
    finally:
        with suppress(Exception):
            if original is not None:
                async with aiohttp.ClientSession(headers=headers) as session:
                    await session.post(f'{args.http_url}/api/config',
                                       json={'cmd': 'SET_MODE', 'mode': 'std'})
                    await session.post(f'{args.http_url}/api/config',
                                       json={'cmd': 'SET_FREQ', 'center': original['center'],
                                             'span': original['span']})
    return result


def compare(current: dict, baseline: dict) -> list[str]:
    problems: list[str] = []

    def fps(mode: str) -> float:
        return float((current.get(mode) or {}).get('fps') or 0.0)

    for mode in ('swp', 'rta'):
        base = float((baseline.get(mode) or {}).get('fps') or 0.0)
        if base and fps(mode) < base * FPS_FLOOR:
            problems.append(f'{mode} fps {fps(mode)} < {base * FPS_FLOOR:.1f} '
                            f'({FPS_FLOOR:.0%} of baseline {base})')
    base_audio = float((baseline.get('sdr') or {}).get('audio', {}).get('fps') or 0.0)
    now_audio = float((current.get('sdr') or {}).get('audio', {}).get('fps') or 0.0)
    if base_audio and now_audio < base_audio * FPS_FLOOR:
        problems.append(f'sdr audio fps {now_audio} < {base_audio * FPS_FLOOR:.1f}')
    for key in ('switch_to_rta_ms', 'switch_to_sdr_ms'):
        base = baseline.get(key)
        now = current.get(key)
        if base and now and now > base * SWITCH_CEIL:
            problems.append(f'{key} {now} > {base * SWITCH_CEIL:.0f} (baseline {base})')
    for mode in ('swp', 'rta', 'sdr'):
        base = (baseline.get(mode) or {}).get('cpu_s')
        now = (current.get(mode) or {}).get('cpu_s')
        if base and now and now > base * CPU_CEIL:
            problems.append(f'{mode} cpu {now}s > {base * CPU_CEIL:.2f}s (baseline {base})')
    return problems


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--http-url', default='http://127.0.0.1:8080')
    parser.add_argument('--ws-url', default='ws://127.0.0.1:8080')
    parser.add_argument('--token', default='')
    parser.add_argument('--frequency', type=float, default=100.2e6)
    parser.add_argument('--span', type=float, default=10e6)
    parser.add_argument('--duration', type=float, default=5.0)
    parser.add_argument('--points', type=int, default=1000,
                        help='requested points for the measurement (fixed so runs compare)')
    parser.add_argument('--write-baseline', metavar='FILE')
    parser.add_argument('--check', metavar='FILE')
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    started = time.monotonic()
    result = asyncio.run(run(args))
    result['took_s'] = round(time.monotonic() - started, 1)
    print(json.dumps(result, indent=1))

    if args.write_baseline:
        Path(args.write_baseline).write_text(json.dumps(result, indent=1) + '\n')
        print(f'baseline written: {args.write_baseline}')
    if args.check:
        baseline = json.loads(Path(args.check).read_text())
        problems = compare(result, baseline)
        if problems:
            print('bench regression:', file=sys.stderr)
            for problem in problems:
                print('  ' + problem, file=sys.stderr)
            return 1
        print(f'bench OK (baseline {args.check})')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
