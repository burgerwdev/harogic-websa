#!/usr/bin/env python3
"""Three-channel daily workflow with actual PCM/results/IQ files and real browser input.

Isolated synthetic AM/IQ and committed FT8 replay; no RF or long-run acceptance.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

from native_multichannel import api
from native_multichannel_audio import configure
from native_multichannel_audio import download as download_audio
from native_multichannel_audio import read_journal as read_audio
from native_multichannel_processing import assert_receivers_continue, setup
from native_multichannel_recording import read_archive, task, until
from native_multichannel_results import download as download_results
from native_multichannel_results import read_journal as read_results
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def browser_gate(origin, snapshot, artifacts):
    uid, second, third = [r['receiver_uid'] for r in snapshot['receivers']]
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1600, 'height': 1100})
        page = context.new_page(); errors = []; commands = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda r: commands.append(json.loads(r.post_data))
            if r.method == 'POST' and r.url == origin + '/api/sdr/receivers' else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')

        def row(target):
            return view.locator(f'.mc-row[data-uid="{target}"]')

        def select(target):
            row(target).locator('[data-action=select]').click()
            expect(view).to_have_attribute('data-receiver-uid', target)

        def review(surface=view):
            surface.locator('[data-config] button[type=submit]').click()
            expect(surface.locator('[data-preview] button')).to_be_enabled()

        def apply(surface=view):
            previous = next(r['revision'] for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
            surface.locator('[data-preview] button').focus()
            surface.locator('[data-preview] button').press('Space', delay=1100)
            until(lambda: next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['revision'] != previous)
            expect(surface.locator('[data-preview] button')).to_have_count(0)

        def save_preset(name):
            panel = view.locator('[data-receiver-presets]')
            if not panel.evaluate('e=>e.open'):
                panel.locator('summary').click()
            panel.locator('[data-preset-name]').fill(name)
            panel.locator('[data-preset-save]').click()
            expect(panel.locator('[data-preset-status]')).to_have_text('Saved')

        # All three native workers are occupied by different channels and track kinds.
        for target, kind in [(uid, 'audio'), (second, 'results'), (third, 'iq')]:
            row(target).locator(f'[data-action=rec-{kind}]').click()
        until(lambda: len([t for t in api(origin)['recordings'] if t['state'] == 'recording']) == 3)
        tasks = {t['tracks'][0]['kind']: t['task_id'] for t in api(origin)['recordings']}
        row(uid).locator('[data-action=listen]').click()
        row(second).locator('[data-action=listen]').click()
        page.wait_for_function('uids=>uids.every(uid=>window.websaAudioStatus().some(r=>r.channel===uid&&r.playedFrames>1000&&!r.error))', arg=[uid, second])
        row(second).locator('[data-action=results]').click()
        results = page.locator(f'[data-view-id="results-{second}"]')
        expect(results.locator('.mc-message-table tbody tr')).not_to_have_count(0, timeout=35000)
        results.locator('[data-close]').click()
        save_preset('Daily AM')
        select(second); save_preset('Daily FT8'); select(uid)
        before = api(origin)
        initial_tasks = {kind: task(origin, task_id) for kind, task_id in tasks.items()}
        expect(view.locator('.mc-channel-actions [data-action=duplicate]')).to_be_disabled()

        # First use of a mode uses defaults, not another mode's audio settings.
        view.locator('[name=mode]').select_option('usb')
        expect(view.locator('[name=pcm-gain]')).to_have_value('0.5')
        expect(view.locator('[name=agc]')).to_be_checked()
        view.locator('[data-section=processor] summary').click()
        view.locator('[name=processor-bandwidth]').fill('2700')
        view.locator('[name=pcm-gain]').fill('0.65')
        review(); apply()
        usb = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
        # Same-tab reload retains previously confirmed AM parameters, without applying them.
        page.reload(); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        view.locator('[name=mode]').select_option('am')
        expect(view.locator('[name=processor-bandwidth]')).to_have_value('8500')
        expect(view.locator('[name=pcm-gain]')).to_have_value('0.3')
        expect(view.locator('[name=agc]')).not_to_be_checked()
        expect(view.locator('[name=squelch]')).to_have_value('-90')
        count = len(commands)
        # Editing after preview removes its old Apply, including select and checkbox edits.
        review(); view.locator('[name=frequency]').fill('100001000')
        expect(view.locator('[data-preview] button')).to_have_count(0)
        review(); view.locator('[name=mode]').select_option('usb')
        expect(view.locator('[data-preview] button')).to_have_count(0)
        expect(view.locator('[name=processor-bandwidth]')).to_have_value('2700')
        # Unconfirmed changes never replace confirmed mode memory.
        view.locator('[data-section=processor] summary').click()
        view.locator('[name=processor-bandwidth]').fill('2900')
        view.locator('[name=mode]').select_option('am'); view.locator('[name=mode]').select_option('usb')
        expect(view.locator('[name=processor-bandwidth]')).to_have_value('2700')
        assert len(commands) == count
        view.locator('[name=mode]').select_option('am'); review()
        view.locator('[name=agc]').check()
        expect(view.locator('[data-preview] button')).to_have_count(0)
        view.locator('[name=agc]').uncheck(); review()
        # Pop out/Dock retains the original form, its draft, preview and selection.
        with page.expect_popup() as opened:
            view.locator('[data-pop]').click()
        child = opened.value; child.on('pageerror', lambda error: errors.append(str(error)))
        child.set_viewport_size({'width': 420, 'height': 760})
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(link=>link.sheet!==null)")
        child.evaluate('document.fonts.ready')
        compact = child.locator('[data-view-id=overview-follow]')
        compact.locator('[name=name]').fill('Daily AM edited')
        expect(compact.locator('[data-preview] button')).to_have_count(0)
        compact.locator('[name=name]').press('Home'); compact.locator('[name=name]').press('Shift+ArrowRight')
        compact.locator('[name=name]').evaluate('e=>window.keptName=e')
        child.wait_for_timeout(1200)
        assert compact.locator('[name=name]').evaluate('e=>e===window.keptName&&e.selectionStart===0&&e.selectionEnd===1')
        review(compact)
        page.locator('#btn-lang').click(); page.locator('#btn-theme').click()
        expect(child.locator('html')).to_have_attribute('lang', 'zh-CN')
        expect(child.locator('html')).to_have_attribute('data-theme', 'light')
        assert compact.locator('.mc-body').evaluate('e=>e.scrollWidth-e.clientWidth') == 0
        child.screenshot(path=str(artifacts / 'workflow-420-zh-light.png'))
        dock = compact.locator('[data-dock]'); dock.scroll_into_view_if_needed(); box = dock.bounding_box(); assert box
        with child.expect_event('close'):
            child.mouse.click(box['x'] + box['width']/2, box['y'] + box['height']/2)
        expect(view.locator('[name=name]')).to_have_value('Daily AM edited')
        page.locator('#btn-lang').click(); page.locator('#btn-theme').click()
        apply()
        am = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
        assert am['confirmed']['name'] == 'Daily AM edited'
        assert am['confirmed']['baseband']['frequency_hz'] == 100001000
        assert am['confirmed']['processors'][0]['bandwidth_hz'] == 8500
        assert am['confirmed']['processors'][0]['agc'] is False
        # Presets replace the draft explicitly; mode memory only follows their eventual confirmation.
        panel = view.locator('[data-receiver-presets]')
        if not panel.evaluate('e=>e.open'):
            panel.locator('summary').click()
        panel.locator('[data-preset-list]').select_option('Daily AM'); panel.locator('[data-preset-load]').click()
        expect(view.locator('[name=frequency]')).to_have_value('100000000')
        apply()
        restored = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
        assert restored['confirmed']['name'] == 'Daily AM edited'
        assert_receivers_continue(before['receivers'][1:], api(origin)['receivers'][1:])
        for kind in ['iq', 'results']:
            current = task(origin, tasks[kind]); old = initial_tasks[kind]
            assert current['state'] == 'recording' and len(current['tracks'][0]['segments']) == 1
            assert int(current['tracks'][0]['raw_bytes']) > int(old['tracks'][0]['raw_bytes'])
        # Closing every view then the whole page leaves all backend recordings active.
        closed_audio_bytes = int(task(origin, tasks['audio'])['tracks'][0]['raw_bytes'])
        view.locator('[data-close]').click(); page.close()
        until(lambda: all(task(origin, task_id)['state'] == 'recording' for task_id in tasks.values()))
        until(lambda: int(task(origin, tasks['audio'])['tracks'][0]['raw_bytes']) > closed_audio_bytes)
        page = context.new_page(); page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin); wait_ui_ready(page, 'sdr'); page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')
        expect(view).to_have_attribute('data-receiver-uid', uid)
        # Reception stop ends only that channel's audio task; restart cannot restart recording.
        view.locator('[data-action=toggle-receiver]').click()
        until(lambda: task(origin, tasks['audio'])['state'] == 'completed')
        expect(view.locator('[data-action=toggle-receiver]')).to_have_attribute('aria-checked', 'false')
        assert task(origin, tasks['iq'])['state'] == 'recording' and task(origin, tasks['results'])['state'] == 'recording'
        view.locator('[data-action=toggle-receiver]').click()
        until(lambda: api(origin)['receivers'][0]['state'] == 'running')
        expect(view.locator('[data-action=toggle-receiver]')).to_have_attribute('aria-checked', 'true')
        assert task(origin, tasks['audio'])['state'] == 'completed'
        view.locator('[data-action=toggle-receiver]').click()
        until(lambda: api(origin)['receivers'][0]['state'] == 'stopped')
        for target, kind in [(second, 'results'), (third, 'iq')]:
            row(target).locator(f'[data-action=rec-{kind}]').click()
            until(lambda kind=kind: task(origin, tasks[kind])['state'] == 'completed')
        finished = {kind: task(origin, task_id) for kind, task_id in tasks.items()}
        # Free one definition, duplicate the stopped channel and reuse another preset.
        select(third)
        view.locator('.mc-channel-actions [data-action=retire]').click()
        until(lambda: len(api(origin)['receivers']) == 2)
        select(uid)
        view.locator('.mc-channel-actions [data-action=duplicate]').click()
        added = until(lambda: next((r for r in api(origin)['receivers'] if r['receiver_uid'] not in [uid, second]), None))
        new_uid = added['receiver_uid']; expect(view).to_have_attribute('data-receiver-uid', new_uid)
        assert added['confirmed']['enabled'] is False and added['confirmed']['processors'] == restored['confirmed']['processors']
        view.locator('[name=mode]').select_option('usb')
        default_usb = next(m['defaultBandwidthHz'] for m in snapshot['capabilities']['mode_descriptors'] if m['id'] == 'usb')
        expect(view.locator('[name=processor-bandwidth]')).to_have_value(f'{default_usb:g}')
        panel = view.locator('[data-receiver-presets]')
        if not panel.evaluate('e=>e.open'):
            panel.locator('summary').click()
        panel.locator('[data-preset-list]').select_option('Daily FT8'); panel.locator('[data-preset-load]').click()
        view.locator('[data-preview] button').click()
        until(lambda: next(r for r in api(origin)['receivers'] if r['receiver_uid'] == new_uid)['confirmed']['purpose'] == 'digital')
        new = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == new_uid)
        assert new['confirmed']['name'] == added['confirmed']['name'] and not new['confirmed']['enabled']
        presets = api(origin, '/api/sdr/receiver-presets')['presets']
        assert new['confirmed']['processors'] == next(p['settings']['processors'] for p in presets if p['name'] == 'Daily FT8')
        assert all(t['state'] == 'completed' for t in api(origin)['recordings'])
        page.screenshot(path=str(artifacts / 'workflow-final.png'))
        assert not errors, errors
        context.close(); browser.close()
        return {'source': 'isolated synthetic AM/IQ and committed FT8 replay; no RF hardware', 'tasks': finished,
            'usb': usb, 'restored': restored, 'copy': new, 'commands_before_page_close': commands, 'page_errors': errors}


def verify_files(origin, report, storage, artifacts):
    tasks = report['tasks']; audio = tasks['audio']; results = tasks['results']; iq = tasks['iq']
    for t in tasks.values():
        assert t['accepted_bytes'] == t['tracks'][0]['raw_bytes'] and int(t['accepted_bytes']) > 0
        saved = json.loads((storage / t['task_id'] / 'task.json').read_text())
        assert saved['state'] == 'completed' and saved['receiver_uid'] == t['receiver_uid']
    manifest, frames, waves = download_audio(origin, audio['task_id'], artifacts / 'audio.tar')
    assert manifest['task'] == audio
    rows, _ = read_audio(storage / audio['task_id'])
    assert len(rows) == len(frames) and waves
    assert all(row['receiver_uid'] == audio['receiver_uid'] for row, _ in rows)
    pcm = {segment: bytearray() for segment in waves}
    for (source, frame), row in zip(rows, frames, strict=True):
        assert source == row['source'] and frame[:96] == bytes.fromhex(row['original_aud2_header_hex'])
        if len(frame) > 96:
            pcm[row['segment_id']].extend(frame[96:])
    assert {segment: bytes(data) for segment, data in pcm.items()} == waves
    manifest, events = download_results(origin, results['task_id'], artifacts / 'results.tar')
    rows, _, _ = read_results(storage / results['task_id'])
    assert manifest['task'] == results
    assert rows == events and any(e['envelope']['event']['type'] == 'ft8' for e in events)
    assert all(row['envelope']['receiver_uid'] == results['receiver_uid'] for row in rows)
    samples = 0
    for volume in iq['tracks'][0]['volumes']:
        blocks, complete = read_archive(storage / iq['task_id'] / volume['file']); assert complete
        for index, _ in blocks:
            assert index['meta']['clock']['receiver_uid'] == iq['receiver_uid']
            samples += index['meta']['clock']['samples']
    assert samples == sum(int(s['samples']) for s in iq['tracks'][0]['segments']) and samples > 0
    return {'audio_frames': len(frames), 'wav_segments': len(waves), 'result_events': len(events), 'iq_samples': str(samples),
        'accepted_equals_persisted': True, 'independent_crc_zstd_readers': True}


def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args(); artifacts = args.artifacts.resolve(); artifacts.mkdir(parents=True, exist_ok=True)
    storage = artifacts / 'recordings'; os.environ['WEBSA_RECORDING_DIR'] = str(storage)
    with (artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        try:
            snapshot = setup(origin)
            configure(origin, snapshot['receivers'][0]['receiver_uid'], 'custom-am', lambda c: c['processors'][0].update(
                bandwidth_hz=8500, pcm_gain=0.3, agc=False, squelch_dbm=-90))
            report = browser_gate(origin, api(origin), artifacts)
            report['files'] = verify_files(origin, report, storage, artifacts)
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: three-channel daily workflow, mode memory, explicit preview and actual saved PCM/results/IQ')
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
