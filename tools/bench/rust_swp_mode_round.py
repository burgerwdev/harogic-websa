#!/usr/bin/env python3
"""One exclusive SAN-90 round: vendor SWP std -> FPGA RTA -> SDR IQS -> std, one handle.

Proves on the real device (and in fixture mode without it):
  P1 websad mode=std publishes real vendor SWP FREQ/POWR (monotonic vendor axis, points,
     sweep estimate) and /api/state keeps the existing contract.
  P2 mode=rta still streams vendor FPGA RTAF after the SWP handle closed; a known carrier
     lands inside the RTA span.
  P3 mode=sdr reopens vendor IQS after RTA closed and streams consecutive IQBF.
  P4 the second mode=std reopens vendor SWP after both, with the same real frames, and a
     narrow-window sweep shows the Pluto carrier at the requested frequency.
  P5 Restore the native service: stop the round, RF off, ./run.sh, restore, field diff.

Hardware mode saves /api/state, ./stop.sh, starts Pluto AM at 100.020 MHz, and ALWAYS:
stops websad, waits for the TX to end (RF off), ./run.sh, restore, diff.
"""
from __future__ import annotations

import argparse
import json
import os
import struct
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

import numpy as np
import websockets.sync.client as wsc

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'
WEBSAD_PORT = 8108
CARRIER = 100_020_000.0          # Pluto: --lo 100e6 --base-hz 20000
FULL_CENTER, FULL_SPAN = 4_510_004_000.0, 9_019_992_000.0
WEBSAD = ROOT / 'native-ddc/target/release/websad'


def post(url, payload):
    headers = {'Content-Type': 'application/json'}
    request = urllib.request.Request(url + '/api/config', json.dumps(payload).encode(),
                                     headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        body = exc.read().decode('utf-8', 'replace')
        raise RuntimeError(f'{payload.get("cmd")} HTTP {exc.code}: {body}') from exc


def get(url, path='/api/state'):
    with urllib.request.urlopen(url + path, timeout=5) as response:
        return json.load(response)


class DisplayRx(threading.Thread):
    """Display-side frames: FREQ/POWR/RTAF binaries plus the 1 Hz status JSON."""

    def __init__(self, url):
        super().__init__(daemon=True)
        self.sock = wsc.connect(url, max_size=1 << 24)
        self.halt = threading.Event()
        self.error = None
        self.freq, self.powr, self.rtaf = [], [], []
        self.last_freq = None
        self.status = {}

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    data = self.sock.recv(timeout=0.1)
                except TimeoutError:
                    continue
                if isinstance(data, str) or len(data) < 16:
                    continue
                tag = bytes(data[:4])
                if tag == b'FREQ':
                    points = int.from_bytes(data[8:12], 'little')
                    axis = np.frombuffer(data, '<f8', points, 16)
                    self.freq.append(axis)
                    self.last_freq = axis
                elif tag == b'POWR':
                    points = int.from_bytes(data[8:12], 'little')
                    power = np.frombuffer(data, '<f4', points, 16)
                    self.powr.append((self.last_freq, power))
                elif tag in (b'RTAF', b'RTA2'):
                    points = int.from_bytes(data[8:12], 'little')
                    wf_len = int.from_bytes(data[12:14], 'little')
                    start = struct.unpack_from('<d', data, 16)[0]
                    step = 0.0
                    if points > 1:
                        stop = struct.unpack_from(
                            '<d', data, 24 + points * 8 + points * 4 + wf_len * 2)[0]
                        step = (stop - start) / points
                    power = np.frombuffer(data, '<f4', points, 24 + points * 8)
                    freq = start + step * np.arange(points)
                    self.rtaf.append((freq, power))
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


class IqRx(threading.Thread):
    def __init__(self, url):
        super().__init__(daemon=True)
        self.sock = wsc.connect(url, max_size=1 << 24)
        self.halt = threading.Event()
        self.error = None
        self.frames = []

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    data = self.sock.recv(timeout=0.1)
                except TimeoutError:
                    continue
                if not isinstance(data, bytes) or data[:4] != b'IQBF':
                    continue
                if len(data) < 56:
                    raise ValueError('short IQBF header')
                version, seq, count = struct.unpack_from('<III', data, 4)
                if version != 2 or len(data) != 56 + count * 8:
                    raise ValueError('invalid IQBF v2 frame')
                rate, center = struct.unpack_from('<dd', data, 16)
                self.frames.append((version, seq, count, rate, center))
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


def start_websad(port, log_path, shim=None, *, hardware=True):
    env = {**os.environ, 'WEBSA_PORT': str(port), 'WEBSA_HOST': '127.0.0.1'}
    if hardware:
        selected_shim = shim or env.get('WEBSA_RUST_IQS_SHIM')
        if not selected_shim:
            raise SystemExit('set WEBSA_RUST_IQS_SHIM to the shim built for the current native backend')
        env['WEBSA_RUST_IQS_SHIM'] = selected_shim
        env['WEBSA_FAKE'] = '0'
    else:
        env.pop('WEBSA_RUST_IQS_SHIM', None)
        env['WEBSA_FAKE'] = '1'
    with open(log_path, 'w') as log:
        proc = subprocess.Popen([str(WEBSAD)], cwd=ROOT, env=env, stdout=log, stderr=log)
    url = f'http://127.0.0.1:{port}'
    for _ in range(50):
        try:
            with urllib.request.urlopen(url + '/api/state', timeout=1):
                return proc, url
        except OSError:
            time.sleep(0.1)
    proc.terminate()
    raise SystemExit(f'websad did not come up; log: {log_path}')


def peak_of(freq, power):
    index = int(np.argmax(power))
    return float(freq[index]), float(power[index]), float(np.percentile(power, 30))


def carrier_hit(freq, power, center, window=50e3):
    """Peak near the known carrier vs the 30th-percentile floor; RF bands hold stronger
    strangers (FM broadcast, LO feedthrough), so the global maximum is not the check."""
    band = np.abs(freq - center) <= window
    if not band.any():
        return None
    floor = float(np.percentile(power, 30))
    local = float(np.max(power[band]))
    near_f = float(freq[band][int(np.argmax(power[band]))])
    return {'near_hz': near_f, 'near_dbm': round(local, 2),
            'floor_dbm': round(floor, 2), 'snr_db': round(local - floor, 1)}


def run_round(url, seconds):
    result = {}

    def ws_url(query):
        base = url.replace('http://', 'ws://') + '/ws'
        return base + f'?{query}' if query else base

    def phase_std(label, expect_frames):
        rx = DisplayRx(ws_url(''))
        rx.start()
        time.sleep(0.3)
        reply = post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
        assert reply.get('mode') == 'std'
        time.sleep(seconds)
        st = get(url)
        rx.finish()
        freq_count, powr_count = len(rx.freq), len(rx.powr)
        row = {'freq_frames': freq_count, 'powr_frames': powr_count, 'rx_error': rx.error,
               'connected': st['connected'], 'last_error': st['last_error']}
        row['actual'] = {k: st['actual'].get(k) for k in
                         ('start', 'stop', 'points', 'rbw', 'vbw', 'refclk', 'refclk_src')}
        if rx.powr:
            axis, power = rx.powr[-1]
            row['axis_monotonic'] = bool(
                axis is not None and axis.size > 1 and np.all(np.diff(axis) > 0))
            peak_f, peak_dbm, floor = peak_of(axis, power)
            row.update(peak_hz=peak_f, peak_dbm=round(peak_dbm, 2), floor_dbm=round(floor, 2))
        result[label] = row
        problems = []
        if freq_count < 1 or powr_count < expect_frames or rx.error:
            problems.append(f'{label}: frames freq={freq_count} powr={powr_count}')
        if not st['connected'] or st['last_error']:
            problems.append(f"{label}: connected={st['connected']} err={st['last_error']!r}")
        if rx.powr:
            axis, _ = rx.powr[-1]
            if axis is None or axis.size < 2 or not np.all(np.diff(axis) > 0):
                problems.append(f'{label}: axis not monotonic')
        return problems

    problems = []
    # P1: std on the full band request.
    problems += phase_std('std_full', 3)

    # P2: vendor FPGA RTA after SWP closed.
    rx = DisplayRx(ws_url(''))
    rx.start()
    post(url, {'cmd': 'SET_MODE', 'mode': 'rta'})
    post(url, {'cmd': 'SET_RTA', 'center': CARRIER, 'span': 2e6, 'decimate': 1})
    time.sleep(seconds)
    st = get(url)
    rx.finish()
    row = {'rtaf_frames': len(rx.rtaf), 'rx_error': rx.error,
           'connected': st['connected'], 'last_error': st['last_error'],
           'actual': {k: st['rta_actual'].get(k) for k in ('center', 'span', 'points')}}
    if rx.rtaf:
        freq, power = rx.rtaf[-1]
        hit = carrier_hit(freq, power, CARRIER)
        row.update(rta_hit=hit, span_actual=float(freq[-1] - freq[0]))
    result['rta'] = row
    if len(rx.rtaf) < max(10, seconds * 10) or rx.error:
        problems.append(f"rta: frames={len(rx.rtaf)} err={rx.error}")
    if not st['connected'] or st['last_error']:
        problems.append(f"rta: connected={st['connected']} err={st['last_error']!r}")
    if not row.get('rta_hit') or row['rta_hit']['snr_db'] < 10:
        problems.append(f"rta: no carrier near {CARRIER:,.0f}: {row.get('rta_hit')}")

    # P3: vendor IQS after RTA closed.
    post(url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(url, {'cmd': 'SET_SDR', 'center': CARRIER, 'decimate': 1})
    post(url, {'cmd': 'SET_SDR_TUNE', 'listen': CARRIER})
    post(url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am'})
    iq = IqRx(ws_url('iq=1'))
    iq.start()
    time.sleep(seconds)
    st = get(url)
    iq.finish()
    seqs = [f[1] for f in iq.frames]
    row = {'frames': len(iq.frames), 'rx_error': iq.error,
           'seq_ok': bool(seqs) and seqs == list(range(len(seqs))),
           'connected': st['connected'], 'last_error': st['last_error'],
           'iqs_packets': st['sdr']['actual'].get('iqs_capture', {}).get('packets')}
    if iq.frames:
        row['rate'] = iq.frames[0][3]
        row['center'] = iq.frames[0][4]
    result['sdr'] = row
    if not row['seq_ok'] or row['frames'] < seconds * 10 or iq.error:
        problems.append(f"sdr: frames={row['frames']} seq_ok={row['seq_ok']} err={iq.error}")
    if iq.frames and abs(iq.frames[0][3] - 62.5e6 / 64 / 12) > 1.0:
        problems.append(f"sdr: rate {iq.frames[0][3]}")
    if iq.frames and abs(iq.frames[0][4] - CARRIER) > 1.0:
        problems.append(f"sdr: center {iq.frames[0][4]}")
    if not st['connected'] or st['last_error']:
        problems.append(f"sdr: connected={st['connected']} err={st['last_error']!r}")

    # P4: std again after both RTA and IQS closed, then the narrow carrier window.
    problems += phase_std('std_again', 3)
    row = {}
    # Reconfigure to the narrow window; the new display client receives the fresh FREQ axis
    # as its initial frame, so POWR frames pair with the 20 MHz window.
    rx = DisplayRx(ws_url(''))
    rx.start()
    post(url, {'cmd': 'SET_FREQ', 'center': CARRIER, 'span': 20e6})
    time.sleep(1.0 + seconds)
    st = get(url)
    row.update(connected=st['connected'], last_error=st['last_error'],
               actual={k: st['actual'].get(k) for k in ('start', 'stop', 'points', 'rbw')})
    rx.finish()
    if rx.powr:
        axis, power = rx.powr[-1]
        hit = carrier_hit(axis, power, CARRIER, window=200e3)
        global_f, global_dbm, _ = peak_of(axis, power)
        row.update(hit=hit, global_peak_hz=global_f, global_peak_dbm=round(global_dbm, 2),
                   freq_frames=len(rx.freq), powr_frames=len(rx.powr))
        if not hit or hit['snr_db'] < 10:
            problems.append(f'narrow: no carrier near {CARRIER:,.0f}: {hit}')
    else:
        problems.append('narrow: no POWR frames')
    if st['last_error'] or not st['connected']:
        problems.append(f"narrow: err={st['last_error']!r} connected={st['connected']}")
    result['std_narrow'] = row
    # Leave the vendor SWP on the original full-band request.
    post(url, {'cmd': 'SET_FREQ', 'center': FULL_CENTER, 'span': FULL_SPAN})
    return result, problems


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
    parser.add_argument('--seconds', type=float, default=5.0)
    parser.add_argument('--port', type=int, default=WEBSAD_PORT)
    args = parser.parse_args()
    url = f'http://127.0.0.1:{args.port}'
    websad = None
    pluto = None
    problems = ['not run']
    if not args.hardware:
        websad, url = start_websad(args.port, '/tmp/websad_swp_round.log', hardware=False)
        try:
            result, problems = run_round(url, args.seconds)
        finally:
            websad.terminate()
            try:
                websad.wait(timeout=5)
            except subprocess.TimeoutExpired:
                websad.kill()
        print(json.dumps(result, indent=1))
        print('PROBLEMS' if problems else 'OK', problems)
        return 1 if problems else 0


    saved = state(SERVICE)
    subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
    if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
        raise SystemExit('WebSA still running after ./stop.sh')
    duration = args.seconds * 6 + 30
    pluto = subprocess.Popen(
        [sys.executable, 'tools/pluto_tx.py', 'am', '--lo', '100e6', '--base-hz', '20000',
         '--am-depth', '0.5', '--duration', str(duration)],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        websad, url = start_websad(args.port, '/tmp/websad_swp_round.log')
        result, problems = run_round(url, args.seconds)
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
