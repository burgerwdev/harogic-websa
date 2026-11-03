#!/usr/bin/env python3
"""Real monitor gestures and bounded large SigMF downloads on an isolated service.

The large archive repeats a committed IQ fixture, with explicit synthetic frequency
and relative clocks. It is not an RF or sustained acquisition capacity test.
"""
from __future__ import annotations

import argparse
import ctypes
import ctypes.util
import json
import os
import socket
import struct
import subprocess
import sys
import tarfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import assert_receivers_continue, setup
from native_multichannel_recording import finish, start, task, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def large_fixture(root, output, samples=4194304, *, split_at=None):
    """Independent WBIQ v2 writer; startup validates/rebuilds its sidecar normally."""
    task_id = 'iq-stream-fixture'
    path = root / task_id
    path.mkdir(parents=True)
    seed = (ROOT / 'tests/fixtures/ft8/real_slot_ja0txu.iq').read_bytes()[:65536 * 8]
    origin = 9007199254740997
    rate = 48840.130763116205
    zstd = ctypes.CDLL(ctypes.util.find_library('zstd'))
    zstd.ZSTD_compressBound.argtypes = [ctypes.c_size_t]
    zstd.ZSTD_compressBound.restype = ctypes.c_size_t
    zstd.ZSTD_compress.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    zstd.ZSTD_compress.restype = ctypes.c_size_t
    page, blocks, digest = [], [], 0
    with (path / 'iq-000000.wbiq').open('wb') as archive, output.open('wb') as expected:
        archive.write(b'WBIQ0002')

        def record(kind, meta, payload=b''):
            nonlocal digest
            encoded = json.dumps(meta, separators=(',', ':')).encode()
            head = struct.pack('<4sII', kind, len(encoded), len(payload))
            crc = zlib.crc32(head + encoded + payload)
            offset = archive.tell()
            archive.write(head + struct.pack('<I', crc) + encoded + payload)
            if kind != b'END2':
                digest = zlib.crc32(struct.pack('<I', crc), digest)
            return offset

        for position in range(0, samples, 65536):
            count = min(65536, samples - position)
            raw = seed[:count * 8]
            expected.write(raw)
            second = split_at is not None and position >= split_at
            block_rate = rate * (2 if second else 1)
            clock = {'receiver_uid': 'fixture-old-uid', 'source_id': 'fixture', 'track_id': 'fixture-track', 'epoch': '7', 'channel_id': '10' if second else '9',
                     'config_revision': '9007199254740993', 'first_input_index': str(origin + position), 'first_output_index': str(origin + position),
                     'fraction_num': '1', 'fraction_den': '3', 'samples': count, 'sample_format': 'cf32_le', 'amplitude_scale': 1.0,
                     'input_rate_hz': block_rate, 'actual_rate_hz': block_rate, 'delay_seconds': 0.001}
            meta = {'service_id': 'fixture-service', 'segment_id': '7-10' if second else '7-9', 'sequence': str(len(blocks)), 'frequency_hz': 102e6 if second else 100e6,
                    'baseband': {'frequency_hz': 102e6 if second else 100e6, 'passband_hz': 3000, 'transition_hz': 1000, 'minimum_rate_hz': 6000}, 'clock': clock}
            codec, payload = 'raw', raw
            if len(blocks) % 2:
                compressed = ctypes.create_string_buffer(zstd.ZSTD_compressBound(len(raw)))
                size = zstd.ZSTD_compress(compressed, len(compressed), raw, len(raw), 1)
                assert size < len(raw)
                codec, payload = 'zstd', compressed.raw[:size]
            header = {'meta': meta, 'codec': codec, 'raw_crc32': zlib.crc32(raw)}
            offset = record(b'BLK2', header, payload)
            index = {'offset': str(offset), **header, 'raw_bytes': len(raw), 'stored_bytes': len(payload)}
            page.append(index); blocks.append(index)
            if len(page) == 128:
                record(b'IDX2', page); page = []
        if page:
            record(b'IDX2', page)
        record(b'END2', {'blocks': str(len(blocks)), 'samples': str(samples), 'records_crc32': digest})
    value = {'storage_version': 2, 'task_id': task_id, 'receiver_uid': 'fixture-old-uid', 'service_id': 'fixture-service',
             'created_at_ms': '1793510000000', 'updated_at_ms': '1793510000000', 'request_id': 'offline-fixture', 'state': 'completed', 'fault': None,
             'accepted_bytes': str(samples * 8), 'queued_bytes': '0', 'tracks': [{'track_id': 'fixture-track', 'kind': 'iq', 'format': 'cf32_le',
             'source': 'baseband', 'compression': 'zstd_auto_lossless', 'container': 'websa-iq-v2', 'state': 'completed', 'segments': [],
             'raw_bytes': str(samples * 8), 'stored_bytes': str((path / 'iq-000000.wbiq').stat().st_size), 'blocks': str(len(blocks)),
             'volumes': [{'file': 'iq-000000.wbiq', 'index': 'iq-000000.jsonl', 'complete': True}]}]}
    (path / 'task.json').write_text(json.dumps(value))
    return task_id, blocks


def url(task_id, count, endpoint='export'):
    return '/api/sdr/recordings/' + task_id + '/' + endpoint + '?' + urllib.parse.urlencode({'segment': '7-9', 'start': '0', 'count': str(count)})


def get(origin, path, status=200):
    try:
        response = urllib.request.urlopen(origin + path, timeout=30)
        assert response.status == status
        return response
    except urllib.error.HTTPError as error:
        assert error.code == status, (error.code, error.read())
        return error


def validate(path, expected, python):
    code = '''import json,hashlib,sys,sigmf
from pathlib import Path
reader=sigmf.SigMFArchiveReader(sys.argv[1]); reader.sigmffile.validate()
actual=hashlib.sha256(reader.sigmffile.read_samples().tobytes()).hexdigest()
assert actual==hashlib.sha256(Path(sys.argv[2]).read_bytes()).hexdigest()
print(json.dumps({'reader':sigmf.__version__,'schema':sigmf.__specification__,'samples':len(reader),'sha256':actual}))
'''
    result = subprocess.run([str(python), '-c', code, str(path), str(expected)], check=True, capture_output=True, text=True)
    with tarfile.open(path) as archive:
        metadata = json.load(archive.extractfile('recording/recording.sigmf-meta'))
    assert metadata['global']['core:sample_rate'] == 48840.130763116205
    assert metadata['annotations'][0]['websa:clock']['first_input_index'] == '9007199254740997'
    assert metadata['annotations'][0]['websa:clock']['fraction_den'] == '3'
    return json.loads(result.stdout)


def rss(pid):
    return int(next(line.split()[1] for line in Path(f'/proc/{pid}/status').read_text().splitlines() if line.startswith('VmRSS:'))) * 1024


def protocol(origin, proc, root, task_id, blocks, out, expected, python):
    # Warm codec/HTTP code paths without starting live receiver DSP.
    with get(origin, url(task_id, 1048576)) as warmup:
        while warmup.read(65536):
            pass
    before = rss(proc.pid)
    readings, done = [], threading.Event()

    def sample():
        while not done.wait(0.01):
            readings.append(rss(proc.pid))
    monitor = threading.Thread(target=sample)
    monitor.start()
    try:
        with get(origin, url(task_id, 4194304)) as response, (out / 'large.sigmf').open('wb') as file:
            length = int(response.headers['Content-Length'])
            while chunk := response.read(65536):
                file.write(chunk)
        assert (out / 'large.sigmf').stat().st_size == length
    finally:
        done.set(); monitor.join()
    peak = max(readings + [rss(proc.pid)])
    assert peak - before < 24 * 1024 * 1024, (before, peak)
    result = validate(out / 'large.sigmf', expected, python)
    (out / 'profile.json').write_text(json.dumps({'rss_before': before, 'rss_peak': peak, 'rss_growth': peak-before, 'reader': result}, indent=2)+'\n')
    with get(origin, url(task_id, 4194304, 'range'), 400):
        pass
    setup(origin)
    healthy_uid = api(origin)['receivers'][0]['receiver_uid']
    healthy_id = start(origin, healthy_uid, 'export-health')
    until(lambda: int(task(origin, healthy_id)['tracks'][0]['raw_bytes']) > 10000)
    old_size = int(task(origin, healthy_id)['tracks'][0]['raw_bytes'])
    # Cancel actual HTTP delivery, then regain the shared reader budget.
    response = get(origin, url(task_id, 4194304)); response.read(65536); response.close()

    def available():
        try:
            with urllib.request.urlopen(origin + url(task_id, 1, 'range'), timeout=5) as reply:
                reply.read()
                return reply.status == 200
        except urllib.error.HTTPError as error:
            assert error.code == 429
            return False
    until(available, timeout=8)
    # Corrupt a last block after HTTP preparation. It must terminate a short response.
    file_path = root / task_id / 'iq-000000.wbiq'
    position = int(blocks[-1]['offset']) + 25
    response = get(origin, url(task_id, 4194304))
    with file_path.open('r+b') as file:
        file.seek(position); original = file.read(1); file.seek(position); file.write(bytes([original[0] ^ 1]))
    transferred = 0
    try:
        while chunk := response.read(65536):
            transferred += len(chunk)
    except Exception:
        pass
    finally:
        response.close()
    assert transferred < int(response.headers['Content-Length'])
    with get(origin, url(task_id, 4194304), 422):
        pass
    with file_path.open('r+b') as file:
        file.seek(position); file.write(original)
    until(available, timeout=8)
    # A genuinely stalled TCP consumer fills only its bounded export pipe.
    parsed = urllib.parse.urlsplit(origin)
    slow = socket.socket(); slow.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
    slow.connect((parsed.hostname, parsed.port)); slow.settimeout(10)
    slow.sendall(f'GET {url(task_id, 4194304)} HTTP/1.1\r\nHost: {parsed.netloc}\r\nConnection: close\r\n\r\n'.encode())
    head = b''
    while not head.endswith(b'\r\n\r\n'):
        head += slow.recv(1)
    assert b'200 OK' in head
    time.sleep(2.3); slow.close(); until(available, timeout=8)
    until(lambda: int(task(origin, healthy_id)['tracks'][0]['raw_bytes']) > old_size + 10000)
    finish(origin, healthy_id)
    return {'reader': result, 'rss_before': before, 'rss_peak': peak, 'rss_growth': peak - before,
            'raw_bytes': expected.stat().st_size, 'partial_response_bytes': transferred,
            'checks': ['32 MiB raw/Zstd mixed fixture', 'independent SigMF reader', 'bounded server RSS', 'HTTP cancellation releases budget',
                       'changed archive produces incomplete download', 'corruption rejected before success', 'stalled TCP consumer', 'healthy recording continues']}


def drag(page, control, value):
    control.scroll_into_view_if_needed()
    box = control.bounding_box()
    old = float(control.input_value())
    low, high = float(control.get_attribute('min')), float(control.get_attribute('max'))
    def x(v):
        return box['x'] + 8 + (box['width'] - 16) * (v - low) / (high - low)
    y = box['y'] + box['height'] / 2
    target = box['x'] - 4 if value == low else box['x'] + box['width'] + 4 if value == high else x(value)
    page.mouse.move(x(old), y); page.mouse.down(); page.mouse.move(target, y, steps=20)
    page.wait_for_timeout(200); page.mouse.up(); page.wait_for_timeout(250)


def browser_gate(origin, snapshot, out, expected, python):
    uid = snapshot['receivers'][0]['receiver_uid']
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1440, 'height': 1000})
        errors, commands = [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda r: commands.append(r.url) if r.method == 'POST' else None)
        page.goto(origin); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        pane = page.locator('[data-view-id=overview-follow]')
        gain, pan = pane.locator('[data-gain]'), pane.locator('[data-pan]')
        expect(gain).to_be_disabled(); expect(pan).to_be_disabled()
        assert pane.locator('[data-key=workflow]').count() == 0
        pane.locator('.mc-row').nth(0).locator('[data-action=listen]').click()
        page.wait_for_function('(uid)=>window.websaAudioStatus().some(r=>r.channel===uid&&r.playedFrames>2000)', arg=uid)
        before, count = api(origin), len(commands)
        page.evaluate('window.websaNativeMixLevels()'); page.wait_for_timeout(250)
        initial = page.evaluate('window.websaNativeMixLevels()')
        gain.focus()
        for _ in range(35):
            page.keyboard.press('ArrowLeft')
        page.wait_for_timeout(250)
        quiet = page.evaluate('window.websaNativeMixLevels()')
        expect(pane.locator('[data-gain-value]')).to_have_text('35%')
        assert .40 < quiet['left'] / initial['left'] < .60, (initial, quiet)
        drag(page, gain, .2)
        actual_gain = float(gain.input_value())
        dragged = page.evaluate('window.websaNativeMixLevels()')
        assert 0 < actual_gain < .35, actual_gain
        assert .8 * actual_gain / .35 < dragged['left'] / quiet['left'] < 1.2 * actual_gain / .35, (actual_gain, quiet, dragged)
        expect(pane.locator('[data-gain-value]')).to_have_text(f'{round(actual_gain * 100)}%')
        drag(page, pan, 1)
        right = page.evaluate('window.websaNativeMixLevels()')
        assert right['right'] > .001 and right['left'] < 1e-5, right
        expect(pane.locator('[data-pan-value]')).to_have_text('R 100%')
        with page.expect_popup() as popup:
            pane.locator('[data-pop]').click()
        child = popup.value; child.on('pageerror', lambda e: errors.append(str(e)))
        child.set_viewport_size({'width': 420, 'height': 760})
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(e=>e.sheet)"); child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=overview-follow]')
        drag(child, compact.locator('[data-pan]'), -1)
        left = page.evaluate('window.websaNativeMixLevels()')
        assert left['left'] > .001 and left['right'] < 1e-5, left
        page.locator('#btn-lang').click()
        expect(compact.locator('[data-pan-value]')).to_have_text('左 100%')
        child.screenshot(path=str(out / 'monitor-420-zh.png'))
        compact.locator('[data-dock]').click()
        assert len(commands) == count, commands[count:]
        assert_receivers_continue(before['receivers'], api(origin)['receivers'])
        pane.locator('[data-section=recording-history] summary').click()
        pane.locator('[data-archive-task=iq-stream-fixture]').click()
        view = page.locator('[data-view-id=iq-recording]')
        expect(view.locator('[data-boundary]')).not_to_be_visible()
        with page.expect_download() as download:
            view.locator('[data-download-full]').click()
        path = out / 'browser-full.sigmf'; download.value.save_as(path)
        verified = validate(path, expected, python)
        page.screenshot(path=str(out / 'archive-compact-zh.png'))
        assert not errors, errors
        browser.close()
    return {'initial': initial, 'half_gain': quiet, 'pan_right': right, 'pan_left': left, 'download': verified, 'pageerrors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-stream-export'))
    parser.add_argument('--sigmf-python', type=Path, default=Path(sys.executable))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    root = (args.artifacts / 'recordings').resolve()
    expected = args.artifacts / 'fixture.expected-cf32'
    task_id, blocks = large_fixture(root, expected)
    os.environ['WEBSA_RECORDING_DIR'] = str(root)
    proc = None
    with (args.artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(free_port(), log=log)
            report = {'source_kind': 'offline repeated committed IQ fixture + synthetic native receivers',
                      'protocol': protocol(origin, proc, root, task_id, blocks, args.artifacts, expected, args.sigmf_python)}
            (args.artifacts / 'protocol-report.json').write_text(json.dumps(report, indent=2)+'\n')
            report['browser'] = browser_gate(origin, api(origin), args.artifacts, expected, args.sigmf_python)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({'ok': True, 'artifacts': str(args.artifacts), 'report': report}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
