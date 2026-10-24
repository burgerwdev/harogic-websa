#!/usr/bin/env python3
"""30-minute hot-state round for one hardware mode (rta or sdr) through websad.

  H  continuous streaming in the chosen mode with a live consumer: frame-rate
     stability, error-free state, and (sdr) strict IQBF sequence continuity with the
     capture stats from /api/state.sdr.actual.iqs_capture.

Source: direct-connect tinySA CW at 100.2 MHz -25 dBm (realistic load; the stability
verdict does not depend on the RF content). Hardware mode always: save state,
./stop.sh, tinySA output off + close in finally, ./run.sh, restore, field diff.
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
IQ_RATE = 62_500_000.0 / 64 / 12


class HotRx(threading.Thread):
    """Counts RTAF / IQBF frames; verifies IQBF sequence continuity."""

    def __init__(self, url):
        super().__init__(daemon=True)
        self.sock = wsc.connect(url, max_size=1 << 24)
        self.halt = threading.Event()
        self.error = None
        self.rtaf = 0
        self.iqbf = 0
        self.iq_seq_ok = True
        self.last_iq_seq = None
        self.iq_rate = None
        self.iq_center = None

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    data = self.sock.recv(timeout=0.2)
                except TimeoutError:
                    continue
                if not isinstance(data, bytes) or len(data) < 16:
                    continue
                tag = data[:4]
                if tag in (b'RTAF', b'RTA2'):
                    self.rtaf += 1
                elif tag == b'IQBF':
                    import struct
                    if len(data) < 56:
                        raise ValueError('short IQBF header')
                    version, seq, count = struct.unpack_from('<III', data, 4)
                    if version != 2 or len(data) != 56 + count * 8:
                        raise ValueError('invalid IQBF v2 frame')
                    rate, center = struct.unpack_from('<dd', data, 16)
                    if self.last_iq_seq is not None and seq != self.last_iq_seq + 1:
                        self.iq_seq_ok = False
                    self.last_iq_seq = seq
                    self.iqbf += 1
                    self.iq_rate = rate
                    self.iq_center = center
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


def same(before, after):
    keys = ('center', 'decimate', 'listen', 'demod', 'if_bw', 'squelch', 'agc', 'volume')
    diff = ['mode'] if before['mode'] != after['mode'] else []
    diff += [f'swp.{k}' for k, v in before['req']['swp'].items()
             if after['req']['swp'].get(k) != v]
    diff += [f'sdr.{k}' for k in keys if before['req']['sdr'][k] != after['req']['sdr'][k]]
    return diff


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=['rta', 'sdr'], required=True)
    parser.add_argument('--minutes', type=float, default=30.0)
    parser.add_argument('--port', type=int, default=8108)
    parser.add_argument('--tinysa-port', default='/dev/ttyACM0')
    args = parser.parse_args()


    saved = state(SERVICE)
    source = None
    websad = None
    result, diff, problems = {}, ['not run'], []
    try:
        source = TinySA(args.tinysa_port)
        source.cw(CARRIER, -25.0)
        time.sleep(1.0)
        subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
        if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
            raise SystemExit('WebSA still running after ./stop.sh')
        websad, url = start_websad(args.port, f'/tmp/websa_hot_{args.mode}.log')
        base = url.replace('http://', 'ws://') + '/ws'
        if args.mode == 'rta':
            post(url, {'cmd': 'SET_MODE', 'mode': 'rta'})
            post(url, {'cmd': 'SET_RTA', 'center': CARRIER, 'span': 2e6, 'decimate': 1})
            rx = HotRx(base)
        else:
            post(url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            post(url, {'cmd': 'SET_SDR', 'center': CARRIER, 'decimate': 1})
            post(url, {'cmd': 'SET_SDR_TUNE', 'listen': CARRIER})
            post(url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am'})
            rx = HotRx(base + '?iq=1')
        rx.start()
        samples = []
        t0 = time.monotonic()
        last_snap = -300.0
        while time.monotonic() - t0 < args.minutes * 60:
            time.sleep(5)
            now = time.monotonic() - t0
            if now - last_snap >= 300:
                last_snap = now
                st = get_state(url)
                cap = st['sdr']['actual'].get('iqs_capture') or {}
                samples.append({
                    't_min': round(now / 60, 1),
                    'rtaf': rx.rtaf, 'iqbf': rx.iqbf, 'seq_ok': rx.iq_seq_ok,
                    'connected': st['connected'], 'err': st['last_error'],
                    'iqs_packets': cap.get('packets'),
                    'queue_wait_max_ms': cap.get('queue_wait_max_ms'),
                    'skips': cap.get('skips'),
                    'recoveries': cap.get('recoveries'),
                    'ddc_error_count': st['sdr']['actual'].get('ddc_error_count'),
                    'ddc_error_last': st['sdr']['actual'].get('ddc_error_last'),
                    'iqs_stamp_gaps': st['sdr']['actual'].get('iqs_stamp_gaps'),
                })
                if st['last_error'] or not st['connected']:
                    problems.append(f'sample died: {samples[-1]}')
        rx.finish()
        st = get_state(url)
        result = {'mode': args.mode, 'minutes': args.minutes, 'samples': samples,
                  'rx_error': rx.error, 'final_state': {
                      'connected': st['connected'], 'last_error': st['last_error']},
                  'rtaf_frames': rx.rtaf, 'iqbf_frames': rx.iqbf,
                  'iq_seq_ok': rx.iq_seq_ok, 'iq_rate': rx.iq_rate,
                  'iq_center': rx.iq_center}
        seconds = args.minutes * 60
        if args.mode == 'rta':
            if result['rtaf_frames'] < seconds * 50:
                problems.append(f"rta frames {result['rtaf_frames']} < {seconds * 50}")
        else:
            if result['iqbf_frames'] < seconds * 20:
                problems.append(f"iqbf frames {result['iqbf_frames']}")
            if not result['iq_seq_ok']:
                problems.append('iqbf sequence gap')
            if rx.iq_rate and abs(rx.iq_rate - IQ_RATE) > 1.0:
                problems.append(f'iq rate {rx.iq_rate}')
            if rx.iq_center and abs(rx.iq_center - CARRIER) > 1.0:
                problems.append(f'iq center {rx.iq_center}')
        if rx.error or st['last_error'] or not st['connected']:
            problems.append(f"final: {rx.error} {st['last_error']!r} {st['connected']}")
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
        guarded('restore', lambda: restore(SERVICE, saved))
        try:
            diff = same(saved, state(SERVICE))
        except Exception as exc:
            diff = [f'state readback failed: {exc!r}']
        print('restore diff', diff or 'none')
    print(json.dumps(result, indent=1)[:6000])
    problems += ['restore'] if diff else []
    print('PROBLEMS' if problems else 'OK', problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
