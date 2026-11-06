#!/usr/bin/env python3
"""Native-menu refresh gate on an isolated synthetic service (Chromium and Firefox).

FFT availability is injected into STATUS because synthetic capture cannot run SDK FFT.
This checks UI refresh mutations, not hardware FFT computation. No live instance is used.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def run(browser_type, artifacts):
    checks = []
    os.environ['WEBSA_RECORDING_DIR'] = str(artifacts / 'recordings')
    with (artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        browser = None
        try:
            snapshot = setup(origin)
            first, second, healthy = [r['receiver_uid'] for r in snapshot['receivers']]
            browser = browser_type.launch(headless=True)
            context = browser.new_context(viewport={'width': 1600, 'height': 1100})
            page = context.new_page()
            errors, routes, fft_requests = [], [], []
            page.on('pageerror', lambda error: errors.append(str(error)))

            def intercept(route):
                state = {'route': route, 'status': None}
                routes.append(state)
                server = route.connect_to_server()

                def received(message):
                    if isinstance(message, str):
                        payload = json.loads(message)
                        if payload.get('cmd') == 'STATUS':
                            payload.setdefault('sdr', {}).setdefault('actual', {})['display_fft'] = {
                                'available': True, 'size': state.get('fft_size', 32768), 'state': 'active',
                                'observation_ms': 16.78, 'bin_spacing_hz': 59.6, 'enbw_hz': 224.7,
                            }
                            message = json.dumps(payload)
                            state['status'] = message
                    route.send(message)

                server.on_message(received)

                def sent(message):
                    payload = json.loads(message) if isinstance(message, str) else {}
                    if payload.get('cmd') == 'SET_SDR_FFT':
                        fft_requests.append(payload['size'])
                        state['fft_size'] = payload['size']
                        # Confirm the chosen UI tier without asking the fake SDK to run it.
                        confirmed = json.loads(state['status'])
                        confirmed['sdr']['actual']['display_fft']['size'] = payload['size']
                        state['status'] = json.dumps(confirmed)
                        route.send(state['status'])
                    else:
                        server.send(message)

                route.on_message(sent)

            page.route_web_socket('**/ws?*', intercept)

            def menus(target_page, scope, surface):
                for select in scope.locator('select:visible:enabled').element_handles():
                    if not select.is_visible() or not select.is_enabled() or select.evaluate('s=>s.options.length') < 2:
                        continue
                    name = select.evaluate('s=>s.id || s.name || s.outerHTML.slice(0,100)')
                    select.evaluate('''s => {
                      const descriptor = Object.getOwnPropertyDescriptor(s.ownerDocument.defaultView.HTMLSelectElement.prototype, 'value');
                      const check = {writes:0, changes:[], options:[...s.options]};
                      Object.defineProperty(s,'value',{configurable:true,get(){return descriptor.get.call(this)},set(v){check.writes++;descriptor.set.call(this,v)}});
                      check.observer = new MutationObserver(records=>{
                        check.changes.push(...records.map(r=>({type:r.type,attribute:r.attributeName})))
                      });
                      check.observer.observe(s,{attributes:true,childList:true,subtree:true,characterData:true});
                      s._selectCheck=check;
                    }''')
                    select.evaluate("s=>s.scrollIntoView({block:'center'})")
                    select.focus()
                    select.click()
                    # Deliver actual STATUS handling while the native menu is open.
                    # Holding the pointer away from the selected entry also exercises
                    # native hover state; the assertions cover its DOM reset triggers.
                    box = select.bounding_box()
                    if box:
                        target_page.mouse.move(box['x'] + box['width'] / 2, box['y'] + box['height'] * 2)
                    for _ in range(6):
                        assert routes and routes[0]['status'], 'no intercepted STATUS'
                        routes[0]['route'].send(routes[0]['status'])
                        target_page.wait_for_timeout(200)
                    result = select.evaluate('''s=>({writes:s._selectCheck.writes,changes:s._selectCheck.changes,
                      connected:s.isConnected,focused:s.contains(s.ownerDocument.activeElement),
                      active:s.ownerDocument.activeElement?.outerHTML.slice(0,180),
                      sameOptions:s.options.length===s._selectCheck.options.length && [...s.options].every((o,i)=>o===s._selectCheck.options[i])})''')
                    if name == 'select-sdr-fft':
                        target_page.screenshot(path=str(artifacts / 'fft-menu.png'))
                    target_page.keyboard.press('Escape')
                    select.evaluate('s=>{s._selectCheck.observer.disconnect();delete s.value;delete s._selectCheck;s.blur()}')
                    assert result['writes'] == 0 and not result['changes'] and result['connected'] and result['sameOptions'] and result['focused'], (surface, name, result)
                    checks.append({'surface': surface, 'select': name, **result})

            page.goto(origin)
            wait_ui_ready(page, 'sdr')
            fft = page.locator('#select-sdr-fft')
            expect(fft).to_be_enabled()
            graph_toggle = page.locator('#graph-panel .panel-toggle')
            if not fft.is_visible():
                graph_toggle.click()
            expect(fft).to_be_visible()
            menus(page, page.locator('body'), 'main')
            assert any(check['select'] == 'select-sdr-fft' for check in checks), 'FFT must never be skipped'
            fft.click()
            page.keyboard.press('ArrowDown')
            for _ in range(6):
                routes[0]['route'].send(routes[0]['status'])
                page.wait_for_timeout(200)
            page.keyboard.press('Enter')
            expect(fft).to_have_value('65536')
            assert fft_requests == [65536], fft_requests
            page.locator('[data-action=sdr-workbench]').click()
            overview = page.locator('[data-view-id=overview-follow]')
            expect(overview).to_be_visible()
            menus(page, overview, 'workbench')
            overview.locator(f'[data-uid="{second}"] [data-action=detail]').first.click()
            detail = page.locator(f'[data-view-id="detail-{second}"]')
            with page.expect_popup() as popup:
                detail.locator('[data-pop]').click()
            child = popup.value
            child.on('pageerror', lambda error: errors.append(str(error)))
            expect(child.locator('[name=mode]')).to_be_visible()
            menus(child, child.locator('body'), 'independent window')
            with child.expect_event('close'):
                child.locator('.mc-channel-actions [data-action=retire]').click()
            expect(overview.locator(f'[data-uid="{second}"]')).to_have_count(0)
            assert all(r['receiver_uid'] != second for r in api(origin)['receivers'])
            overview.locator(f'[data-uid="{first}"] [data-action=select]').click()
            overview.locator('.mc-channel-actions [data-action=retire]').click()
            expect(overview.locator(f'[data-uid="{first}"]')).to_have_count(0)
            assert [r['receiver_uid'] for r in api(origin)['receivers']] == [healthy]
            assert not errors, errors
            report = {'browser': browser_type.name, 'menus': checks, 'fft_metadata': 'injected',
                      'fft_choice_after_refresh': fft_requests,
                      'first_click_delete': ['independent window', 'workbench'], 'page_errors': errors}
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2))
            print(f'PASS {browser_type.name}: {len(checks)} menus including enabled FFT; first-click deletion')
        finally:
            if browser:
                browser.close()
            stop(proc)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    with sync_playwright() as playwright:
        for browser_type in [playwright.chromium, playwright.firefox]:
            artifacts = args.artifacts / browser_type.name
            artifacts.mkdir(parents=True, exist_ok=True)
            run(browser_type, artifacts)


if __name__ == '__main__':
    main()
