#!/usr/bin/env python3
"""Native browser clipboard, shared EQ, pan and master mute on real rendered PCM."""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path

from native_multichannel import api, config
from native_multichannel_processing import assert_receivers_continue
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready


def setup(origin):
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(origin, {'cmd': 'SET_SDR', 'center': 100e6, 'listen': 100e6})
    for row in api(origin)['receivers']:
        if row['display_number'] == 1:
            stopped = copy.deepcopy(row['confirmed'])
            stopped['enabled'] = False
            reply = api(origin, payload={'type': 'configure', 'request_id': 'stop-current',
                        'receiver_uid': row['receiver_uid'], 'expected_revision': row['revision'], 'config': stopped})
        else:
            reply = api(origin, payload={'type': 'retire', 'request_id': 'clear-' + row['receiver_uid'],
                        'receiver_uid': row['receiver_uid']})
        assert reply['fault'] is None, reply['fault']
    for index in range(3):
        candidate = config(100e6 + index * 10000)
        candidate.update(name=f'Acceptance {index}', purpose='listen')
        candidate['baseband'].update(passband_hz=16000, minimum_rate_hz=48000)
        candidate['processors'] = [{'id': 'primary', 'kind': 'demodulator', 'mode': 'am',
                                    'minimum_rate_hz': 0, 'bandwidth_hz': 6000,
                                    'pcm_gain': 0.5, 'agc': False, 'squelch_dbm': -160}]
        reply = api(origin, payload={'type': 'create', 'request_id': f'create-{index}', 'config': candidate})
        assert reply['fault'] is None, reply['fault']
    return api(origin)


def gate(origin, snapshot, artifacts):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        context = browser.new_context(permissions=['clipboard-read', 'clipboard-write'],
                                      viewport={'width': 1500, 'height': 1100})
        page = context.new_page()
        errors, writes = [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: writes.append(request.url) if request.method == 'POST' else None)
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')
        saved = [row for row in snapshot['receivers'] if row['display_number'] != 1]
        uid = saved[0]['receiver_uid']
        first = view.locator(f'.mc-row[data-uid="{uid}"]')
        first.locator('[data-action=select]').click()
        diagnostics = view.locator('[data-section=live-diagnostics]')
        diagnostics.locator('summary').click()
        diagnostics.locator('[data-action=copy-raw]').click()
        copied = page.evaluate('async()=>JSON.parse(await navigator.clipboard.readText())')
        assert copied['stream'] == saved[0]['stream']
        assert 'clock' in copied and 'reception' in copied and 'fault' in copied
        view.locator('[data-action=tab-output]').click()
        stream = view.locator('[data-section=clock]')
        stream.locator('summary').click()
        stream.locator('[data-action=copy-stream]').click()
        copied_stream = page.evaluate('async()=>JSON.parse(await navigator.clipboard.readText())')
        assert copied_stream['stream'] == saved[0]['stream'] and 'clock' in copied_stream
        preview = view.locator('[data-section=iq-preview]')
        preview.locator('summary').click()
        preview.locator('[data-action=copy-iq-preview]').click()
        assert isinstance(page.evaluate('async()=>JSON.parse(await navigator.clipboard.readText())'), list)
        processing = view.locator('[data-processing-runtime] details')
        processing.locator('summary').click()
        processing.locator('[data-action=copy-raw]').click()
        runtime = page.evaluate('async()=>JSON.parse(await navigator.clipboard.readText())')
        assert 'state' in runtime and 'processing_generation' in runtime
        view.locator('[data-action=tab-receive]').click()
        if first.locator('[data-action=listen]').get_attribute('aria-pressed') != 'true':
            first.locator('[data-action=listen]').click()
        if view.locator('[data-action=master-mute]').get_attribute('aria-pressed') == 'true':
            view.locator('[data-action=master-mute]').click()
        page.wait_for_function('(uid)=>window.websaAudioStatus().some(r=>r.channel===uid&&r.playedFrames>4000&&!r.error)', arg=uid)
        page.locator('#btn-sdr-eq').click()
        eq = page.locator('[data-view-id=speaker-eq]')
        eq.locator('[data-eq-flat]').click()
        expect(eq.locator('[data-eq-trim]')).to_have_value('0')
        def levels():
            page.evaluate('window.websaNativeMixLevels()')
            page.wait_for_timeout(350)
            return page.evaluate('window.websaNativeMixLevels()')
        before, count = api(origin), len(writes)
        flat = levels()
        assert min(flat.values()) > 0.001, flat
        trim = eq.locator('[data-eq-trim]')
        trim.evaluate("e=>{e.value='-6';e.dispatchEvent(new Event('input',{bubbles:true}));}")
        reduced = levels()
        assert 0.42 < reduced['left'] / flat['left'] < 0.58, (flat, reduced)
        view.locator('[data-pan]').press('End')
        right = levels()
        assert right['left'] < 1e-5 and right['right'] > 0.001, right
        view.locator('[data-action=master-mute]').click()
        muted = levels()
        assert max(muted.values()) < 1e-5, muted
        # The workbench now owns foreground focus; exercise keyboard EQ access.
        eq.locator('[data-eq-bypass]').press('Space')
        assert max(levels().values()) < 1e-5
        view.locator('[data-action=master-mute]').click()
        resumed = levels()
        assert resumed['left'] < 1e-5 and resumed['right'] > right['right'] * 1.7, (right, resumed)
        assert_receivers_continue(before['receivers'], api(origin)['receivers'])
        assert len(writes) == count, writes[count:]
        stable = next(row for row in api(origin)['receivers'] if row['receiver_uid'] == uid)
        changing = saved[2]['receiver_uid']
        switches, replacements = 0, 0
        for index in range(16):
            row = next(row for row in api(origin)['receivers'] if row['receiver_uid'] == changing)
            candidate = copy.deepcopy(row['confirmed'])
            mode = ['am', 'nfm', 'usb', 'lsb'][index % 4]
            candidate['processors'][0].update(mode=mode, bandwidth_hz=12000 if mode == 'nfm' else 6000 if mode == 'am' else 2400)
            reply = api(origin, payload={'type': 'configure', 'request_id': f'switch-{index}',
                        'receiver_uid': changing, 'expected_revision': row['revision'], 'config': candidate})
            assert reply['fault'] is None, reply['fault']
            switches += 1
            page.wait_for_function("""async uid=>{
                const row=(await (await fetch('/api/sdr/receivers')).json()).receivers.find(r=>r.receiver_uid===uid);
                return row?.processing_state==='active'&&!row.fault&&!row.processing_fault;
            }""", arg=changing)
            if index % 4 == 3:
                reply = api(origin, payload={'type': 'retire', 'request_id': f'delete-{index}', 'receiver_uid': changing})
                assert reply['fault'] is None, reply['fault']
                reply = api(origin, payload={'type': 'create', 'request_id': f'replace-{index}', 'config': candidate})
                assert reply['fault'] is None, reply['fault']
                replacement = next(item for item in reply['snapshot']['receivers'] if item['display_number'] == row['display_number'])
                assert replacement['receiver_uid'] != changing
                changing = replacement['receiver_uid']
                replacements += 1
        after = next(row for row in api(origin)['receivers'] if row['receiver_uid'] == uid)
        assert_receivers_continue([stable], [after])
        assert not errors, errors
        page.screenshot(path=str(artifacts / 'clipboard-eq.png'))
        browser.close()
        return {'source': 'native synthetic IQ -> Receiver -> AudioWorklet PCM; no RF',
                'clipboard_diagnostics': copied, 'clipboard_processing': runtime, 'clipboard_stream': copied_stream,
                'flat': flat, 'minus_6_db': reduced, 'pan_right': right,
                'master_muted': muted, 'bypassed_and_resumed': resumed,
                'local_control_configuration_writes': 0, 'mode_switches': switches,
                'retire_and_replace_cycles': replacements, 'errors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-copy-eq'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    with (args.artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log)
        try:
            report = gate(origin, setup(origin), args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: native diagnostic clipboard, EQ PCM gain, pan, master mute and bypass')
        finally:
            stop(process)


if __name__ == '__main__':
    main()
