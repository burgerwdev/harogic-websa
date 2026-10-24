#!/usr/bin/env python3
"""C5 physical unplug/replug round for the websad hardware backend.

Exercises the real cable-pull recovery chain on one mode (default sdr):
abandoned stale handles are never handed back to the vendor library, the
per-owner retry cooldown reopens after the device returns, and the error is
visible through /api/state the whole time. A replug resets the vendor stream,
so for sdr the IQBF sequence restarts — the acceptance is recovery and error
visibility, not sequence continuity across the physical gap.

Phases (each asserted; the operator acts on the banner prompts):
  pre        streaming confirmed on the chosen mode
  unplug     operator pulls the SAN-90 USB cable; expect connected=false with a
             non-empty last_error while the websad process stays alive
             (a stale-handle vendor close would crash it — that is the point)
  replug     operator reinserts the cable; expect auto-reopen: connected=true,
             last_error cleared, frames flowing again

finally: websad down, tinySA off, ./run.sh, wait healthy, restore, print the
restore diff. Exit 0 only when every phase passed.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

import websockets.sync.client as wsc

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / 'tools/sdr_probe'))
from tinysa import TinySA  # noqa: E402

from tools.bench.rust_swp_mode_round import CARRIER, post, start_websad  # noqa: E402
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'

# Frames each mode must deliver within a window (conservative against the
# measured 21.7 fps sdr / ~70 fps rta / ~90 fps std).
PRE_WINDOW_S = 30
PRE_FRAMES = {'std': 100, 'rta': 100, 'sdr': 50}
POST_WINDOW_S = 30
POST_FRAMES = {'std': 400, 'rta': 300, 'sdr': 150}
UNPLUG_BUDGET_S = 240
REPLUG_BUDGET_S = 300


class FrameRx(threading.Thread):
    """Counts frames per wire tag; tracks the raw IQBF sequence without gap
    verdicts (a physical replug legitimately restarts the stream)."""

    def __init__(self, url):
        super().__init__(daemon=True)
        self.sock = wsc.connect(url, max_size=1 << 24)
        self.halt = threading.Event()
        self.error = None
        self.counts = {'POWR': 0, 'RTAF': 0, 'IQBF': 0, 'other': 0}
        self.last_iq_seq = None
        self.mark = 0  # caller-set frame marker for window deltas

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    data = self.sock.recv(timeout=0.2)
                except TimeoutError:
                    continue
                if not isinstance(data, bytes) or len(data) < 4:
                    continue
                tag = data[:4].decode('ascii', 'replace')
                if tag == 'RTA2':
                    tag = 'RTAF'
                if tag == 'IQBF':
                    import struct
                    if len(data) < 56:
                        raise ValueError('short IQBF header')
                    version, seq, count = struct.unpack_from('<III', data, 4)
                    if version != 2 or len(data) != 56 + count * 8:
                        raise ValueError('invalid IQBF v2 frame')
                    self.last_iq_seq = seq
                    self.counts['IQBF'] += 1
                elif tag in self.counts:
                    self.counts[tag] += 1
                else:
                    self.counts['other'] += 1
        except Exception as exc:
            if not self.halt.is_set():
                self.error = repr(exc)

    def finish(self):
        self.halt.set()
        try:
            self.sock.close()
        except Exception:
            pass
        self.join(timeout=2)


def get_state(url):
    with urllib.request.urlopen(url + '/api/state', timeout=5) as response:
        return json.load(response)


def safe_state(url):
    """websad may die or hang mid-phase; a dead API must read as None, not raise."""
    try:
        return get_state(url)
    except Exception:
        return None


def same(before, after):
    keys = ('center', 'decimate', 'listen', 'demod', 'if_bw', 'squelch', 'agc', 'volume')
    diff = ['mode'] if before['mode'] != after['mode'] else []
    diff += [f'swp.{k}' for k, v in before['req']['swp'].items()
             if after['req']['swp'].get(k) != v]
    diff += [f'sdr.{k}' for k in keys if before['req']['sdr'][k] != after['req']['sdr'][k]]
    return diff


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=['std', 'rta', 'sdr'], default='sdr')
    parser.add_argument('--port', type=int, default=8108)
    parser.add_argument('--tinysa-port', default='/dev/ttyACM0')
    args = parser.parse_args()


    saved = state(SERVICE)
    source = None
    websad = None
    problems: list[str] = []
    timeline: dict = {}
    try:
        source = TinySA(args.tinysa_port)
        source.cw(CARRIER, -25.0)
        time.sleep(1.0)
        subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
        if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
            raise SystemExit('WebSA still running after ./stop.sh')
        websad, url = start_websad(args.port, f'/tmp/websa_unplug_{args.mode}.log')
        base = url.replace('http://', 'ws://') + '/ws'
        if args.mode == 'std':
            post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
            tag = 'POWR'
        elif args.mode == 'rta':
            post(url, {'cmd': 'SET_MODE', 'mode': 'rta'})
            post(url, {'cmd': 'SET_RTA', 'center': CARRIER, 'span': 2e6, 'decimate': 1})
            tag = 'RTAF'
        else:
            post(url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            post(url, {'cmd': 'SET_SDR', 'center': CARRIER, 'decimate': 1})
            post(url, {'cmd': 'SET_SDR_TUNE', 'listen': CARRIER})
            post(url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am'})
            tag = 'IQBF'
            base += '?iq=1'
        rx = FrameRx(base)
        rx.start()

        def frames():
            return rx.counts[tag]

        # ---- pre: streaming --------------------------------------------------
        t0 = time.monotonic()
        while frames() < PRE_FRAMES[args.mode]:
            if time.monotonic() - t0 > PRE_WINDOW_S:
                problems.append(f'pre: only {frames()} {tag} frames in {PRE_WINDOW_S}s')
                break
            if websad.poll() is not None:
                problems.append('pre: websad exited')
                break
            time.sleep(0.5)
        print(f'PHASE pre done frames={frames()} '
              f'connected={get_state(url)["connected"]}', flush=True)

        # ---- unplug ----------------------------------------------------------
        print('>>>', '=' * 30, 'UNPLUG THE SAN-90 USB CABLE NOW', '=' * 30, flush=True)
        t_unplug = None
        error_seen = ''
        t0 = time.monotonic()
        while True:
            st = safe_state(url)
            if st is not None and (not st['connected'] or st['last_error']):
                t_unplug = time.monotonic()
                error_seen = st['last_error']
                break
            if time.monotonic() - t0 > UNPLUG_BUDGET_S:
                problems.append(f'unplug: no error within {UNPLUG_BUDGET_S}s')
                break
            if websad.poll() is not None:
                problems.append('unplug: websad exited (stale-handle close?)')
                break
            time.sleep(0.5)
        if t_unplug is not None:
            # The error must stay visible and the process must survive the stale
            # handle: a vendor close on it would crash websad here.
            stable_until = t_unplug + 10.0
            while time.monotonic() < stable_until:
                if websad.poll() is not None:
                    problems.append('unplug hold: websad exited')
                    break
                time.sleep(0.5)
            st = safe_state(url) or {}
            if st.get('connected'):
                problems.append('unplug hold: connected still true')
            if not st.get('last_error'):
                problems.append('unplug hold: last_error empty')
            print(f'PHASE unplug detected after '
                  f'{t_unplug - t0:.1f}s wait, error={error_seen!r}', flush=True)
        timeline['unplug'] = {'error': error_seen,
                              'detect_wait_s': None if t_unplug is None
                              else round(t_unplug - t0, 1)}

        # ---- replug ----------------------------------------------------------
        print('>>>', '=' * 30, 'REPLUG THE SAN-90 USB CABLE NOW', '=' * 30, flush=True)
        t0 = time.monotonic()
        back_at = None
        while True:
            st = safe_state(url)
            if st is not None and st['connected'] and not st['last_error']:
                back_at = time.monotonic()
                break
            if time.monotonic() - t0 > REPLUG_BUDGET_S:
                problems.append(f'replug: no recovery within {REPLUG_BUDGET_S}s')
                break
            if websad.poll() is not None:
                problems.append('replug: websad exited')
                break
            time.sleep(0.5)
        if back_at is not None:
            mark = frames()
            t0 = back_at
            while time.monotonic() - t0 < POST_WINDOW_S:
                if websad.poll() is not None:
                    problems.append('post: websad exited')
                    break
                time.sleep(0.5)
            got = frames() - mark
            # A no-op round (operator never unplugged) legitimately reaches here with
            # t_unplug unset; the unplug-phase problem already flags it.
            if t_unplug is not None:
                print(f'PHASE replug recovered in {back_at - t_unplug:.1f}s from unplug, '
                      f'post window {got} {tag} frames', flush=True)
                timeline['replug'] = {
                    'recover_s': round(back_at - t_unplug, 1), 'post_frames': got}
            else:
                print(f'PHASE replug (no unplug detected), '
                      f'post window {got} {tag} frames', flush=True)
                timeline['replug'] = {'recover_s': None, 'post_frames': got}
            if got < POST_FRAMES[args.mode]:
                problems.append(f'post: only {got} {tag} frames in {POST_WINDOW_S}s')
            st = safe_state(url) or {}
            if st.get('last_error'):
                problems.append(f'post: last_error {st["last_error"]!r}')
        if rx.error:
            problems.append(f'rx: {rx.error}')

        result = {'mode': args.mode, 'timeline': timeline,
                  'frames': rx.counts, 'final_state': {
                      'connected': get_state(url)['connected'],
                      'last_error': get_state(url)['last_error']}}
    finally:
        def guarded(label, action):
            try:
                action()
            except Exception as exc:
                print(f'teardown {label}: {exc!r}', flush=True)
        if websad is not None:
            websad.terminate()
            try:
                websad.wait(timeout=5)
            except subprocess.TimeoutExpired:
                websad.kill()
                websad.wait(timeout=5)
        if source is not None:
            guarded('tinysa off', source.off)
            guarded('tinysa close', source.close)
        guarded('run.sh', lambda: subprocess.run(['./run.sh'], cwd=ROOT, check=True))
        for _ in range(30):
            try:
                if state(SERVICE).get('connected'):
                    break
            except OSError:
                pass
            time.sleep(1)
        # C5 ends with the cable possibly still out: Python's link loop only reopens
        # once the analyzer is back, and restore() needs a connected device (SET_MODE
        # is rejected with device_not_connected otherwise). Wait for the replug before
        # re-applying the saved request.
        print('teardown: waiting for the analyzer to come back before restore',
              flush=True)
        back = False
        for _ in range(120):
            try:
                if state(SERVICE).get('connected'):
                    back = True
                    break
            except OSError:
                pass
            time.sleep(1)
        if not back:
            print('teardown: analyzer still absent after 120s; '
                  'restore NOT applied — re-run tools/bench/sdr_network_diagnostics '
                  'restore manually once plugged back in', flush=True)
        guarded('restore', lambda: restore(SERVICE, saved))
        try:
            diff = same(saved, state(SERVICE))
        except Exception as exc:
            diff = [f'state readback failed: {exc!r}']
        print('restore diff', diff or 'none', flush=True)
    print(json.dumps(result, indent=1)[:4000])
    problems += ['restore'] if diff else []
    print('PROBLEMS' if problems else 'OK', problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
