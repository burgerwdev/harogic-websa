#!/usr/bin/env python3
"""Per-channel audio controls and main-panel recovery on an isolated synthetic service."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

from native_multichannel import api, config
from native_multichannel_processing import assert_receivers_continue, setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def browser_gate(origin, snapshot, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1440, 'height': 1000})
        page.add_init_script("localStorage.setItem('web-sa-sdr-audio','1');localStorage.setItem('web-sa-lang','zh')")
        errors, retirements = [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: retirements.append(json.loads(request.post_data)['receiver_uid'])
                if request.method == 'POST' and request.url.endswith('/api/sdr/receivers')
                and json.loads(request.post_data).get('type') == 'retire' else None)
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        expect(page.locator('.sdr-mode-common [data-sdr-demod=drm]')).to_be_visible()
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        expect(overview).to_be_visible()
        uid = snapshot['receivers'][0]['receiver_uid']
        overview.locator(f'[data-uid="{uid}"] [data-action=select]').click()
        name = overview.locator('[data-channel-name]')
        name.fill('AM receiver'); name.press('Enter')
        page.wait_for_function('''async uid => (await (await fetch('/api/sdr/receivers')).json())
          .receivers.find(r=>r.receiver_uid===uid)?.confirmed.name==='AM receiver' ''', arg=uid)
        expect(overview.locator('[name=nr]')).to_be_visible()
        expect(overview.locator('[name=squelch]')).to_be_visible()
        overview.locator('[name=nr]').check()
        overview.locator('[name=nr-algorithm]').select_option('wiener')
        overview.locator('[name=agc]').uncheck()
        for name, value in [('squelch', '-105.3'), ('nr-strength', '0.25')]:
            overview.locator(f'[name={name}]').evaluate(
                '(input,value)=>{input.value=value;input.dispatchEvent(new Event("input",{bubbles:true}));}', value)
        overview.locator('[data-config] [type=submit]').click()
        page.wait_for_function('''async uid => {
          const snapshot = await (await fetch('/api/sdr/receivers')).json();
          const p = snapshot.receivers.find(r=>r.receiver_uid===uid)?.confirmed.processors[0];
          return p?.nr && p.nr_algorithm==='wiener' && p.nr_strength===0.25 && p.squelch_dbm===-105.3 && !p.agc;
        }''', arg=uid)
        confirmed = api(origin)
        assert_receivers_continue(snapshot['receivers'][1:], confirmed['receivers'][1:])
        compact_layout = overview.evaluate('''root=>{
          const box = selector => {const r=root.querySelector(selector).getBoundingClientRect();
            return {x:r.x,y:r.y,width:r.width,height:r.height};};
          return {gain:box('[name=pcm-gain]'),squelch:box('[name=squelch]'),audio:box('.mc-channel-audio')};
        }''')
        assert abs(compact_layout['gain']['y'] - compact_layout['squelch']['y']) < 2, compact_layout
        assert compact_layout['gain']['width'] < compact_layout['audio']['width'] * 0.35, compact_layout
        overview.screenshot(path=str(artifacts / 'channel-audio-settings.png'))
        overview.locator('[data-action=advanced-settings]').click()
        overview.locator('[data-section=processor] summary').click()
        overview.locator('[data-bandwidth="3000"]').click()
        expect(overview.locator('[name=processor-bandwidth]')).to_have_value('3000')
        overview.locator('[data-section=iq] summary').click()
        overview.screenshot(path=str(artifacts / 'channel-advanced-settings.png'))
        overview.locator('[data-action=cancel]').click()
        overview.locator(f'[data-uid="{uid}"] .mc-row-more > summary').click()
        overview.locator(f'[data-uid="{uid}"] .mc-selection-mark[data-action=detail]').click()
        detail = page.locator(f'[data-view-id="detail-{uid}"]')
        detail.evaluate('root=>{root.style.width="420px";root.style.height="760px";}')
        expect(detail.locator('[name=pcm-gain]')).to_be_visible()
        overflow = detail.evaluate('''root=>[...root.querySelectorAll('.mc-body,[data-config],.mc-channel-audio,.mc-name-editor')]
          .filter(e=>e.scrollWidth>e.clientWidth+2).map(e=>({className:e.className,width:e.clientWidth,scroll:e.scrollWidth}))''')
        assert not overflow, overflow
        detail.screenshot(path=str(artifacts / 'channel-compact-420.png'))
        detail.locator('[data-close]').click()
        # Algorithm-specific controls keep this receiver's own draft across selection.
        overview.locator('[name=nr-algorithm]').select_option('dfn')
        expect(overview.locator('[name=nr-atten]')).to_be_visible()
        expect(overview.locator('[name=nr-strength]')).not_to_be_visible()
        overview.locator('[name=nr-atten]').evaluate(
            '(input)=>{input.value="14";input.dispatchEvent(new Event("input",{bubbles:true}));}')
        second = snapshot['receivers'][1]['receiver_uid']
        overview.locator(f'[data-uid="{second}"] [data-action=select]').click()
        expect(overview.locator('[name=nr]')).not_to_be_checked()
        overview.locator(f'[data-uid="{uid}"] [data-action=select]').click()
        expect(overview.locator('[name=nr-algorithm]')).to_have_value('dfn')
        expect(overview.locator('[name=nr-atten]')).to_have_value('14')

        def delete_all():
            if not overview.is_visible():
                page.locator('[data-action=sdr-workbench]').click()
            remaining = len(api(origin)['receivers'])
            expect(overview.locator('.mc-row')).to_have_count(remaining)
            # Burst clicks across every UID also exercise the shared admission queue.
            overview.locator('.mc-row [data-action=retire]').evaluate_all('''buttons => {
              for (const button of buttons) for (let i=0;i<20;i++) button.click();
            }''')
            expect(overview.locator('.mc-row')).to_have_count(0)
            expect(overview.locator('[data-key=session-fault]')).to_have_count(0)
            overview.locator('[data-close]').click()
            page.wait_for_function('() => window.websaAudioStatus().every(r=>!r.enabled)')

        recoveries = []
        for index, action in enumerate(['spectrum', 'listen', 'center']):
            delete_all()
            if action != 'spectrum':
                # Reusing C1 for IQ must not retain its absent audio processor after retirement.
                reply = api(origin, payload={'type': 'create', 'request_id': f'iq-{action}', 'config': config(100e6)})
                assert reply['fault'] is None, reply
                delete_all()
            old_uids = {r['receiver_uid'] for r in confirmed['receivers']}
            if action == 'spectrum':
                canvas = page.locator('#spectrum')
                box = canvas.bounding_box()
                canvas.click(position={'x': box['width'] * 0.52, 'y': box['height'] * 0.45})
            else:
                frequency = page.locator('#input-sdr-' + action)
                frequency.fill(str(100 + index * 0.01))
                frequency.press('Enter')
            page.wait_for_function('''async () => {
              const s = await (await fetch('/api/sdr/receivers')).json();
              return s.receivers.length===1 && s.receivers[0].state==='running' && s.receivers[0].confirmed.purpose==='listen';
            }''')
            receiver = api(origin)['receivers'][0]
            assert receiver['receiver_uid'] not in old_uids
            page.wait_for_function('''uid => window.websaAudioStatus().some(r=>r.channel===uid
              && r.owner==='workbench' && r.enabled && r.playedFrames>2000 && !r.error)''', arg=receiver['receiver_uid'])
            audio = page.evaluate('(uid)=>window.websaAudioStatus().find(r=>r.channel===uid)', receiver['receiver_uid'])
            assert not audio['masterMuted'] and audio['rms'] > 0, audio
            recoveries.append({'action': action, 'receiver': receiver, 'audio': audio})
        page.screenshot(path=str(artifacts / 'main-listening-restored.png'))
        assert len(retirements) == len(set(retirements)), retirements
        assert not errors, errors
        browser.close()
        return {'recoveries': recoveries, 'retirements': retirements, 'compact_layout': compact_layout, 'errors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-multichannel-audio-settings'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        try:
            report = browser_gate(origin, setup(origin), args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: independent audio controls and playback after retiring all channels (spectrum/listen/centre)')
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
