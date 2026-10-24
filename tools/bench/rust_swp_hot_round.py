#!/usr/bin/env python3
"""30-minute native std hot-state round with before/after calibration.

Answers two questions in ONE exclusive session:

  D1 Measure the same native calibration request before and after the hot run to
     quantify carrier frequency, level and full-trace drift in one device session.
  H  30 minutes of continuous vendor SWP streaming in websad: connected, error-free, and
     a stable frame rate, with a full-trace snapshot every 5 minutes to watch the LO
     feedthrough drift.

All traces are saved to /tmp/cal_*.npz for offline re-analysis. Hardware mode always:
saves /api/state, restores it at the end (field diff), waits for the Pluto TX to end
(RF off), and restarts the native service through ./run.sh. Fixture mode needs no device.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from tools.bench.rust_swp_calibration import apply_same_config  # noqa: E402
from tools.bench.rust_swp_mode_round import (  # noqa: E402
    CARRIER,
    DisplayRx,
    carrier_hit,
    start_websad,
)
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'


def measure_trace(url, seconds, tag):
    """Collect one FREQ+POWR pair; save the raw arrays and return the summary row."""
    base = url.replace('http://', 'ws://') + '/ws'
    rx = DisplayRx(base)
    rx.start()
    time.sleep(seconds)
    with urllib.request.urlopen(url + '/api/state', timeout=5) as response:
        st = json.load(response)
    rx.finish()
    row = {'tag': tag, 'powr_frames': len(rx.powr), 'rx_error': rx.error,
           'connected': st.get('connected'), 'last_error': st.get('last_error')}
    if 'actual' in st:
        row['actual'] = {k: st['actual'].get(k)
                         for k in ('start', 'stop', 'points', 'rbw', 'vbw', 'ref')}
    if rx.powr and rx.last_freq is not None:
        axis, power = rx.powr[-1]
        if axis is not None and axis.size > 1:
            row['axis'] = {'start': float(axis[0]), 'stop': float(axis[-1]),
                           'points': int(axis.size),
                           'monotonic': bool(np.all(np.diff(axis) > 0))}
            row['hit'] = carrier_hit(axis, power, CARRIER, window=200e3)
            path = f'/tmp/cal_{tag}.npz'
            np.savez(path, freq=axis, power=power)
            row['trace'] = path
    return row


def collect(url, seconds, tag):
    apply_same_config(url)
    time.sleep(1.0)
    return measure_trace(url, seconds, tag)


def hit_distance(a, b):
    """Report how much two measurements of the same feature moved."""
    if not (a and b and a.get('hit') and b.get('hit')):
        return None
    ha, hb = a['hit'], b['hit']
    return {'freq_hz': round(hb['near_hz'] - ha['near_hz'], 1),
            'peak_dbm': round(hb['near_dbm'] - ha['near_dbm'], 2),
            'floor_dbm': round(hb['floor_dbm'] - ha['floor_dbm'], 2)}


def bin_diff(path_a, path_b):
    """Bin-by-bin difference of two saved traces (grid must match)."""
    try:
        a = np.load(path_a)
        b = np.load(path_b)
        if a['freq'].shape != b['freq'].shape:
            return {'error': f'shape {a["freq"].shape} vs {b["freq"].shape}'}
        delta = b['power'] - a['power']
        return {'bins': int(delta.size),
                'mean_abs_db': round(float(np.mean(np.abs(delta))), 2),
                'median_abs_db': round(float(np.median(np.abs(delta))), 2),
                'p95_abs_db': round(float(np.percentile(np.abs(delta), 95)), 2),
                'max_abs_db': round(float(np.max(np.abs(delta))), 2)}
    except Exception as exc:
        return {'error': repr(exc)}


def same(before, after):
    keys = ('center', 'decimate', 'listen', 'demod', 'if_bw', 'squelch', 'agc', 'volume')
    diff = ['mode'] if before['mode'] != after['mode'] else []
    diff += [f'swp.{k}' for k, v in before['req']['swp'].items()
             if after['req']['swp'].get(k) != v]
    diff += [f'sdr.{k}' for k in keys if before['req']['sdr'][k] != after['req']['sdr'][k]]
    return diff


def main():
    # Any trappable termination must run the finally block: leaving production stopped
    # with the device mid-round is exactly the failure this script must never cause.
    import signal

    def _term(signum, _frame):
        raise SystemExit(f'terminated by signal {signum}')

    signal.signal(signal.SIGTERM, _term)
    signal.signal(signal.SIGINT, _term)

    parser = argparse.ArgumentParser()
    parser.add_argument('--hardware', action='store_true')
    parser.add_argument('--seconds', type=float, default=10.0,
                        help='per-measurement collection window')
    parser.add_argument('--hot-minutes', type=float, default=30.0)
    parser.add_argument('--port', type=int, default=8108)
    args = parser.parse_args()

    if not args.hardware:
        websad, url = start_websad(args.port, '/tmp/websa_hot_fixture.log', hardware=False)
        try:
            row = collect(url, args.seconds, 'FIX')
            hot = {'frames': 0}
            t0 = time.monotonic()
            rx = DisplayRx(url.replace('http://', 'ws://') + '/ws')
            rx.start()
            while time.monotonic() - t0 < min(args.hot_minutes * 60, 15.0):
                time.sleep(1)
            rx.finish()
            hot = {'frames': len(rx.powr), 'rx_error': rx.error}
            print(json.dumps({'fixture': row, 'hot': hot}, indent=1))
            ok = bool(row.get('hit') and row['hit']['snr_db'] >= 10
                      and not row.get('rx_error') and hot['frames'] > 5)
            print('OK' if ok else 'PROBLEMS')
            return 0 if ok else 1
        finally:
            websad.terminate()
            try:
                websad.wait(timeout=5)
            except subprocess.TimeoutExpired:
                websad.kill()


    saved = state(SERVICE)
    pluto = None
    websad = None
    result, diff, problems = {}, ['not run'], []
    pluto = subprocess.Popen(
        [sys.executable, 'tools/pluto_tx.py', 'am', '--lo', '100e6', '--base-hz', '20000',
         '--am-depth', '0.5'],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        time.sleep(1.0)
        subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
        if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
            raise SystemExit('WebSA still running after ./stop.sh')
        websad, url = start_websad(args.port, '/tmp/websa_hot_round.log')
        before = collect(url, args.seconds, 'BEFORE')
        # H: continuous vendor SWP streaming with 5-minute trace snapshots.
        base = url.replace('http://', 'ws://') + '/ws'
        rx = DisplayRx(base)
        rx.start()
        snapshots = []
        t0 = time.monotonic()
        last_snap = 0.0
        while time.monotonic() - t0 < args.hot_minutes * 60:
            time.sleep(5)
            if time.monotonic() - t0 - last_snap >= 300 or last_snap == 0.0:
                last_snap = time.monotonic() - t0
                st = state(url)
                snap = {'t_min': round(last_snap / 60, 1), 'powr': len(rx.powr),
                        'connected': st['connected'], 'err': st['last_error']}
                if rx.last_freq is not None and rx.powr:
                    hit = carrier_hit(rx.last_freq, rx.powr[-1][1], CARRIER, window=200e3)
                    snap.update(hit or {})
                    np.savez(f'/tmp/cal_HOT_{len(snapshots)}.npz',
                             freq=rx.last_freq, power=rx.powr[-1][1])
                snapshots.append(snap)
                if st['last_error'] or not st['connected']:
                    problems.append(f"hot snapshot: {snap}")
        rx.finish()
        hot = {'minutes': args.hot_minutes, 'powr_frames': len(rx.powr),
               'rx_error': rx.error, 'snapshots': snapshots}
        result['hot'] = hot
        after = collect(url, args.seconds, 'AFTER')
        result['before'] = before
        result['after'] = after
        result['drift'] = hit_distance(before, after)
        if before.get('trace') and after.get('trace'):
            result['bins_before_vs_after'] = bin_diff(before['trace'], after['trace'])
        if hot['rx_error'] or hot['powr_frames'] < args.hot_minutes * 60 * 5 * 0.8:
            problems.append(f"hot: frames={hot['powr_frames']} err={hot['rx_error']}")
        for row in (before, after):
            hit = row.get('hit')
            if not hit or hit['snr_db'] < 10:
                problems.append(f"{row['tag']}: carrier lost: {hit}")
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
        if pluto is not None:
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
    print(json.dumps(result, indent=1))
    problems += ['restore'] if diff else []
    print('PROBLEMS' if problems else 'OK', problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
