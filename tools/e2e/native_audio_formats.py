#!/usr/bin/env python3
"""Direct audio recording, immutable downloads, restart history and browser defaults.

Runs an isolated synthetic service. Requires built frontend/backend, FFmpeg and Playwright.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def until(check, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.05)
    raise AssertionError('audio condition did not become true')


def fetch(origin, path):
    deadline = time.monotonic() + 5
    while True:
        try:
            with urllib.request.urlopen(origin + path, timeout=30) as response:
                data = response.read()
                assert len(data) == int(response.headers['Content-Length'])
                return data
        except urllib.error.HTTPError as error:
            # The previous response body can finish before its reader lease is reaped.
            if error.code != 429 or time.monotonic() >= deadline:
                raise
            time.sleep(0.05)


def inspect_recording(origin, root, task):
    track = task['tracks'][0]
    assert task['state'] == 'completed', task
    assert track['container'] == 'websa-audio-files-v1'
    directory = root / task['task_id']
    assert not list(directory.glob('*.wba'))
    archive = fetch(origin, f"/api/sdr/recordings/{task['task_id']}/audio")
    with tarfile.open(fileobj=io.BytesIO(archive)) as package:
        manifest = json.load(package.extractfile('task.json'))
        assert manifest['encoded_during_recording'] and not manifest['transcoded_on_download']
        for member in package:
            if member.name != 'task.json':
                assert package.extractfile(member).read() == (directory / member.name).read_bytes()
    for segment in track['segments']:
        if not int(segment['samples']):
            continue
        media = directory / segment['file']
        assert hashlib.sha256(media.read_bytes()).hexdigest() == segment['sha256']
        probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(media)]))['streams'][0]
        assert probe['codec_name'] == {'wav': 'pcm_f32le', 'flac': 'flac', 'opus': 'opus'}[task['audio_format']], probe
        assert int(probe['channels']) == segment['channels']
        expected_rate = 48000 if task['audio_format'] == 'opus' else segment['actual_rate_hz']
        assert int(probe['sample_rate']) == expected_rate
        count = min(int(segment['samples']), 1000)
        preview = fetch(origin, f"/api/sdr/recordings/{task['task_id']}/audio/range?segment={segment['segment_id']}&start=0&count={count}")
        with tarfile.open(fileobj=io.BytesIO(preview)) as package:
            assert len(package.extractfile('audio.wav').read()) == 58 + count * segment['channels'] * 4
    mismatch = 'flac' if task['audio_format'] != 'flac' else 'opus'
    try:
        fetch(origin, f"/api/sdr/recordings/{task['task_id']}/audio/{mismatch}")
        raise AssertionError('recorded-format mismatch must be rejected')
    except urllib.error.HTTPError as error:
        assert error.code == 422


def browser_gate(origin, artifacts):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        try:
            page = browser.new_page(viewport={'width': 1440, 'height': 1000})
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.goto(origin)
            wait_ui_ready(page, 'sdr')
            page.locator('[data-action=sdr-workbench]').click()
            overview = page.locator('[data-view-id=overview-follow]')
            default = overview.locator('[data-default-audio-format]')
            expect(default).to_have_value('opus')
            default.select_option('wav')
            overview.locator('[data-monitor-mode]').select_option('follow')
            volume = overview.locator('[data-master]')
            volume.fill('0.35')
            volume.dispatch_event('input')
            page.reload()
            wait_ui_ready(page, 'sdr')
            page.locator('[data-action=sdr-workbench]').click()
            expect(default).to_have_value('wav')
            expect(overview.locator('[data-monitor-mode]')).to_have_value('follow')
            expect(volume).to_have_value('0.35')
            default.select_option('flac')
            overview.locator('[data-action=tab-recording]').click()
            button = overview.locator('[data-editor] [data-action=rec-audio]')
            button.click()
            running = until(lambda: next((t for t in api(origin)['recordings'] if t['state'] == 'recording'), None))
            assert running['audio_format'] == 'flac'
            default.select_option('opus')
            expect(overview.locator('[data-editor] .mc-record-format')).to_have_text('FLAC')
            button.click()
            until(lambda: next((t for t in api(origin)['recordings'] if t['task_id'] == running['task_id'] and t['state'] == 'completed'), None))
            link = overview.locator(f'[data-task-id="{running["task_id"]}"] .mc-audio-download')
            expect(link).to_have_attribute('href', f'/api/sdr/recordings/{running["task_id"]}/audio/flac')
            expect(overview.locator(f'[data-format-task="{running["task_id"]}"]')).to_have_count(0)
            default.select_option('wav')
            page.locator('[data-action=preset]').evaluate('(button) => button.click()')
            expect(default).to_have_value('opus')
            expect(volume).to_have_value('0.7')
            expect(overview.locator('[data-monitor-mode]')).to_have_value('manual')
            page.screenshot(path=str(artifacts / 'audio-default.png'))
            assert not errors, errors
        finally:
            browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-audio-formats'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    proc = None
    with tempfile.TemporaryDirectory(prefix='websa-audio-formats-') as temporary, (args.artifacts / 'service.log').open('w') as log:
        root = Path(temporary)
        os.environ['WEBSA_RECORDING_DIR'] = str(root)
        port = free_port()
        try:
            proc, origin = start_service(port, log=log, startup_timeout=30)
            snapshot = setup(origin)
            assert snapshot['capabilities']['max_recording_tasks'] == snapshot['capabilities']['max_channels']
            assert snapshot['recording_resources']['max_tasks'] == snapshot['capabilities']['max_channels']
            uid = snapshot['receivers'][0]['receiver_uid']
            tasks = []
            for format in ['opus', 'flac', 'wav']:
                command = {'type': 'record_start', 'request_id': f'format-{format}', 'receiver_uid': uid, 'kinds': ['audio']}
                if format != 'opus':
                    command['audio_format'] = format
                reply = api(origin, payload=command)
                assert reply['fault'] is None, reply
                task = reply['snapshot']['recordings'][-1]
                assert task['audio_format'] == format
                task_id = task['task_id']
                def current(task_id=task_id):
                    return next(t for t in api(origin)['recordings'] if t['task_id'] == task_id)
                until(lambda current=current: int(current()['tracks'][0]['raw_bytes']) > 192000)
                assert list((root / task_id).glob(f'*.{format}')), 'media must exist before stopping'
                reply = api(origin, payload={'type': 'record_stop', 'request_id': f'stop-{format}', 'task_id': task_id})
                assert reply['fault'] is None, reply
                task = until(lambda current=current: t if (t := current())['state'] in ['completed', 'failed'] else None)
                inspect_recording(origin, root, task)
                tasks.append(task)
            stop(proc)
            proc, origin = start_service(port, log=log, startup_timeout=30)
            assert api(origin)['recordings'] == sorted(tasks, key=lambda t: t['task_id'])
            setup(origin)
            browser_gate(origin, args.artifacts)
            print(json.dumps({'formats': [t['audio_format'] for t in tasks], 'downloads': 'byte-identical',
                              'restart_history': 'passed', 'browser_defaults_and_preset': 'passed'}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
