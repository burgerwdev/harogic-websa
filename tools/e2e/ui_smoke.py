#!/usr/bin/env python3
"""UI smoke test against the fake backend - the hardware-free half of the UI regression.

Why this exists (report finding A1): the full state regression needs a real SAN-90, so the
only tests that assert *user-visible* results (canvas pixels, DOM text, real clicks) could not
run in CI. That is where the blank-canvas defect and the dead-control defects slipped through.

This script drives the same UI against `WEBSA_FAKE=1 python3 -m web_sa.main` and checks the
properties that are independent of the analyzer: the spectrum is actually drawn, the controls
reach the backend, modes switch and keep drawing, the measurement tabs render, and the
waterfall yields to a measurement. Everything it asserts used to be covered only on the bench.

Usage:  python3 tools/e2e/ui_smoke.py [--url http://127.0.0.1:8099]
Exit status is non-zero when any check fails, so it can be a CI gate.
"""
from __future__ import annotations

import argparse
import json
import sys
import urllib.request

sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent.parent.parent))

from playwright.sync_api import sync_playwright  # noqa: E402


def state(url: str) -> dict:
    with urllib.request.urlopen(f'{url}/api/state', timeout=5) as response:
        return json.load(response)


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def js_click(page, selector: str) -> None:
    """Click through the DOM: the measurement panel overlays its own toggle button.

    A real user collapses/scrolls the rail first; the smoke test only cares that the bound
    handler runs, so it dispatches the click directly.
    """
    page.evaluate(f"document.querySelector('{selector}').click()")


def painted_pixels(page) -> int:
    """Non-transparent pixels on the spectrum canvas (0 = nothing drawn)."""
    return page.evaluate(
        """() => {
          const c = document.getElementById('spectrum');
          const g = c.getContext('2d');
          const d = g.getImageData(0, 0, c.width, c.height).data;
          let n = 0;
          for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++;
          return n;
        }""")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    parser.add_argument('--painted-min', type=int, default=5000)
    args = parser.parse_args()
    failures: list[str] = []

    def check(name: str, ok: bool, detail: str = '') -> None:
        print(f'  {"PASS" if ok else "FAIL"}  {name}' + (f'  <- {detail}' if detail else ''))
        if not ok:
            failures.append(name)

    with sync_playwright() as p:
        browser = p.chromium.launch()
        # The measurement panel overlays the toggle in a small viewport; use the same size as
        # the hardware regression so the layout matches what a user sees on a desktop.
        page = browser.new_page(viewport={'width': 1600, 'height': 1000})
        errors: list[str] = []
        page.on('pageerror', lambda e: errors.append(f'pageerror: {e}'))
        page.on('console', lambda m: errors.append(f'console: {m.text[:160]}') if m.type == 'error' else None)

        page.goto(f'{args.url}/', wait_until='networkidle')
        page.wait_for_timeout(3000)

        print('1) the spectrum is drawn')
        painted = painted_pixels(page)
        check('spectrum is actually drawn (renderer registered)', painted > args.painted_min,
              f'{painted} painted pixels')

        print('2) controls reach the backend')
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(1500)
        page.click('[data-action="full-span"]')
        page.wait_for_timeout(1500)
        caps = state(args.url)['caps']
        expected_center = (caps['fmin'] + caps['fmax']) / 2
        check('Full Span reaches the device', abs(state(args.url)['center'] - expected_center) < 1e3,
              f"center {state(args.url)['center']:.0f}")
        page.fill('#input-center', '433')
        page.click('#unit-center-group button:text-is("MHz")')
        page.wait_for_timeout(1500)
        check('the centre field and its unit button reach the device',
              abs(state(args.url)['center'] - 433e6) < 1e3, f"{state(args.url)['center'] / 1e6} MHz")
        check('the canvas is still drawn after re-tuning', painted_pixels(page) > args.painted_min)

        print('2a) the Auto Scale message goes to the canvas status stack')
        # It used to be the last item of the Ref parameter row, so every message moved the input,
        # the buttons and the arrows sideways (reported twice); it now lives in the canvas stack
        # with the warnings, which also has no width limit.
        row_box = ("() => { const r = document.getElementById('input-ref').getBoundingClientRect();"
                   " const s = document.getElementById('btn-ref-set').getBoundingClientRect();"
                   " const u = document.getElementById('btn-ref-up').getBoundingClientRect();"
                   " return [Math.round(r.left), Math.round(r.top), Math.round(s.left),"
                   " Math.round(u.right)]; }")
        before = page.evaluate(row_box)
        page.click('#btn-ref-auto')                  # a real decision posts a notice
        page.wait_for_timeout(1200)
        notice = page.evaluate("document.getElementById('spectrum').dataset.notice")
        after = page.evaluate(row_box)
        check('a decision posts a canvas notice', bool((notice or '').strip()), repr(notice))
        check('the notice is drawn on the canvas, not in the control row',
              page.evaluate("!document.getElementById('ref-hint')"))
        check('the Ref row does not move when the notice appears', before == after,
              f'{before} -> {after}')

        print('2b) the peak list works off the threshold slot')
        # The threshold moved from the input element into a parameter slot (one decision per
        # measurement geometry, see render/peaklist.ts). Peak finding, marker peak search and
        # the CSV export all read that slot now, so a desync would empty the table.
        js_click(page, '#btn-markers-all')        # All On: the auto threshold needs a marker
        page.wait_for_timeout(1200)
        js_click(page, '#btn-peaklist')
        page.wait_for_timeout(1500)
        rows = page.eval_on_selector_all('#peak-tbody tr', 'els => els.length')
        threshold = page.input_value('#input-peakthr')
        check('the peak list finds peaks above the threshold', rows > 0, f'{rows} rows')
        check('the auto threshold is a level, not the input default',
              float(threshold) > -150, f'threshold {threshold} dBm')
        js_click(page, '#btn-peaklist')
        js_click(page, '#btn-markers-all')        # back to All Off
        page.wait_for_timeout(600)

        print('3) mode switching keeps drawing')
        for mode, selector, clicks in (('rta', '#btn-mode-rta', 1), ('sdr', '#btn-mode-sdr', 1),
                                       ('std', '#btn-mode-rta', 2)):
            # the buttons toggle: RTA -> SDR -> RTA (from SDR) -> SWP (RTA again)
            for _ in range(clicks):
                page.click(selector)
                page.wait_for_timeout(2500)
            painted = painted_pixels(page)
            check(f'{mode} mode is applied', state(args.url)['mode'] == mode,
                  state(args.url)['mode'])
            check(f'{mode} view is drawn', painted > args.painted_min, f'{painted} pixels')

        print('4) measurement tabs render')
        js_click(page, '#btn-meas-onoff')
        page.wait_for_timeout(800)
        for tab, label in (('#tab-harm', 'harmonic'), ('#tab-pnm', 'phase noise'),
                           ('#tab-amp', 'amplitude'), ('#tab-chan', 'channel')):
            js_click(page, tab)
            page.wait_for_timeout(1800)
            painted = painted_pixels(page)
            check(f'{label} tab gives the canvas content', painted > args.painted_min,
                  f'{painted} pixels')
        js_click(page, '#tab-amp')
        page.wait_for_timeout(600)
        js_click(page, '#btn-meas-onoff')        # leave measurement mode for the next step
        page.wait_for_timeout(1200)

        print('5) a measurement owns the display (waterfall disabled)')
        page.click('[data-action="toggle-waterfall"]')
        page.wait_for_timeout(1200)
        check('waterfall can be enabled', page.evaluate(
            "document.getElementById('btn-waterfall').classList.contains('active')"))
        js_click(page, '#btn-meas-onoff')
        page.wait_for_timeout(1200)
        check('waterfall is turned off and disabled while measuring',
              not page.evaluate("document.getElementById('btn-waterfall').classList.contains('active')")
              and page.eval_on_selector('#btn-waterfall', 'e => e.disabled'))
        js_click(page, '#btn-meas-onoff')
        page.wait_for_timeout(1200)
        check('the waterfall choice comes back afterwards', page.evaluate(
            "document.getElementById('btn-waterfall').classList.contains('active')"))

        print('6) i18n, keypad and the status page')
        label_en = page.inner_text('#btn-connect') if page.query_selector('#btn-connect') else ''
        page.click('#btn-lang')
        page.wait_for_timeout(800)
        label_zh = page.inner_text('#btn-connect') if page.query_selector('#btn-connect') else ''
        check('switching the language relabels the UI', label_en != label_zh, f'{label_en} -> {label_zh}')
        page.click('#btn-lang')
        page.wait_for_timeout(500)
        js_click(page, '#btn-keypad')          # enable the pad, then focus a numeric field
        page.wait_for_timeout(400)
        page.click('#input-center')
        page.wait_for_timeout(600)
        check('the virtual keypad opens on a numeric field', page.evaluate(
            "(() => { const el = document.getElementById('keypad'); "
            "return !!el && getComputedStyle(el).display !== 'none'; })()"))
        js_click(page, '#btn-keypad')
        page.wait_for_timeout(300)

        check('no page errors', not errors, '; '.join(errors[:3]))
        browser.close()

    print()
    if failures:
        print(f'FAILED ({len(failures)}): ' + ', '.join(failures))
        return 1
    print('all UI smoke checks passed')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
