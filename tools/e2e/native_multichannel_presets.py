#!/usr/bin/env python3
"""Server presets and ordinary three-channel editing, using an isolated fake service."""
from __future__ import annotations

import argparse
import copy
import json
import os
import urllib.error
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import assert_receivers_continue, setup
from native_multichannel_recording import finish, start, task, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready

ENDPOINT = '/api/sdr/receiver-presets'
NAME = 'AM 日常守听'


def configure(origin, uid, key, change):
    row = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    config = copy.deepcopy(row['confirmed']); change(config)
    reply = api(origin, payload={'type': 'configure', 'request_id': key, 'receiver_uid': uid,
        'expected_revision': row['revision'], 'config': config})
    assert reply['fault'] is None, reply
    return next(r for r in reply['snapshot']['receivers'] if r['receiver_uid'] == uid)


def rejected(origin, payload, status):
    try:
        api(origin, ENDPOINT, payload)
    except urllib.error.HTTPError as error:
        assert error.code == status, error.read()
    else:
        raise AssertionError(f'expected {status}')


def browser_gate(origin, snapshot, recording, artifacts):
    uid = snapshot['receivers'][0]['receiver_uid']
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1500, 'height': 1000})
        page = context.new_page(); errors = []; writes = []
        page.on('pageerror', lambda e: errors.append(str(e)))
        page.on('request', lambda r: writes.append(r.post_data_json) if r.method == 'POST' and r.url == origin + '/api/sdr/receivers' else None)
        page.goto(origin); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')
        panel = view.locator('[data-receiver-presets]')
        panel.locator('summary').click()
        expect(panel.locator('[data-preset-save]')).to_be_enabled()
        panel.locator('[data-preset-name]').fill(NAME)
        before = api(origin)['receivers']
        panel.locator('[data-preset-save]').click(delay=1100)
        expect(panel.locator('[data-preset-status]')).to_have_text('Saved')
        saved = api(origin, ENDPOINT)
        entry = saved['presets'][0]
        assert entry['name'] == NAME
        assert entry['settings'] == {key: before[0]['confirmed'][key]
            for key in ['purpose', 'baseband', 'processors', 'continuity']}
        assert_receivers_continue(before, api(origin)['receivers'])
        assert not writes
        # The server, rather than the page session, owns the saved preset.
        page.reload(); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        panel.locator('summary').click()
        expect(panel.locator('[data-preset-list] option')).to_have_count(2)
        configure(origin, uid, 'retune', lambda c: c['baseband'].update(frequency_hz=100001000))
        expect(view.locator('[name=frequency]')).to_have_value('100.001000')
        view.locator('[data-frequency-unit=Hz]').click()
        expect(view.locator('[name=frequency]')).to_have_value('100001000')
        panel.locator('[data-preset-list]').select_option(NAME)
        panel.locator('[data-preset-load]').click()
        preview = view.locator('[data-preview]')
        expect(preview).to_contain_text('100.001000 → 100.000000')
        assert not writes
        # An external stop after preview must not be undone by confirming stale settings.
        def stop_and_change_agc(c):
            c['enabled'] = False
            c['processors'][0]['agc'] = not entry['settings']['processors'][0]['agc']
        stopped = configure(origin, uid, 'concurrent-stop', stop_and_change_agc)
        expect(view.locator('[data-action=toggle-receiver]')).to_have_attribute('aria-checked', 'false')
        preview.locator('button').click()
        expect(view).to_contain_text('Channel changed; review settings again')
        assert not writes
        assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['confirmed'] == stopped['confirmed']
        panel.locator('[data-preset-load]').click()
        with page.expect_request(lambda r: r.method == 'POST' and r.url == origin + '/api/sdr/receivers'):
            preview.locator('button').click()
        until(lambda: api(origin)['receivers'][0]['confirmed']['baseband']['frequency_hz'] == 100e6)
        loaded = api(origin)['receivers'][0]
        assert loaded['receiver_uid'] == uid and not loaded['confirmed']['enabled']
        assert loaded['confirmed']['name'] == stopped['confirmed']['name']
        assert loaded['confirmed']['processors'] == entry['settings']['processors']
        assert len(writes) == 1, writes
        assert_receivers_continue(snapshot['receivers'][1:], api(origin)['receivers'][1:])
        assert task(origin, recording)['state'] == 'recording'
        assert int(task(origin, recording)['tracks'][0]['raw_bytes']) > 0
        # Save uses confirmed settings, even with an uncommitted frequency draft.
        expect(panel.locator('[data-preset-save]')).to_be_enabled()
        view.locator('[name=frequency]').fill('999999999')
        panel.locator('[data-preset-name]').fill('temporary')
        panel.locator('[data-preset-save]').focus(); page.keyboard.press('Space', delay=1300)
        expect(panel.locator('[data-preset-status]')).to_have_text('Saved')
        temporary = next(p for p in api(origin, ENDPOINT)['presets'] if p['name'] == 'temporary')
        assert temporary['settings']['baseband']['frequency_hz'] == 100e6
        page.once('dialog', lambda dialog: dialog.dismiss())
        panel.locator('[data-preset-save]').click()
        assert len(api(origin, ENDPOINT)['presets']) == 2
        page.once('dialog', lambda dialog: dialog.accept())
        panel.locator('[data-preset-delete]').click()
        expect(panel.locator('[data-preset-status]')).to_have_text('Deleted')
        # Real Pop out and a held keyboard activation across snapshot polling.
        with page.expect_popup() as opened:
            view.locator('[data-pop]').click()
        child = opened.value; child.on('pageerror', lambda e: errors.append(str(e)))
        child.set_viewport_size({'width': 420, 'height': 760})
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(link=>link.sheet!==null)")
        child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=overview-follow]')
        child_panel = compact.locator('[data-receiver-presets]')
        child_panel.locator('[data-preset-list]').select_option(NAME)
        child_panel.locator('[data-preset-name]').fill('kept draft')
        child_panel.locator('[data-preset-name]').press('Home')
        child_panel.locator('[data-preset-name]').press('Shift+ArrowRight')
        child_panel.locator('[data-preset-name]').evaluate('e=>window.presetName=e')
        child.wait_for_timeout(1300)
        assert child_panel.locator('[data-preset-name]').evaluate('e=>e===window.presetName&&e.selectionStart===0&&e.selectionEnd===1')
        page.locator('#btn-lang').click(); page.locator('#btn-theme').click()
        expect(child_panel.locator('summary')).to_have_text('频点预设')
        child_panel.locator('[data-preset-load]').focus(); child.keyboard.press('Space', delay=1300)
        expect(compact.locator('[data-preview]')).to_contain_text('100.000000')
        compact.locator('[data-frequency-unit=Hz]').click()
        expect(compact.locator('[name=frequency]')).to_have_value('100000000')
        assert compact.locator('.mc-body').evaluate('e=>e.scrollWidth-e.clientWidth') == 0
        child.screenshot(path=str(artifacts / 'presets-420-zh-light.png'))
        dock = compact.locator('[data-dock]'); dock.scroll_into_view_if_needed(); box = dock.bounding_box(); assert box
        with child.expect_event('close'):
            child.mouse.click(box['x']+box['width']/2, box['y']+box['height']/2)
        expect(panel.locator('[data-preset-name]')).to_have_value('kept draft')
        page.screenshot(path=str(artifacts / 'presets-docked.png'))
        assert len(writes) == 1 and not errors, (writes, errors)
        context.close(); browser.close()
        return {'saved': saved, 'loaded': loaded, 'receiver_writes': writes, 'page_errors': errors}


def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args(); artifacts = args.artifacts.resolve(); artifacts.mkdir(parents=True, exist_ok=True)
    storage = artifacts / 'recordings'; os.environ['WEBSA_RECORDING_DIR'] = str(storage)
    proc = None; port = free_port()
    try:
        with (artifacts / 'service.log').open('w') as log:
            proc, origin = start_service(port, log=log)
            snapshot = setup(origin); assert snapshot['capabilities']['receiver_presets']
            recording = start(origin, snapshot['receivers'][2]['receiver_uid'], 'healthy-iq')
            report = browser_gate(origin, snapshot, recording, artifacts)
            report['recording'] = finish(origin, recording)
            saved = api(origin, ENDPOINT)
            row = api(origin)['receivers'][0]
            request = {'type': 'save', 'service_id': snapshot['service_id'], 'expected_revision': saved['revision'],
                'name': 'conflict', 'receiver_uid': row['receiver_uid'], 'receiver_revision': row['revision']}
            rejected(origin, {**request, 'expected_revision': '0'}, 409)
            rejected(origin, {**request, 'receiver_revision': '0'}, 409)
            assert api(origin, ENDPOINT) == saved
            stop(proc); proc = None
            proc, origin = start_service(port, log=log)
            assert api(origin, ENDPOINT) == saved
            assert api(origin)['service_id'] != snapshot['service_id']
            rejected(origin, request, 409)
            report['restart_presets'] = saved
            report['restart_snapshot'] = api(origin)
            assert report['restart_snapshot']['recordings'][0]['state'] == 'completed'
            # No receiver identity or active-state replay from the stored preset.
            assert report['loaded']['receiver_uid'] not in [r['receiver_uid'] for r in report['restart_snapshot']['receivers']]
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2, ensure_ascii=False))
            print(json.dumps({'result': 'passed', 'presets': len(saved['presets']), 'receiver_writes': 1}))
    finally:
        stop(proc)


if __name__ == '__main__':
    main()
