#!/usr/bin/env python3
"""Native calibration + hot-state round for the SAN-90.

One exclusive session answers, with the same Pluto AM source (100.020 MHz, 1 kHz, 50%):

  C1 Measure the native SWP carrier and optionally compare a saved calibration row
     (--baseline JSON; identical RBW/VBW are required for comparable dBm).
  H1 Reverse direction sdr -> rta -> std still reopens every handle (the vendor exit()/
     segfault constraints must hold in both directions).
  H2 Repeated std -> rta -> std cycles (fresh Device_Open each time) survive.
  H3 60 s continuous vendor SWP streaming stays error-free at a stable rate.

Fixture mode collects a native measurement without claiming the device. Hardware mode
saves /api/state, ./stop.sh, runs native measurements + combos + hot run, and ALWAYS:
stops websad, waits for RF off, ./run.sh, restores the saved native request, and diffs it.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from tools.bench.rust_swp_mode_round import (  # noqa: E402
    CARRIER,
    DisplayRx,
    carrier_hit,
    start_websad,
)
from tools.bench.sdr_network_diagnostics import command, restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'
SAME_CONFIG = [
    {'cmd': 'SET_MODE', 'mode': 'std'},
    {'cmd': 'SET_FREQ', 'center': CARRIER, 'span': 20e6},
    {'cmd': 'SET_POINTS', 'points': 4000},
    {'cmd': 'SET_RBW', 'mode': 'auto', 'rbw': 10000.0},
    {'cmd': 'SET_VBW', 'mode': 'equal', 'vbw': 10000.0},
    {'cmd': 'SET_REF', 'mode': 'manual', 'ref': 0.0},
    {'cmd': 'SET_AMP', 'atten': -1, 'preamp': 0, 'ifgain': 2, 'gain_strategy': 0},
    {'cmd': 'SET_WINDOW', 'window': 1},
    {'cmd': 'SET_DETECTOR', 'mode': 'auto'},
]


def measure(url, seconds):
    """Apply nothing; just collect one FREQ+POWR pair plus status over `seconds`."""
    base = url.replace('http://', 'ws://') + '/ws'
    rx = DisplayRx(base)
    rx.start()
    t0 = time.monotonic()
    time.sleep(seconds)
    try:
        with urllib.request.urlopen(url + '/api/state', timeout=5) as response:
            st = json.load(response)
    except Exception as exc:
        st = {'error': repr(exc)}
    rx.finish()
    row = {'powr_frames': len(rx.powr), 'freq_frames': len(rx.freq),
           'rx_error': rx.error, 'elapsed': round(time.monotonic() - t0, 2)}
    if rx.powr:
        axis, power = rx.powr[-1]
        if axis is not None and axis.size > 1:
            import numpy as np

            hit = carrier_hit(axis, power, CARRIER, window=200e3)
            row['axis'] = {'start': float(axis[0]), 'stop': float(axis[-1]),
                           'points': int(axis.size),
                           'monotonic': bool(np.all(np.diff(axis) > 0))}
            row['hit'] = hit
    if 'actual' in st:
        row['actual'] = {k: st['actual'].get(k)
                         for k in ('start', 'stop', 'points', 'rbw', 'vbw', 'ref',
                                   'refclk', 'refclk_src')}
        row['connected'] = st.get('connected')
        row['last_error'] = st.get('last_error')
    elif 'error' in st:
        row['state_error'] = st['error']
    return row


def apply_same_config(url):
    for payload in SAME_CONFIG:
        command(url, payload)


def post_config(url, payload):
    import urllib.request
    request = urllib.request.Request(
        url + '/api/config', json.dumps(payload).encode(),
        headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def run_websad_side(url, seconds, hot_seconds):
    """Calibration collection, H1/H2 combos, H3 hot run. Assumes websad already up."""
    apply_same_config(url)
    time.sleep(1.0)
    result = {'rust_calibration': measure(url, seconds)}

    # H1: reverse direction sdr -> rta -> std (needs an IQ client for the sdr leg).
    import websockets.sync.client as wsc
    ws_base = url.replace('http://', 'ws://') + '/ws'
    iq_url = ws_base + '?iq=1'
    iq = wsc.connect(iq_url, max_size=1 << 24)
    post_config(url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post_config(url, {'cmd': 'SET_SDR', 'center': CARRIER, 'decimate': 1})
    post_config(url, {'cmd': 'SET_SDR_TUNE', 'listen': CARRIER})
    post_config(url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am'})
    time.sleep(2)
    post_config(url, {'cmd': 'SET_MODE', 'mode': 'rta'})
    time.sleep(2)
    iq.close()
    post_config(url, {'cmd': 'SET_MODE', 'mode': 'std'})
    time.sleep(2)
    st = state(url)
    result['h1_reverse'] = {'connected': st['connected'],
                            'last_error': st['last_error'],
                            'points': st['actual'].get('points')}

    # H2: repeated std -> rta -> std cycles; every cycle is a fresh Device_Open.
    cycles = []
    for i in range(3):
        post_config(url, {'cmd': 'SET_MODE', 'mode': 'rta'})
        post_config(url, {'cmd': 'SET_RTA', 'center': CARRIER, 'span': 2e6,
                          'decimate': 1})
        time.sleep(1)
        post_config(url, {'cmd': 'SET_MODE', 'mode': 'std'})
        time.sleep(1)
        st = state(url)
        cycles.append({'cycle': i, 'connected': st['connected'],
                       'last_error': st['last_error']})
    result['h2_cycles'] = cycles

    # H3: continuous vendor SWP streaming; watch state while a display client consumes.
    disp = DisplayRx(ws_base)
    disp.start()
    errors, samples = [], []
    t0 = time.monotonic()
    while time.monotonic() - t0 < hot_seconds:
        time.sleep(5)
        try:
            st = state(url)
            samples.append({'t': round(time.monotonic() - t0, 1),
                            'powr': len(disp.powr), 'connected': st['connected'],
                            'err': st['last_error']})
            if st['last_error'] or not st['connected']:
                errors.append(samples[-1])
        except Exception as exc:
            errors.append({'t': round(time.monotonic() - t0, 1), 'exc': repr(exc)})
    disp.finish()
    result['h3_hot'] = {'seconds': hot_seconds, 'powr_frames': len(disp.powr),
                        'samples': samples, 'errors': errors, 'rx_error': disp.error}
    return result


def compare(baseline, native):
    out = {'baseline': baseline, 'native': native}
    problems = []
    try:
        if baseline.get('hit') and native.get('hit'):
            out['delta'] = {
                'peak_freq_hz': round(native['hit']['near_hz'] - baseline['hit']['near_hz'], 1),
                'peak_dbm': round(native['hit']['near_dbm'] - baseline['hit']['near_dbm'], 2),
                'floor_dbm': round(native['hit']['floor_dbm'] - baseline['hit']['floor_dbm'], 2),
                'points': native['axis']['points'] - baseline['axis']['points'],
            }
        for key in ('rbw', 'vbw'):
            before = baseline.get('actual', {}).get(key)
            after = native.get('actual', {}).get(key)
            out[f'{key}_match'] = (before is not None and after is not None
                                   and abs(before - after) < 1.0)
            if not out[f'{key}_match']:
                problems.append(f'{key} mismatch baseline={before} native={after} (dBm not comparable)')
        for label, row in (('baseline', baseline), ('native', native)):
            hit = row.get('hit')
            if not hit or hit['snr_db'] < 10:
                problems.append(f'{label}: no carrier near {CARRIER:,.0f}: {hit}')
            if not row.get('connected', True):
                problems.append(f'{label}: not connected')
    except Exception as exc:
        problems.append(f'compare failed: {exc!r}')
    out['problems'] = problems
    return out


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
    parser.add_argument('--seconds', type=float, default=6.0)
    parser.add_argument('--hot-seconds', type=float, default=60.0)
    parser.add_argument('--port', type=int, default=8108)
    parser.add_argument('--baseline', type=Path, help='saved calibration measurement row JSON')
    args = parser.parse_args()
    baseline = json.loads(args.baseline.read_text()) if args.baseline else None

    if not args.hardware:
        websad, url = start_websad(args.port, '/tmp/websa_cal_fixture.log', hardware=False)
        try:
            apply_same_config(url)
            time.sleep(1.0)
            row = measure(url, args.seconds)
            print(json.dumps({'fixture_rust': row}, indent=1))
            ok = row.get('hit') and row['hit']['snr_db'] >= 10 and not row.get('rx_error')
            print('OK' if ok else 'PROBLEMS', row.get('hit'))
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
    duration = args.seconds * 2 + args.hot_seconds + 90
    pluto = subprocess.Popen(
        [sys.executable, 'tools/pluto_tx.py', 'am', '--lo', '100e6', '--base-hz', '20000',
         '--am-depth', '0.5', '--duration', str(duration)],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        time.sleep(1.0)
        subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
        if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
            raise SystemExit('WebSA still running after ./stop.sh')
        websad, url = start_websad(args.port, '/tmp/websa_cal_round.log')
        rust = run_websad_side(url, args.seconds, args.hot_seconds)
        result = rust
        row = rust['rust_calibration']
        if not row.get('hit') or row['hit']['snr_db'] < 10 or not row.get('connected'):
            problems.append(f'calibration: missing carrier or disconnected: {row}')
        if row.get('rx_error') or row.get('last_error'):
            problems.append(f'calibration: stream error: {row}')
        if baseline is not None:
            result['comparison'] = compare(baseline, row)
            problems.extend(result['comparison']['problems'])
        h1 = rust['h1_reverse']
        if not h1['connected'] or h1['last_error']:
            problems.append(f"H1 reverse switch: {h1}")
        for cycle in rust['h2_cycles']:
            if not cycle['connected'] or cycle['last_error']:
                problems.append(f"H2 cycle: {cycle}")
        hot = rust['h3_hot']
        if hot['errors'] or hot['rx_error'] or hot['powr_frames'] < args.hot_seconds * 5:
            problems.append(f"H3 hot: frames={hot['powr_frames']} errors={hot['errors'][:2]}")
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
