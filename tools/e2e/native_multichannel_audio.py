#!/usr/bin/env python3
"""Persistent published PCM, independent WAV/CRC/Zstd readers and real browser controls.

Isolated synthetic/replay sources; excludes RF, physical sound-card and long-run acceptance.
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
import subprocess
import tarfile
import urllib.request
import zlib
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import setup
from native_multichannel_recording import task, until
from native_multichannel_results import start, status
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, WEBSAD, free_port, start_service, stop
from scipy.io import wavfile
from viewport_baseline import wait_ui_ready
from websockets.asyncio.client import connect


def finish_audio(origin, recording):
    reply = api(origin, payload={'type': 'record_stop', 'request_id': 'stop-' + recording, 'task_id': recording})
    assert reply['fault'] is None, reply
    saved = until(lambda: t if (t := task(origin, recording))['state'] in ['completed', 'failed'] else None)
    assert saved['state'] == 'completed', saved
    assert saved['accepted_bytes'] == saved['tracks'][0]['raw_bytes']
    return saved


def read_journal(path):
    lib = ctypes.CDLL(ctypes.util.find_library('zstd'))
    lib.ZSTD_decompress.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t]
    lib.ZSTD_decompress.restype = ctypes.c_size_t
    rows, codecs = [], []
    for file in sorted(path.glob('audio-*.wba')):
        raw = file.read_bytes(); assert raw[:8] == b'WBAU0001'
        at, digest, count, size = 8, 0, 0, 0
        while at + 16 <= len(raw):
            kind, length, stored, crc = struct.unpack_from('<4sIII', raw, at)
            if at + 16 + stored > len(raw):
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
            if kind == b'END1':
                assert json.loads(data) == {'records': str(count), 'raw_bytes': str(size), 'digest': digest}
                assert at == len(raw)
                break
            digest = zlib.crc32(struct.pack('<I', crc), digest); count += 1; size += len(data)
            n = struct.unpack_from('<I', data)[0]
            rows.append((json.loads(data[4:4+n]), data[4+n:])); codecs.append(kind.decode())
    return rows, sorted(set(codecs))


def verify_tar(data):
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        manifest = json.load(archive.extractfile('task.json'))
        rows = [json.loads(line) for line in archive.extractfile('frames.jsonl')]
        assert manifest['protocol_input'] is False
        segments = manifest['task']['tracks'][0]['segments']
        waves = {}
        for segment in segments:
            if not int(segment['samples']):
                continue
            name = segment['segment_id'] + '.wav'; payload = archive.extractfile(name).read()
            rate, samples = wavfile.read(io.BytesIO(payload))
            assert samples.dtype.str == '<f4' and rate == segment['actual_rate_hz']
            assert len(samples) == int(segment['samples'])
            channels = samples.shape[1] if samples.ndim == 2 else 1
            assert channels == segment['channels']
            assert len(payload) == struct.unpack_from('<I', payload, 4)[0] + 8
            waves[segment['segment_id']] = samples.tobytes()
        assert archive.getnames() == ['task.json', 'frames.jsonl', *[s['segment_id']+'.wav' for s in segments if int(s['samples'])]]
    assert len(rows) == int(manifest['task']['tracks'][0]['blocks'])
    return manifest, rows, waves


def download(origin, recording, destination):
    with urllib.request.urlopen(origin + f'/api/sdr/recordings/{recording}/audio', timeout=30) as response:
        data = response.read(); assert len(data) == int(response.headers['Content-Length'])
    destination.write_bytes(data)
    return verify_tar(data)


def configure(origin, uid, key, edit):
    receiver = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    candidate = copy.deepcopy(receiver['confirmed']); edit(candidate)
    reply = api(origin, payload={'type': 'configure', 'request_id': key, 'receiver_uid': uid,
        'expected_revision': receiver['revision'], 'config': candidate})
    assert reply['fault'] is None, reply
    return reply


async def protocol(origin, root, artifacts):
    snapshot = setup(origin); uid = snapshot['receivers'][0]['receiver_uid']
    healthy = snapshot['receivers'][2]; network = {}
    async with connect(origin.replace('http', 'ws') + '/ws/audio?receiver_uid=' + uid, max_size=4*1024*1024) as socket:
        async def consume():
            async for frame in socket:
                assert frame[:4] == b'AUD2'
                if struct.unpack_from('<I', frame, 12)[0]:
                    key = (frame[16:24], frame[32:40], frame[56:60], frame[64:72])
                    network[key] = frame
        reader = asyncio.create_task(consume())
        recording = await asyncio.to_thread(start, origin, uid, 'actual-audio', 'audio')
        iq = await asyncio.to_thread(start, origin, uid, 'same-uid-iq', 'iq')
        results = await asyncio.to_thread(start, origin, uid, 'same-uid-results', 'results')
        rejected = api(origin, payload={'type': 'record_start', 'request_id': 'fourth-worker', 'receiver_uid': snapshot['receivers'][1]['receiver_uid'], 'kinds': ['audio']})
        assert rejected['fault'], rejected
        status(origin, f'/api/sdr/recordings/{recording}/audio', 409)
        await asyncio.to_thread(until, lambda: int(task(origin, recording)['tracks'][0]['raw_bytes']) > 30000)
        # Backend PCM gain changes, while its IQ stream stays stable.
        stream = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['stream']
        await asyncio.to_thread(configure, origin, uid, 'shared-gain', lambda c: c['processors'][0].update(pcm_gain=0.25))
        await asyncio.to_thread(until, lambda: len(task(origin, recording)['tracks'][0]['segments']) >= 2)
        assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['stream'] == stream
        # A decoder's audio companion is recordable with the digital purpose.
        def ft8(c):
            c['purpose'] = 'digital'; c['processors'][0].update(mode='ft8', kind='decoder', bandwidth_hz=3000)
        await asyncio.to_thread(configure, origin, uid, 'audio-companion', ft8)
        await asyncio.to_thread(until, lambda: any(s['source']['config']['mode'] == 'ft8' and int(s['samples']) > 12000 for s in task(origin, recording)['tracks'][0]['segments']))
        saved = await asyncio.to_thread(finish_audio, origin, recording)
        await asyncio.to_thread(finish_audio, origin, iq); await asyncio.to_thread(finish_audio, origin, results)
        await asyncio.sleep(0.1); await socket.close(); await reader
    original, codecs = read_journal(root / recording)
    manifest, rows, waves = download(origin, recording, artifacts / 'actual-audio.tar')
    assert len(original) == len(rows)
    by_segment = {key: bytearray() for key in waves}; verified = 0
    for (source, frame), row in zip(original, rows, strict=True):
        assert row['source'] == source and bytes.fromhex(row['original_aud2_header_hex']) == frame[:96]
        assert source['receiver_uid'] == uid and source['service_id'] == snapshot['service_id']
        if len(frame) > 96:
            published = network[(frame[16:24], frame[32:40], frame[56:60], frame[64:72])]
            # SubscriberStream translates seq only; all PCM and other source fields are exact.
            assert published[:8] == frame[:8] and published[12:] == frame[12:]
            assert len(by_segment[row['segment_id']]) == int(row['segment_sample_start']) * row['audio']['channels'] * 4
            by_segment[row['segment_id']].extend(frame[96:]); verified += 1
    assert {k: bytes(v) for k, v in by_segment.items()} == waves
    assert manifest['task'] == saved and len(waves) >= 3
    current = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == healthy['receiver_uid'])
    assert current['confirmed'] == healthy['confirmed'] and current['stream'] == healthy['stream']
    status(origin, f'/api/sdr/recordings/{recording}/audio?unexpected=1', 400)
    status(origin, f'/api/sdr/recordings/{results}/audio', 422)
    rejected = api(origin, payload={'type': 'record_start', 'request_id': 'iq-only-audio', 'receiver_uid': healthy['receiver_uid'], 'kinds': ['audio']})
    assert rejected['fault']
    return {'task': saved, 'network_pcm_blocks_verified': verified, 'codecs': codecs, 'snapshot': api(origin)}


def browser_gate(origin, artifacts):
    uid = api(origin)['receivers'][1]['receiver_uid']
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True); page = browser.new_page(viewport={'width': 1440, 'height': 1000}); errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]'); row = overview.locator(f'.mc-row[data-uid="{uid}"]')
        button = row.locator('[data-action=rec-audio]'); button.scroll_into_view_if_needed(); button.hover(); page.mouse.down(); page.wait_for_timeout(1100); page.mouse.up()
        recording = until(lambda: next((t['task_id'] for t in api(origin)['recordings'] if t['receiver_uid'] == uid and t['tracks'][0]['kind'] == 'audio' and t['state'] == 'recording'), None))
        row.locator('[data-action=select]').click(); overview.locator('[data-action=tab-recording]').click()
        # Local speaker changes occur while the server file continues independently.
        before = int(task(origin, recording)['tracks'][0]['raw_bytes'])
        configuration = copy.deepcopy(next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['confirmed'])
        writes = []
        page.on('request', lambda request: writes.append(request.post_data_json) if request.method == 'POST' and request.url.endswith('/api/sdr/receivers') else None)
        row.locator('[data-action=listen]').click()
        gain = overview.locator('[data-editor] [data-gain]'); expect(gain).to_be_enabled(); gain.focus(); page.keyboard.press('Home')
        pan = overview.locator('[data-editor] [data-pan]'); expect(pan).to_be_enabled(); pan.focus(); page.keyboard.press('End')
        row.locator('[data-action=solo]').click(); overview.locator('[data-action=master-mute]').click()
        assert not writes, writes
        assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['confirmed'] == configuration
        page.reload(); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]'); overview.locator(f'.mc-row[data-uid="{uid}"] [data-action=select]').click(); overview.locator('[data-action=tab-recording]').click()
        with page.expect_popup() as info:
            overview.locator('[data-pop]').click()
        popup = info.value; popup.set_viewport_size({'width': 420, 'height': 760})
        popup.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(e=>e.sheet)"); popup.evaluate('document.fonts.ready')
        shell = popup.locator('[data-view-id=overview-follow]'); button = shell.locator('[data-editor] [data-action=rec-audio]')
        button.focus(); popup.keyboard.down('Space'); popup.wait_for_timeout(1300); popup.keyboard.up('Space')
        saved = until(lambda: t if (t := task(origin, recording))['state'] == 'completed' else None)
        assert int(saved['tracks'][0]['raw_bytes']) > before
        link = shell.locator(f'[data-task-id="{recording}"] .mc-audio-download'); expect(link).to_be_visible()
        page.locator('#btn-lang').click(); expect(popup.locator('html')).to_have_attribute('lang', 'zh-CN'); page.locator('#btn-theme').click()
        with popup.expect_download() as info:
            link.click()
        target = artifacts / 'browser-audio.tar'; info.value.save_as(target)
        manifest, rows, waves = verify_tar(target.read_bytes()); assert manifest['task']['task_id'] == recording and rows and waves
        # The file remains nonzero with this browser's local gain at zero and master muted.
        assert any(abs(value[0]) > 0.001 for raw in waves.values() for value in struct.iter_unpack('<f', raw))
        overflow = shell.evaluate('e=>e.scrollWidth-e.clientWidth'); assert overflow <= 1
        popup.screenshot(path=str(artifacts / 'audio-420-zh-light.png')); shell.locator('[data-dock]').click(); expect(overview).to_be_visible()
        recording = start(origin, uid, 'browser-exit-audio', 'audio'); before = int(task(origin, recording)['tracks'][0]['raw_bytes']); browser.close()
    until(lambda: int(task(origin, recording)['tracks'][0]['raw_bytes']) > before + 30000)
    iq = start(origin, uid, 'remove-processing-iq', 'iq')
    configure(origin, uid, 'remove-audio-processor', lambda c: c.update(processors=[], purpose='iq'))
    ended = until(lambda: t if (t := task(origin, recording))['state'] == 'completed' else None)
    assert ended['tracks'][0]['segments'][-1]['end_reason'] == 'processing_stopped'
    until(lambda: int(task(origin, iq)['tracks'][0]['raw_bytes']) > 10000); finish_audio(origin, iq)
    assert not errors, errors
    return {'task': saved, 'browser_exit_task': ended, 'overflow': overflow, 'pageerrors': errors}


def write_failure_gate(artifacts):
    root = (artifacts / 'write-failure-recordings').resolve()
    env = {**os.environ, 'WEBSA_RECORDING_DIR': str(root), 'WEBSA_PORT': str(free_port()), 'WEBSA_HOST': '127.0.0.1'}; env.pop('WEBSA_RUST_IQS_SHIM', None)
    origin = 'http://127.0.0.1:' + env['WEBSA_PORT']
    def limit():
        signal.signal(signal.SIGXFSZ, signal.SIG_IGN); resource.setrlimit(resource.RLIMIT_FSIZE, (16384, 16384))
    def ready():
        try:
            return api(origin)
        except OSError:
            return None
    proc = None
    with (artifacts / 'write-failure-service.log').open('w') as log:
        try:
            proc = subprocess.Popen([str(WEBSAD)], cwd=ROOT, env=env, stdout=log, stderr=log, preexec_fn=limit); until(ready)
            uid = setup(origin)['receivers'][0]['receiver_uid']; recording = start(origin, uid, 'efbig-audio', 'audio')
            failed = until(lambda: t if (t := task(origin, recording))['state'] == 'failed' else None, timeout=25)
            assert failed['fault']['code'] == 'RECORDING_IO' and 'File too large' in failed['fault']['message'], failed
            async def healthy():
                uid = api(origin)['receivers'][2]['receiver_uid']
                async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + uid) as socket:
                    count = 0
                    async with asyncio.timeout(12):
                        while count < 12:
                            frame = await socket.recv()
                            count += isinstance(frame, bytes) and len(frame) > 56
                    return count
            frames = asyncio.run(healthy()); stop(proc)
            proc = subprocess.Popen([str(WEBSAD)], cwd=ROOT, env=env, stdout=log, stderr=log); until(ready)
            recovered = task(origin, recording); assert recovered['fault']['code'] == 'RECORDING_IO' and 'File too large' in recovered['fault']['message']
            _, rows, _ = download(origin, recording, artifacts / 'write-failure-prefix.tar'); assert rows
            return {'failed': failed, 'recovered': recovered, 'healthy_iq_frames': frames}
        finally:
            stop(proc)


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-audio'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    root = (args.artifacts / 'recordings').resolve(); os.environ['WEBSA_RECORDING_DIR'] = str(root)
    proc = None; port = free_port()
    with (args.artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(port, log=log)
            report = {'protocol': asyncio.run(protocol(origin, root, args.artifacts)), 'browser': browser_gate(origin, args.artifacts)}
            uid = api(origin)['receivers'][0]['receiver_uid']; recording = start(origin, uid, 'kill-audio', 'audio')
            until(lambda: int(task(origin, recording)['tracks'][0]['raw_bytes']) > 30000); prior = task(origin, recording); proc.kill(); proc.wait(timeout=3)
            proc, origin = start_service(port, log=log); recovered = task(origin, recording)
            assert recovered['fault']['code'] == 'SERVICE_INTERRUPTED' and recovered['receiver_uid'] == uid and recovered['service_id'] == prior['service_id']
            assert api(origin)['service_id'] != prior['service_id']
            _, rows, _ = download(origin, recording, args.artifacts / 'recovered-audio.tar'); assert len(rows) >= int(prior['tracks'][0]['blocks'])
            assert not any(t['state'] in ['preparing', 'waiting', 'recording', 'finalizing'] for t in api(origin)['recordings'])
            report['recovered'] = recovered; report['write_failure'] = write_failure_gate(args.artifacts)
            target = report['protocol']['task']['task_id']; file = root / target / 'audio-000000.wba'; raw = bytearray(file.read_bytes()); raw[24] ^= 1; file.write_bytes(raw)
            status(origin, f'/api/sdr/recordings/{target}/audio', 422)
            report['checks'] = ['PCM/header fields equal actual AUD2 network', 'independent CRC/Zstd and SciPy Float32 WAV reader',
                'same UID IQ/results/audio with shared three-worker cap', 'gain and FT8 companion boundaries',
                '1100ms mouse and 1300ms Space', '420px Chinese/light Pop out/download/Dock', 'browser exit and processor removal',
                'SIGKILL original identity prefix', 'real EFBIG isolation', 'corruption 422']
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2)+'\n'); print(json.dumps({'ok': True, 'checks': report['checks']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
