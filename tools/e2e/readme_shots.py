#!/usr/bin/env python3
"""Capture paired English/Chinese feature screenshots from a prepared live service.

Build the current frontend and service first. Prepare tuning, saved receivers,
scan results or recordings on the target instance before capturing. This tool
changes the displayed mode but does not create receivers, recordings or RF signals.
Decoder captures require a running FT8 receiver with decoded messages;
the committed fixture may be replayed on a separate --fake instance.

  python tools/e2e/readme_shots.py --only spectrum,measurements,waterfall
  python tools/e2e/readme_shots.py --only channels,scan,recordings
  python tools/e2e/readme_shots.py --url http://127.0.0.1:8099 --only decoder

Output defaults to /tmp. Review before copying the curated images to screenshots/.
"""
from __future__ import annotations

import argparse
import json
import urllib.request
from contextlib import closing
from pathlib import Path

from browser_preferences import toggle_theme
from playwright.sync_api import expect, sync_playwright
from viewport_baseline import wait_ui_ready

VIEWS = {
    'spectrum': ('std', 'main'),
    'measurements': ('std', 'measure_amplitude'),
    'waterfall': ('rta', 'waterfall_rta'),
    'channels': ('sdr', 'sdr_channels'),
    'scan': ('sdr', 'sdr_scan'),
    'recordings': ('sdr', 'sdr_recordings'),
    'decoder': ('sdr', 'sdr_ft8'),
}


def post(origin: str, payload: dict) -> dict:
    request = urllib.request.Request(origin + '/api/config', json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def toggle(page, selector: str, on: bool, *, active: bool = False) -> None:
    control = page.locator(selector)
    state = (control.evaluate("el => el.classList.contains('active')") if active
             else control.inner_text().strip().casefold() in {'on', '开'})
    if state != on:
        control.click()


def prepare(page, view: str, scan_type: str) -> None:
    if view == 'measurements':
        toggle(page, '#btn-markers-all', True, active=True)
        toggle(page, '#btn-meas-onoff', True)
        toggle(page, '#btn-peaklist', True)
        page.locator('[data-meas-tab=amp]').click()
    elif view == 'waterfall':
        toggle(page, '[data-action=toggle-waterfall]', True, active=True)
        page.wait_for_timeout(12000)
    elif view == 'channels':
        page.locator('[data-action=sdr-overview]').click()
        expect(page.locator('.mc-overview-card').first).to_be_visible()
        page.locator('[data-overview-layout]').select_option('2')
        cards = page.locator('.mc-overview-card')
        for index in range(min(3, cards.count())):
            listen = cards.nth(index).locator('[data-action=listen]')
            if listen.count() and listen.is_enabled() and listen.get_attribute('aria-pressed') != 'true':
                listen.click()
        if cards.count() >= 3:
            cards.nth(1).locator('[data-card-pan]').click()
            cards.nth(2).locator('[data-card-pan]').click(click_count=2, delay=150)
    elif view == 'scan':
        page.locator('#sdr-scan-toggle').click()
        expect(page.locator('#sdr-scan-window tr[data-signal]').first).to_be_visible()
        page.locator('[data-scan-type]').select_option(scan_type)
        expect(page.locator('#sdr-scan-window tr[data-signal]').first).to_be_visible()
    elif view == 'recordings':
        page.locator('[data-action=sdr-workbench]').click()
        page.locator('[data-view-id=overview-follow] [data-action=recording-files]').click()
        page.locator('[data-view-id=overview-follow] [data-close]').click()
        expect(page.locator('[data-view-id=recording-library]')).to_be_visible()
        expect(page.locator('.mc-file').first).to_be_visible()
    elif view == 'decoder':
        page.locator('[data-action=sdr-workbench]').click()
        workbench = page.locator('[data-view-id=overview-follow]')
        row = workbench.locator('.mc-row', has_text='FT8').first
        row.locator('.mc-row-more > summary').click()
        row.locator('[data-action=results]').click()
        workbench.locator('[data-close]').click()
        expect(page.locator('.mc-message-table tbody tr').first).to_be_visible(timeout=45000)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--out', type=Path, default=Path('/tmp/websa-readme-shots'))
    parser.add_argument('--chromium', help='optional system Chromium executable')
    parser.add_argument('--scan-type', default='all', choices=['all', 'am', 'fm', 'digital', 'carrier', 'unknown'])
    parser.add_argument('--only', required=True, help='comma-separated: ' + ','.join(VIEWS))
    args = parser.parse_args()
    views = args.only.split(',')
    if set(views) - VIEWS.keys():
        parser.error('unknown view')
    args.out.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as playwright, closing(playwright.chromium.launch(executable_path=args.chromium)) as browser:
        for view in views:
            mode, name = VIEWS[view]
            post(args.url, {'cmd': 'SET_MODE', 'mode': mode})
            for lang in ['en', 'zh']:
                with closing(browser.new_context(viewport={'width': 1600, 'height': 1000})) as context:
                    context.add_init_script(f"""localStorage.setItem('web-sa-lang', '{lang}');
                        localStorage.setItem('web-sa-theme', 'dark');
                        localStorage.setItem('web-sa-ui-scale', '1');""")
                    page = context.new_page()
                    errors: list[str] = []
                    page.on('pageerror', lambda error, errors=errors: errors.append(str(error)))
                    page.goto(args.url, wait_until='domcontentloaded')
                    wait_ui_ready(page, mode)
                    page.evaluate('() => document.fonts.ready')
                    prepare(page, view, args.scan_type)
                    page.wait_for_timeout(1800)
                    suffix = '_zh' if lang == 'zh' else ''
                    target = ('main_zh' if lang == 'zh' else 'main_dark') if view == 'spectrum' else name + suffix
                    page.mouse.move(1580, 980)
                    page.screenshot(path=str(args.out / f'{target}.png'))
                    if view == 'spectrum':
                        toggle_theme(page)
                        page.wait_for_timeout(600)
                        page.screenshot(path=str(args.out / f'main_light{suffix}.png'))
                    if errors:
                        raise RuntimeError(errors)
                    print(target, flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
