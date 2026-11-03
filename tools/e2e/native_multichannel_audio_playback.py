#!/usr/bin/env python3
"""Original PCM range playback: independent CRC/WAV reader, real media UI and isolation.

Only owned synthetic/replay services; no hardware or physical sound-card acceptance.
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import http.client
import io
import json
import os
import re
import socket
import struct
import tarfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

from native_multichannel import api
from native_multichannel_audio import download, finish_audio, protocol
from native_multichannel_flac import boundary_fixture
from native_multichannel_processing import assert_receivers_continue
from native_multichannel_recording import task, until
from native_multichannel_results import start, status
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from scipy.io import wavfile
from viewport_baseline import wait_ui_ready


def get(origin, path):
    with urllib.request.urlopen(origin + path, timeout=30) as response:
        data = response.read()
        assert len(data) == int(response.headers['Content-Length'])
        return data


def split_fixture(root, recording):
    """Preserve independently written records, rebuild each volume's checked footer."""
    import zlib
    path = root / recording / 'audio-000000.wba'; raw = path.read_bytes(); records = []; at = 8
    while at < len(raw):
        kind, size, stored, crc = struct.unpack_from('<4sIII', raw, at)
        assert zlib.crc32(raw[at:at+12] + raw[at+16:at+16+stored]) == crc
        if kind == b'END1':
            break
        records.append((raw[at:at+16+stored], size, crc)); at += 16 + stored
    midpoint = len(records) // 2
    for ordinal, group in enumerate([records[:midpoint], records[midpoint:]]):
        digest = 0
        for _, _, crc in group:
            digest = zlib.crc32(struct.pack('<I', crc), digest)
        footer = json.dumps({'records': str(len(group)), 'raw_bytes': str(sum(r[1] for r in group)), 'digest': digest}).encode()
        head = struct.pack('<4sII', b'END1', len(footer), len(footer))
        (path.parent / f'audio-{ordinal:06}.wba').write_bytes(b'WBAU0001' + b''.join(r[0] for r in group) + head + struct.pack('<I', zlib.crc32(head+footer)) + footer)


def url(recording, segment='audio-000000', first=500001, count=200000):
    return f'/api/sdr/recordings/{recording}/audio/range?' + urllib.parse.urlencode({'segment': segment, 'start': first, 'count': count})


def verify(data, saved, waves, rows, segment='audio-000000', first=500001, count=200000):
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        assert archive.getnames() == ['selection.json', 'frames.jsonl', 'audio.wav']
        manifest = json.load(archive.extractfile('selection.json'))
        timeline = archive.extractfile('frames.jsonl').read(); selected = [json.loads(line) for line in timeline.splitlines()]
        rate, pcm = wavfile.read(io.BytesIO(archive.extractfile('audio.wav').read()))
    source = next(s for s in saved['tracks'][0]['segments'] if s['segment_id'] == segment)
    stride = source['channels'] * 4
    expected = waves[segment][first*stride:(first+count)*stride]
    assert pcm.dtype.str == '<f4' and len(pcm) == count and pcm.tobytes() == expected
    assert rate == source['actual_rate_hz'] and (pcm.shape[1] if pcm.ndim == 2 else 1) == source['channels']
    assert manifest['task'] == saved and manifest['format'] == 'websa-audio-range-v1'
    assert manifest['selection'] == {'segment': segment, 'start': str(first), 'count': str(count), 'first_audio_index': str(int(source['first_audio_index']) + first)}
    assert manifest['audio']['pcm_sha256'] == hashlib.sha256(expected).hexdigest()
    assert manifest['provenance']['sha256'] == hashlib.sha256(timeline).hexdigest()
    assert manifest['processing'] == {'resampled': False, 'quantized': False, 'gain_applied': False, 'protocol_input': False, 'local_controls_included': False}
    original = [r for r in rows if r['segment_id'] == segment]
    at = 0
    for row in selected:
        selection = row.pop('selection'); assert row in original
        skip, n = int(selection['record_skip_frames']), int(selection['frames'])
        assert int(selection['range_sample_start']) == at
        assert int(row['segment_sample_start']) + skip == first + at and skip + n <= row['audio']['frames']
        at += n
    assert at == count
    return {'selection': manifest['selection'], 'rate_hz': rate, 'channels': source['channels'], 'pcm_sha256': manifest['audio']['pcm_sha256'], 'records': len(selected)}


def transport_gate(origin, recording, root):
    query = url(recording, first=0, count=1048576)
    small = url(recording, first=0, count=1000)
    port = urllib.parse.urlsplit(origin).port
    def available():
        try:
            return len(get(origin, small)) > 0
        except Exception as error:
            assert getattr(error, 'code', None) == 429, error
            return False
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=20)
    connection.request('GET', query); response = connection.getresponse(); assert response.status == 200
    status(origin, small, 429)
    response.close(); connection.close(); until(available)
    # The completed preparation is immutable even if a source changes after headers.
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=20)
    connection.request('GET', query); response = connection.getresponse(); assert response.status == 200
    file = root / recording / 'audio-000001.wba'; original = file.read_bytes(); altered = bytearray(original); altered[30] ^= 1
    file.write_bytes(altered); immutable = response.read(); connection.close()
    assert len(immutable) == int(response.headers['Content-Length'])
    status(origin, small, 422); file.write_bytes(original); until(available)
    assert immutable == get(origin, query)
    stalled = socket.create_connection(('127.0.0.1', port), timeout=20); stalled.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
    stalled.sendall(f'GET {query} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'.encode())
    head = b''
    while b'\r\n\r\n' not in head:
        head += stalled.recv(1)
    assert b'200 OK' in head
    time.sleep(3); status(origin, small, 429); stalled.close(); until(available)
    return {'cancel_releases_permit': True, 'shared_429': True, 'immutable_bytes': len(immutable), 'corruption_422': True, 'stalled_tcp_body_lease': True}


def browser_gate(origin, saved, waves, rows, artifacts, first=500001, count=200000):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True); page = browser.new_page(viewport={'width': 1366, 'height': 900})
        errors, writes = [], []
        page.on('pageerror', lambda e: errors.append(str(e)))
        page.on('request', lambda r: writes.append(r.url) if r.method == 'POST' else None)
        page.goto(origin); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]'); overview.locator('[data-action=tab-recording]').click()
        mute = overview.locator('[data-action=master-mute]')
        if mute.get_attribute('aria-pressed') != 'true':
            mute.click()
        expect(mute).to_have_attribute('aria-pressed', 'true')
        if any(r['receiver_uid'] == saved['receiver_uid'] for r in api(origin)['receivers']):
            overview.locator(f'.mc-row[data-uid="{saved["receiver_uid"]}"] [data-action=select]').click()
        else:
            overview.locator('[data-section=recording-history] summary').click()
        overview.locator(f'[data-action=audio-archive][data-archive-task="{saved["task_id"]}"]').click(delay=1100)
        view = page.locator('[data-view-id=recorded-audio]'); expect(view).to_be_visible()
        view.locator('[name=start]').fill(str(first)); view.locator('[name=count]').fill(str(count)); baseline = len(writes)
        view.locator('[data-audio-load]').click(delay=1100); expect(view.locator('[data-audio-status]')).to_have_text('Ready')
        audio = view.locator('audio'); expect(audio).to_have_attribute('src', re.compile('blob:'))
        audio.evaluate("e=>new Promise((resolve,reject)=>{if(e.readyState>=1)return resolve();e.addEventListener('loadedmetadata',resolve,{once:true});e.addEventListener('error',reject,{once:true})})")
        rate = saved['tracks'][0]['segments'][0]['actual_rate_hz']
        assert abs(audio.evaluate('e=>e.duration') - count/rate) < 0.001
        audio.click(position={'x': 20, 'y': 27}); page.wait_for_function("document.querySelector('[data-recorded-audio] audio').currentTime > 0.05")
        audio.click(position={'x': 20, 'y': 27})
        page.screenshot(path=str(artifacts / 'media-before-seek.png'))
        box = audio.bounding_box(); assert box
        audio.click(position={'x': box['width'] * 0.6, 'y': 27})
        page.wait_for_function("minimum=>document.querySelector('[data-recorded-audio] audio').currentTime > minimum", arg=min(1, count/rate/4))
        page.screenshot(path=str(artifacts / 'audio-playback-desktop.png'))
        with page.expect_popup() as opened:
            view.locator('[data-pop]').click()
        child = opened.value; child.on('pageerror', lambda e: errors.append(str(e))); child.set_viewport_size({'width': 420, 'height': 760})
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(link=>link.sheet!==null)"); child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=recorded-audio]')
        child.evaluate("""()=>{const b=document.querySelector('[data-audio-save]');window.originalSave=b;window.removedSave=false;
          new MutationObserver(rows=>{if(rows.some(r=>Array.from(r.removedNodes).some(n=>n===b||n.contains?.(b))))window.removedSave=true}).observe(document.body,{subtree:true,childList:true})}""")
        compact.locator('[data-audio-save]').focus(); child.wait_for_timeout(1300)
        with child.expect_download() as downloaded:
            child.keyboard.press('Enter')
        path = artifacts / 'browser-audio-range.tar'; downloaded.value.save_as(path)
        result = verify(path.read_bytes(), saved, waves, rows, first=first, count=count)
        assert not child.evaluate('window.removedSave')
        page.locator('#btn-lang').click(); page.locator('#btn-theme').click()
        expect(compact.locator('[data-audio-status]')).to_have_text('就绪')
        child.screenshot(path=str(artifacts / 'audio-playback-420-zh-light.png'))
        assert compact.locator('.mc-body').evaluate('e=>e.scrollWidth-e.clientWidth') == 0
        with child.expect_download() as downloaded:
            compact.locator('[data-audio-save]').click(delay=1100)
        mouse = artifacts / 'browser-audio-mouse.tar'; downloaded.value.save_as(mouse); assert mouse.read_bytes() == path.read_bytes()
        dock = compact.locator('[data-dock]'); dock.scroll_into_view_if_needed(); box = dock.bounding_box(); assert box
        with child.expect_event('close'):
            child.mouse.click(box['x']+box['width']/2, box['y']+box['height']/2)
        expect(audio).to_have_attribute('src', re.compile('blob:'))
        view.locator('[name=start]').fill('18446744073709551615'); expect(audio).not_to_have_attribute('src')
        expect(view.locator('[data-audio-save]')).to_be_disabled()
        view.locator('[data-audio-load]').click(); expect(view.locator('[data-audio-status]')).not_to_have_text('')
        view.locator('[name=start]').fill(str(first)); view.locator('[data-audio-load]').focus(); page.keyboard.down('Space'); page.wait_for_timeout(1300); page.keyboard.up('Space')
        expect(view.locator('[data-audio-status]')).to_have_text('就绪'); expect(audio).to_have_attribute('src', re.compile('blob:'))
        view.locator('[data-close]').click(); expect(audio).not_to_have_attribute('src')
        assert len(writes) == baseline, writes[baseline:]
        browser.close(); assert not errors, errors
        return {'download': result, 'overflow': 0, 'pageerrors': errors, 'reception_posts': len(writes)-baseline, 'native_play_seek': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-audio-playback'))
    args = parser.parse_args(); artifacts = args.artifacts.resolve(); artifacts.mkdir(parents=True, exist_ok=True)
    root = artifacts / 'recordings'; recording, expected, _ = boundary_fixture(root); split_fixture(root, recording)
    os.environ['WEBSA_RECORDING_DIR'] = str(root); port = free_port(); proc = None
    with (artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(port, log=log); saved = task(origin, recording)
            caps = api(origin)['capabilities']; assert caps['audio_archive_read'] and caps['audio_range_max_samples'] == '1048576' and caps['audio_range_max_seconds'] == 60
            _, rows, waves = download(origin, recording, artifacts / 'fixture-source.tar'); assert waves == expected
            hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (root / recording).glob('*.wba')}
            report = {'fixture': verify(get(origin, url(recording)), saved, waves, rows)}
            for segment, first, count in [('audio-000001', 1, 2), ('audio-000002', 0, 1)]:
                report[segment] = verify(get(origin, url(recording, segment, first, count)), saved, waves, rows, segment, first, count)
            report['actual'] = asyncio.run(protocol(origin, root, artifacts)); actual = report['actual']['task']
            _, actual_rows, actual_waves = download(origin, actual['task_id'], artifacts / 'actual-source.tar')
            report['actual_ranges'] = []
            for segment in actual['tracks'][0]['segments']:
                n = int(segment['samples']); first = min(137, n-1); count = min(n-first, 20000)
                report['actual_ranges'].append(verify(get(origin, url(actual['task_id'], segment['segment_id'], first, count)), actual, actual_waves, actual_rows, segment['segment_id'], first, count))
            uid = api(origin)['receivers'][0]['receiver_uid']; healthy = start(origin, uid, 'healthy-iq-playback', 'iq'); before = api(origin)
            report['transport'] = transport_gate(origin, recording, root)
            report['browser'] = browser_gate(origin, saved, waves, rows, artifacts)
            until(lambda: int(task(origin, healthy)['tracks'][0]['raw_bytes']) > 30000); assert_receivers_continue(before['receivers'], api(origin)['receivers']); finish_audio(origin, healthy)
            for path, digest in hashes.items():
                assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest
            for query, code in [(url(recording, count=1048577), 400), (url(recording)+'&format=opus', 400), (url(recording, first=-1), 400), (url(recording, segment='missing'), 422), (url(recording, first=1048578, count=2), 422), (url('missing'), 404), (url(healthy), 422)]:
                status(origin, query, code)
            active = start(origin, uid, 'active-audio-playback', 'audio'); status(origin, url(active), 409)
            until(lambda: int(task(origin, active)['tracks'][0]['raw_bytes']) > 30000); prior = task(origin, active); proc.kill(); proc.wait(timeout=3)
            proc, origin = start_service(port, log=log); recovered = task(origin, active)
            assert recovered['fault']['code'] == 'SERVICE_INTERRUPTED' and recovered['service_id'] == prior['service_id'] and recovered['receiver_uid'] == prior['receiver_uid']
            _, recovered_rows, recovered_waves = download(origin, active, artifacts / 'recovered-source.tar')
            segment = next(s for s in recovered['tracks'][0]['segments'] if int(s['samples']) > 0)
            count = min(20000, int(segment['samples'])); report['recovered'] = verify(get(origin, url(active, segment['segment_id'], 0, count)), recovered, recovered_waves, recovered_rows, segment['segment_id'], 0, count)
            report['snapshot'] = api(origin); report['source_hashes_unchanged'] = hashes
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2)+'\n'); print(json.dumps({'ok': True, 'fixture': report['fixture'], 'transport': report['transport'], 'browser': report['browser']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
