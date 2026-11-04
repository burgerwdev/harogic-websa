#!/usr/bin/env python3
"""Quantized FLAC export: reference decoder, exact source clocks, streaming and real UI.

Uses isolated synthetic/replay services and an explicitly synthetic boundary fixture.
"""
from __future__ import annotations

import argparse
import asyncio
import ctypes
import ctypes.util
import hashlib
import http.client
import json
import os
import socket
import struct
import subprocess
import tarfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib
from pathlib import Path

import numpy as np
from native_multichannel import api
from native_multichannel_audio import download, finish_audio, protocol
from native_multichannel_recording import task, until
from native_multichannel_results import start, status
from native_multichannel_stream_export import get, rss
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def packed_s24(pcm):
    scaled = np.frombuffer(pcm, dtype='<f4').astype(np.float64) * 8388608
    rounded = np.copysign(np.floor(np.abs(scaled) + 0.5), scaled)
    clipped = np.count_nonzero((rounded < -8388608) | (rounded > 8388607))
    integers = np.clip(rounded, -8388608, 8388607).astype('<i4')
    packed = integers.view(np.uint8).reshape(-1, 4)[:, :3].tobytes()
    return packed, int(clipped)


def verify_flac(path, waves, timeline, original_task):
    with tarfile.open(path) as archive:
        manifest = json.load(archive.extractfile('task.json'))
        rows = [json.loads(line) for line in archive.extractfile('frames.jsonl')]
        assert manifest['task'] == original_task and rows == timeline
        assert manifest['source'] == 'published_audio' and manifest['protocol_input'] is False
        assert manifest['quantization'] == {'scale': 8388608, 'rounding': 'nearest, ties away from zero',
            'minimum': -8388608, 'maximum': 8388607,
            'clipping': 'saturate rounded values outside signed 24-bit range; count per interleaved channel value',
            'dither': False, 'normalization': False, 'resampling': False, 'float32_bit_exact': False}
        assert archive.getnames() == ['task.json', 'frames.jsonl', *[s['file'] for s in manifest['segments']]]
        results = []
        for entry in manifest['segments']:
            segment = next(s for s in original_task['tracks'][0]['segments'] if s['segment_id'] == entry['segment_id'])
            data = archive.extractfile(entry['file']).read()
            assert data[:8] == b'fLaC\x80\0\0\x22' and len(data) == int(entry['flac_bytes'])
            info = int.from_bytes(data[18:26], 'big')
            assert info >> 44 == segment['actual_rate_hz']
            assert ((info >> 41) & 7) + 1 == segment['channels']
            assert ((info >> 36) & 31) + 1 == 24
            assert info & ((1 << 36) - 1) == int(segment['samples']) == int(entry['samples'])
            expected, clipped = packed_s24(waves[entry['segment_id']])
            assert clipped == int(entry['clipped_values'])
            assert data[26:42] == hashlib.md5(expected).digest()
            subprocess.run(['flac', '--silent', '--test', '-'], input=data, check=True, capture_output=True)
            decoded = subprocess.run(['flac', '--silent', '--decode', '--stdout', '--force-raw-format',
                '--endian=little', '--sign=signed', '-'], input=data, check=True, capture_output=True).stdout
            assert decoded == expected
            results.append({**entry, 'rate_hz': segment['actual_rate_hz'], 'channels': segment['channels'],
                'pcm_sha256': hashlib.sha256(decoded).hexdigest(), 'md5': data[26:42].hex()})
    return {'segments': results, 'frames': len(rows), 'source_task': original_task['task_id']}


def flac_download(origin, recording, path):
    with get(origin, f'/api/sdr/recordings/{recording}/audio/flac') as response:
        length = int(response.headers['Content-Length'])
        with path.open('wb') as output:
            while chunk := response.read(65536):
                output.write(chunk)
    assert path.stat().st_size == length


def boundary_fixture(root):
    recording = 'audio-flac-fixture'; path = root / recording; path.mkdir(parents=True)
    rng = np.random.default_rng(9639)
    zstd = ctypes.CDLL(ctypes.util.find_library('zstd'))
    zstd.ZSTD_compressBound.argtypes = [ctypes.c_size_t]; zstd.ZSTD_compressBound.restype = ctypes.c_size_t
    zstd.ZSTD_compress.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    zstd.ZSTD_compress.restype = ctypes.c_size_t
    source = {'receiver_uid': 'old-fixture-uid', 'service_id': 'old-fixture-service',
        'baseband_stream': {'source_id': 'fixture', 'epoch': '2', 'channel_id': '3'},
        'baseband': {'frequency_hz': 100e6, 'passband_hz': 3000, 'transition_hz': 1000, 'minimum_rate_hz': 6000},
        'baseband_revision': '9007199254740993',
        'config': {'mode': 'am', 'fixture': 'synthetic floating-point quantization boundaries, no RF'}}
    metadata = json.dumps(source).encode(); count = size = digest = 0; offsets = []
    waves = {}
    with (path / 'audio-000000.wba').open('wb') as output:
        output.write(b'WBAU0001')

        def append(raw, compress=False, footer=False):
            nonlocal count, size, digest
            data = raw; kind = b'END1' if footer else b'EVR1'
            if compress:
                buffer = ctypes.create_string_buffer(zstd.ZSTD_compressBound(len(raw)))
                n = zstd.ZSTD_compress(buffer, len(buffer), raw, len(raw), 1)
                assert n < len(raw); data = buffer.raw[:n]; kind = b'EVZ1'
            header = struct.pack('<4sII', kind, len(raw), len(data)); crc = zlib.crc32(header + data)
            offsets.append(output.tell()); output.write(header + struct.pack('<I', crc) + data)
            if not footer:
                count += 1; size += len(raw); digest = zlib.crc32(struct.pack('<I', crc), digest)

        for ordinal, (channels, rate, samples) in enumerate([(2, 32000, 1048579), (1, 11025, 3), (2, 8000, 1)]):
            signal = rng.uniform(-1.1, 1.1, samples * channels).astype('<f4')
            edges = np.array([-2, -1, -0.5, -0.5 / 8388608, -0.0, 0, np.nextafter(np.float32(0), np.float32(1)),
                0.5 / 8388608, 0.5, 8388607 / 8388608, 1, 2], dtype='<f4')
            signal[:min(len(signal), len(edges))] = edges[:min(len(signal), len(edges))]
            waves[f'audio-{ordinal:06}'] = signal.tobytes()
            for seq, position in enumerate([None, *range(0, samples, 32768)]):
                frames = 0 if position is None else min(32768, samples - position)
                audio_index = 9007199254741011 + (position or 0)
                input_index = 9007199254741011 + 3 * (position or 0)
                header = bytearray(96); header[:4] = b'AUD2'
                struct.pack_into('<III', header, 4, 1, seq, frames)
                struct.pack_into('<ddIIQdIIQdQQ', header, 16, rate, 100e6, 10 + ordinal, 7, input_index,
                    0.001, channels, 0x30001, audio_index, rate * 3, 1, 3)
                pcm = b'' if position is None else signal[position*channels:(position+frames)*channels].tobytes()
                raw = struct.pack('<I', len(metadata)) + metadata + header + pcm
                append(raw, compress=seq % 2 == 1)
        append(json.dumps({'records': str(count), 'raw_bytes': str(size), 'digest': digest}).encode(), footer=True)
    manifest = {'storage_version': 2, 'task_id': recording, 'receiver_uid': source['receiver_uid'], 'service_id': source['service_id'],
        'created_at_ms': '1793550000000', 'updated_at_ms': '1793550000000', 'request_id': 'offline-fixture',
        'state': 'completed', 'fault': None, 'accepted_bytes': str(sum(map(len, waves.values()))), 'queued_bytes': '0',
        'tracks': [{'track_id': recording+'-audio', 'kind': 'audio', 'format': 'f32_le', 'source': 'published_audio',
            'compression': 'zstd_auto_lossless', 'container': 'websa-audio-v1', 'state': 'completed',
            'segments': [], 'volumes': [], 'blocks': '0', 'raw_bytes': '0', 'stored_bytes': '0'}]}
    (path / 'task.json').write_text(json.dumps(manifest))
    return recording, waves, offsets


def transport_gate(origin, proc, root, recording, healthy):
    url = f'/api/sdr/recordings/{recording}/audio/flac'
    small = f'/api/sdr/recordings/{healthy}/audio'

    def available():
        try:
            with urllib.request.urlopen(origin + small, timeout=5) as response:
                response.read(); return True
        except urllib.error.HTTPError as error:
            assert error.code == 429; return False

    response = get(origin, url); response.read(65536)
    status(origin, small, 429); response.close(); until(available, timeout=8)
    file = root / recording / 'audio-000000.wba'; original = file.read_bytes()
    response = get(origin, url)
    with file.open('r+b') as output:
        output.seek(3 * len(original) // 4); byte = output.read(1); output.seek(-1, 1); output.write(bytes([byte[0] ^ 1]))
    transferred = 0
    try:
        while chunk := response.read(65536):
            transferred += len(chunk)
    except (OSError, ValueError, http.client.IncompleteRead):
        pass
    finally:
        response.close()
    assert transferred < int(response.headers['Content-Length'])
    status(origin, url, 422); file.write_bytes(original); until(available, timeout=8)
    parsed = urllib.parse.urlsplit(origin)
    slow = socket.socket(); slow.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
    slow.connect((parsed.hostname, parsed.port)); slow.settimeout(30)
    slow.sendall(f'GET {url} HTTP/1.1\r\nHost: {parsed.netloc}\r\nConnection: close\r\n\r\n'.encode())
    head = b''
    while not head.endswith(b'\r\n\r\n'):
        head += slow.recv(1)
    assert b'200 OK' in head
    before = rss(proc.pid); time.sleep(2.3); peak = rss(proc.pid)
    assert peak - before < 8*1024*1024
    slow.close(); until(available, timeout=8)
    return {'incomplete_response_bytes': transferred, 'rss_before_stall': before, 'rss_after_stall': peak,
        'checks': ['shared WAV/FLAC reader contention', 'HTTP cancel releases lease', 'changed source short response',
            'corrupt source 422', 'real stalled TCP and bounded RSS']}


def browser_gate(origin, recording, artifacts):
    saved = task(origin, recording); uid = saved['receiver_uid']; commands = []; errors = []
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True); page = browser.new_page(viewport={'width': 1440, 'height': 1000})
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda r: commands.append(r.post_data_json) if r.method == 'POST' and r.url.endswith('/api/sdr/receivers') else None)
        page.goto(origin); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        overview.locator(f'.mc-row[data-uid="{uid}"] [data-action=select]').click(); overview.locator('[data-action=tab-recording]').click()
        with page.expect_popup() as info:
            overview.locator('[data-pop]').click()
        popup = info.value; popup.set_viewport_size({'width': 420, 'height': 760})
        popup.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(e=>e.sheet)"); popup.evaluate('document.fonts.ready')
        page.locator('#btn-lang').click(); expect(popup.locator('html')).to_have_attribute('lang', 'zh-CN'); page.locator('#btn-theme').click()
        shell = popup.locator('[data-view-id=overview-follow]')
        saved = shell.locator(f'[data-task-id="{recording}"]')
        saved.locator('.mc-audio-export summary').click()
        saved.locator('[data-format=flac]').click()
        link = saved.locator('.mc-audio-flac')
        expect(link).to_be_visible(); expect(link).to_contain_text('FLAC')
        link.evaluate("e=>{window.originalFlacLink=e;window.removedFlacLink=false;new MutationObserver(()=>{if(!e.isConnected)window.removedFlacLink=true}).observe(document.body,{childList:true,subtree:true})}")
        link.hover(); popup.mouse.down(); popup.wait_for_timeout(1100)
        with popup.expect_download() as info:
            popup.mouse.up()
        destination = artifacts / 'browser-flac.tar'; info.value.save_as(destination)
        assert popup.evaluate('!window.removedFlacLink && window.originalFlacLink.isConnected')
        _, rows, waves = download(origin, recording, artifacts / 'browser-source.wav.tar')
        verified = verify_flac(destination, waves, rows, saved)
        link.focus(); popup.wait_for_timeout(1300)
        with popup.expect_download() as info:
            popup.keyboard.press('Enter')
        keyboard = artifacts / 'keyboard-flac.tar'; info.value.save_as(keyboard)
        assert keyboard.read_bytes() == destination.read_bytes()
        overflow = shell.evaluate('e=>e.scrollWidth-e.clientWidth'); assert overflow <= 1
        popup.screenshot(path=str(artifacts / 'flac-420-zh-light.png')); shell.locator('[data-dock]').click(); expect(overview).to_be_visible()
        browser.close()
    assert not commands and not errors, (commands, errors)
    return {'download': verified, 'overflow': overflow, 'pageerrors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-flac'))
    args = parser.parse_args(); artifacts = args.artifacts.resolve(); artifacts.mkdir(parents=True, exist_ok=True)
    root = artifacts / 'recordings'; recording, expected, _ = boundary_fixture(root)
    os.environ['WEBSA_RECORDING_DIR'] = str(root); proc = None; port = free_port()
    with (artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(port, log=log)
            assert api(origin)['capabilities']['audio_flac_export'] is True
            report = {'decoder': subprocess.check_output(['flac', '--version'], text=True).strip()}
            saved = task(origin, recording)
            _, rows, waves = download(origin, recording, artifacts / 'fixture-source.wav.tar'); assert waves == expected
            before = hashlib.sha256((root / recording / 'audio-000000.wba').read_bytes()).hexdigest()
            flac_download(origin, recording, artifacts / 'fixture-flac.tar')
            report['fixture'] = verify_flac(artifacts / 'fixture-flac.tar', waves, rows, saved)
            assert any(int(s['clipped_values']) > 0 for s in report['fixture']['segments'])
            assert all(int(r['audio']['input_index']) > 2**53 and r['audio']['input_fraction_den'] == '3' for r in rows)
            for prior, current in zip(rows[:-1], rows[1:], strict=True):
                if prior['segment_id'] == current['segment_id']:
                    assert int(current['audio']['input_index']) - int(prior['audio']['input_index']) == 3 * prior['audio']['frames']
                    assert int(current['audio']['audio_index']) - int(prior['audio']['audio_index']) == prior['audio']['frames']
            report['actual'] = asyncio.run(protocol(origin, root, artifacts))
            actual_id = report['actual']['task']['task_id']
            _, rows, waves = download(origin, actual_id, artifacts / 'actual-source.wav.tar')
            flac_download(origin, actual_id, artifacts / 'actual-flac.tar')
            report['network_flac'] = verify_flac(artifacts / 'actual-flac.tar', waves, rows, report['actual']['task'])
            uid = api(origin)['receivers'][0]['receiver_uid']; healthy = start(origin, uid, 'healthy-iq-during-flac', 'iq')
            count = int(task(origin, healthy)['tracks'][0]['raw_bytes'])
            report['transport'] = transport_gate(origin, proc, root, recording, actual_id)
            until(lambda: int(task(origin, healthy)['tracks'][0]['raw_bytes']) > count + 10000); finish_audio(origin, healthy)
            assert hashlib.sha256((root / recording / 'audio-000000.wba').read_bytes()).hexdigest() == before
            status(origin, f'/api/sdr/recordings/{recording}/audio/flac?format=opus', 400)
            status(origin, f'/api/sdr/recordings/{healthy}/audio/flac', 422)
            status(origin, '/api/sdr/recordings/not-found/audio/flac', 404)
            active = start(origin, uid, 'active-flac', 'audio'); status(origin, f'/api/sdr/recordings/{active}/audio/flac', 409)
            until(lambda: int(task(origin, active)['tracks'][0]['raw_bytes']) > 30000)
            report['browser'] = browser_gate(origin, actual_id, artifacts)
            prior = task(origin, active); proc.kill(); proc.wait(timeout=3)
            proc, origin = start_service(port, log=log); recovered = task(origin, active)
            assert recovered['fault']['code'] == 'SERVICE_INTERRUPTED' and recovered['service_id'] == prior['service_id']
            _, rows, waves = download(origin, active, artifacts / 'recovered-source.wav.tar')
            flac_download(origin, active, artifacts / 'recovered-flac.tar')
            report['recovered'] = verify_flac(artifacts / 'recovered-flac.tar', waves, rows, recovered)
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2)+'\n')
            print(json.dumps({'ok': True, 'decoder': report['decoder'], 'fixture': report['fixture'], 'network': report['network_flac']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
