#!/usr/bin/env python3
"""Persistent published-result tracks, independent journal/JSONL readers and real browser controls.

Uses isolated synthetic/replay services. Image pixels come from native SSTV decoding;
this does not qualify RF, audio recording, long-term disk throughput or complete pictures.
"""
from __future__ import annotations

import argparse
import asyncio
import copy
import ctypes
import ctypes.util
import io
import json
import os
import resource
import signal
import struct
import tarfile
import urllib.error
import urllib.request
import zlib
from pathlib import Path

from browser_preferences import toggle_theme
from native_multichannel import api
from native_multichannel_processing import setup
from native_multichannel_recording import finish, task, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, WEBSAD, free_port, start_owned_service, start_service, stop
from viewport_baseline import wait_ui_ready
from websockets.asyncio.client import connect


def start(origin, uid, key, kind='results', *, audio_format='wav'):
    request = {'type': 'record_start', 'receiver_uid': uid, 'request_id': key, 'kinds': [kind]}
    # These gates compare original PCM bytes. Default-format/Opus behavior has
    # its own native_audio_formats gate; never use a lossy recording as its oracle.
    if kind == 'audio':
        request['audio_format'] = audio_format
    reply = api(origin, payload=request)
    assert reply['fault'] is None, reply
    assert api(origin, payload=request) == reply
    return next(t['task_id'] for t in reply['snapshot']['recordings'] if t['request_id'] == key)


def read_journal(path):
    """Independent stdlib CRC and system-libzstd parser, without the Rust reader."""
    lib = ctypes.CDLL(ctypes.util.find_library('zstd'))
    lib.ZSTD_decompress.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t]
    lib.ZSTD_decompress.restype = ctypes.c_size_t
    rows, raws, codecs = [], [], []
    for file in sorted(path.glob('results-*.wbr')):
        raw = file.read_bytes()
        assert raw[:8] == b'WBRS0001'
        at, digest, count, size = 8, 0, 0, 0
        while at < len(raw):
            if len(raw) - at < 16:
                break
            kind, length, stored, crc = struct.unpack_from('<4sIII', raw, at)
            if len(raw) - at - 16 < stored:
                break
            data = raw[at+16:at+16+stored]
            assert 0 < length <= 3*1024*1024 and 0 < stored <= 3*1024*1024
            assert zlib.crc32(raw[at:at+12] + data) == crc
            at += 16 + stored
            if kind == b'EVZ1':
                output = ctypes.create_string_buffer(length)
                assert lib.ZSTD_decompress(output, length, data, stored) == length
                data = output.raw
            else:
                assert kind in [b'EVR1', b'END1'] and len(data) == length
            row = json.loads(data)
            if kind == b'END1':
                assert row == {'records': str(count), 'raw_bytes': str(size), 'digest': digest}
                assert at == len(raw)
                break
            digest = zlib.crc32(struct.pack('<I', crc), digest)
            count += 1; size += len(data)
            rows.append(row); raws.append(data); codecs.append(kind.decode())
    return rows, raws, codecs


def download(origin, task_id, destination):
    with urllib.request.urlopen(origin + f'/api/sdr/recordings/{task_id}/results', timeout=30) as response:
        data = response.read()
        assert len(data) == int(response.headers['Content-Length'])
    destination.write_bytes(data)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        assert archive.getnames() == ['task.json', 'events.jsonl']
        manifest = json.load(archive.extractfile('task.json'))
        rows = [json.loads(line) for line in archive.extractfile('events.jsonl')]
    assert manifest['includes_prior_history'] is False
    return manifest, rows


def status(origin, path, code):
    try:
        urllib.request.urlopen(origin + path, timeout=20)
    except urllib.error.HTTPError as error:
        assert error.code == code, (error.code, error.read())
    else:
        raise AssertionError(f'expected HTTP {code}')


def change_mode(origin, uid, mode, key):
    receiver = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    candidate = copy.deepcopy(receiver['confirmed'])
    candidate['processors'][0].update(mode=mode, bandwidth_hz=3000)
    candidate['baseband'].update(passband_hz=6000, transition_hz=500, minimum_rate_hz=8000)
    reply = api(origin, payload={'type': 'configure', 'request_id': key, 'receiver_uid': uid,
        'expected_revision': receiver['revision'], 'config': candidate})
    assert reply['fault'] is None, reply


async def protocol(origin, root, artifacts):
    snapshot = setup(origin); uid = snapshot['receivers'][1]['receiver_uid']
    healthy = snapshot['receivers'][0]
    network = []
    async with connect(origin.replace('http', 'ws') + '/ws/decode?receiver_uid=' + uid, max_size=4*1024*1024) as socket:
        async def consume():
            async for message in socket:
                assert message[:4] == b'SDE2'
                network.append(json.loads(message[8:]))
        reader = asyncio.create_task(consume())
        recording = await asyncio.to_thread(start, origin, uid, 'actual-results')
        iq = await asyncio.to_thread(start, origin, uid, 'same-uid-iq', 'iq')
        status(origin, f'/api/sdr/recordings/{recording}/results', 409)
        rejected = api(origin, payload={'type': 'record_start', 'request_id': 'iq-has-no-results',
            'receiver_uid': snapshot['receivers'][2]['receiver_uid'], 'kinds': ['results']})
        assert rejected['fault'], rejected
        async def wait(predicate):
            async with asyncio.timeout(35):
                while not predicate():
                    if reader.done():
                        reader.result()
                    await asyncio.sleep(0.05)
        await wait(lambda: any(e['event']['type'] == 'ft8' for e in network))
        await asyncio.to_thread(change_mode, origin, uid, 'sstv', 'actual-sstv')
        await wait(lambda: any(e['event']['type'] == 'sstv-image' and e['event']['row'] >= 48 for e in network))
        saved = await asyncio.to_thread(finish, origin, recording)
        await asyncio.to_thread(finish, origin, iq)
        await socket.close(); await reader
    original, raw, codecs = read_journal(root / recording)
    assert len(original) == int(saved['tracks'][0]['blocks']) and len(original) > 10
    assert sum(map(len, raw)) == int(saved['accepted_bytes']) == int(saved['tracks'][0]['raw_bytes'])
    manifest, exported = download(origin, recording, artifacts / 'actual-results.tar')
    assert exported == original and manifest['task'] == saved
    def key(e):
        return e['epoch'], e['channel_id'], e['input_index'], json.dumps(e['event'], sort_keys=True)
    observed = {key(e) for e in network}
    for row in exported:
        e = row['envelope']
        assert e['receiver_uid'] == uid and e['service_id'] == snapshot['service_id']
        assert isinstance(e['input_index'], str) and isinstance(e['input_fraction_den'], str)
        assert key(e) in observed, e
    ft8 = [r for r in exported if r['envelope']['event']['type'] == 'ft8']
    images = [r for r in exported if r['envelope']['event']['type'] == 'sstv-image']
    assert ft8 and images and len(saved['tracks'][0]['segments']) == 2
    # Reconstruct the recorded partial native image using a separate, tiny PNG encoder.
    first = images[0]['envelope']['event']; width, height = first['width'], first['height']
    pixels = bytearray(width*height*3); rows = set()
    for record in images:
        image = record['envelope']['event']; begin = image['row']*width*3
        rgb = bytes(image['rgb']); assert len(rgb) == image['rows']*width*3
        pixels[begin:begin+len(rgb)] = rgb; rows.update(range(image['row'], image['row']+image['rows']))
    nonzero = sum(any(pixels[i:i+3]) for i in range(0, len(pixels), 3)); assert nonzero > 10000
    def chunk(kind, body):
        return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', zlib.crc32(kind+body))
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
    png += chunk(b'IDAT', zlib.compress(b''.join(b'\0'+pixels[y*width*3:(y+1)*width*3] for y in range(height)))) + chunk(b'IEND', b'')
    (artifacts / 'independent-recorded-sstv-partial.png').write_bytes(png)
    current = api(origin)['receivers'][0]
    assert current['confirmed'] == healthy['confirmed'] and current['stream'] == healthy['stream']
    status(origin, f'/api/sdr/recordings/{recording}/results?unexpected=1', 400)
    return {'task': saved, 'event_count': len(exported), 'ft8': ft8, 'pixel_rows': len(rows),
            'nonzero_pixels': nonzero, 'codecs': sorted(set(codecs)), 'snapshot': api(origin)}


def browser_gate(origin, artifacts):
    snapshot = api(origin); uid = snapshot['receivers'][0]['receiver_uid']
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1440, 'height': 1000}); errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        row = overview.locator(f'.mc-row[data-uid="{uid}"]')
        # AM has useful telemetry even though its internal purpose is listen.
        row.locator('.mc-row-more > summary').click()
        button = row.locator('[data-action=rec-results]'); button.scroll_into_view_if_needed()
        button.hover(); page.mouse.down(); page.wait_for_timeout(1100); page.mouse.up()
        recording = until(lambda: next((t['task_id'] for t in api(origin)['recordings'] if t['receiver_uid'] == uid
            and t['tracks'][0]['kind'] == 'results' and t['state'] in ['preparing', 'recording', 'waiting']), None))
        row.locator('[data-action=select]').click(); overview.locator('[data-action=tab-recording]').click()
        page.reload(); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        overview.locator(f'.mc-row[data-uid="{uid}"] [data-action=select]').click()
        overview.locator('[data-action=tab-recording]').click()
        with page.expect_popup() as popup_info:
            overview.locator('[data-pop]').click()
        popup = popup_info.value; popup.set_viewport_size({'width': 420, 'height': 760})
        popup.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(e=>e.sheet)"); popup.evaluate('document.fonts.ready')
        shell = popup.locator('[data-view-id=overview-follow]')
        # Stop through a real Space press while service snapshots continue updating.
        stop_button = shell.locator('[data-editor] [data-action=rec-results]')
        stop_button.focus(); popup.keyboard.down('Space'); popup.wait_for_timeout(1300); popup.keyboard.up('Space')
        saved = until(lambda: t if (t := task(origin, recording))['state'] == 'completed' else None)
        link = shell.locator(f'[data-task-id="{recording}"] .mc-results-download')
        expect(link).to_be_visible()
        page.locator('#btn-lang').click(); expect(popup.locator('html')).to_have_attribute('lang', 'zh-CN')
        toggle_theme(page); expect(popup.locator('html')).to_have_attribute('data-theme', 'light')
        with popup.expect_download() as info:
            link.click()
        downloaded = artifacts / 'browser-results.tar'; info.value.save_as(downloaded)
        with tarfile.open(downloaded) as archive:
            events = [json.loads(line) for line in archive.extractfile('events.jsonl')]
        assert len(events) == int(saved['tracks'][0]['blocks']) > 0
        overflow = shell.evaluate('e=>e.scrollWidth-e.clientWidth'); assert overflow <= 1, overflow
        popup.screenshot(path=str(artifacts / 'results-420.png'))
        shell.locator('[data-dock]').click(); expect(overview).to_be_visible()
        # Browser exits with another live task: the producer must keep publishing.
        recording = start(origin, uid, 'browser-exit-results')
        before = int(task(origin, recording)['tracks'][0]['blocks'])
        browser.close()
    until(lambda: int(task(origin, recording)['tracks'][0]['blocks']) > before+3)
    iq = start(origin, uid, 'processor-removal-iq', 'iq')
    until(lambda: int(task(origin, iq)['tracks'][0]['raw_bytes']) > 5000)
    receiver = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    candidate = copy.deepcopy(receiver['confirmed']); candidate['processors'] = []; candidate['purpose'] = 'iq'
    reply = api(origin, payload={'type': 'configure', 'request_id': 'remove-recorded-processor', 'receiver_uid': uid,
        'expected_revision': receiver['revision'], 'config': candidate})
    assert reply['fault'] is None, reply
    ended = until(lambda: t if (t := task(origin, recording))['state'] == 'completed' else None)
    # Analog fixture IQ is exactly 48 kS/s; IQ-only uses the actual integer DDC plan.
    # This changes the source rate and waveform, so it must create a new IQ segment.
    iq_saved = until(lambda: t if len((t := task(origin, iq))['tracks'][0]['segments']) == 2 else None)
    assert iq_saved['state'] == 'recording'
    rates = [segment['actual_rate_hz'] for segment in iq_saved['tracks'][0]['segments']]
    assert rates[0] == 48000 and rates[1] != rates[0], rates
    assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['stream'] != receiver['stream']
    finish(origin, iq)
    # Restore the processor for the subsequent interrupted-task check.
    current = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    reply = api(origin, payload={'type': 'configure', 'request_id': 'restore-recorded-processor', 'receiver_uid': uid,
        'expected_revision': current['revision'], 'config': receiver['confirmed']})
    assert reply['fault'] is None, reply
    assert not errors, errors
    return {'task': saved, 'closed_browser_task': ended, 'processor_removal_keeps_iq': True, 'overflow': overflow, 'errors': errors}


def write_failure_gate(artifacts):
    root = (artifacts / 'write-failure-recordings').resolve()
    env = {**os.environ, 'WEBSA_RECORDING_DIR': str(root), 'WEBSA_PORT': str(free_port()), 'WEBSA_HOST': '127.0.0.1'}
    env.pop('WEBSA_RUST_IQS_SHIM', None)
    origin = 'http://127.0.0.1:' + env['WEBSA_PORT']
    def limit():
        signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
        resource.setrlimit(resource.RLIMIT_FSIZE, (8192, 8192))
    proc = None
    with (artifacts / 'write-failure-service.log').open('w') as log:
        try:
            proc, origin = start_owned_service([str(WEBSAD)], port=int(env['WEBSA_PORT']), cwd=ROOT,
                env=env, stdout=log, stderr=log, preexec_fn=limit, startup_timeout=12)
            snapshot = setup(origin); uid = snapshot['receivers'][0]['receiver_uid']
            recording = start(origin, uid, 'actual-result-write-failure')
            failed = until(lambda: t if (t := task(origin, recording))['state'] == 'failed' else None, timeout=25)
            assert failed['fault']['code'] == 'RECORDING_IO' and 'File too large' in failed['fault']['message'], failed
            async def healthy():
                async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + snapshot['receivers'][2]['receiver_uid']) as socket:
                    count = 0
                    async with asyncio.timeout(12):
                        while count < 12:
                            data = await socket.recv()
                            count += isinstance(data, bytes) and len(data) > 56
                    return count
            frames = asyncio.run(healthy())
            stop(proc)
            proc, origin = start_owned_service([str(WEBSAD)], port=int(env['WEBSA_PORT']), cwd=ROOT,
                env=env, stdout=log, stderr=log, startup_timeout=12)
            recovered = task(origin, recording)
            assert recovered['state'] == 'failed' and int(recovered['tracks'][0]['blocks']) > 0
            assert recovered['fault']['code'] == 'RECORDING_IO' and 'File too large' in recovered['fault']['message'], recovered
            manifest, rows = download(origin, recording, artifacts / 'write-failure-prefix.tar')
            assert len(rows) == int(recovered['tracks'][0]['blocks']) and manifest['task']['state'] == 'failed'
            return {'failed': failed, 'recovered': recovered, 'healthy_iq_frames_after_failure': frames, 'os_error': 'EFBIG / RLIMIT_FSIZE'}
        finally:
            stop(proc)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-results'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    root = (args.artifacts / 'recordings').resolve(); os.environ['WEBSA_RECORDING_DIR'] = str(root)
    proc = None; port = free_port()
    with (args.artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(port, log=log)
            report = {'protocol': asyncio.run(protocol(origin, root, args.artifacts))}
            report['browser'] = browser_gate(origin, args.artifacts)
            uid = api(origin)['receivers'][0]['receiver_uid']; interrupted = start(origin, uid, 'kill-results')
            until(lambda: int(task(origin, interrupted)['tracks'][0]['blocks']) > 3)
            prior = task(origin, interrupted); proc.kill(); proc.wait(timeout=3)
            proc, origin = start_service(port, log=log); recovered = task(origin, interrupted)
            assert recovered['fault']['code'] == 'SERVICE_INTERRUPTED'
            assert recovered['receiver_uid'] == uid and recovered['service_id'] == prior['service_id']
            assert api(origin)['service_id'] != prior['service_id']
            manifest, rows = download(origin, interrupted, args.artifacts / 'recovered-results.tar')
            assert len(rows) >= int(prior['tracks'][0]['blocks']) and manifest['task']['state'] == 'failed'
            assert not any(t['state'] in ['preparing', 'waiting', 'recording', 'finalizing'] for t in api(origin)['recordings'])
            report['recovered'] = recovered
            report['write_failure'] = write_failure_gate(args.artifacts)
            # Corruption after recovery must be rejected before a successful HTTP response.
            target = report['protocol']['task']['task_id']; file = root / target / 'results-000000.wbr'
            raw = bytearray(file.read_bytes()); raw[24] ^= 1; file.write_bytes(raw)
            status(origin, f'/api/sdr/recordings/{target}/results', 422)
            report['checks'] = ['native FT8 and SSTV pixels equal real network events', 'same UID IQ and results',
                'processing boundaries and processor removal keeps IQ', 'independent CRC/Zstd and JSONL/tar readers', 'stop drains',
                '1100ms mouse and 1300ms Space', '420px Pop out/download/Dock', 'refresh/browser exit',
                'SIGKILL verified prefix', 'old UID/service preserved', 'corruption 422', 'real EFBIG isolated from healthy IQ']
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2)+'\n')
            print(json.dumps({'ok': True, 'checks': report['checks'], 'artifacts': str(args.artifacts)}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
