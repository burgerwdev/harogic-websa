#!/usr/bin/env python3
"""DRM demodulator switching on the live bench: select DRM, get the readout, switch
away and back — all without a page refresh.

Why this exists: the reported mode-switch regression was "after DRM, another demodulator
gives no audio until a page refresh". The cause (a trapped wasm worker kept referenced by
the page) is fixed and unit-tested, but only a real click sequence over the live bench
signal covers the whole path. The DRM readout is the pass mark: it only appears when the
receiver locked and the SDC decoded.

Run the bench first (Pluto transmitting, the app tuned to the signal, ref -40 dBm), then:

    python3 tools/e2e/drm_switch.py --url http://127.0.0.1:8080

Exit status is non-zero when any check fails. Firefox on purpose: headless Chromium
crashes in its compositor on this machine.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.request

sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent.parent.parent))

from playwright.sync_api import sync_playwright  # noqa: E402

LABEL = 'SAN90 DRM BENCH'
failures: list[str] = []


def post(url: str, cmd: dict) -> dict:
    req = urllib.request.Request(f'{url}/api/config', data=json.dumps(cmd).encode(),
                                 headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=10) as response:
        return json.load(response)


def js_click(page, selector: str) -> None:
    page.evaluate(f"document.querySelector('{selector}').click()")


def check(label: str, ok: bool, detail: str = '') -> None:
    print(f'  {"PASS" if ok else "FAIL"}  {label}' + (f'  <- {detail}' if detail else ''), flush=True)
    if not ok:
        failures.append(label)


def demod(page, mode: str) -> None:
    js_click(page, f'[data-sdr-demod="{mode}"]')


def iq_diag(page) -> str:
    return page.evaluate("document.getElementById('spectrum').dataset.sdrIq || ''") or ''


def counter(page, name: str) -> int:
    found = re.search(rf'{name}=(\d+)', iq_diag(page))
    return int(found.group(1)) if found else 0


def drm_lines(page) -> str:
    el = page.query_selector('#decode-window-drm-lines')
    return (el.text_content() if el else '') or ''


def wait_for_drm(page, seconds: float = 40.0) -> str:
    """The readout needs the lock (~2 s) plus the first pass (~2 s more); allow for a
    slow start of the stream."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        text = drm_lines(page)
        if 'DRM' in text or LABEL in text:
            return text
        time.sleep(2)
    return drm_lines(page)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    args = parser.parse_args()

    post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})

    with sync_playwright() as playwright:
        browser = playwright.firefox.launch()
        context = browser.new_context()
        context.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
        page = context.new_page()
        page.goto(args.url, wait_until='domcontentloaded')
        page.wait_for_selector('[data-sdr-demod="drm"]', state='attached', timeout=30000)
        page.wait_for_timeout(3000)
        # A reload would wipe this marker; check it at the end.
        page.evaluate("window.__drmSwitchMarker = 42")

        print('A) DRM selected from the panel locks and reports the station', flush=True)
        if page.query_selector('#btn-sdr-audio'):
            js_click(page, '#btn-sdr-audio')
        demod(page, 'drm')
        page.wait_for_timeout(1000)
        disabled = page.evaluate(
            "() => { const b = document.getElementById('btn-decode-window'); return b && b.disabled; }")
        check('DRM offers the decode window', not disabled)
        js_click(page, '#btn-decode-window')
        text = wait_for_drm(page)
        check('the DRM readout appears (lock + SDC)', bool(text),
              f'lines={text[:120]!r}')
        check('the station label matches the bench transmitter', LABEL in text,
              f'lines={text[:120]!r}')

        print('B) away to AM: the baseband keeps running, the page does not reload', flush=True)
        blocks_before = counter(page, 'blocks')
        demod(page, 'am')
        page.wait_for_timeout(4000)
        check('AM: the baseband pipeline keeps running',
              counter(page, 'blocks') > blocks_before,
              f'blocks {blocks_before} -> {counter(page, "blocks")}')
        marker = page.evaluate('window.__drmSwitchMarker')
        check('no page refresh happened', marker == 42, f'marker={marker}')

        print('C) back to DRM: it locks again without a refresh', flush=True)
        demod(page, 'drm')
        page.wait_for_timeout(1000)
        shown = page.evaluate(
            "getComputedStyle(document.getElementById('decode-window')).display")
        if shown == 'none':
            js_click(page, '#btn-decode-window')
        js_click(page, '#btn-decode-clear')
        text = wait_for_drm(page)
        check('the DRM readout returns after switching back', bool(text),
              f'lines={text[:120]!r}')
        marker = page.evaluate('window.__drmSwitchMarker')
        check('still no page refresh', marker == 42, f'marker={marker}')

        browser.close()

    print(f'\n{"FAILED (%d): %s" % (len(failures), ", ".join(failures)) if failures else "ALL PASS"}')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
