#!/usr/bin/env python3
"""Live version-display comparison against the official About screenshot (sa_ver.png).

Starts the real service, reads STATUS.device + device_detail, and asserts the decoded
values match the bench unit: N90 R0 / MCU 0.55.109 / FPGA 0.55.104 / PMU 0.1.15(271) /
AGU 0.1.5(261) / API 0.55.100 / UID 5230500c00380025.
Stop any running service first. Usage:  python3 tools/sdr_probe/probe_version_check.py
"""
from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
import time
from pathlib import Path

import aiohttp

ROOT = Path(__file__).resolve().parent.parent.parent
PORT = int(os.environ.get('WEBSA_TEST_PORT', '8126'))
WS = f'ws://127.0.0.1:{PORT}/ws'

EXPECTED = {
    'model_name': 'N90 R0',
    'mfw_str': '0.55.109',
    'ffw_str': '0.55.104',
    'pmu_str': '0.1.15(271)',
    'agu_str': '0.1.5(261)',
    'api_str': '0.55.100',
    'uid_prefix': '5230500C0038',
}


async def main():
    import subprocess
    env = dict(os.environ, WEBSA_PORT=str(PORT), WEBSA_LOG_LEVEL='INFO',
               WEBSA_LOGFILE='/tmp/websa_version_probe.log')
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
                    raise RuntimeError('server exited early; see /tmp/websa_version_probe.log') from None
                await asyncio.sleep(0.5)
        async with aiohttp.ClientSession() as http, http.ws_connect(WS) as ws:
            await ws.send_str(json.dumps({'cmd': 'CONNECT'}))
            end = time.monotonic() + 30.0
            status = None
            while time.monotonic() < end:
                raw = await asyncio.wait_for(ws.receive(), timeout=end - time.monotonic())
                if raw.type != aiohttp.WSMsgType.TEXT:
                    continue
                msg = json.loads(raw.data)
                if msg.get('cmd') == 'STATUS' and msg.get('connected'):
                    status = msg
                    break
            dd = (status or {}).get('device_detail', {})
            label = (status or {}).get('device', '')
            print('label:', label)
            rows = {k: dd.get(k) for k in
                    ('model_name', 'mfw_str', 'ffw_str', 'pmu_str', 'agu_str', 'api_str')}
            print('rows:', json.dumps(rows, ensure_ascii=False))
            ok = all(rows[k] == v for k, v in EXPECTED.items() if k != 'uid_prefix')
            ok = ok and str(dd.get('uid', '')).upper().startswith(EXPECTED['uid_prefix'])
            ok = ok and EXPECTED['model_name'] in label
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
