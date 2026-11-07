#!/usr/bin/env python3
"""Checked IQ range playback/downloads with the independent official SigMF reader.

Owns a synthetic service and storage only. Requires built frontend/websad, browser
dependencies, and --sigmf-python with sigmf==1.6.1 (SigMF schema 1.2.6).
"""
from __future__ import annotations

import argparse
import asyncio
import io
import json
import os
import subprocess
import sys
import tarfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from native_multichannel import api
from native_multichannel_recording import finish, protocol, read_archive, setup, start, task, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready


def range_url(saved, segment, offset, count, kind='sigmf'):
    return '/api/sdr/recordings/' + saved['task_id'] + '/range?' + urllib.parse.urlencode(
        {'segment': segment['segment_id'], 'start': str(offset), 'count': str(count), 'format': kind})


def get(origin, path, status=200):
    try:
        with urllib.request.urlopen(origin + path, timeout=15) as response:
            assert status == response.status
            return response.read()
    except urllib.error.HTTPError as error:
        assert error.code == status, (error.code, error.read())
        return error.read()


def expected_range(root, saved, segment, offset, count):
    data = bytearray()
    for volume in saved['tracks'][0]['volumes']:
        blocks, _ = read_archive(root / saved['task_id'] / volume['file'])
        for index, raw in blocks:
            if index['meta']['segment_id'] == segment['segment_id']:
                data.extend(raw)
    return bytes(data[offset * 8:(offset + count) * 8])


def verify_download(path, expected, segment, saved, sigmf_python):
    with tarfile.open(fileobj=io.BytesIO(path.read_bytes())) as archive:
        names = archive.getnames()
        assert set(names) == {'recording/recording.sigmf-meta', 'recording/recording.sigmf-data'}
        metadata = json.load(archive.extractfile('recording/recording.sigmf-meta'))
        assert archive.extractfile('recording/recording.sigmf-data').read() == expected
    assert metadata['global']['core:sample_rate'] == segment['actual_rate_hz']
    assert metadata['captures'][0]['core:frequency'] == segment['frequency_hz']
    assert 'core:datetime' not in metadata['captures'][0]
    assert metadata['global']['websa:receiver_uid'] == saved['receiver_uid']
    assert metadata['global']['websa:service_id'] == saved['service_id']
    assert sum(a['core:sample_count'] for a in metadata['annotations']) * 8 == len(expected)
    expected_path = path.with_suffix('.expected-cf32')
    expected_path.write_bytes(expected)
    code = '''import json, sys, sigmf
from pathlib import Path
reader = sigmf.SigMFArchiveReader(sys.argv[1])
reader.sigmffile.validate()
assert reader.sigmffile.read_samples().tobytes() == Path(sys.argv[2]).read_bytes()
assert reader.sigmffile.get_global_field('core:sample_rate') == float(sys.argv[3])
print(json.dumps({'reader_version':sigmf.__version__, 'schema':sigmf.__specification__, 'samples':len(reader)}))
'''
    result = subprocess.run([str(sigmf_python), '-c', code, str(path), str(expected_path), str(segment['actual_rate_hz'])],
                            check=True, text=True, capture_output=True)
    return json.loads(result.stdout)


def browser_gate(origin, saved, root, artifacts, sigmf_python):
    segment = saved['tracks'][0]['segments'][0]
    expected = expected_range(root, saved, segment, 137, 1500)
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1366, 'height': 900})
        errors = []
        parent_downloads = []
        requests = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('download', lambda download: parent_downloads.append(download.suggested_filename))
        page.on('request', lambda request: requests.append(request.url) if '/range?' in request.url else None)
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        overview.locator('[data-action=tab-recording]').click()
        overview.locator(f'[data-task-id="{saved["task_id"]}"] [data-action=archive]').click(delay=1100)
        view = page.locator('[data-view-id=iq-recording]')
        view.locator('[name=start]').fill('137')
        view.locator('[name=count]').fill('1500')
        view.locator('[data-load]').click()
        expect(view.locator('[data-status]')).to_have_text('Ready')
        assert view.locator('[data-iq-wave]').evaluate("c => Array.from(c.getContext('2d').getImageData(0,0,c.width,c.height).data).filter((v,i)=>i%4===3&&v>0).length") > 1000
        view.locator('[data-play]').click()
        page.wait_for_timeout(100)
        assert int(view.locator('[data-position]').input_value()) > 0
        view.locator('[data-position]').focus()
        page.keyboard.press('End')
        expect(view.locator('[data-position-info]')).to_contain_text('1636')
        page.screenshot(path=str(artifacts / 'archive-waveform.png'))
        with page.expect_popup() as popup:
            view.locator('[data-pop]').click()
        child = popup.value
        child.on('pageerror', lambda error: errors.append(str(error)))
        child.set_viewport_size({'width': 420, 'height': 760})
        # Portal styles/fonts load in the new document before native pointer geometry is stable.
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(link=>link.sheet!==null)")
        child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=iq-recording]')
        expect(compact.locator('[name=start]')).to_have_value('137')
        child.evaluate("""() => { window.archivePresses=[]; for (const type of ['pointerdown','pointerup','click']) document.addEventListener(type,e=>window.archivePresses.push([type,e.target.tagName,e.target.hasAttribute('data-download'),e.target.disabled]),true); }""")
        try:
            with child.expect_download() as download:
                compact.locator('[data-download]').click(delay=1100)
        except Exception:
            child.screenshot(path=str(artifacts / 'download-failure.png'))
            print(json.dumps({'download_status': compact.locator('[data-status]').text_content(), 'pageerrors': errors, 'parent_downloads': parent_downloads,
                              'requests': requests, 'presses': child.evaluate('window.archivePresses')}), flush=True)
            raise
        downloaded = artifacts / 'browser-range.sigmf'
        download.value.save_as(downloaded)
        checked = verify_download(downloaded, expected, segment, saved, sigmf_python)
        page.locator('#btn-lang').click()
        expect(child.locator('html')).to_have_attribute('lang', 'zh-CN')
        expect(compact.locator('[data-download]')).to_have_text('下载 SigMF')
        page.locator('#btn-theme').click()
        expect(child.locator('html')).to_have_attribute('data-theme', 'light')
        child.screenshot(path=str(artifacts / 'archive-420-zh-light.png'))
        assert compact.locator('.mc-body').evaluate('(e)=>e.scrollWidth-e.clientWidth') == 0
        compact.locator('[data-dock]').click()
        expect(view.locator('[name=start]')).to_have_value('137')
        view.locator('[data-close]').click()
        page.locator('[data-action=sdr-workbench]').click()
        overview.locator(f'[data-task-id="{saved["task_id"]}"] [data-action=archive]').click()
        expect(view.locator('[data-position-info]')).to_contain_text('1636')
        assert not errors, errors
        browser.close()
    return {'independent_reader': checked, 'pageerrors': errors, 'popup_width': 420}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-iq-archive'))
    parser.add_argument('--sigmf-python', type=Path, default=Path(sys.executable))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    root = (args.artifacts / 'recordings').resolve()
    os.environ['WEBSA_RECORDING_DIR'] = str(root)
    proc = None
    with (args.artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(free_port(), log=log)
            setup(origin)
            report = {'source_kind': 'synthetic native baseband', 'recording': asyncio.run(protocol(origin, root))}
            saved = report['recording']['task']
            healthy = api(origin)['receivers'][2]
            active_id = start(origin, healthy['receiver_uid'], 'continues-during-export')
            baseline = api(origin)
            until(lambda: int(task(origin, active_id)['tracks'][0]['raw_bytes']) > 10000)
            active = task(origin, active_id)
            get(origin, range_url(active, active['tracks'][0]['segments'][0], 0, 1), 409)
            ranges = []
            for index, segment in enumerate(saved['tracks'][0]['segments']):
                offset, count = 137, 1500
                path = args.artifacts / f'segment-{index}.sigmf'
                path.write_bytes(get(origin, range_url(saved, segment, offset, count)))
                expected = expected_range(root, saved, segment, offset, count)
                assert get(origin, range_url(saved, segment, offset, count, 'cf32')) == expected
                ranges.append(verify_download(path, expected, segment, saved, args.sigmf_python))
                get(origin, range_url(saved, segment, int(segment['samples']), 1), 422)
                get(origin, range_url(saved, segment, 0, 1048577), 400)
            report['ranges'] = ranges
            report['browser'] = browser_gate(origin, saved, root, args.artifacts, args.sigmf_python)
            assert api(origin)['revision'] == baseline['revision']
            assert [r['stream'] for r in api(origin)['receivers']] == [r['stream'] for r in baseline['receivers']]
            until(lambda: int(task(origin, active_id)['tracks'][0]['raw_bytes']) > int(active['tracks'][0]['raw_bytes']) + 10000)
            finish(origin, active_id)
            interrupted_id = start(origin, healthy['receiver_uid'], 'archive-interrupted')
            until(lambda: int(task(origin, interrupted_id)['tracks'][0]['raw_bytes']) > 10000)
            # Recovered history remains readable with its old service/UID, after a new boot.
            proc.kill()
            proc.wait(timeout=3)
            proc, origin = start_service(free_port(), log=log)
            assert api(origin)['service_id'] != saved['service_id']
            recovered = task(origin, saved['task_id'])
            segment = recovered['tracks'][0]['segments'][0]
            assert get(origin, range_url(recovered, segment, 137, 1500, 'cf32')) == expected_range(root, saved, segment, 137, 1500)
            interrupted = task(origin, interrupted_id)
            assert interrupted['state'] == 'failed' and interrupted['fault']['code'] == 'SERVICE_INTERRUPTED'
            interrupted_segment = interrupted['tracks'][0]['segments'][0]
            interrupted_path = args.artifacts / 'recovered-prefix.sigmf'
            interrupted_path.write_bytes(get(origin, range_url(interrupted, interrupted_segment, 137, 1000)))
            report['recovered_prefix'] = verify_download(interrupted_path, expected_range(root, interrupted, interrupted_segment, 137, 1000),
                                                        interrupted_segment, interrupted, args.sigmf_python)
            with sync_playwright() as p:
                browser = p.chromium.launch(headless=True)
                page = browser.new_page(viewport={'width': 1366, 'height': 900})
                page.goto(origin)
                wait_ui_ready(page)
                # Native service startup may be in STD; opening the workbench is available in SDR.
                post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
                page.reload()
                wait_ui_ready(page, 'sdr')
                page.locator('[data-action=sdr-workbench]').click()
                overview = page.locator('[data-view-id=overview-follow]')
                overview.locator('.mc-workspace-actions [data-action=recording-files]').click()
                page.locator(f'[data-view-id=recording-library] [data-archive-task="{interrupted_id}"]').click()
                view = page.locator('[data-view-id=iq-recording]')
                expect(view.locator('[data-task-fault]')).to_contain_text('Task failed')
                view.locator('[data-load]').click()
                expect(view.locator('[data-status]')).to_have_text('Ready')
                expect(view.locator('[data-owner]')).to_contain_text(interrupted['receiver_uid'])
                page.screenshot(path=str(args.artifacts / 'recovered-history.png'))
                browser.close()
            # Exercise the actual new capabilities against the portable contract.
            schema_path = ROOT / 'tests/fixtures/multichannel/draft-v1.schema.json'
            snapshot_path = args.artifacts / 'snapshot.json'
            snapshot_path.write_text(json.dumps(api(origin)))
            subprocess.run([str(args.sigmf_python), '-c', "import json,sys,jsonschema;s=json.load(open(sys.argv[1]));jsonschema.validate(json.load(open(sys.argv[2])),{'$defs':s['$defs'],'$ref':'#/$defs/snapshot'})", str(schema_path), str(snapshot_path)], check=True)
            report['checks'] = ['network IQ equals archive bytes', 'two actual sample rates exported separately', 'cross-block sample-exact trims',
                                'official SigMF 1.2.6 validation and reading', 'real waveform cursor and keyboard seek', '420px Pop out/download/Dock',
                                'Chinese and light theme', 'healthy recording continues', 'range errors are explicit', 'old UID/service readable after restart', 'actual snapshot schema']
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({'ok': True, 'checks': report['checks']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
