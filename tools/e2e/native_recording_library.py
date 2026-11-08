#!/usr/bin/env python3
"""Audio format selection, independent Opus decode and durable file management."""
from __future__ import annotations

import argparse
import io
import json
import os
import shutil
import subprocess
import tarfile
import urllib.request
from pathlib import Path

import rust_iq_browser
from native_multichannel import api
from native_multichannel_flac import boundary_fixture
from native_multichannel_processing import setup
from native_multichannel_recording import task, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def download(origin, recording, format):
    path = f'/api/sdr/recordings/{recording}/audio' + ('' if format == 'wav' else f'/{format}')
    with urllib.request.urlopen(origin + path, timeout=30) as response:
        data = response.read()
        assert len(data) == int(response.headers['Content-Length'])
    return data


def command(origin, kind, key, **fields):
    reply = api(origin, payload={'type': kind, 'request_id': key, **fields})
    assert reply['fault'] is None, reply
    return reply


def browser(origin, recording, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1440, 'height': 1000}, accept_downloads=True)
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        overview.locator('[data-action=tab-recording]').click()
        menu = overview.locator('[data-editor] .mc-record-controls [data-format-menu]')
        expect(menu).to_be_visible()
        menu.locator('summary').click()
        expect(menu.locator('[data-format=opus]')).to_be_enabled()
        expect(menu.locator('[data-format=flac]')).to_be_visible()
        menu.locator('[data-format=opus]').click()
        expect(menu.locator('summary')).to_contain_text('Opus')
        overview.locator('[data-editor] [data-action=rec-audio]').click()
        until(lambda: any(t.get('audio_format') == 'opus' and t['state'] == 'recording' for t in api(origin)['recordings']))
        overview.locator('.mc-workspace-actions [data-action=recording-files]').click()
        files = page.locator('[data-view-id=recording-library]')
        expect(files).to_be_visible()
        active = files.locator('[data-task-state=recording]')
        expect(active.locator('[data-file-action=delete]')).to_be_disabled()
        expect(active.locator('[data-file-select]')).to_be_disabled()
        active.locator('[data-file-action=stop]').click()
        expect(files.locator('[data-task-state=recording]')).to_have_count(0, timeout=10000)
        row = files.locator(f'[data-task-id="{recording}"]')
        row.locator('[data-file-action=rename]').click()
        row.locator('[name=recording-name]').fill('Evening station · 晚间电台')
        page.wait_for_timeout(1200)
        expect(row.locator('[name=recording-name]')).to_have_value('Evening station · 晚间电台')
        row.locator('[type=submit]').click()
        expect(row.locator('.mc-file-info>strong')).to_have_text('Evening station · 晚间电台')
        files.locator('[data-file-search]').fill('晚间')
        expect(files.locator('.mc-file')).to_have_count(1)
        row.locator('.mc-audio-export summary').click()
        row.locator('[data-format=opus]').hover()
        page.screenshot(path=str(artifacts / 'recording-files-dark.png'))
        row.locator('[data-format=opus]').click()
        with page.expect_download(timeout=30000) as result:
            row.locator('a.mc-audio-download').click()
        result.value.save_as(str(artifacts / 'browser-opus.tar'))
        # Narrow windows and light Chinese UI use the same controls and live draft.
        page.evaluate("document.getElementById('btn-lang').click()")
        page.evaluate("document.getElementById('btn-theme').click()")
        with page.expect_popup() as popup_result:
            files.locator('[data-pop]').click()
        popup = popup_result.value
        popup.set_viewport_size({'width': 420, 'height': 760})
        popup.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(e=>e.sheet)")
        files = popup.locator('[data-view-id=recording-library]')
        row = files.locator(f'[data-task-id="{recording}"]')
        row.locator('.mc-audio-export summary').click()
        expect(row.locator('[data-format=flac]')).to_be_visible()
        popup.screenshot(path=str(artifacts / 'recording-files-420-light.png'))
        assert files.evaluate('e => e.scrollWidth <= e.clientWidth + 1')
        row.locator('summary').click()
        row.locator('[data-file-action=delete]').click()
        expect(row.locator('[data-file-action=confirm-delete]')).to_be_visible()
        row.locator('[data-file-action=cancel-delete]').click()
        expect(row).to_be_visible()
        # Selection is limited to the filtered files and survives native polling.
        files.locator('[data-file-search]').fill('Batch')
        expect(files.locator('.mc-file')).to_have_count(2)
        expect(files.locator('[data-file-size]').first).to_contain_text('MB')
        files.locator('[data-file-select]').first.check()
        expect(files.locator('[data-file-select-all]')).to_have_js_property('indeterminate', True)
        files.locator('[data-file-select-all]').check()
        expect(files.locator('[data-selected-info]')).to_contain_text('已选 2 项')
        popup.wait_for_timeout(1100)
        expect(files.locator('[data-file-select]:checked')).to_have_count(2)
        files.locator('[data-file-action=delete-selected]').click()
        expect(files.locator('.mc-files-batch-confirm')).to_contain_text('删除 2 项录制')
        popup.screenshot(path=str(artifacts / 'recording-batch-delete-420-light.png'))
        assert files.evaluate('e => e.scrollWidth <= e.clientWidth + 1')
        files.locator('[data-file-action=cancel-batch-delete]').click()
        expect(files.locator('.mc-file')).to_have_count(2)
        files.locator('[data-file-action=delete-selected]').click()
        files.locator('[data-file-action=confirm-batch-delete]').click()
        expect(files.locator('.mc-file')).to_have_count(0, timeout=15000)
        expect(files.locator('[role=status]')).to_contain_text('已删除 2 项录制')
        files.locator('[data-file-search]').fill('')
        expect(files.locator(f'[data-task-id="{recording}"]')).to_be_visible()
        assert not any(t.get('name', '').startswith('Batch') for t in api(origin)['recordings'])
        assert not errors, errors
        browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-recording-library'))
    parser.add_argument('--binary', type=Path, default=rust_iq_browser.WEBSAD)
    args = parser.parse_args()
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    root = artifacts / 'recordings'
    recording, waves, _ = boundary_fixture(root)
    for index in range(2):
        identifier = f'audio-batch-{index}'
        shutil.copytree(root / recording, root / identifier)
        manifest_path = root / identifier / 'task.json'
        fixture = json.loads(manifest_path.read_text())
        fixture.update(task_id=identifier, name=f'Batch {index + 1}')
        fixture['tracks'][0]['track_id'] = f'{identifier}-audio'
        manifest_path.write_text(json.dumps(fixture))
    rust_iq_browser.WEBSAD = args.binary.resolve()
    os.environ['WEBSA_RECORDING_DIR'] = str(root)
    port = free_port()
    log = (artifacts / 'service.log').open('w')
    proc, origin = start_service(port, log=log)
    try:
        snapshot = setup(origin)
        assert snapshot['capabilities']['audio_opus_export']
        original = task(origin, recording)
        data = download(origin, recording, 'opus')
        (artifacts / 'opus.tar').write_bytes(data)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            manifest = json.load(archive.extractfile('task.json'))
            assert manifest['task'] == original
            assert len(manifest['segments']) == len(waves)
            for segment in manifest['segments']:
                encoded = archive.extractfile(segment['file']).read()
                probe = subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', 'pipe:0'], input=encoded, capture_output=True, check=True)
                stream = json.loads(probe.stdout)['streams'][0]
                assert stream['codec_name'] == 'opus' and stream['sample_rate'] == '48000'
                assert stream['channels'] == segment['channels']
                decoded = subprocess.run(['ffmpeg', '-v', 'error', '-i', 'pipe:0', '-f', 'f32le', 'pipe:1'], input=encoded, capture_output=True, check=True).stdout
                assert decoded
                source = next(s for s in original['tracks'][0]['segments'] if s['segment_id'] == segment['segment_id'])
                frames = len(decoded) // (4 * segment['channels'])
                assert abs(frames - round(int(source['samples']) * 48000 / source['actual_rate_hz'])) <= 2, (frames, source)
        browser(origin, recording, artifacts)
        saved_name = task(origin, recording)['name']
        stop(proc)
        proc, origin = start_service(port, log=log)
        assert task(origin, recording)['name'] == saved_name
        assert not any(t.get('name', '').startswith('Batch') for t in api(origin)['recordings'])
        request = {'type': 'record_delete', 'request_id': 'delete-files', 'task_id': recording}
        reply = api(origin, payload=request)
        assert reply['fault'] is None, reply
        assert api(origin, payload=request) == reply
        assert not (root / recording).exists()
        assert not any(t['task_id'] == recording for t in api(origin)['recordings'])
        stop(proc)
        proc, origin = start_service(port, log=log)
        assert not any(t['task_id'] == recording for t in api(origin)['recordings'])
        print(json.dumps({'ok': True, 'formats': ['wav', 'flac', 'opus'], 'opus_segments': len(waves), 'rename_restart': True, 'delete_restart': True, 'batch_delete_restart': True, 'artifacts': str(artifacts)}))
    finally:
        stop(proc)
        log.close()


if __name__ == '__main__':
    main()
