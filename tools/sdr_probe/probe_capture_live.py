#!/usr/bin/env python3
"""Live level-triggered capture verification (I1/L3): run the real service, arm a
level-below-noise trigger (fires immediately), and assert a WAV lands with samples.

Stop any running service first (the probe spawns its own web_sa.main).
Usage:  python3 tools/sdr_probe/probe_capture_live.py [--level -140]
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import sys
import time
from pathlib import Path

import aiohttp

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

PORT = int(os.environ.get('WEBSA_TEST_PORT', '8124'))
WS = f'ws://127.0.0.1:{PORT}/ws'


async def send(ws, payload):
    await ws.send_str(json.dumps(payload))


async def wait_connected(ws, timeout=40.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        msg = json.loads((await ws.receive()).data)
        if msg.get('cmd') == 'STATUS' and msg.get('connected'):
            return msg
    raise TimeoutError('device never connected')


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--level', type=float, default=-140.0)   # below the floor: fires now
    args = ap.parse_args()

    import subprocess
    env = dict(os.environ, WEBSA_PORT=str(PORT), WEBSA_LOG_LEVEL='INFO',
               WEBSA_LOGFILE='/tmp/websa_capture_probe.log')
    proc = subprocess.Popen([sys.executable, '-m', 'web_sa.main'], cwd=str(ROOT), env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            preexec_fn=os.setsid)
    ok = False
    try:
        import socket as _socket
        deadline = time.monotonic() + 40.0
        while time.monotonic() < deadline:
            try:
                with _socket.create_connection(('127.0.0.1', PORT), timeout=1.0):
                    break
            except OSError:
                if proc.poll() is not None:
                    raise RuntimeError('server exited early; see the probe log')
                await asyncio.sleep(0.5)
        else:
            raise TimeoutError('server never bound the port')

        async with aiohttp.ClientSession() as http:
            async with http.ws_connect(WS) as ws:
                await send(ws, {'cmd': 'CONNECT'})
                await wait_connected(ws)
                await send(ws, {'cmd': 'SET_MODE', 'mode': 'sdr'})
                await send(ws, {'cmd': 'SET_SDR', 'center': 100e6, 'decimate': 64})
                await send(ws, {'cmd': 'SET_SDR_TUNE', 'listen': 100e6})
                await asyncio.sleep(2.0)
                # Known RF source (tinysa-ultra skill): CW at 100.2 MHz, -25 dBm.
                tiny = None
                try:
                    import serial
                    tiny = serial.Serial('/dev/ttyACM1', 115200, timeout=5)
                except Exception as exc:
                    print('tinySA unavailable:', exc)
                if tiny is not None:
                    def cmd(c, to=8.0):
                        tiny.write((c + '\r').encode()); tiny.flush()
                        end = time.monotonic() + to; buf = b''
                        while time.monotonic() < end:
                            buf += tiny.read(256)
                            if b'ch> ' in buf[-8:]:
                                break
                        return buf
                    for c in ('pause', 'mode generator', 'sweeptime 0.003',
                              'sweep abort', 'sweep cw 100200000', 'wait',
                              'level -25', 'output on', 'resume'):
                        cmd(c)
                    print('tinySA CW on 100.2 MHz @ -25 dBm')
                await send(ws, {'cmd': 'SET_SDR_CAPTURE', 'level': args.level, 'seconds': 0.5})
                last = None
                end = time.monotonic() + 130.0
                while time.monotonic() < end:
                    raw = await asyncio.wait_for(ws.receive(), timeout=end - time.monotonic())
                    if raw.type != aiohttp.WSMsgType.TEXT:
                        break
                    msg = json.loads(raw.data)
                    if msg.get('cmd') == 'ERROR':
                        print('ERROR frame:', json.dumps(msg)[:300])
                    if msg.get('cmd') == 'STATUS':
                        cap = msg.get('req', {}).get('sdr', {}).get('capture_last') or {}
                        if cap:
                            last = cap
                            if cap.get('path'):
                                break
                print('capture_last:', last)
                if last and last.get('path') and os.path.isfile(last['path']):
                    size = os.path.getsize(last['path'])
                    expect = 4 * last['samples']       # stereo int16
                    print(f"WAV: {last['path']} bytes={size} expect~{expect}")
                    ok = last['samples'] > 0 and abs(size - expect) <= 44 + 4
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
