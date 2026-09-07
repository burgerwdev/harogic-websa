#!/usr/bin/env python3
"""Hardware smoke test for WebSA with optional safe TinySA CW control."""
from __future__ import annotations

import argparse
import asyncio
import json
import struct
import time
from contextlib import suppress

import aiohttp
import numpy as np


def safe_tinysa_level(frequency_hz: float) -> float:
    if frequency_hz <= 6e9:
        return -25.0
    if frequency_hz <= 7e9:
        return -35.0
    if frequency_hz <= 9e9:
        return -42.0
    raise ValueError('TinySA hardware smoke tests are limited to 9 GHz')


class TinySaSource:
    def __init__(self, port: str):
        import serial

        self.serial = serial.Serial(port, 115200, timeout=0.4, write_timeout=1)
        self.serial.reset_input_buffer()

    def command(self, command: str) -> str:
        self.serial.write((command + '\r\n').encode('ascii'))
        self.serial.flush()
        time.sleep(0.4)
        return self.serial.read(65535).decode('utf-8', 'replace')

    def identify(self) -> str:
        return self.command('info')

    @staticmethod
    def _require_ok(command: str, response: str) -> None:
        lowered = response.lower()
        if 'usage:' in lowered or 'error' in lowered:
            raise RuntimeError(f'TinySA rejected {command!r}: {response.strip()}')

    def enable_cw(self, frequency_hz: float, output_mode: str) -> float:
        level = safe_tinysa_level(frequency_hz)
        try:
            for command in (
                'output off',
                'level -42',
                'mode output',
                'output off',
                'level -42',
                f'output {output_mode}',
                'output off',
                'level -42',
                f'sweep cw {int(frequency_hz)}',
            ):
                self._require_ok(command, self.command(command))
            sweep_response = self.command('sweep')
            sweep_values = []
            for line in sweep_response.splitlines():
                fields = line.strip().split()
                if len(fields) >= 2 and fields[0].isdigit() and fields[1].isdigit():
                    sweep_values = [int(fields[0]), int(fields[1])]
                    break
            if len(sweep_values) != 2 or any(
                abs(value - frequency_hz) > 1 for value in sweep_values
            ):
                raise RuntimeError(
                    f'TinySA frequency verification failed: {sweep_response.strip()}')
            for command in (f'level {level:g}', 'output on', 'resume'):
                self._require_ok(command, self.command(command))
            status_response = self.command('status')
            if 'Resumed' not in status_response:
                raise RuntimeError(f'TinySA output did not resume: {status_response.strip()}')
        except Exception:
            with suppress(Exception):
                self.command('output off')
            raise
        return level

    def close(self, disable_output: bool) -> None:
        if disable_output:
            with suppress(Exception):
                self.command('output off')
        self.serial.close()


async def wait_status(ws, predicate, timeout: float = 15.0) -> dict:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        message = await asyncio.wait_for(ws.receive(), timeout=3)
        if message.type != aiohttp.WSMsgType.TEXT:
            continue
        payload = json.loads(message.data)
        if payload.get('cmd') == 'ERROR':
            raise RuntimeError(payload.get('msg', 'WebSA command failed'))
        if payload.get('cmd') == 'STATUS' and predicate(payload):
            return payload
    raise TimeoutError('timed out waiting for WebSA status')


async def collect_swp(ws, duration: float) -> dict:
    axes: dict[int, np.ndarray] = {}
    peaks: list[tuple[float, float, int]] = []
    bad_frames = 0
    started = time.monotonic()
    while time.monotonic() - started < duration:
        message = await asyncio.wait_for(ws.receive(), timeout=3)
        if message.type != aiohttp.WSMsgType.BINARY:
            continue
        frame = message.data
        if len(frame) < 16:
            bad_frames += 1
            continue
        magic = frame[:4]
        version, points, _sweep_ms = struct.unpack_from('<IIf', frame, 4)
        item_size = 8 if magic == b'FREQ' else 4
        if magic not in (b'FREQ', b'POWR') or len(frame) != 16 + points * item_size:
            bad_frames += 1
            continue
        if magic == b'FREQ':
            axes[version] = np.frombuffer(frame, dtype='<f8', offset=16, count=points).copy()
        elif version in axes:
            power = np.frombuffer(frame, dtype='<f4', offset=16, count=points)
            index = int(np.nanargmax(power))
            peaks.append((float(axes[version][index]), float(power[index]), int(points)))
    elapsed = time.monotonic() - started
    return summarize('swp', peaks, elapsed, bad_frames)


async def collect_rta(ws, duration: float, command_time: float) -> dict:
    peaks: list[tuple[float, float, int]] = []
    bad_frames = 0
    frame_sizes: set[int] = set()
    first_frame_at = None
    started = time.monotonic()
    deadline = started + 15 + duration
    while time.monotonic() < deadline:
        if first_frame_at is not None and time.monotonic() - first_frame_at >= duration:
            break
        message = await asyncio.wait_for(ws.receive(), timeout=3)
        if message.type != aiohttp.WSMsgType.BINARY or message.data[:4] != b'RTAF':
            continue
        frame = message.data
        if first_frame_at is None:
            first_frame_at = time.monotonic()
        if len(frame) < 32:
            bad_frames += 1
            continue
        _version, points, waterfall_len, _max_density, _start = struct.unpack_from(
            '<IIHHd', frame, 4)
        expected = 24 + points * 12 + waterfall_len * 2 + 8
        if points < 2 or len(frame) != expected:
            bad_frames += 1
            continue
        frequency = np.frombuffer(frame, dtype='<f8', offset=24, count=points)
        power = np.frombuffer(frame, dtype='<f4', offset=24 + points * 8, count=points)
        index = int(np.nanargmax(power))
        peaks.append((float(frequency[index]), float(power[index]), int(points)))
        frame_sizes.add(len(frame))
    elapsed = time.monotonic() - (first_frame_at or started)
    result = summarize('rta', peaks, elapsed, bad_frames)
    result['frame_bytes'] = sorted(frame_sizes)
    result['switch_ms'] = (
        round((first_frame_at - command_time) * 1000, 1) if first_frame_at else None)
    return result


def summarize(mode: str, peaks: list[tuple[float, float, int]], elapsed: float, bad: int) -> dict:
    if not peaks:
        return {'mode': mode, 'frames': 0, 'fps': 0, 'bad_frames': bad}
    return {
        'mode': mode,
        'frames': len(peaks),
        'fps': round(len(peaks) / elapsed, 2),
        'bad_frames': bad,
        'points': sorted({item[2] for item in peaks}),
        'peak_frequency_hz': round(sum(item[0] for item in peaks) / len(peaks), 1),
        'peak_power_dbm': round(sum(item[1] for item in peaks) / len(peaks), 3),
    }


async def run(args) -> dict:
    source = None
    source_enabled = False
    result: dict = {'center_hz': args.frequency, 'span_hz': args.span}
    headers = {'Authorization': f'Bearer {args.token}'} if args.token else {}
    ws_query = f'?token={args.token}' if args.token else ''
    original = None
    try:
        if args.tinysa_port:
            source = TinySaSource(args.tinysa_port)
            identity = source.identify()
            result['tinysa'] = next(
                (line.strip() for line in identity.splitlines() if 'tinySA ULTRA' in line),
                'TinySA detected',
            )
            if args.configure_tinysa:
                result['tinysa_level_dbm'] = source.enable_cw(
                    args.frequency, args.tinysa_output_mode)
                result['tinysa_output_mode'] = args.tinysa_output_mode
                source_enabled = True
                await asyncio.sleep(args.source_settle)

        async with aiohttp.ClientSession(headers=headers) as session:
            async with session.get(
                f'{args.http_url}/static/modern/dist/%2Fetc%2Fhostname') as response:
                result['static_escape_status'] = response.status
            async with session.get(f'{args.http_url}/api/state') as response:
                original = await response.json()
                if not original.get('connected'):
                    raise RuntimeError('SAN device is not connected')
                if original.get('mode') != 'std':
                    raise RuntimeError('hardware smoke test must start in std mode')

            async with session.ws_connect(
                f'{args.ws_url}/ws{ws_query}', max_msg_size=32 * 1024 * 1024) as ws:
                await ws.send_json({'cmd': 'NO_SUCH_COMMAND'})
                error = await wait_status_or_error(ws)
                result['invalid_command_error'] = error

                await ws.send_json({'cmd': 'SET_FREQ', 'center': args.frequency, 'span': args.span})
                await wait_status(ws, lambda status: abs(status['span'] - args.span) < 1)
                result['swp'] = await collect_swp(ws, args.duration)

                command_time = time.monotonic()
                await ws.send_json({'cmd': 'SET_MODE', 'mode': 'rta'})
                await ws.send_json({
                    'cmd': 'SET_RTA',
                    'center': args.frequency,
                    'span': min(args.span, 50.78125e6),
                })
                result['rta'] = await collect_rta(ws, args.duration, command_time)
    finally:
        # Use REST for restoration so cleanup still runs if the streaming socket failed.
        with suppress(Exception):
            if original is not None:
                async with aiohttp.ClientSession(headers=headers) as session:
                    await session.post(
                        f'{args.http_url}/api/config', json={'cmd': 'SET_MODE', 'mode': 'std'})
                    await session.post(
                        f'{args.http_url}/api/config',
                        json={
                            'cmd': 'SET_FREQ',
                            'center': original['center'],
                            'span': original['span'],
                        },
                    )
        if source is not None:
            source.close(disable_output=source_enabled)
    return result


async def wait_status_or_error(ws) -> str:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        message = await asyncio.wait_for(ws.receive(), timeout=3)
        if message.type != aiohttp.WSMsgType.TEXT:
            continue
        payload = json.loads(message.data)
        if payload.get('cmd') == 'ERROR':
            return payload.get('msg', '')
    raise TimeoutError('expected command error was not received')


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument('--http-url', default='http://127.0.0.1:8080')
    parser.add_argument('--ws-url', default='ws://127.0.0.1:8080')
    parser.add_argument('--token', default='')
    parser.add_argument('--frequency', type=float, default=1e9)
    parser.add_argument('--span', type=float, default=10e6)
    parser.add_argument('--duration', type=float, default=3.0)
    parser.add_argument('--tinysa-port', default='')
    parser.add_argument('--configure-tinysa', action='store_true')
    parser.add_argument('--tinysa-output-mode', choices=('normal', 'mixer'), default='normal')
    parser.add_argument('--source-settle', type=float, default=1.0)
    args = parser.parse_args()
    if args.configure_tinysa and not args.tinysa_port:
        parser.error('--configure-tinysa requires --tinysa-port')
    return args


def main() -> None:
    print(json.dumps(asyncio.run(run(parse_args())), indent=2))


if __name__ == '__main__':
    main()
