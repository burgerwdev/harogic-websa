#!/usr/bin/env python3
"""Live-path probe: feed the DRM fixture through a PulseAudio null sink.

This exercises the feed path the SDR pipeline will use: Dream reads from a
virtual sound card (`-I <sink>.monitor`) while `pacat` replays the fixture I/Q as
raw int16 stereo at 48 kHz. Unlike `-f file.wav`, this streams continuously and
never rewinds.

Requires a running PulseAudio/PipeWire server and `pactl`/`pacat`.

Usage:
    python3 tools/drm_dream/loopback_probe.py [--duration S] [--keep-sink]
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from probe_fixture import FIXTURE, MANIFEST, best, flatten_service  # noqa: E402
import json  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--dream-bin', default=os.environ.get('DRM_DREAM_BIN', str(HERE / 'build/dream')))
    ap.add_argument('--duration', type=float, default=15.0)
    ap.add_argument('--keep-sink', action='store_true',
                    help='do not unload the null sink afterwards')
    args = ap.parse_args()
    if not shutil.which('pactl') or not shutil.which('pacat'):
        print('pactl/pacat not available', file=sys.stderr)
        return 2

    sink = os.environ.get('DRM_DREAM_FEED_SINK', 'drm_dream_feed')
    sock = '/tmp/drm-dream-loopback.sock'
    if os.path.exists(sock):
        os.unlink(sock)

    module = subprocess.run(['pactl', 'load-module', 'module-null-sink', f'sink_name={sink}',
                             'rate=48000', 'channels=2',
                             f'sink_properties=device.description={sink}'],
                            capture_output=True, text=True, check=True).stdout.strip()
    try:
        subprocess.run(['pactl', 'set-sink-volume', sink, '100%'], check=False,
                       capture_output=True)
        subprocess.run(['pactl', 'set-sink-mute', sink, '0'], check=False,
                       capture_output=True)

        iq = np.fromfile(FIXTURE, dtype='<f4').reshape(-1, 2)
        iq48 = (np.clip(iq, -1, 1) * 32767.0).astype('<i2')
        with tempfile.NamedTemporaryFile(prefix='drm-dream-feed-', suffix='.iq48', delete=False) as f:
            f.write(iq48.reshape(-1).tobytes())
            raw = f.name

        feeder = subprocess.Popen(
            f"while true; do cat {raw}; done | pacat --raw --rate=48000 --channels=2 "
            f"--format=s16le --device={sink}",
            shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.0)

        cmd = [args.dream_bin, '-I', f'{sink}.monitor', '-c', '6', '--sigsrate', '48000',
               '-m', '1', '--status-socket', sock]
        proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            from probe_fixture import read_status
            msgs = read_status(sock, args.duration)
        finally:
            proc.terminate()
            feeder.terminate()
            for p in (proc, feeder):
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill()
            subprocess.run("pkill -f 'pacat --raw' ", shell=True, check=False)
            os.unlink(raw)

        if not msgs:
            print('FAIL: no status messages')
            return 1
        m = best(msgs)
        svcs = [flatten_service(s) for s in m.get('service_list', [])]
        ground = json.loads(MANIFEST.read_text())
        print(f'streamed {len(msgs)} messages, best snr={m.get("signal", {}).get("snr_db")} dB, '
              f'mode={m.get("mode", {}).get("robustness")}, services={svcs}')
        ok = bool(svcs) and svcs[0]['label'] == ground['sdc']['station_label'] \
            and m.get('mode', {}).get('robustness') == 1
        print('OK: loopback decode matches ground truth' if ok else 'FAIL: unexpected metadata')
        return 0 if ok else 1
    finally:
        if not args.keep_sink:
            subprocess.run(['pactl', 'unload-module', module], check=False, capture_output=True)


if __name__ == '__main__':
    sys.exit(main())
