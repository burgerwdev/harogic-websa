#!/usr/bin/env python3
"""Feasibility probe: decode the known DRM fixture with the background Dream.

This is the task-2 spike harness. It converts the interleaved float32 I/Q
fixture into a 16-bit stereo WAV (Dream's libsndfile input path), runs the
vendored console Dream with `--status-socket`, reads the newline-delimited JSON
status stream and asserts the decoded metadata against `manifest.json`.

Feed decision (see SPIKE.md): deterministic tests use the WAV file input; the
live SDR pipeline uses a PulseAudio null-sink loopback, because Dream rewinds
to offset 0 on EOF and therefore cannot follow a growing file.

Usage:
    python3 tools/drm_dream/probe_fixture.py [--dream-bin BIN] [--duration S]
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import wave
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
FIXTURE = REPO / 'tests/fixtures/drm/drm_modeB_so3_48k.f32'
MANIFEST = REPO / 'tests/fixtures/drm/manifest.json'


def f32_to_wav(src: Path, dst: Path, sample_rate: int = 48000) -> None:
    iq = np.fromfile(src, dtype='<f4').reshape(-1, 2)
    pcm = (np.clip(iq, -1.0, 1.0) * 32767.0).astype('<i2')
    with wave.open(str(dst), 'wb') as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm.reshape(-1).tobytes())


def read_status(sock_path: str, duration: float) -> list[dict]:
    """Collect status messages for `duration` seconds (Dream is the server)."""
    deadline = time.time() + 30
    while not os.path.exists(sock_path) and time.time() < deadline:
        time.sleep(0.05)
    if not os.path.exists(sock_path):
        raise RuntimeError(f'dream never created the status socket {sock_path}')

    msgs: list[dict] = []
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock_path)
    s.settimeout(1.0)
    buf = ''
    end = time.time() + duration
    try:
        while time.time() < end:
            try:
                chunk = s.recv(16384)
            except TimeoutError:
                continue
            if not chunk:
                break
            buf += chunk.decode('utf-8', 'ignore')
            while '\n' in buf:
                line, buf = buf.split('\n', 1)
                if line.strip():
                    msgs.append(json.loads(line))
    finally:
        s.close()
    return msgs


def best(msgs: list[dict]) -> dict:
    return max(msgs, key=lambda m: m.get('signal', {}).get('snr_db', 0.0) or 0.0)


def flatten_service(svc: dict) -> dict:
    return {
        'label': svc.get('label'),
        'bitrate_kbps': svc.get('bitrate_kbps'),
        'audio_mode': svc.get('audio_mode'),
        'protection_mode': svc.get('protection_mode'),
        'country': (svc.get('country') or {}).get('code'),
    }


def check(dream_bin: str, duration: float) -> int:
    ground = json.loads(MANIFEST.read_text())
    sock = tempfile.mktemp(prefix='drm-dream-probe-', suffix='.sock')
    failures: list[str] = []

    with tempfile.TemporaryDirectory(prefix='drm-dream-probe-') as tmp:
        wav = Path(tmp) / 'fixture.wav'
        f32_to_wav(FIXTURE, wav)
        cmd = [dream_bin, '-f', str(wav), '-c', '6', '--sigsrate', '48000',
               '-m', '1', '--status-socket', sock]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, cwd=tmp)
        try:
            msgs = read_status(sock, duration)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    if not msgs:
        print('FAIL: no status messages received')
        return 1

    m = best(msgs)
    mode = m.get('mode', {})
    status = m.get('status', {})
    svcs = [flatten_service(s) for s in m.get('service_list', [])]
    print(f'messages={len(msgs)} snr={m.get("signal", {}).get("snr_db")} dB '
          f'robustness={mode.get("robustness")} bandwidth={mode.get("bandwidth_khz")} kHz '
          f'status={status}')

    exp_label = ground['sdc']['station_label']
    exp_bitrate = ground['msc']['bitrate_kbps']
    exp_bw = ground['signal']['bandwidth_khz']

    if not svcs:
        failures.append('no service decoded')
    else:
        svc = svcs[0]
        print(f'service={svc}')
        if svc['label'] != exp_label:
            failures.append(f'label {svc["label"]!r} != {exp_label!r}')
        if svc['bitrate_kbps'] is None or abs(svc['bitrate_kbps'] - exp_bitrate) > 0.1:
            failures.append(f'bitrate {svc["bitrate_kbps"]} != {exp_bitrate}')
    if mode.get('robustness') != 1:
        failures.append(f'robustness {mode.get("robustness")} != 1 (mode B)')
    if mode.get('bandwidth_khz') != exp_bw:
        failures.append(f'bandwidth {mode.get("bandwidth_khz")} != {exp_bw}')
    if status.get('sdc') != 0 or status.get('msc') not in (0, 1):
        failures.append(f'sdc/msc status not OK: {status}')

    if failures:
        for f in failures:
            print(f'FAIL: {f}')
        return 1
    print('OK: decoded metadata matches manifest ground truth')
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    default_bin = os.environ.get('DRM_DREAM_BIN', str(HERE / 'build/dream'))
    ap.add_argument('--dream-bin', default=default_bin)
    ap.add_argument('--duration', type=float, default=12.0)
    args = ap.parse_args()
    if not os.path.exists(args.dream_bin):
        print(f'dream binary not found: {args.dream_bin}\n'
              f'build it with tools/drm_dream/build_dream.sh', file=sys.stderr)
        return 2
    return check(args.dream_bin, args.duration)


if __name__ == '__main__':
    sys.exit(main())
