#!/usr/bin/env python3
"""One exclusive SAN-90 round with a DIRECT-CONNECTED tinySA Ultra CW source.

The radiated Pluto bench carrier (-58 dBm) is below the vendor PNM carrier detector
(-203 CarrierLoss, AutoSearch finds nothing). A coax-direct tinySA at -25 dBm should
cross it, which also gives a strong-level calibration point and a clean harmonic source.

  P1 pnm       : first real vendor PNM data through websad (partial -> done -> restart).
  P2 level     : native SWP near-carrier peak vs the tinySA set level.
  P3 gnss      : first hardware /api/state.gnss snapshot (whatever the receiver reports).
  P4 harmonic  : HARM rows on the tinySA's own (clean) fundamental and harmonics.

Hardware mode always: save state, ./stop.sh, tinySA output off + close in finally,
./run.sh, restore, field diff. Never port 8080 for websad.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / 'tools/sdr_probe'))
from tinysa import TinySA  # noqa: E402

from tools.bench.rust_harm_pnm_round import MsgRx  # noqa: E402
from tools.bench.rust_swp_calibration import apply_same_config, measure  # noqa: E402
from tools.bench.rust_swp_mode_round import post, start_websad  # noqa: E402
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402

SERVICE = 'http://127.0.0.1:8080'
FREQ = 100_200_000.0
LEVEL = -25.0


def get_state(url, path='/api/state'):
    import urllib.request
    with urllib.request.urlopen(url + path, timeout=5) as response:
        return json.load(response)


def wait_health(url):
    for _ in range(30):
        try:
            return get_state(url)
        except OSError:
            time.sleep(1)
    raise SystemExit(f'service not healthy: {url}')


def run_websad_side(url, rx, seconds, level):
    result, problems = {}, []
    apply_same_config(url)
    time.sleep(1.0)
    result['rust'] = measure(url, seconds)

    # P3: GNSS snapshot (first hardware observation; report whatever the receiver says).
    st = get_state(url)
    result['gnss'] = st.get('gnss')

    # P1: PNM on the direct carrier.
    post(url, {'cmd': 'SET_MODE', 'mode': 'pnm'})
    post(url, {'cmd': 'SET_PNM', 'center': FREQ, 'threshold': -50.0,
               'traceavg': 1, 'start': 100.0, 'stop': 1e6})
    done_msg = rx.await_text(lambda m: m.get('cmd') == 'PNM' and m.get('done') is True, 120)
    restart = rx.await_text(
        lambda m: m.get('cmd') == 'PNM' and m.get('done') is False
        and done_msg is not None and m is not done_msg
        and (m.get('progress') or 0) < 100, 30)
    result['pnm_done'] = done_msg
    result['pnm_restarted'] = restart is not None
    st = get_state(url)
    result['pnm_state'] = {'connected': st['connected'], 'last_error': st['last_error']}
    if done_msg is None:
        problems.append('pnm: no done message in 120 s (carrier still below detector?)')
    else:
        carrier = done_msg.get('carrier_freq')
        offsets = done_msg.get('offset') or []
        pn = done_msg.get('pn') or []
        if not isinstance(carrier, (int, float)) or abs(carrier - FREQ) > 50e3:
            problems.append(f'pnm: carrier {carrier} not near {FREQ:,.0f}')
        if len(offsets) < 8 or len(offsets) != len(pn):
            problems.append(f'pnm: trace shape offsets={len(offsets)} pn={len(pn)}')
        if offsets != sorted(offsets):
            problems.append('pnm: offset axis not ascending')
    if done_msg is not None and restart is None:
        problems.append('pnm: cycle did not restart after done')
    if st['last_error'] or not st['connected']:
        problems.append(f"pnm: state {st['last_error']!r} connected={st['connected']}")

    # P4: harmonic on the clean tinySA fundamental.
    post(url, {'cmd': 'SET_MODE', 'mode': 'harmonic'})
    post(url, {'cmd': 'SET_HARM', 'f0': FREQ, 'count': 3, 'span': 2e6})
    harm = rx.await_text(lambda m: m.get('cmd') == 'HARM', 120)
    result['harm'] = harm
    st = get_state(url)
    result['harm_state'] = {'connected': st['connected'], 'last_error': st['last_error']}
    if harm is None:
        problems.append('harmonic: no HARM message in 120 s')
    else:
        rows = harm.get('list') or []
        first = rows[0] if rows else {}
        f1, amp1 = first.get('f'), first.get('amp')
        if not isinstance(f1, (int, float)) or abs(f1 - FREQ) > 30e3:
            problems.append(f'harmonic: n=1 peak {f1} not near {FREQ:,.0f}')
        if not isinstance(amp1, (int, float)) or abs(amp1 - level) > 3.0:
            problems.append(f'harmonic: n=1 amp {amp1} not near {level} dBm')
    if st['last_error'] or not st['connected']:
        problems.append(f"harmonic: state {st['last_error']!r} connected={st['connected']}")
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
    parser.add_argument('--port', type=int, default=8108)
    parser.add_argument('--tinysa-port', default='/dev/ttyACM1')
    parser.add_argument('--seconds', type=float, default=6.0)
    parser.add_argument('--level', type=float, default=LEVEL,
                        help='tinySA CW output dBm (P2c: -25 was too weak for the '
                             'vendor PNM carrier detector; tinySA Ultra tops ~0)')
    args = parser.parse_args()

    if not args.hardware:
        websad, url = start_websad(args.port, '/tmp/websa_tinysa_fixture.log', hardware=False)
        try:
            rx = MsgRx(url.replace('http://', 'ws://') + '/ws')
            rx.start()
            result, problems = run_websad_side(url, rx, args.seconds, args.level)
            rx.finish()
        finally:
            websad.terminate()
            try:
                websad.wait(timeout=5)
            except subprocess.TimeoutExpired:
                websad.kill()
        print(json.dumps(result, indent=1, default=str)[:3000])
        print('PROBLEMS' if problems else 'OK', problems)
        return 1 if problems else 0


    saved = state(SERVICE)
    source = None
    websad = None
    result, diff, problems = {}, ['not run'], []
    try:
        source = TinySA(args.tinysa_port)
        source.cw(FREQ, args.level)  # full generator reset + output on exactly once
        time.sleep(1.0)
        subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
        if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
            raise SystemExit('WebSA still running after ./stop.sh')
        websad, url = start_websad(args.port, '/tmp/websa_tinysa_round.log')
        rx = MsgRx(url.replace('http://', 'ws://') + '/ws')
        rx.start()
        result, problems = run_websad_side(url, rx, args.seconds, args.level)
        rx.finish()
        # P2 verdict: native measured level agrees with the direct source.
        hit = (result.get('rust') or {}).get('hit')
        if hit:
            result['level_delta'] = {'native_vs_set_dbm': round(hit['near_dbm'] - args.level, 2)}
            if abs(hit['near_dbm'] - args.level) > 3.0:
                problems.append(f"level: native peak {hit['near_dbm']} vs set {args.level}")
        else:
            problems.append('level: missing native carrier hit')
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
    # The locked-PNM result carries the full pn trace (10k+ journal lines); dump it
    # to a file and keep the journal slice small, or journald rate-limiting drops
    # the PROBLEMS verdict that follows.
    try:
        dump = Path('/tmp/websa_pnm_result.json')
        dump.write_text(json.dumps(result, indent=1, default=str))
        print(f'result file: {dump} ({dump.stat().st_size} bytes)')
    except Exception as exc:
        print(f'result dump failed: {exc!r}')
    print(json.dumps(result, indent=1, default=str)[:3000])
    problems += ['restore'] if diff else []
    print('PROBLEMS' if problems else 'OK', problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
