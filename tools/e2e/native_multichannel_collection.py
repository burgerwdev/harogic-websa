#!/usr/bin/env python3
"""Multi-rate SigMF Collections, checked by the official independent Python reader.

Uses isolated native synthetic recording plus a 32 MiB offline repeated fixture.
No hardware RF, file DSP replay or long-term throughput qualification is implied.
"""
from __future__ import annotations

import argparse
import asyncio
import http.client
import json
import os
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request
from pathlib import Path

from native_multichannel import api
from native_multichannel_archive import expected_range
from native_multichannel_processing import assert_receivers_continue
from native_multichannel_recording import finish, protocol, setup, start, task, until
from native_multichannel_stream_export import get, large_fixture, rss
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def url(task_id):
    return f'/api/sdr/recordings/{task_id}/collection'


def save(origin, task_id, path):
    with get(origin, url(task_id)) as response, path.open('wb') as output:
        length = int(response.headers['Content-Length'])
        assert response.headers['Cache-Control'] == 'no-store'
        while chunk := response.read(65536):
            output.write(chunk)
    assert path.stat().st_size == length


def verify(path, saved, root, python):
    """Load the collection directly, verify SHA512 references and all CF32 bytes."""
    directory = path.with_suffix('')
    directory.mkdir()
    segments = [s for s in saved['tracks'][0]['segments'] if int(s['samples'])]
    names = ['recording.sigmf-collection'] + [f'segment-{n:06}.{suffix}' for n in range(len(segments)) for suffix in ['sigmf-meta', 'sigmf-data']]
    with tarfile.open(path) as archive:
        assert set(archive.getnames()) == set(names)
        assert all(member.isfile() for member in archive.getmembers())
        archive.extractall(directory, filter='data')
    for n, segment in enumerate(segments):
        (directory / f'expected-{n}.cf32').write_bytes(expected_range(root, saved, segment, 0, int(segment['samples'])))
    (directory / 'task.json').write_text(json.dumps(saved))
    code = '''import json,sys,hashlib,sigmf,jsonschema
from pathlib import Path
root=Path(sys.argv[1]); task=json.loads((root/'task.json').read_text())
collection=sigmf.fromfile(root/'recording.sigmf-collection')
assert isinstance(collection,sigmf.SigMFCollection)
collection.verify_stream_hashes()
segments=[s for s in task['tracks'][0]['segments'] if int(s['samples'])]
assert len(collection)==len(segments)
records=[]
for n,segment in enumerate(segments):
    name=collection.get_stream_names()[n]
    assert name==f'segment-{n:06}'
    reader=collection.get_SigMFFile(stream_index=n); reader.validate()
    raw=reader.read_samples().tobytes()
    assert raw==(root/f'expected-{n}.cf32').read_bytes()
    meta=json.loads((root/(name+'.sigmf-meta')).read_text())
    assert meta['global']['core:sample_rate']==segment['actual_rate_hz']
    assert meta['captures']==[{'core:sample_start':0,'core:frequency':segment['frequency_hz']}]
    for field in ['receiver_uid','service_id','task_id','task_state','task_fault']:
        expected=task[field.removeprefix('task_')] if field in ['task_state','task_fault'] else task[field]
        assert meta['global']['websa:'+field]==expected
    assert meta['global']['websa:segment_id']==segment['segment_id']
    assert sum(a['core:sample_count'] for a in meta['annotations'])==int(segment['samples'])
    clock=meta['annotations'][0]['websa:clock']
    assert clock['first_input_index']==segment['first_input_index']
    assert clock['fraction_num']==segment['fraction_num'] and clock['fraction_den']==segment['fraction_den']
    records.append({'samples':len(raw)//8,'rate':segment['actual_rate_hz'],'frequency':segment['frequency_hz'],'sha256':hashlib.sha256(raw).hexdigest()})
# The upstream 1.2.6 collection schema still only admits the older tuple form,
# although its prose and SigMFCollection reader use name/hash objects. Preserve
# this discrepancy as evidence; do not patch the independent reader/schema.
schema=json.loads((Path(sigmf.__file__).parent/'schema-collection.json').read_text())
try:
    jsonschema.validate(json.loads((root/'recording.sigmf-collection').read_text()),schema)
except jsonschema.ValidationError as error:
    assert error.absolute_path[0]=='collection' and 'core:streams' in error.absolute_path
    schema_status='upstream tuple-only schema rejects spec-recommended objects; official collection reader passed'
else:
    schema_status='passed'
print(json.dumps({'reader':sigmf.__version__,'specification':sigmf.__specification__,'records':records,'collection_schema':schema_status}))
'''
    result = subprocess.run([str(python), '-c', code, str(directory)], check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def browser_gate(origin, saved, root, output, python):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1366, 'height': 900})
        errors, commands = [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: commands.append(request.url) if request.method == 'POST' else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        pane = page.locator('[data-view-id=overview-follow]')
        pane.locator('[data-action=tab-recording]').click()
        pane.locator(f'[data-task-id="{saved["task_id"]}"] [data-action=archive]').click()
        view = page.locator('[data-view-id=iq-recording]')
        view.locator('[data-load]').click()
        expect(view.locator('[data-status]')).to_have_text('Ready')
        baseline, count = api(origin), len(commands)
        with page.expect_download() as download:
            view.locator('[data-download-collection]').click(delay=1100)
        desktop = output / 'browser-collection.sigmf'; download.value.save_as(desktop)
        checked = verify(desktop, saved, root, python)
        with page.expect_popup() as popup:
            view.locator('[data-pop]').click()
        child = popup.value; child.on('pageerror', lambda error: errors.append(str(error)))
        child.set_viewport_size({'width': 420, 'height': 760})
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(e=>e.sheet)")
        child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=iq-recording]')
        page.locator('#btn-lang').click(); expect(child.locator('html')).to_have_attribute('lang', 'zh-CN')
        page.locator('#btn-theme').click(); expect(child.locator('html')).to_have_attribute('data-theme', 'light')
        expect(compact.locator('[data-download-collection]')).to_have_text('全部片段（2）')
        with child.expect_download() as download:
            compact.locator('[data-download-collection]').click(delay=1100)
        downloaded = output / 'browser-collection-420.sigmf'; download.value.save_as(downloaded)
        verify(downloaded, saved, root, python)
        expect(compact.locator('[data-status]')).to_have_text('已请求下载')
        assert compact.locator('.mc-body').evaluate('e=>e.scrollWidth-e.clientWidth') == 0
        child.screenshot(path=str(output / 'collection-420-zh-light.png'))
        compact.locator('[data-dock]').click()
        expect(view.locator('[data-download-collection]')).to_be_visible()
        assert len(commands) == count, commands[count:]
        assert_receivers_continue(baseline['receivers'], api(origin)['receivers'])
        assert not errors, errors
        browser.close()
    return {'reader': checked, 'popup_width': 420, 'pageerrors': errors, 'configuration_writes': len(commands)-count}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-collection'))
    parser.add_argument('--sigmf-python', type=Path, default=Path(sys.executable))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    root = (args.artifacts / 'recordings').resolve()
    fixture_id, blocks = large_fixture(root, args.artifacts / 'fixture.expected-cf32', split_at=2097152)
    os.environ['WEBSA_RECORDING_DIR'] = str(root)
    proc = None
    with (args.artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(free_port(), log=log)
            snapshot = api(origin)
            assert snapshot['capabilities']['iq_archive_collection']
            assert snapshot['capabilities']['iq_collection_max_samples'] == '1073741823'
            assert snapshot['capabilities']['iq_collection_max_segments'] == 4096
            with get(origin, url('missing-task'), 404), get(origin, url(fixture_id) + '?start=0', 400):
                pass
            saved = task(origin, fixture_id)
            assert len(saved['tracks'][0]['segments']) == 2
            warm = args.artifacts / 'warm.sigmf'; save(origin, fixture_id, warm)
            before = rss(proc.pid)
            large = args.artifacts / 'large-collection.sigmf'; save(origin, fixture_id, large)
            after = rss(proc.pid)
            assert after-before < 24 * 1024 * 1024, (before, after)
            report = {'source_kind': '32 MiB repeated offline fixture + actual synthetic native recording',
                      'large': verify(large, saved, root, args.sigmf_python), 'rss_before': before, 'rss_after': after}
            setup(origin)
            report['recording'] = asyncio.run(protocol(origin, root))
            recorded = report['recording']['task']
            actual = args.artifacts / 'native-collection.sigmf'; save(origin, recorded['task_id'], actual)
            report['native'] = verify(actual, recorded, root, args.sigmf_python)
            healthy = api(origin)['receivers'][2]['receiver_uid']
            active_id = start(origin, healthy, 'collection-healthy')
            until(lambda: int(task(origin, active_id)['tracks'][0]['raw_bytes']) > 10000)
            size = int(task(origin, active_id)['tracks'][0]['raw_bytes'])
            with get(origin, url(active_id), 409):
                pass
            response = get(origin, url(fixture_id)); response.read(65536)
            with get(origin, url(recorded['task_id']), 429):
                pass
            response.close()

            def available():
                try:
                    with urllib.request.urlopen(origin + url(recorded['task_id']), timeout=10) as response:
                        response.read()
                        return response.status == 200
                except urllib.error.HTTPError as error:
                    assert error.code == 429
                    return False
            until(available, timeout=8)
            response = get(origin, url(fixture_id))
            archive_path = root / fixture_id / 'iq-000000.wbiq'
            position = int(blocks[-1]['offset']) + 25
            with archive_path.open('r+b') as file:
                file.seek(position); original = file.read(1); file.seek(position); file.write(bytes([original[0] ^ 1]))
            transferred = 0
            try:
                while chunk := response.read(65536):
                    transferred += len(chunk)
            except (OSError, http.client.IncompleteRead):
                pass
            finally:
                response.close()
            assert transferred < int(response.headers['Content-Length'])
            with get(origin, url(fixture_id), 422):
                pass
            with archive_path.open('r+b') as file:
                file.seek(position); file.write(original)
            until(available, timeout=8)
            report['browser'] = browser_gate(origin, recorded, root, args.artifacts, args.sigmf_python)
            until(lambda: int(task(origin, active_id)['tracks'][0]['raw_bytes']) > size + 10000)
            finish(origin, active_id)
            # Crash a task with two real source segments, then export only its checked prefix.
            interrupted_id = start(origin, recorded['receiver_uid'], 'collection-interrupted')
            until(lambda: int(task(origin, interrupted_id)['tracks'][0]['raw_bytes']) > 10000)
            receiver = api(origin)['receivers'][0]
            candidate = json.loads(json.dumps(receiver['confirmed']))
            candidate['baseband']['frequency_hz'] += 10000
            reply = api(origin, payload={'type': 'configure', 'request_id': 'collection-last-retune', 'receiver_uid': receiver['receiver_uid'],
                                         'expected_revision': receiver['revision'], 'config': candidate})
            assert reply['fault'] is None
            until(lambda: len(s := task(origin, interrupted_id)['tracks'][0]['segments']) == 2 and int(s[-1]['samples']) > 1000)
            proc.kill(); proc.wait(timeout=3)
            proc, origin = start_service(free_port(), log=log)
            recovered = task(origin, interrupted_id)
            assert recovered['state'] == 'failed' and recovered['fault']['code'] == 'SERVICE_INTERRUPTED'
            assert api(origin)['service_id'] != recovered['service_id']
            prefix = args.artifacts / 'recovered-collection.sigmf'; save(origin, interrupted_id, prefix)
            report['recovered'] = verify(prefix, recovered, root, args.sigmf_python)
            snapshot_file = args.artifacts / 'snapshot.json'; snapshot_file.write_text(json.dumps(api(origin)))
            subprocess.run([str(args.sigmf_python), '-c', "import json,sys,jsonschema;s=json.load(open(sys.argv[1]));jsonschema.validate(json.load(open(sys.argv[2])),{'$defs':s['$defs'],'$ref':'#/$defs/snapshot'})", str(ROOT / 'tests/fixtures/multichannel/draft-v1.schema.json'), str(snapshot_file)], check=True)
            report['partial_response_bytes'] = transferred
            report['checks'] = ['multi-rate collection and SHA512 metadata references', 'official SigMFCollection opens extracted archive',
                'all CF32 bytes and exact source clocks', 'actual network publication equals recorded blocks', '32 MiB bounded streaming',
                'active/unknown/invalid/concurrent/corrupt requests', 'HTTP cancellation releases shared lease', 'late second-record corruption truncates response',
                'healthy recording continues', 'desktop and 420px native downloads, Chinese/light/Dock', 'two-segment SIGKILL prefix keeps old UID/service', 'actual snapshot schema']
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2)+'\n')
            print(json.dumps({'ok': True, 'checks': report['checks']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
