#!/usr/bin/env python3
"""Main SDR quick-add, explicit replacement, channel selection and shared hover on a synthetic source."""
from __future__ import annotations

import argparse
import json
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready


def api(origin, payload=None):
    request = urllib.request.Request(origin + '/api/sdr/receivers',
        data=None if payload is None else json.dumps(payload).encode(), headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def browser_gate(origin, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1600, 'height': 1000})
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin); wait_ui_ready(page, 'sdr')
        expect(page.locator('#waterfall')).to_be_visible()
        page.wait_for_function('Math.abs(document.querySelector(".spectrum-box").getBoundingClientRect().height-document.querySelector("#waterfall-container").getBoundingClientRect().height)<2')
        expect(page.locator('[data-action=sdr-channel-add]')).to_have_attribute('title', 'Add the current frequency and demodulation settings to an independent channel')
        page.locator('#btn-lang').click()
        add = page.locator('[data-action=sdr-channel-add]')
        expect(add).to_have_attribute('title', '将当前频点和解调设置快速加入独立信道')
        listen = page.locator('#input-sdr-listen')
        waterfall = page.locator('#waterfall')
        box = waterfall.bounding_box()
        page.mouse.click(box['x'] + box['width'] * 0.75, box['y'] + box['height'] * 0.5)
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>Math.abs(s.receivers[0].confirmed.baseband.frequency_hz-100048828.125)<500)')
        listen.fill('100'); listen.press('Enter')
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers[0].confirmed.baseband.frequency_hz===100000000)')
        page.locator('#btn-spectrum-zoom').click()
        box = waterfall.bounding_box()
        before_zoom = api(origin)['receivers'][0]['confirmed']
        page.mouse.move(box['x'] + box['width'] * 0.25, box['y'] + box['height'] * 0.5)
        page.mouse.down(); page.mouse.move(box['x'] + box['width'] * 0.65, box['y'] + box['height'] * 0.5, steps=8)
        page.screenshot(path=str(artifacts / 'waterfall-marquee.png'))
        page.mouse.up()
        expect(page.locator('#btn-zoom-reset')).to_be_enabled()
        assert api(origin)['receivers'][0]['confirmed'] == before_zoom
        page.locator('#btn-zoom-reset').click(); page.locator('#btn-spectrum-zoom').click()
        add.click()
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.length===2)')
        snapshot = api(origin)
        assert len({r['audio_signature'] for r in snapshot['receivers']}) == 1
        page.wait_for_function('window.websaAudioStatus().filter(r=>typeof r.channel==="string"&&r.enabled).length===1')
        # The shortcut does not also open the workbench.
        assert page.locator('[data-view-id=overview-follow]:visible').count() == 0
        listen.fill('100.02'); listen.press('Enter')
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.display_number===1).confirmed.baseband.frequency_hz===100020000)')
        page.wait_for_function('window.websaAudioStatus().filter(r=>typeof r.channel==="string"&&r.enabled).length===2')
        add.click()
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.length===3)')
        listen.fill('100.04'); listen.press('Enter')
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.display_number===1).confirmed.baseband.frequency_hz===100040000)')
        add.click()
        dialog = page.locator('[data-replace-channel]')
        expect(dialog).to_be_visible()
        expect(dialog.locator('[name=replace]')).to_have_count(3)
        expect(dialog.locator('[type=submit]')).to_be_disabled()
        before = api(origin)['receivers']
        second = next(r for r in before if r['display_number'] == 2)
        dialog.screenshot(path=str(artifacts / 'replace-channel.png'))
        dialog.locator(f'[value="{second["receiver_uid"]}"]').check()
        dialog.locator('[type=submit]').click()
        expect(dialog).not_to_be_visible()
        after = api(origin)['receivers']
        assert all(next(r for r in after if r['receiver_uid'] == old['receiver_uid'])['confirmed'] == old['confirmed']
            for old in before if old['display_number'] != 2)
        assert next(r for r in after if r['display_number'] == 2)['confirmed']['baseband']['frequency_hz'] == 100040000
        page.locator('[data-action=sdr-channel-next]').click()
        expect(page.locator('[data-action=sdr-channel-prev]')).to_have_attribute('title', '上一个接收信道')
        expect(page.locator('[data-action=sdr-channel-next]')).to_have_attribute('title', '下一个接收信道')
        expect(page.locator('#sdr-current-channel')).to_have_text('C2')
        listen.fill('100.05'); listen.press('Enter')
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.display_number===2).confirmed.baseband.frequency_hz===100050000)')
        assert next(r for r in api(origin)['receivers'] if r['display_number'] == 1)['confirmed']['baseband']['frequency_hz'] == 100040000
        # All channel buttons start at the same value-column edge as the tuning buttons.
        workbench = page.locator('[data-action=sdr-workbench]').bounding_box()
        tune = page.locator('[data-action=sdr-tune-down]').bounding_box()
        assert abs(workbench['x'] - tune['x']) < 1, (workbench, tune)
        if not page.locator('#waterfall').is_visible():
            page.locator('#btn-waterfall').click()
        page.locator('#btn-wf-pause').click()
        page.locator('#btn-view-freeze').evaluate('button=>button.click()')
        page.mouse.move(5, 5); page.wait_for_timeout(300)
        # Compare actual canvas pixels with history paused. A horizontal stripe avoids OSD text.
        stripe = '''id => {
          const c=document.getElementById(id),g=c.getContext('2d');
          return [...g.getImageData(0,Math.floor(c.height*0.7),c.width,1).data];
        }'''
        baseline = {id: page.evaluate(stripe, id) for id in ['spectrum', 'waterfall']}
        box = page.locator('#waterfall').bounding_box()
        page.mouse.move(box['x'] + box['width'] * 0.25, box['y'] + box['height'] * 0.5)
        page.wait_for_timeout(150)
        hovered = {id: page.evaluate(stripe, id) for id in baseline}
        assert all(hovered[id] != baseline[id] for id in baseline), 'Waterfall hover must paint both plots'
        page.screenshot(path=str(artifacts / 'shared-hover.png'))
        page.mouse.move(5, 5); page.wait_for_timeout(150)
        assert page.evaluate(stripe, 'waterfall') == baseline['waterfall'], 'Hover must erase from paused history'
        spectrum = page.locator('#spectrum').bounding_box()
        page.mouse.move(box['x'] + box['width'] * 0.35, spectrum['y'] + spectrum['height'] * 0.5)
        page.wait_for_timeout(150)
        assert page.evaluate(stripe, 'waterfall') != baseline['waterfall'], 'Spectrum hover must paint waterfall'
        page.mouse.move(5, 5); page.wait_for_timeout(150)
        assert page.evaluate(stripe, 'waterfall') == baseline['waterfall']
        page.screenshot(path=str(artifacts / 'channels-desktop.png'))
        page.set_viewport_size({'width': 480, 'height': 800})
        add.scroll_into_view_if_needed()
        workbench = page.locator('[data-action=sdr-workbench]').bounding_box()
        tune = page.locator('[data-action=sdr-tune-down]').bounding_box()
        assert abs(workbench['x'] - tune['x']) < 1
        page.screenshot(path=str(artifacts / 'channels-narrow.png'))
        page.set_viewport_size({'width': 1600, 'height': 1000})
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        toggle = overview.locator('.mc-channel-actions [role=switch]')
        expect(toggle).to_have_attribute('aria-checked', 'true')
        positions = overview.locator('.mc-channel-actions button').evaluate_all('buttons=>buttons.map(b=>b.getBoundingClientRect().y)')
        assert max(positions)-min(positions)<1
        toggle.click(); expect(toggle).to_have_attribute('aria-checked', 'false')
        toggle.click(); expect(toggle).to_have_attribute('aria-checked', 'true')
        snapshot = api(origin)
        third = next(r for r in snapshot['receivers'] if r['display_number']==3)
        third_row = overview.locator(f'[data-uid="{third["receiver_uid"]}"]')
        third_row.locator('.mc-row-more summary').click()
        expect(third_row.locator('[data-action=duplicate]')).to_be_disabled()
        third_row.locator('[data-action=retire]').click()
        expect(third_row).to_have_count(0)
        assert next(r for r in api(origin)['receivers'] if r['display_number']==2)['confirmed']['enabled']
        first_uid = next(r for r in snapshot['receivers'] if r['display_number']==1)['receiver_uid']
        first_row = overview.locator(f'[data-uid="{first_uid}"]')
        first_row.locator('.mc-row-more summary').click()
        first_row.locator('[data-action=duplicate]').click()
        page.wait_for_function('fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.length===3)')
        page.screenshot(path=str(artifacts / 'workbench-actions.png'))
        page.set_viewport_size({'width': 420, 'height': 800})
        overview.locator('.mc-channel-actions').scroll_into_view_if_needed()
        positions = overview.locator('.mc-channel-actions button').evaluate_all('buttons=>buttons.map(b=>b.getBoundingClientRect().y)')
        assert max(positions)-min(positions)<1
        page.screenshot(path=str(artifacts / 'workbench-actions-narrow.png'))
        overview.locator('[data-close]').click()
        page.set_viewport_size({'width': 1600, 'height': 1000})
        page.locator('#btn-waterfall').click()
        expect(waterfall).not_to_be_visible()
        page.locator('[data-action=preset]').click()
        expect(waterfall).to_be_visible()
        expect(page.locator('#sdr-current-channel')).to_have_text('C1')
        page.wait_for_function('Math.abs(document.querySelector(".spectrum-box").getBoundingClientRect().height-document.querySelector("#waterfall-container").getBoundingClientRect().height)<2')
        page.screenshot(path=str(artifacts / 'sdr-preset-default.png'))
        assert not errors, errors
        browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-channel-shortcuts'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    process = None
    try:
        process, origin = start_service(free_port())
        post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        post(origin, {'cmd': 'SET_SDR', 'center': 100e6, 'listen': 100e6, 'decimate': 256})
        post(origin, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True, 'freq': 100e6, 'mode': 'am', 'ifbw': 6000})
        for receiver in api(origin)['receivers']:
            if receiver['display_number'] != 1:
                reply = api(origin, {'type': 'retire', 'request_id': 'clear-' + receiver['receiver_uid'], 'receiver_uid': receiver['receiver_uid']})
                assert reply['fault'] is None, reply
        browser_gate(origin, args.artifacts)
        print(f'PASS: SDR preset split, waterfall tuning/zoom, duplicate audio, tooltips and workbench actions; screenshots: {args.artifacts}')
    finally:
        stop(process)


if __name__ == '__main__':
    main()
