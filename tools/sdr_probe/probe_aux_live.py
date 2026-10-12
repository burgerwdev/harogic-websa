#!/usr/bin/env python3
"""Live aux-DDC verification (G1'): run the real service against the real device,
enable aux channel 2, and assert IQAX frames arrive on ?iqaux=1 (and nowhere else).

Starts its own web_sa.main on a scratch port; stop any running service first.
Usage:  python3 tools/sdr_probe/probe_aux_live.py [--center 100e6] [--aux-freq 101e6]
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import struct
import sys
import time
from pathlib import Path

import aiohttp

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

PORT = int(os.environ.get('WEBSA_TEST_PORT', '8123'))
WS = f'ws://127.0.0.1:{PORT}/ws'


async def send(ws, payload):
    await ws.send_str(json.dumps(payload))


async def wait_connected(ws, timeout=30.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        msg = json.loads((await ws.receive()).data)
        if msg.get('cmd') == 'STATUS':
            if msg.get('connected'):
                return msg
            if 'Device open' in str(msg.get('last_error', '')):
                pass
    raise TimeoutError('device never connected')


async def drain_status(ws, seconds=2.0):
    """Collect messages for a while; return (last STATUS, non-STATUS messages)."""
    last = None
    others = []
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            raw = await asyncio.wait_for(ws.receive(), timeout=end - time.monotonic())
        except (asyncio.TimeoutError, TimeoutError):
            break
        if raw.type == aiohttp.WSMsgType.BINARY:
            continue
        if raw.type != aiohttp.WSMsgType.TEXT:
            break
        msg = json.loads(raw.data)
        if msg.get('cmd') == 'STATUS':
            last = msg
        else:
            others.append(msg)
    return last, others


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--center', type=float, default=100e6)
    ap.add_argument('--aux-freq', type=float, default=100.5e6)
    args = ap.parse_args()

    env = dict(os.environ, WEBSA_PORT=str(PORT), WEBSA_LOG_LEVEL='INFO',
               WEBSA_LOGFILE='/tmp/websa_aux_probe.log')
    import subprocess
    proc = subprocess.Popen(
        [sys.executable, '-m', 'web_sa.main'], cwd=str(ROOT), env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        preexec_fn=os.setsid)
    ok = False
    try:
        # Poll for the port: device open + server bind can take a while on the real unit.
        import socket as _socket
        deadline = time.monotonic() + 40.0
        while time.monotonic() < deadline:
            try:
                with _socket.create_connection(('127.0.0.1', PORT), timeout=1.0):
                    break
            except OSError:
                if proc.poll() is not None:
                    raise RuntimeError(f'server exited early: {proc.returncode}; '
                                       'see /tmp/websa_aux_probe.log') from None
                await asyncio.sleep(0.5)
        else:
            raise TimeoutError('server never bound the port')
        async with aiohttp.ClientSession() as http, http.ws_connect(WS) as ws:
            await send(ws, {'cmd': 'CONNECT'})
            await wait_connected(ws)
            await send(ws, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            await send(ws, {'cmd': 'SET_SDR', 'center': args.center, 'decimate': 32})
            await send(ws, {'cmd': 'SET_SDR_TUNE', 'listen': args.center})
            await drain_status(ws, 3.0)           # let the SDR chain settle
            await send(ws, {'cmd': 'SET_SDR_DDC', 'channel': 2, 'enabled': True,
                            'freq': args.aux_freq})
            st, others = await drain_status(ws, 3.0)
            for m in others:
                print('non-STATUS:', json.dumps(m)[:300])
            aux_echo = (st or {}).get('req', {}).get('sdr', {}).get('aux', {})
            print('aux echo:', aux_echo)

            async with http.ws_connect(WS + '?iqaux=1') as aux_ws:
                end = time.monotonic() + 5.0
                iqax = 0
                iqbf_on_aux = 0
                centers = set()
                while time.monotonic() < end:
                    try:
                        raw = await asyncio.wait_for(aux_ws.receive(),
                                                     timeout=end - time.monotonic())
                    except (asyncio.TimeoutError, TimeoutError):
                        break
                    if raw.type != aiohttp.WSMsgType.BINARY:
                        continue
                    magic = raw.data[:4]
                    if magic == b'IQAX':
                        iqax += 1
                        centers.add(struct.unpack_from('<d', raw.data, 24)[0])
                    elif magic == b'IQBF':
                        iqbf_on_aux += 1
            print(f'IQAX frames: {iqax}, centers: {centers}, IQBF on aux socket: {iqbf_on_aux}')
            ok = iqax > 0 and centers == {args.aux_freq} and iqbf_on_aux == 0
            print('RESULT:', 'PASS' if ok else 'FAIL')
    finally:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=10)
        except Exception:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    asyncio.run(main())
