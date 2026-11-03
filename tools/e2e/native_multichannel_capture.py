#!/usr/bin/env python3
"""Isolated synthetic gate for shared capture controls and UID edit isolation.

Creates its own fake service; never accepts an existing/hardware origin.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def until(origin, predicate):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        snapshot = api(origin)
        if predicate(snapshot):
            return snapshot
        time.sleep(0.05)
    raise AssertionError('service did not confirm the requested change')


def browser_gate(origin, artifacts):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1600, 'height': 1100}, reduced_motion='no-preference')
        errors = []
        requests = []
        def sent_frame(payload):
            if isinstance(payload, str):
                message = json.loads(payload)
                if isinstance(message, dict) and 'cmd' in message:
                    requests.append(message)
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('websocket', lambda socket: socket.on('framesent', sent_frame))
        page.on('request', lambda request: requests.append(request.post_data_json) if request.method == 'POST' and request.url.endswith('/api/config') else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        expect(overview).to_be_visible()
        before = until(origin, lambda s: all(r['stream'] for r in s['receivers']))
        first, second, third = before['receivers']
        overview.locator(f'[data-uid="{second["receiver_uid"]}"] [data-action=select]').click()
        expect(overview.locator('[name=frequency]')).to_have_value(str(int(second['confirmed']['baseband']['frequency_hz'])))
        # Main Center explicitly tunes C1; selecting C2 in Channels cannot redirect it.
        overview.locator('[data-close]').click()
        page.locator('#btn-spectrum-zoom').click()
        page.locator('#zoom-rail-horizontal').evaluate("e=>{e.value='600';e.dispatchEvent(new Event('input',{bubbles:true}))}")
        zoom = page.locator('#zoom-overview')
        bounds = zoom.bounding_box()
        page.mouse.click(bounds['x'] + bounds['width'] * .2, bounds['y'] + bounds['height'] / 2)
        page.wait_for_timeout(350)
        page.evaluate('''() => {
          window.__centerMotion = [];
          window.__sampleCenterMotion = true;
          const sample = time => {
            const hz = Number(document.getElementById('zoom-overview').getAttribute('aria-valuenow'));
            window.__centerMotion.push({time, hz});
            if (window.__sampleCenterMotion) requestAnimationFrame(sample);
          };
          requestAnimationFrame(sample);
        }''')
        requests.clear()
        page.locator('#input-sdr-center').fill('100.02')
        page.locator('#input-sdr-center').press('Enter')
        shared = until(origin, lambda s: abs(s['capture']['center_hz'] - 100_020_000) < 1
                       and s['receivers'][0]['confirmed']['baseband']['frequency_hz'] == 100_020_000)
        page.wait_for_function("Math.abs(Number(document.getElementById('zoom-overview').getAttribute('aria-valuenow')) - 100020000) < 1")
        motion = page.evaluate('() => {window.__sampleCenterMotion=false;return window.__centerMotion}')
        intermediate = {round(row['hz'], 2) for row in motion if abs(row['hz'] - 100_020_000) > 1}
        assert len(intermediate) >= 4, motion
        expected = json.loads(json.dumps(first['confirmed']))
        expected['baseband']['frequency_hz'] = 100_020_000
        for processor in expected['processors']:
            processor.setdefault('agc', True)
        assert shared['receivers'][0]['confirmed'] == expected
        assert [r['confirmed'] for r in shared['receivers'][1:]] == [r['confirmed'] for r in before['receivers'][1:]]
        assert requests and all(request['cmd'] == 'SET_SDR' and request['listen'] == 100_020_000 for request in requests), requests
        first = shared['receivers'][0]
        page.locator('[data-action=sdr-workbench]').click()
        expect(overview.locator('[data-capture-status]')).to_contain_text('100.02 MHz')
        frequency = second['confirmed']['baseband']['frequency_hz'] + 1234.25
        overview.locator('[name=frequency]').fill(str(frequency))
        overview.locator('[data-config] button[type=submit]').click()
        page.screenshot(path=str(artifacts / 'c2-review.png'))
        (artifacts / 'c2-review.json').write_text(json.dumps({
            'uid': overview.get_attribute('data-receiver-uid'), 'page_errors': errors,
            'invalid_fields': overview.locator('[data-config]').evaluate('form=>Array.from(form.querySelectorAll(":invalid")).map(e=>({name:e.name,value:e.value,error:e.validationMessage}))'),
            'preview': overview.locator('[data-preview]').inner_text(), 'snapshot': api(origin)}, indent=2) + '\n')
        expect(overview.locator('[data-preview] button')).to_be_enabled()
        overview.locator('[data-preview] button').click()
        edited = until(origin, lambda s: s['receivers'][1]['confirmed']['baseband']['frequency_hz'] == frequency)
        assert edited['receivers'][0]['confirmed'] == first['confirmed']
        assert edited['receivers'][0]['stream'] == shared['receivers'][0]['stream']
        assert edited['receivers'][2]['confirmed'] == third['confirmed']
        assert edited['receivers'][2]['stream'] == shared['receivers'][2]['stream']
        page.screenshot(path=str(artifacts / 'shared-capture-c2-isolation.png'))
        # Reuse the production observer against this owned fixture, with no POSTs.
        observation = artifacts / 'observer'
        subprocess.run([sys.executable, str(ROOT / 'tools/bench/sdr_capture_observe.py'),
                        '--origin', origin, '--expect-service-id', edited['service_id'],
                        '--seconds', '1', '--interval', '0.5', '--artifacts', str(observation)], check=True)
        after = api(origin)
        assert [r['confirmed'] for r in after['receivers']] == [r['confirmed'] for r in edited['receivers']]
        assert [r['stream'] for r in after['receivers']] == [r['stream'] for r in edited['receivers']]
        assert not errors, errors
        browser.close()
        return {'source': 'owned synthetic service; no RF or physical listening', 'before': before,
                'shared_center': shared, 'c2_edited': edited, 'observer_after': after,
                'shared_panel_commands': requests, 'center_slide': motion, 'page_errors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args(); artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=False)
    os.environ['WEBSA_RECORDING_DIR'] = str(artifacts / 'recordings')
    with (artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log)
        try:
            setup(origin)
            report = browser_gate(origin, artifacts)
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: main Center tunes C1 and preserves C2/C3; C2 edit preserves C1/C3')
        finally:
            stop(process)


if __name__ == '__main__':
    main()
