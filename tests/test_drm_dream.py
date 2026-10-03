"""DRM-via-Dream decoder tests: status parsing, feed accounting, subprocess lifecycle."""
from __future__ import annotations

import os
import stat
import time

import numpy as np

from web_sa.drm_dream import DreamDecoder, extract_metadata, parse_status_line
from web_sa.drm_dream.status import ROBUSTNESS_MODES, RX_OK

FAKE_DREAM = '''#!/usr/bin/env python3
import json, os, socket, sys, time

sock = None
args = sys.argv[1:]
for i, a in enumerate(args):
    if a == '--status-socket' and i + 1 < len(args):
        sock = args[i + 1]

msg = {
    "status": {"io": 0, "time": 0, "frame": 0, "fac": 0, "sdc": 0, "msc": 0},
    "signal": {"snr_db": 42.5},
    "mode": {"robustness": 1, "bandwidth_khz": 10.0, "interleaver": 0},
    "service_list": [{
        "id": "123456", "label": "SAN90 DRM TEST", "is_audio": True,
        "audio_coding": 0, "bitrate_kbps": 20.96, "audio_mode": "Mono",
        "protection_mode": "EEP", "language": {"code": "eng"},
        "country": {"code": "gb"},
    }],
}

if sock:
    try:
        os.unlink(sock)
    except FileNotFoundError:
        pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(sock)
    srv.listen(1)
    srv.settimeout(0.2)
    conn = None
    while True:
        if conn is None:
            try:
                conn, _ = srv.accept()
            except socket.timeout:
                continue
        try:
            conn.sendall((json.dumps(msg) + "\\n").encode())
        except OSError:
            conn = None
        time.sleep(0.1)
else:
    while True:
        time.sleep(0.5)
'''


def _write_fake_dream(tmp_path) -> str:
    path = tmp_path / 'fake_dream'
    path.write_text(FAKE_DREAM)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return str(path)


def test_parse_status_line_ignores_garbage():
    assert parse_status_line('') is None
    assert parse_status_line('not json') is None
    assert parse_status_line('   ') is None
    assert parse_status_line('"a string"') is None
    assert parse_status_line('{"a": 1}') == {'a': 1}


def test_extract_metadata_flattens_service_and_mode():
    raw = {
        'status': {'io': 0, 'time': 0, 'frame': 0, 'fac': 0, 'sdc': 0, 'msc': 1},
        'signal': {'snr_db': 37.4},
        'mode': {'robustness': 1, 'bandwidth_khz': 10.0},
        'service_list': [{
            'id': '123456', 'label': 'SAN90 DRM TEST', 'is_audio': True,
            'audio_coding': 0, 'bitrate_kbps': 20.96, 'audio_mode': 'Mono',
            'protection_mode': 'EEP', 'language': {'code': 'eng'},
            'country': {'code': 'gb'},
        }],
    }
    meta = extract_metadata(raw)
    assert meta['station'] == 'SAN90 DRM TEST'
    assert meta['robustness'] == 'B'
    assert meta['bandwidth_khz'] == 10.0
    assert meta['bitrate_kbps'] == 20.96
    assert meta['audio_codec'] == 'AAC'
    assert meta['audio_mode'] == 'Mono'
    assert meta['language'] == 'eng'
    assert meta['country'] == 'gb'
    assert meta['sync'] is True
    assert meta['status']['msc'] == 1
    assert extract_metadata(None) == {}


def test_status_and_mode_constants():
    assert ROBUSTNESS_MODES[0] == 'A' and ROBUSTNESS_MODES[3] == 'D'
    assert RX_OK == 0


def test_feed_drops_oldest_when_queue_is_full(tmp_path):
    dec = DreamDecoder(dream_bin='/nonexistent', manage_pulse=False, capture_audio=False,
                       status_socket=str(tmp_path / 'unused.sock'), queue_blocks=1)
    i = np.zeros(480, dtype=np.float32)
    assert dec.feed(i, i) == 480
    assert dec.feed(i, i) == 480
    assert dec.dropped_blocks == 1


def test_feed_converts_complex_to_interleaved_int16(tmp_path):
    dec = DreamDecoder(dream_bin='/nonexistent', manage_pulse=False, capture_audio=False,
                       status_socket=str(tmp_path / 'unused.sock'), queue_blocks=4)
    i = np.array([1.0, -1.0, 0.0], dtype=np.float32)
    q = np.array([0.0, 0.5, -0.5], dtype=np.float32)
    assert dec.feed(i, q) == 3
    payload = dec._queue.get_nowait()
    pcm = np.frombuffer(payload, dtype='<i2')
    assert list(pcm) == [32767, 0, -32767, 16383, 0, -16383]


def test_lifecycle_start_status_and_clean_stop(tmp_path):
    bin_path = _write_fake_dream(tmp_path)
    sock = str(tmp_path / 'status.sock')
    dec = DreamDecoder(dream_bin=bin_path, status_socket=sock, manage_pulse=False,
                       capture_audio=False)
    dec.start()
    pid = dec.pid
    try:
        assert dec.alive and pid is not None
        deadline = time.time() + 5.0
        while dec.metadata().get('station') != 'SAN90 DRM TEST' and time.time() < deadline:
            time.sleep(0.05)
        meta = dec.metadata()
        assert meta['station'] == 'SAN90 DRM TEST'
        assert meta['robustness'] == 'B'
        assert meta['snr_db'] == 42.5
        assert dec.status_events > 0
    finally:
        dec.stop()

    assert not dec.alive
    assert dec.status() is None
    assert not os.path.exists(sock)
    # The dream child must be reaped, not orphaned.
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        raise AssertionError(f'dream child {pid} is still alive after stop()')


def test_restart_starts_a_fresh_process(tmp_path):
    bin_path = _write_fake_dream(tmp_path)
    dec = DreamDecoder(dream_bin=bin_path, status_socket=str(tmp_path / 'r.sock'),
                       manage_pulse=False, capture_audio=False)
    try:
        dec.start()
        first = dec.pid
        dec.restart()
        assert dec.alive
        assert dec.pid != first
    finally:
        dec.stop()


def test_stop_is_idempotent(tmp_path):
    bin_path = _write_fake_dream(tmp_path)
    dec = DreamDecoder(dream_bin=bin_path, status_socket=str(tmp_path / 'i.sock'),
                       manage_pulse=False, capture_audio=False)
    dec.start()
    dec.stop()
    dec.stop()
    assert not dec.alive
