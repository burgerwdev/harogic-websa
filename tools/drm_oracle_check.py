#!/usr/bin/env python3
"""Decode a baseband capture with Dream, to separate a signal problem from a receiver problem.

Why this exists: the DRM receiver in this repository is under development. When it fails,
two causes are possible. Either the bench signal is not decodable, or the receiver is at
fault. Dream is a complete, independent decoder. It answers that question for one capture.

The input is the raw interleaved float32 file that `tools/drm_capture.py` writes. Dream
takes a 16-bit stereo WAV, so this tool resamples to 48 kHz, normalises the level and
writes a temporary WAV. Dream clips at full scale, so the normalisation is required: a
bench capture is often far above full scale.

Usage:
    python3 tools/drm_oracle_check.py tests/fixtures/drm/drm_live_modeB_so3_48828.f32 48828.125

The decoder binary comes from `--dream-bin`, `DRM_DREAM_BIN` or the console build in the
`feature/drm-dream-decoder` worktree. Dream's `--status-socket` is needed for the metadata;
see that worktree's `docs/en/DRM_DREAM.md`.
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

#: The console Dream build that exposes `--status-socket` (see DECISION.md in the other worktree).
DEFAULT_DREAM = ('/home/hui/git/harogic-websa-drm-dream/tools/drm_dream/build/dream')
#: Dream's input rate, in Hz. DRM mode B spans 10 kHz, so 48 kHz is enough.
DREAM_RATE = 48_000.0


def resample_sinc(z: np.ndarray, fs_in: float, fs_out: float, taps: int = 32) -> np.ndarray:
    """Resample with a windowed sinc. A linear resampler is enough for the DRM signal, but this
    keeps the oracle independent of the receiver's own rate conversion."""
    n_out = int(round(z.size * fs_out / fs_in))
    pos = np.arange(n_out) * (fs_in / fs_out)
    i0 = np.floor(pos).astype(np.int64)
    frac = pos - i0
    half = taps // 2
    idx = np.arange(-half + 1, half + 1)
    kernel = np.sinc(idx[None, :] - frac[:, None]) * np.blackman(taps)[None, :]
    picked = np.clip(i0[:, None] + idx[None, :], 0, z.size - 1)
    return (z[picked] * kernel).sum(axis=1)


def read_status(sock_path: str, duration: float) -> list[dict]:
    """Read the newline-delimited JSON status stream for `duration` seconds."""
    deadline = time.time() + 30
    while not os.path.exists(sock_path) and time.time() < deadline:
        time.sleep(0.05)
    if not os.path.exists(sock_path):
        raise SystemExit(f'dream never created the status socket {sock_path}')
    messages: list[dict] = []
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(sock_path)
    sock.settimeout(1.0)
    buffer = ''
    end = time.time() + duration
    try:
        while time.time() < end:
            try:
                chunk = sock.recv(16384)
            except TimeoutError:
                continue
            if not chunk:
                break
            buffer += chunk.decode('utf-8', 'ignore')
            while '\n' in buffer:
                line, buffer = buffer.split('\n', 1)
                if line.strip():
                    messages.append(json.loads(line))
    finally:
        sock.close()
    return messages


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('capture', help='raw interleaved float32 I/Q file')
    parser.add_argument('rate', type=float, nargs='?', default=DREAM_RATE,
                        help='sample rate of the capture in Hz (default: 48000)')
    parser.add_argument('--dream-bin', default=os.environ.get('DRM_DREAM_BIN', DEFAULT_DREAM),
                        help='Dream binary with --status-socket support')
    parser.add_argument('--seconds', type=float, default=12.0, help='seconds of status to read')
    args = parser.parse_args()

    if not os.path.exists(args.dream_bin):
        print(f'dream binary not found: {args.dream_bin}', file=sys.stderr)
        return 2

    raw = np.fromfile(args.capture, dtype='<f4').reshape(-1, 2)
    z = (raw[:, 0] + 1j * raw[:, 1]).astype(np.complex128)
    if args.rate != DREAM_RATE:
        z = resample_sinc(z, args.rate, DREAM_RATE)
    print(f'{args.capture}: {z.size} complex samples at {DREAM_RATE:.0f} Hz '
          f'({z.size / DREAM_RATE:.2f} s), rms={np.sqrt(np.mean(np.abs(z) ** 2)):.3f}, '
          f'peak={np.abs(z).max():.3f}')
    z = z / np.abs(z).max() * 0.9
    pcm = np.empty(z.size * 2, dtype='<i2')
    pcm[0::2] = np.clip(z.real, -1, 1) * 32767
    pcm[1::2] = np.clip(z.imag, -1, 1) * 32767

    with tempfile.TemporaryDirectory(prefix='drm-oracle-') as tmp:
        wav = Path(tmp) / 'capture.wav'
        with wave.open(str(wav), 'wb') as handle:
            handle.setnchannels(2)
            handle.setsampwidth(2)
            handle.setframerate(int(DREAM_RATE))
            handle.writeframes(pcm.tobytes())
        status_socket = str(Path(tmp) / 'status.sock')
        command = [args.dream_bin, '-f', str(wav), '-c', '6', '--sigsrate', str(int(DREAM_RATE)),
                   '-m', '1', '--status-socket', status_socket]
        print('running:', ' '.join(command), flush=True)
        proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, cwd=tmp)
        try:
            messages = read_status(status_socket, args.seconds)
        finally:
            proc.terminate()
            try:
                tail, _ = proc.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                tail = ''

    if not messages:
        print('FAIL: dream sent no status message')
        if tail:
            print(tail[-1500:])
        return 1
    best = max(messages, key=lambda m: (m.get('signal', {}) or {}).get('snr_db', 0.0) or 0.0)
    mode = best.get('mode', {})
    print(f'messages={len(messages)}  snr={best.get("signal", {}).get("snr_db")} dB  '
          f'robustness={mode.get("robustness")}  bandwidth={mode.get("bandwidth_khz")} kHz  '
          f'status={best.get("status")}')
    for service in best.get('service_list', []):
        print(f'  service: label={service.get("label")!r} bitrate={service.get("bitrate_kbps")} '
              f'audio_mode={service.get("audio_mode")} protection={service.get("protection_mode")}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
