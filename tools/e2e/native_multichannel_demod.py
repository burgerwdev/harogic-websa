#!/usr/bin/env python3
"""Offline native IQ demodulation: known signals, independent WAV/SigMF and real UI.

Owns synthetic fixtures and a separate service only; no RF or sound-card acceptance.
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import io
import json
import os
import re
import shutil
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
from native_multichannel_processing import assert_receivers_continue, setup
from native_multichannel_recording import finish, read_archive, start, task, until
from native_multichannel_stream_export import get, read_http_head
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from scipy.io import wavfile
from viewport_baseline import wait_ui_ready


def fixture(root, mode, rate=48840.130763116205, count=65536, name=None, carrier_offset=0, passband_hz=3000):
    """Independent raw WBIQ writer. Original clock is 3:1 with a 1/3 anchor."""
    task_id = 'file-' + (name or mode)
    path = root / task_id
    path.mkdir(parents=True)
    n = np.arange(count, dtype=np.float64)
    phase = 2 * np.pi * 731 * n / rate
    if mode == 'am':
        iq = (0.5 + 0.2 * np.cos(phase)).astype(np.complex128)
    elif mode == 'nfm':
        iq = 0.5 * np.exp(1j * 500 / 731 * np.sin(phase))
    else:
        iq = 0.2 * np.exp(1j * phase * (-1 if mode == 'lsb' else 1))
    if carrier_offset != 0:
        iq = iq * np.exp(2j * np.pi * carrier_offset * n / rate)
    raw = np.column_stack([iq.real, iq.imag]).astype('<f4').tobytes()
    origin = 18446744073700000000
    page, blocks, digest = [], [], 0
    with (path / 'iq-000000.wbiq').open('wb') as archive:
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

        for at in range(0, count, 4096):
            payload = raw[at * 8:min(count, at + 4096) * 8]
            clock = {'receiver_uid': 'retired-fixture-uid', 'source_id': 'offline-fixture', 'track_id': 'iq', 'epoch': '7', 'channel_id': '9',
                'config_revision': '9007199254740993', 'first_input_index': str(origin + at * 3), 'first_output_index': str(origin + at),
                'fraction_num': '1', 'fraction_den': '3', 'samples': len(payload) // 8, 'sample_format': 'cf32_le', 'amplitude_scale': 1.0,
                'input_rate_hz': rate * 3, 'actual_rate_hz': rate, 'delay_seconds': 0.001}
            meta = {'service_id': 'original-offline-service', 'segment_id': '7-9', 'sequence': str(len(blocks)), 'frequency_hz': 100e6,
                'baseband': {'frequency_hz': 100e6, 'passband_hz': passband_hz, 'transition_hz': 1000, 'minimum_rate_hz': 6000}, 'clock': clock}
            header = {'meta': meta, 'codec': 'raw', 'raw_crc32': zlib.crc32(payload)}
            offset = record(b'BLK2', header, payload)
            index = {'offset': str(offset), **header, 'raw_bytes': len(payload), 'stored_bytes': len(payload)}
            page.append(index); blocks.append(index)
            if len(page) == 128:
                record(b'IDX2', page); page = []
        if page:
            record(b'IDX2', page)
        record(b'END2', {'blocks': str(len(blocks)), 'samples': str(count), 'records_crc32': digest})
    value = {'storage_version': 2, 'task_id': task_id, 'receiver_uid': 'retired-fixture-uid', 'service_id': 'original-offline-service',
        'created_at_ms': '1793510000000', 'updated_at_ms': '1793510000000', 'request_id': 'offline-' + mode, 'state': 'completed', 'fault': None,
        'accepted_bytes': str(len(raw)), 'queued_bytes': '0', 'tracks': [{'track_id': 'iq', 'kind': 'iq', 'format': 'cf32_le', 'source': 'baseband',
        'compression': 'zstd_auto_lossless', 'container': 'websa-iq-v2', 'state': 'completed', 'segments': [], 'raw_bytes': str(len(raw)),
        'stored_bytes': str((path / 'iq-000000.wbiq').stat().st_size), 'blocks': str(len(blocks)),
        'volumes': [{'file': 'iq-000000.wbiq', 'index': 'iq-000000.jsonl', 'complete': True}]}]}
    (path / 'task.json').write_text(json.dumps(value))
    return task_id, raw


def pressure_gate(origin, saved, offset_hz=None):
    path = url(saved, start=0, count=300000, offset_hz=offset_hz)
    host = urllib.parse.urlsplit(origin)
    connection = http.client.HTTPConnection(host.hostname, host.port, timeout=30)
    connection.request('GET', path)
    response = connection.getresponse()
    assert response.status == 200
    get(origin, url(saved, count=100, offset_hz=offset_hz), 429).close()
    response.close(); connection.close()

    def released():
        try:
            with urllib.request.urlopen(origin + url(saved, count=100, offset_hz=offset_hz), timeout=15) as probe:
                probe.read()
                return probe.status == 200
        except urllib.error.HTTPError as error:
            assert error.code == 429
            return False

    # Do not repeatedly queue large DSP jobs while the previous body is draining.
    time.sleep(0.2)
    until(released)
    with socket.socket() as stalled:
        stalled.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024); stalled.settimeout(30)
        stalled.connect((host.hostname, host.port))
        stalled.sendall(f'GET {path} HTTP/1.1\r\nHost: {host.netloc}\r\nConnection: close\r\n\r\n'.encode())
        head = read_http_head(stalled); assert b'200 OK' in head, head
        # A real TCP consumer stops reading; bounded producer timeout releases its work.
        time.sleep(2.3)
        get(origin, url(saved, count=100, offset_hz=offset_hz), 429).close()
    until(released)
    return {'cancelled_http_releases_lease': True, 'concurrent_status': 429, 'tcp_stall_releases_lease': True}


def url(saved, mode='am', start=137, count=20000, bandwidth=1500, offset_hz=None):
    segment = saved['tracks'][0]['segments'][0]
    query = {'segment': segment['segment_id'], 'start': str(start), 'count': str(count), 'mode': mode, 'if_bw': str(bandwidth)}
    if offset_hz is not None:
        query['offset_hz'] = str(offset_hz)
    return f'/api/sdr/recordings/{saved["task_id"]}/demodulate?' + urllib.parse.urlencode(query)


def verify(data, raw, saved, frequency=731, offset=137, count=20000, tuning_offset=0):
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        assert archive.getnames() == ['render.json', 'source.sigmf-meta', 'audio.wav']
        manifest = json.load(archive.extractfile('render.json'))
        source_bytes = archive.extractfile('source.sigmf-meta').read()
        source = json.loads(source_bytes)
        wave = archive.extractfile('audio.wav').read()
    rate, pcm = wavfile.read(io.BytesIO(wave))
    assert rate == 48000 and pcm.dtype == np.float32 and pcm.ndim == 1 and np.isfinite(pcm).all()
    assert manifest['audio']['pcm_sha256'] == hashlib.sha256(pcm.tobytes()).hexdigest()
    assert manifest['source']['cf32_sha256'] == hashlib.sha256(raw[offset * 8:(offset + count) * 8]).hexdigest()
    assert manifest['source']['metadata_sha256'] == hashlib.sha256(source_bytes).hexdigest()
    assert manifest['task'] == saved
    tuning = manifest['processing']['tuning']
    assert tuning['offset_hz'] == tuning_offset and tuning['recorded_center_hz'] == 100e6
    assert tuning['frequency_hz'] == 100e6 + tuning_offset
    assert source['captures'][0]['core:frequency'] == 100e6
    assert manifest['selection']['start'] == str(offset) and manifest['selection']['count'] == str(count)
    assert source['global']['websa:service_id'] == saved['service_id']
    assert source['global']['websa:receiver_uid'] == saved['receiver_uid']
    assert source['annotations'][0]['websa:skip_samples'] == str(offset)
    assert source['global']['websa:utc_reliable'] is False
    assert 'core:datetime' not in source['captures'][0]
    assert abs(len(pcm) / rate - count / manifest['selection']['iq_rate_hz']) < 2 / rate
    tail = pcm[4800:]
    assert len(tail) > 1000 and np.sqrt(np.mean(tail * tail)) > 0.01
    spectrum = abs(np.fft.rfft(tail))
    measured = np.argmax(spectrum[1:]) + 1
    hz = measured * rate / len(tail)
    assert abs(hz - frequency) < 5, hz
    return {'mode': manifest['processing']['mode'], 'samples': len(pcm), 'rate': rate, 'tone_hz': hz,
        'iq_rate': manifest['selection']['iq_rate_hz'], 'source_sha256': manifest['source']['cf32_sha256'], 'source': source, 'manifest': manifest}


def browser_gate(origin, saved, raw, out, frequency=731, bandwidth=1500, tuning_offset=0, mode='am'):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1366, 'height': 900})
        errors, writes = [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: writes.append(request.url) if request.method == 'POST' else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        # Saved files retain the original UID; the Files library includes retired owners.
        overview.locator('.mc-workspace-actions [data-action=recording-files]').click()
        page.locator('[data-view-id=recording-library] [data-action=archive][data-archive-task="' + saved['task_id'] + '"]').click(delay=1100)
        view = page.locator('[data-view-id=iq-recording]')
        view.locator('[name=start]').fill('137'); view.locator('[name=count]').fill('20000')
        view.locator('[data-file-demod] summary').click()
        view.locator('[data-demod-mode]').select_option(mode)
        view.locator('[data-demod-bw]').fill(str(bandwidth))
        view.locator('[data-demod-offset]').fill(str(tuning_offset))
        expect(view.locator('[data-demod-frequency]')).to_have_text(f'{100e6 + tuning_offset:,.6f}'.rstrip('0').rstrip('.'))
        baseline_writes = len(writes)
        view.locator('[data-demod-run]').click(delay=1100)
        expect(view.locator('[data-demod-status]')).to_have_text('Ready · 48 kHz · mono')
        audio = view.locator('[data-demod-audio]')
        audio.evaluate("e=>new Promise((resolve,reject)=>{if(e.readyState>=1)return resolve();e.addEventListener('loadedmetadata',resolve,{once:true});e.addEventListener('error',reject,{once:true})})")
        # The native media play button is operated with a real pointer. Focusing
        # the outer audio element does not focus Chromium's internal play control.
        audio.click(position={'x': 20, 'y': 27})
        page.wait_for_function("document.querySelector('[data-demod-audio]').currentTime > 0.05")
        audio.click(position={'x': 20, 'y': 27})
        page.screenshot(path=str(out / 'demod-desktop.png'))
        with page.expect_popup() as popup:
            view.locator('[data-pop]').click()
        child = popup.value; child.on('pageerror', lambda error: errors.append(str(error)))
        child.set_viewport_size({'width': 420, 'height': 760})
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(link=>link.sheet!==null)")
        child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=iq-recording]')
        child.evaluate("""()=>{const b=document.querySelector('[data-demod-save]');window.originalSave=b;window.removedSave=false;
          new MutationObserver(rows=>{if(rows.some(r=>Array.from(r.removedNodes).some(n=>n===b||n.contains?.(b))))window.removedSave=true}).observe(document.body,{subtree:true,childList:true})}""")
        compact.locator('[data-demod-save]').focus(); child.wait_for_timeout(1300)
        with child.expect_download() as download:
            child.keyboard.press('Enter')
        path = out / 'browser-demod.tar'; download.value.save_as(path)
        verified = verify(path.read_bytes(), raw, saved, frequency=frequency, tuning_offset=tuning_offset)
        assert not child.evaluate('window.removedSave')
        page.locator('#btn-lang').click(); page.locator('#btn-theme').click()
        expect(compact.locator('[data-demod-save]')).to_have_text('下载音频与来源')
        expect(compact.locator('[data-demod-status]')).to_have_text('就绪 · 48 kHz · 单声道')
        child.screenshot(path=str(out / 'demod-420-zh-light.png'))
        assert compact.locator('.mc-body').evaluate('e=>e.scrollWidth-e.clientWidth') == 0
        with child.expect_download() as mouse_download:
            compact.locator('[data-demod-save]').click(delay=1100)
        mouse_path = out / 'browser-demod-mouse.tar'; mouse_download.value.save_as(mouse_path)
        assert mouse_path.read_bytes() == path.read_bytes()
        dock = compact.locator('[data-dock]')
        dock.scroll_into_view_if_needed()
        box = dock.bounding_box(); assert box
        # Dock closes this child synchronously. Use the real pointer without a
        # locator's post-click navigation wait on the deliberately closed page.
        with child.expect_event('close'):
            child.mouse.click(box['x'] + box['width'] / 2, box['y'] + box['height'] / 2)
        expect(view.locator('[data-demod-audio]')).to_have_attribute('src', re.compile('blob:'))
        view.locator('[data-demod-offset]').fill(str(tuning_offset + 1))
        expect(view.locator('[data-demod-save]')).to_be_disabled()
        expect(view.locator('[data-demod-audio]')).not_to_have_attribute('src')
        view.locator('[name=start]').fill('138')
        if tuning_offset != 0:
            view.locator('[data-demod-offset]').fill('9999999')
            view.locator('[data-demod-run]').click(delay=1100)
            expect(view.locator('[data-demod-status]')).to_contain_text('shifted filter must fit')
            expect(view.locator('[data-demod-audio]')).not_to_have_attribute('src')
            view.locator('[data-demod-offset]').fill(str(tuning_offset))
            view.locator('[data-demod-run]').focus()
            page.keyboard.down('Space'); page.wait_for_timeout(1300); page.keyboard.up('Space')
            expect(view.locator('[data-demod-status]')).to_have_text('就绪 · 48 kHz · 单声道')
            expect(view.locator('[data-demod-audio]')).to_have_attribute('src', re.compile('blob:'))
            page.screenshot(path=str(out / 'tuning-docked-recovered.png'))
        assert len(writes) == baseline_writes, writes
        view.locator('[data-close]').click()
        page.screenshot(path=str(out / 'demod-docked.png'))
        assert not errors, errors
        browser.close()
    return {'download': verified, 'pageerrors': errors, 'popup_width': 420, 'configuration_writes': 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-file-demod'))
    parser.add_argument('--sigmf-python', type=Path, default=Path('/tmp/websa-sigmf-venv/bin/python'))
    args = parser.parse_args(); out = args.artifacts.resolve(); out.mkdir(parents=True, exist_ok=True)
    root = out / 'recordings'; os.environ['WEBSA_RECORDING_DIR'] = str(root)
    seeds = {mode: fixture(root, mode) for mode in ['am', 'usb', 'lsb', 'nfm']}
    long_id, _ = fixture(root, 'am', rate=6028.163580246914, count=365000, name='long')
    prefix = root / 'file-prefix'; shutil.copytree(root / seeds['am'][0], prefix)
    value = json.loads((prefix / 'task.json').read_text()); value['task_id'] = 'file-prefix'
    (prefix / 'task.json').write_text(json.dumps(value))
    with (prefix / 'iq-000000.wbiq').open('r+b') as file:
        file.truncate(file.seek(0, 2) - 64)
    proc = None
    with (out / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(free_port(), log=log)
            setup(origin)
            uid = api(origin)['receivers'][0]['receiver_uid']
            active_id = start(origin, uid, 'live-during-file-dsp')
            until(lambda: int(task(origin, active_id)['tracks'][0]['raw_bytes']) > 48000 * 8)
            snapshot = api(origin)
            results = []
            originals = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in root.glob('file-*/*')}
            for mode, (task_id, raw) in seeds.items():
                saved = task(origin, task_id)
                with get(origin, url(saved, mode)) as response:
                    data = response.read(); assert len(data) == int(response.headers['Content-Length'])
                (out / (mode + '.tar')).write_bytes(data)
                result = verify(data, raw, saved)
                meta_path = out / (mode + '.sigmf-meta'); meta_path.write_text(json.dumps(result['source']))
                sample_path = meta_path.with_suffix('.sigmf-data'); sample_path.write_bytes(raw[137 * 8:20137 * 8])
                checked = subprocess.run([str(args.sigmf_python), '-c',
                    'import sigmf,sys,hashlib,json; r=sigmf.fromfile(sys.argv[1]); r.validate(); '
                    'assert hashlib.sha256(r.read_samples().tobytes()).hexdigest()==sys.argv[2]; '
                    'print(json.dumps({"reader":sigmf.__version__,"samples":len(r)}))', str(meta_path), result['source_sha256']],
                    check=True, capture_output=True, text=True)
                result['independent_sigmf'] = json.loads(checked.stdout)
                assert result['source']['annotations'][0]['websa:clock']['fraction_num'] == '1'
                assert result['source']['annotations'][0]['websa:clock']['first_input_index'] == '18446744073700000000'
                results.append(result)
            saved = task(origin, seeds['am'][0])
            for path, status in [(url(saved, 'ft8'), 400), (url(saved, bandwidth='NaN'), 400), (url(saved, count=1048577), 400),
                (url(saved, 'usb', bandwidth=100), 400), (url(saved, 'lsb', bandwidth=100), 400),
                (url(saved, bandwidth=5000), 422), (url(saved, start=65530, count=100), 422),
                (url(task(origin, active_id), count=100), 409), (url(saved) + '&unexpected=1', 400)]:
                get(origin, path, status).close()
            report = {'source_kind': 'synthetic offline known-tone IQ and actual native synthetic AM recording', 'modes': results}
            prefix_task = task(origin, 'file-prefix'); assert prefix_task['state'] == 'failed'
            with get(origin, url(prefix_task)) as response:
                prefix_data = response.read()
            (out / 'recovered-prefix.tar').write_bytes(prefix_data)
            report['recovered_prefix'] = verify(prefix_data, seeds['am'][1], prefix_task)
            report['pressure'] = pressure_gate(origin, task(origin, long_id))
            get(origin, url(task(origin, long_id), start=0, count=365000), 422).close()
            report['browser'] = browser_gate(origin, saved, seeds['am'][1], out)
            assert_receivers_continue(snapshot['receivers'], api(origin)['receivers'])
            baseline = next(t for t in snapshot['recordings'] if t['task_id'] == active_id)
            assert int(task(origin, active_id)['tracks'][0]['raw_bytes']) > int(baseline['tracks'][0]['raw_bytes'])
            live = finish(origin, active_id)
            raw = b''.join(raw for volume in live['tracks'][0]['volumes'] for _, raw in read_archive(root / active_id / volume['file'])[0])
            with get(origin, url(live, count=20000, bandwidth=6000)) as response:
                data = response.read()
            (out / 'actual-am.tar').write_bytes(data)
            report['actual_recording'] = verify(data, raw, live, frequency=1000)
            assert all(hashlib.sha256(p.read_bytes()).hexdigest() == digest for p, digest in originals.items())
            report['original_files_unchanged'] = len(originals)
            # Damage after startup must fail preflight, release the common lease,
            # and leave a different healthy archive readable.
            with (root / seeds['nfm'][0] / 'iq-000000.wbiq').open('r+b') as file:
                file.seek(30); file.write(b'X')
            get(origin, url(task(origin, seeds['nfm'][0]), mode='nfm'), 422).close()
            with get(origin, url(saved)) as response:
                assert response.read()
            report['snapshot'] = api(origin)
            (out / 'report.json').write_text(json.dumps(report, indent=2))
            print(json.dumps({'passed': True, 'modes': len(results), 'artifacts': str(out)}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
