#!/usr/bin/env python3
"""Native systemd user-unit crash self-healing drill on an alternate port.

Exercises the root launcher and its configured WEBSA_UNIT (default `websad`):

  ./run.sh -> transient user unit with Restart=on-failure
  * std/SWP streaming verified (POWR frames),
  * SIGKILL the main process -> the unit restarts itself (NRestarts+1), reopens
    the device and frames resume,
  * ./stop.sh frees the device; ./run.sh restores the production native service
    in finally with the saved request re-applied and diffed.

Exit 0 only when every step passed.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from tools.bench.rust_swp_mode_round import post  # noqa: E402
from tools.bench.rust_unplug_round import FrameRx, safe_state  # noqa: E402
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'
RUN = [str(ROOT / 'run.sh')]
STOP = [str(ROOT / 'stop.sh')]
UNIT = os.environ.get('WEBSA_UNIT', 'websad')


def systemctl(*args: str) -> str:
    result = subprocess.run(['systemctl', '--user', *args],
                            capture_output=True, text=True)
    return result.stdout.strip()


def unit_property(name: str, prop: str) -> str:
    return systemctl('show', name, '--property', prop).split('=', 1)[-1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, default=8108)
    args = parser.parse_args()


    saved = state(SERVICE)
    problems: list[str] = []
    result: dict = {}
    try:
        subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
        if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
            raise SystemExit('WebSA still running after ./stop.sh')

        env = {**os.environ, 'WEBSA_PORT': str(args.port), 'WEBSA_HOST': '127.0.0.1',
               'WEBSA_FAKE': '0'}
        started = subprocess.run(RUN, env=env, cwd=ROOT, capture_output=True, text=True)
        if started.returncode != 0:
            problems.append(f'run.sh failed: {started.stdout} {started.stderr}')
        else:
            url = f'http://127.0.0.1:{args.port}'
            base = url.replace('http://', 'ws://') + '/ws'
            # websad boots hardware sessions in sdr mode ("hardware path never
            # advertises fake SWP/RTA"); the drill counts POWR, so switch to the
            # vendor SWP path explicitly before listening.
            post(url, {'cmd': 'SET_FREQ', 'center': 1e9, 'span': 1e8})
            post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
            rx = FrameRx(base)
            rx.start()

            def wait_frames(target: int, window: float) -> int:
                mark = rx.counts['POWR']
                t0 = time.monotonic()
                while rx.counts['POWR'] - mark < target:
                    if time.monotonic() - t0 > window:
                        break
                    time.sleep(0.5)
                return rx.counts['POWR'] - mark

            got = wait_frames(50, 20)
            pid_before = unit_property(UNIT, 'MainPID')
            print(f'PHASE streaming: {got} POWR frames, MainPID={pid_before}', flush=True)
            if got < 50:
                problems.append(f'streaming: only {got} POWR frames in 20s')
            if rx.error:
                problems.append(f'streaming rx: {rx.error}')

            # Crash: SIGKILL the main process. Restart=on-failure must bring it back,
            # and the fresh process must reopen the SAN-90 and resume the sweep.
            subprocess.run(['systemctl', '--user', 'kill', '--signal=SIGKILL', UNIT], check=True)
            time.sleep(1.0)
            pid_after = None
            t0 = time.monotonic()
            while time.monotonic() - t0 < 20:
                pid_after = unit_property(UNIT, 'MainPID')
                st = safe_state(url)
                if pid_after not in ('', '0', pid_before) and st is not None \
                        and st.get('connected'):
                    break
                time.sleep(0.5)
            restarts = unit_property(UNIT, 'NRestarts')
            active = systemctl('is-active', UNIT)
            print(f'PHASE crash: pid {pid_before} -> {pid_after}, '
                  f'NRestarts={restarts}, active={active}', flush=True)
            if pid_after in ('', '0', pid_before):
                problems.append(f'crash: MainPID unchanged ({pid_before})')
            if active != 'active':
                problems.append(f'crash: unit state {active}')
            if restarts == '0':
                problems.append('crash: NRestarts is 0 (unit did not restart)')

            # The restart boots a fresh process in the default sdr mode (the killed
            # process took the SET_MODE with it) — re-apply std before counting.
            post(url, {'cmd': 'SET_FREQ', 'center': 1e9, 'span': 1e8})
            post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
            # The SIGKILL took the old WS socket with it: reconnect before counting,
            # otherwise the resume window counts a dead connection.
            rx.finish()
            rx = FrameRx(base)
            rx.start()

            got = wait_frames(50, 20)
            print(f'PHASE resume: {got} POWR frames after restart', flush=True)
            if got < 50:
                problems.append(f'resume: only {got} POWR frames in 20s')
            st = safe_state(url)
            if st is None or st.get('last_error'):
                problems.append(f'resume: last_error {(st or {}).get("last_error")!r}')

            result = {'pid_before': pid_before, 'pid_after': pid_after,
                      'nrestarts': restarts, 'frames': rx.counts}
            rx.finish()
    finally:
        def guarded(label, action):
            try:
                action()
            except Exception as exc:
                print(f'teardown {label}: {exc!r}', flush=True)
        guarded('stop.sh', lambda: subprocess.run(STOP, cwd=ROOT, check=True))
        guarded('run.sh', lambda: subprocess.run(['./run.sh'], cwd=ROOT, check=True))
        for _ in range(30):
            try:
                if state(SERVICE).get('connected'):
                    break
            except OSError:
                pass
            time.sleep(1)
        guarded('restore', lambda: restore(SERVICE, saved))
        try:
            st = state(SERVICE)
            diff = ['mode'] if st['mode'] != saved['mode'] else []
            diff += [f'swp.{k}' for k, v in saved['req']['swp'].items()
                     if st['req']['swp'].get(k) != v]
            diff += [f'sdr.{k}' for k in ('center', 'decimate', 'listen', 'demod',
                                          'if_bw', 'squelch', 'agc', 'volume')
                     if saved['req']['sdr'][k] != st['req']['sdr'][k]]
        except Exception as exc:
            diff = [f'state readback failed: {exc!r}']
        print('restore diff', diff or 'none', flush=True)
    print(json.dumps(result, indent=1))
    problems += ['restore'] if diff else []
    print('PROBLEMS' if problems else 'OK', problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
