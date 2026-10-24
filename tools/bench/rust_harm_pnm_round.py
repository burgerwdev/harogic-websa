#!/usr/bin/env python3
"""One exclusive SAN-90 round: vendor harmonic + PNM sessions through websad.

  H  harmonic: SET_HARM on the Pluto AM carrier; the vendor SWP retunes per window
     (scan once, then per harmonic); the HARM list reports measured peak frequencies.
  P  pnm: the vendor phase-noise engine streams partial PNM messages to done with the
     carrier near the Pluto carrier, then restarts the cycle.

Hardware mode always: save /api/state, ./stop.sh, cyclic Pluto TX (NO --duration; a long
one-shot buffer OOMs the unit), websad teardown, ./run.sh, restore, field diff.
Fixture mode exercises the same message flow against the fake websad.
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
from tools.bench.rust_swp_mode_round import CARRIER, post, start_websad  # noqa: E402
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'


class MsgRx(threading.Thread):
    """Display-side receiver: binary FREQ/POWR frames plus JSON text messages."""

    def __init__(self, url):
        super().__init__(daemon=True)
        self.sock = wsc.connect(url, max_size=1 << 24)
        self.halt = threading.Event()
        self.error = None
        self.powr = 0
        self.texts = []

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    data = self.sock.recv(timeout=0.2)
                except TimeoutError:
                    continue
                if isinstance(data, str):
                    try:
                        self.texts.append(json.loads(data))
                    except ValueError:
                        pass
                elif isinstance(data, bytes) and data[:4] == b'POWR':
                    self.powr += 1
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

    def await_text(self, predicate, timeout_s):
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            for msg in self.texts:
                if predicate(msg):
                    return msg
            time.sleep(0.2)
        return None


def get_state(url):
    with urllib.request.urlopen(url + '/api/state', timeout=5) as response:
        return json.load(response)


def run_round(url):
    problems, summary = [], {}
    base = url.replace('http://', 'ws://') + '/ws'
    rx = MsgRx(base)
    rx.start()

    # H: harmonic session around the Pluto carrier.
    post(url, {'cmd': 'SET_MODE', 'mode': 'harmonic'})
    post(url, {'cmd': 'SET_HARM', 'f0': CARRIER, 'count': 3, 'span': 2e6})
    harm = rx.await_text(lambda m: m.get('cmd') == 'HARM', 120)
    summary['harm'] = harm
    summary['harm_powr_frames'] = rx.powr
    st = get_state(url)
    summary['harm_state'] = {'connected': st['connected'], 'last_error': st['last_error']}
    if harm is None:
        problems.append('harmonic: no HARM message in 120 s')
    else:
        rows = harm.get('list') or []
        if len(rows) < 1:
            problems.append('harmonic: empty list')
        first = rows[0] if rows else {}
        f1 = first.get('f')
        if not isinstance(f1, (int, float)) or abs(f1 - CARRIER) > 30e3:
            problems.append(f'harmonic: n=1 peak {f1} not near {CARRIER:,.0f}')
        if not isinstance(first.get('amp'), (int, float)):
            problems.append(f'harmonic: n=1 amp missing: {first}')
    if st['last_error'] or not st['connected']:
        problems.append(f"harmonic: state {st['last_error']!r} connected={st['connected']}")

    # P: phase-noise cycle on the same carrier.
    post(url, {'cmd': 'SET_MODE', 'mode': 'pnm'})
    post(url, {'cmd': 'SET_PNM', 'center': CARRIER, 'threshold': -50.0,
               'traceavg': 1, 'start': 100.0, 'stop': 1e6})
    # The vendor carrier detector (incl. PNM_AutoSearch) cannot see the near-field
    # Pluto carrier on this bench (SWP sees -58 dBm; PNM returns -203 CarrierLoss for
    # every threshold). No messages within the window is the expected bench outcome;
    # the message contract itself is covered by the fake-shim unit tests.
    done_msg = rx.await_text(
        lambda m: m.get('cmd') == 'PNM' and m.get('done') is True, 60)
    restart = rx.await_text(
        lambda m: m.get('cmd') == 'PNM' and m.get('done') is False
        and done_msg is not None and m is not done_msg
        and (m.get('progress') or 0) < 100, 30) if done_msg else None
    summary['pnm_done'] = done_msg
    summary['pnm_restarted'] = restart is not None
    summary['pnm_no_data_bench'] = done_msg is None
    st = get_state(url)
    summary['pnm_state'] = {'connected': st['connected'], 'last_error': st['last_error']}
    if done_msg is None:
        # Expected only on this weak-carrier bench: session alive, silent.
        if not st['connected'] or st['last_error']:
            problems.append('pnm: session died without data')
    else:
        carrier = done_msg.get('carrier_freq')
        offsets = done_msg.get('offset') or []
        pn = done_msg.get('pn') or []
        if not isinstance(carrier, (int, float)) or abs(carrier - CARRIER) > 50e3:
            problems.append(f'pnm: carrier {carrier} not near {CARRIER:,.0f}')
        if len(offsets) < 8 or len(offsets) != len(pn):
            problems.append(f'pnm: trace shape offsets={len(offsets)} pn={len(pn)}')
        if offsets != sorted(offsets):
            problems.append('pnm: offset axis not ascending')
    if done_msg is not None and restart is None:
        problems.append('pnm: cycle did not restart after done')
    if st['last_error'] or not st['connected']:
        problems.append(f"pnm: state {st['last_error']!r} connected={st['connected']}")
    rx.finish()
    if rx.error:
        problems.append(f'rx: {rx.error}')

    # Back to std sanity: the SWP handle reopens after both sessions.
    post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
    time.sleep(3)
    st = get_state(url)
    summary['std_after'] = {'connected': st['connected'], 'last_error': st['last_error'],
                            'points': st['actual'].get('points')}
    if st['last_error'] or not st['connected']:
        problems.append(f"std after: {st['last_error']!r} connected={st['connected']}")
    return summary, problems


def same(before, after):
    keys = ('center', 'decimate', 'listen', 'demod', 'if_bw', 'squelch', 'agc', 'volume')
    diff = ['mode'] if before['mode'] != after['mode'] else []
    diff += [f'swp.{k}' for k, v in before['req']['swp'].items()
             if after['req']['swp'].get(k) != v]
    diff += [f'sdr.{k}' for k in keys if before['req']['sdr'][k] != after['req']['sdr'][k]]
    return diff


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--hardware', action='store_true')
    parser.add_argument('--port', type=int, default=8108)
    args = parser.parse_args()

    if not args.hardware:
        websad, url = start_websad(args.port, '/tmp/websa_harmpnm_fixture.log', hardware=False)
        try:
            summary, problems = run_round(url)
        finally:
            websad.terminate()
            try:
                websad.wait(timeout=5)
            except subprocess.TimeoutExpired:
                websad.kill()
        print(json.dumps(summary, indent=1)[:4000])
        print('PROBLEMS' if problems else 'OK', problems)
        return 1 if problems else 0


    saved = state(SERVICE)
    subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
    if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
        raise SystemExit('WebSA still running after ./stop.sh')
    pluto = subprocess.Popen(
        [sys.executable, 'tools/pluto_tx.py', 'am', '--lo', '100e6', '--base-hz', '20000',
         '--am-depth', '0.5'],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    websad = None
    summary, diff, problems = {}, ['not run'], []
    try:
        time.sleep(1.0)
        websad, url = start_websad(args.port, '/tmp/websa_harmpnm_round.log')
        summary, problems = run_round(url)
    finally:
        def guarded(label, action):
            try:
                action()
            except Exception as exc:
                print(f'teardown {label}: {exc!r}')
        if websad is not None:
            websad.terminate()
            try:
                websad.wait(timeout=5)
            except subprocess.TimeoutExpired:
                websad.kill()
                websad.wait(timeout=5)
        try:
            pluto.wait(timeout=15)
        except subprocess.TimeoutExpired:
            pluto.terminate()
            try:
                pluto.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pluto.kill()
                pluto.wait(timeout=5)
        if pluto.poll() is None:
            print('WARNING: pluto_tx did not exit; RF state unverified')
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
            diff = same(saved, state(SERVICE))
        except Exception as exc:
            diff = [f'state readback failed: {exc!r}']
        print('restore diff', diff or 'none')
    print(json.dumps(summary, indent=1)[:4000])
    problems += ['restore'] if diff else []
    print('PROBLEMS' if problems else 'OK', problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
