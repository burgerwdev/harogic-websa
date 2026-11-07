#!/usr/bin/env python3
"""Persistent published PCM, direct WAV/SHA256 and legacy CRC/Zstd readers, real controls.

Isolated synthetic/replay sources; excludes RF, physical sound-card and long-run acceptance.
"""
from __future__ import annotations

import argparse
import asyncio
import copy
import ctypes
import ctypes.util
import hashlib
import io
import json
import os
import resource
import signal
import struct
import tarfile
import urllib.request
import zlib
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import setup
from native_multichannel_recording import task, until
from native_multichannel_results import start, status
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, WEBSAD, free_port, start_owned_service, start_service, stop
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


def verify_timeline(rows, saved):
    """Reconcile exported metadata with the wire header and every segment's sample clock."""
    segments = {segment['segment_id']: segment for segment in saved['tracks'][0]['segments']}
    offsets, sequences = {key: 0 for key in segments}, {}
    for row in rows:
        header = bytes.fromhex(row['original_aud2_header_hex'])
        assert len(header) == 96 and header[:4] == b'AUD2'
        version, sequence, count = struct.unpack_from('<III', header, 4)
        rate, frequency, channel_id, epoch, input_index, delay, channels, flags, audio_index, input_rate, num, den = struct.unpack_from('<ddIIQdIIQdQQ', header, 16)
        assert version == 1 and row['version'] == 1 and den > 0 and 0 <= num < den
        assert row['audio'] == {
            'stream': {'source_id': 'primary', 'epoch': str(epoch), 'channel_id': str(channel_id)},
            'sequence': sequence, 'frames': count, 'rate_hz': rate, 'channels': channels,
            'frequency_hz': frequency, 'audio_index': str(audio_index), 'input_index': str(input_index),
            'input_fraction_num': str(num), 'input_fraction_den': str(den), 'input_rate_hz': input_rate,
            'delay_seconds': delay, 'delay_known': not bool(flags & 0x10000),
            'causal_presentation': bool(flags & 0x20000)}, row
        identity = row['segment_id']; segment = segments[identity]
        assert row['source'] == segment['source']
        assert row['source']['receiver_uid'] == saved['receiver_uid']
        assert row['source']['service_id'] == saved['service_id']
        assert (segment['stream'], segment['actual_rate_hz'], segment['frequency_hz'], segment['channels']) == (row['audio']['stream'], rate, frequency, channels)
        assert int(row['segment_sample_start']) == offsets[identity]
        assert audio_index == int(segment['first_audio_index']) + offsets[identity]
        if identity not in sequences:
            assert (segment['first_input_index'], segment['fraction_num'], segment['fraction_den']) == (str(input_index), str(num), str(den))
            assert segment['prefix_missing'] == (sequence != 0)
        else:
            assert sequence == sequences[identity] + 1
        offsets[identity] += count; sequences[identity] = sequence
    assert len(rows) == int(saved['tracks'][0]['blocks'])
    for identity, segment in segments.items():
        assert offsets[identity] == int(segment['samples'])
        assert sequences[identity] == segment['last_sequence']
        assert int(segment['next_audio_index']) == int(segment['first_audio_index']) + offsets[identity]


def read_journal(path):
    saved = json.loads((path / 'task.json').read_text())
    track = saved['tracks'][0]
    if track['container'] == 'websa-audio-files-v1':
        assert saved['audio_format'] == 'wav', 'bit-exact comparison requires a direct WAV recording'
        timeline = (path / 'frames.jsonl').read_bytes()
        assert len(timeline) == int(track['timeline_bytes'])
        assert hashlib.sha256(timeline).hexdigest() == track['timeline_sha256']
        rows = [json.loads(line) for line in timeline.splitlines()]
        verify_timeline(rows, saved)
        waves = {}
        for segment in track['segments']:
            if not int(segment['samples']):
                waves[segment['segment_id']] = b''
                continue
            media = (path / segment['file']).read_bytes()
            assert len(media) == int(segment['file_bytes'])
            assert hashlib.sha256(media).hexdigest() == segment['sha256']
            rate, samples = wavfile.read(io.BytesIO(media))
            assert samples.dtype.str == '<f4' and rate == segment['actual_rate_hz']
            assert len(samples) == int(segment['samples'])
            assert (samples.shape[1] if samples.ndim == 2 else 1) == segment['channels']
            waves[segment['segment_id']] = samples.tobytes()
        records, offsets = [], {key: 0 for key in waves}
        for row in rows:
            identity, audio = row['segment_id'], row['audio']
            start, count, stride = int(row['segment_sample_start']), audio['frames'], audio['channels'] * 4
            assert start == offsets[identity]
            payload = waves[identity][start * stride:(start + count) * stride]
            assert len(payload) == count * stride
            header = bytes.fromhex(row['original_aud2_header_hex'])
            records.append((row['source'], header + payload))
            offsets[identity] += count
        assert all(offsets[s['segment_id']] == int(s['samples']) for s in track['segments'])
        return records, ['direct-f32-wav+sha256-timeline']
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
        timeline = archive.extractfile('frames.jsonl').read()
        rows = [json.loads(line) for line in timeline.splitlines()]
        track = manifest['task']['tracks'][0]
        direct = manifest.get('format') == 'websa-audio-files-v1'
        if direct:
            assert manifest['encoding'] == 'wav' and manifest['encoded_during_recording']
            assert not manifest['transcoded_on_download'] and not manifest['local_controls_included']
            assert manifest['source'] == 'published_audio' and manifest['utc_reliable'] is False
            assert len(timeline) == int(track['timeline_bytes'])
            assert hashlib.sha256(timeline).hexdigest() == track['timeline_sha256']
        else:
            assert manifest['protocol_input'] is False
        segments = track['segments']
        waves = {}
        for segment in segments:
            if not int(segment['samples']):
                continue
            name = segment['segment_id'] + '.wav'; payload = archive.extractfile(name).read()
            if direct:
                assert segment['file'] == name and len(payload) == int(segment['file_bytes'])
                assert hashlib.sha256(payload).hexdigest() == segment['sha256']
            rate, samples = wavfile.read(io.BytesIO(payload))
            assert samples.dtype.str == '<f4' and rate == segment['actual_rate_hz']
            assert len(samples) == int(segment['samples'])
            channels = samples.shape[1] if samples.ndim == 2 else 1
            assert channels == segment['channels']
            assert len(payload) == struct.unpack_from('<I', payload, 4)[0] + 8
            waves[segment['segment_id']] = samples.tobytes()
        assert archive.getnames() == ['task.json', 'frames.jsonl', *[s['segment_id']+'.wav' for s in segments if int(s['samples'])]]
    verify_timeline(rows, manifest['task'])
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


def finalize_audio_prefix(origin, root, recording):
    """Create a real source boundary, retaining one complete direct WAV before a crash."""
    uid = task(origin, recording)['receiver_uid']
    until(lambda: int(task(origin, recording)['tracks'][0]['raw_bytes']) > 30000)
    receiver = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    old_gain = receiver['confirmed']['processors'][0]['pcm_gain']
    new_gain = 0.125 if old_gain != 0.125 else 0.25
    changed = configure(origin, uid, 'finalize-prefix-' + recording,
        lambda c: c['processors'][0].update(pcm_gain=new_gain))
    revision = next(r for r in changed['snapshot']['receivers'] if r['receiver_uid'] == uid)['revision']
    receipt = until(lambda: next((r for receiver in api(origin)['receivers'] if receiver['receiver_uid'] == uid
        for r in receiver['control_receipts'] if r['revision'] == revision and r['status'] == 'applied'), None))
    assert receipt['receiver_uid'] == uid and receipt['boundary']['precision'] == 'block', receipt
    def complete_prefix():
        saved = task(origin, recording); segments = saved['tracks'][0]['segments']
        return saved if (len(segments) == 2 and segments[0].get('file_complete') is True
            and int(segments[0]['samples']) > 0 and not segments[1].get('file_complete')
            and int(segments[1]['samples']) > 0) else None
    prior = until(complete_prefix)
    completed, unfinished = prior['tracks'][0]['segments']
    assert completed['source']['config']['volume'] == old_gain
    assert unfinished['source']['config']['volume'] == new_gain
    timeline = (root / recording / 'frames.jsonl').read_bytes()[:int(completed['timeline_end'])]
    rows = [json.loads(line) for line in timeline.splitlines()]
    assert rows and all(row['segment_id'] == completed['segment_id'] for row in rows)
    return {'task': prior, 'receipt': receipt, 'timeline': timeline,
        'media': {completed['file']: (root / recording / completed['file']).read_bytes()}}


def verify_recovered_prefix(origin, recording, prefix, destination):
    recovered = task(origin, recording); prior = prefix['task']; completed = prior['tracks'][0]['segments'][:1]
    assert recovered['state'] == 'failed' and recovered['fault']['code'] == 'SERVICE_INTERRUPTED'
    assert recovered['receiver_uid'] == prior['receiver_uid'] and recovered['service_id'] == prior['service_id']
    assert api(origin)['service_id'] != prior['service_id']
    manifest, rows, waves = download(origin, recording, destination)
    assert manifest['task'] == recovered and rows == [json.loads(line) for line in prefix['timeline'].splitlines()]
    assert recovered['tracks'][0]['segments'] == completed
    with tarfile.open(destination) as archive:
        assert archive.extractfile('frames.jsonl').read() == prefix['timeline']
        for name, payload in prefix['media'].items():
            assert archive.extractfile(name).read() == payload
    assert int(recovered['tracks'][0]['raw_bytes']) == int(completed[0]['samples']) * completed[0]['channels'] * 4
    assert int(recovered['accepted_bytes']) >= int(recovered['tracks'][0]['raw_bytes'])
    assert int(recovered['queued_bytes']) == 0
    resources = api(origin)['recording_resources']
    assert resources['active_tasks'] == 0 and resources['available_tasks'] == resources['max_tasks']
    return recovered, rows, waves


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
        limit = snapshot['capabilities']['max_recording_tasks']
        assert snapshot['recording_resources']['max_tasks'] == limit
        pairs = [(uid, 'audio'), (uid, 'iq'), (uid, 'results'),
                 *[(snapshot['receivers'][1]['receiver_uid'], kind) for kind in ['audio', 'iq', 'results']],
                 (healthy['receiver_uid'], 'iq')]
        index = 0
        while len(pairs) <= limit:
            candidate = copy.deepcopy(snapshot['receivers'][0]['confirmed'])
            candidate['name'] = f'Capacity check {index}'
            candidate['baseband']['frequency_hz'] += 30000 + 10000 * index
            before_uids = {r['receiver_uid'] for r in api(origin)['receivers']}
            created = api(origin, payload={'type': 'create', 'request_id': f'capacity-receiver-{index}', 'config': candidate})
            assert created['fault'] is None, created
            added = [r['receiver_uid'] for r in created['snapshot']['receivers'] if r['receiver_uid'] not in before_uids]
            assert len(added) == 1
            pairs.extend((added[0], kind) for kind in ['audio', 'iq', 'results'])
            index += 1
        extras = []
        for index, (owner, kind) in enumerate(pairs[3:limit]):
            extras.append(await asyncio.to_thread(start, origin, owner, f'fill-shared-{index}', kind))
        assert api(origin)['recording_resources']['active_tasks'] == limit
        owner, kind = pairs[limit]
        rejected = api(origin, payload={'type': 'record_start', 'request_id': 'over-capacity', 'receiver_uid': owner, 'kinds': [kind]})
        assert rejected['fault'] and 'capacity' in rejected['fault']['message'], rejected
        for extra in extras:
            await asyncio.to_thread(finish_audio, origin, extra)
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
        overview.locator('[data-default-audio-format]').select_option('wav')
        row.locator('.mc-row-more > summary').click()
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
    proc = None
    with (artifacts / 'write-failure-service.log').open('w') as log:
        try:
            proc, origin = start_owned_service([str(WEBSAD)], port=int(env['WEBSA_PORT']), cwd=ROOT,
                env=env, stdout=log, stderr=log, preexec_fn=limit, startup_timeout=12)
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
            proc, origin = start_owned_service([str(WEBSAD)], port=int(env['WEBSA_PORT']), cwd=ROOT,
                env=env, stdout=log, stderr=log, startup_timeout=12)
            recovered = task(origin, recording)
            assert recovered['state'] == 'failed' and recovered['fault'] == failed['fault'], recovered
            manifest, rows, waves = download(origin, recording, artifacts / 'write-failure-prefix.tar')
            # The first direct WAV exceeds this limit before its header/checksum can
            # be finalized. An empty verified prefix must not invent playable PCM.
            assert manifest['task'] == recovered and rows == [] and waves == {}
            assert recovered['tracks'][0]['segments'] == []
            assert recovered['accepted_bytes'] == failed['accepted_bytes'] and int(recovered['queued_bytes']) == 0
            assert int(recovered['tracks'][0]['raw_bytes']) == int(recovered['tracks'][0]['blocks']) == 0
            resources = api(origin)['recording_resources']
            assert resources['active_tasks'] == 0 and resources['available_tasks'] == resources['max_tasks']
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
            prefix = finalize_audio_prefix(origin, root, recording)
            (args.artifacts / 'before-kill.json').write_text(json.dumps({key: prefix[key] for key in ['task', 'receipt']}, indent=2) + '\n')
            proc.kill(); proc.wait(timeout=3)
            proc, origin = start_service(port, log=log)
            recovered, _, _ = verify_recovered_prefix(origin, recording, prefix, args.artifacts / 'recovered-audio.tar')
            assert not any(t['state'] in ['preparing', 'waiting', 'recording', 'finalizing'] for t in api(origin)['recordings'])
            report['recovered'] = recovered; report['write_failure'] = write_failure_gate(args.artifacts)
            target = report['protocol']['task']['task_id']; file = root / target / 'frames.jsonl'; raw = bytearray(file.read_bytes()); raw[24] ^= 1; file.write_bytes(raw)
            status(origin, f'/api/sdr/recordings/{target}/audio', 422)
            report['checks'] = ['PCM/header fields equal actual AUD2 network', 'direct WAV/SHA256 and exact AUD2 timeline metadata',
                'same UID IQ/results/audio fills advertised shared task limit', 'gain and FT8 companion boundaries',
                '1100ms mouse and 1300ms Space', '420px Chinese/light Pop out/download/Dock', 'browser exit and processor removal',
                'SIGKILL exact finalized WAV/timeline prefix with original identity', 'real EFBIG empty-prefix fault and IQ isolation', 'corruption 422']
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2)+'\n'); print(json.dumps({'ok': True, 'checks': report['checks']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
