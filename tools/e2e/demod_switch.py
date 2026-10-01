#!/usr/bin/env python3
"""Demodulator switching: every mode change must work on its own.

Why this exists: switching demodulators used to need a page refresh or a Preset to take effect
(the decoder was judged stale against a params object a STATUS had replaced, so it stayed null for
the rest of the session; reported from the bench). The unit tests cover the worker's generation
counter, but only a real click sequence covers the whole path: the panel's selection, the window's
pane, the baseband pipeline, the decoder's wasm load, and the return to the mode that decodes.

The checks deliberately *clear the log* before each round trip and then wait for a **new** decode:
text left over from an earlier step would otherwise make a dead decoder look alive.

Usage:  python3 tools/e2e/demod_switch.py [--url http://127.0.0.1:8099]
Exit status is non-zero when any check fails, so it can be a CI gate.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request

sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent.parent.parent))

from playwright.sync_api import sync_playwright  # noqa: E402

CW_TEXT = 'TEST DE N0CALL'
failures: list[str] = []


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
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
    import re
    found = re.search(rf'{name}=(\d+)', iq_diag(page))
    return int(found.group(1)) if found else 0


def cw_text(page) -> str:
    return page.evaluate("document.getElementById('decode-window-cw').textContent") or ''


def wait_for_cw(page, seconds: float = 60.0) -> str:
    """The fake backend keys the same message forever; wait for it to appear again."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        text = cw_text(page)
        if CW_TEXT in text:
            return text
        time.sleep(2)
    return cw_text(page)


def cw_decodes_again(page, label: str, seconds: float = 60.0) -> None:
    js_click(page, '#btn-decode-clear')
    page.wait_for_timeout(500)
    text = wait_for_cw(page, seconds)
    check(label, CW_TEXT in text, f'text={text[:70]!r}')


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    args = parser.parse_args()

    post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am', 'ifbw': 6000, 'pitch': 700,
                    'volume': 0.8, 'squelch': -140.0, 'agc': True})

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        context = browser.new_context()
        context.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
        page = context.new_page()
        page.goto(args.url, wait_until='domcontentloaded')
        # The window's toggle only exists for a mode that decodes, so wait for the panel itself.
        page.wait_for_selector('[data-sdr-demod="cw"]', state='attached', timeout=30000)
        page.wait_for_timeout(3000)

        print('A) the mode list comes from the DSP registry, not from this test', flush=True)
        modes = page.evaluate(
            "() => [...document.querySelectorAll('[data-sdr-demod]')]"
            ".filter((b) => !b.disabled).map((b) => b.dataset.sdrDemod)")
        check('the panel offers analog and digital demodulators',
              'cw' in modes and 'ft8' in modes and len(modes) >= 4, f'modes={modes}')
        # Started on AM: the window must be unavailable, and become available in a mode that decodes.
        off = page.evaluate(
            "() => { const b = document.getElementById('btn-decode-window');"
            " return { disabled: !!b.disabled, shown: getComputedStyle(document.getElementById('decode-window')).display }; }")
        check('AM offers no decode window', off['disabled'] and off['shown'] == 'none', str(off))

        print('B) CW decodes when it is selected from the panel', flush=True)
        demod(page, 'cw')
        page.wait_for_function(
            "() => { const b = document.getElementById('btn-decode-window'); return b && !b.disabled; }",
            timeout=20000)
        check('CW offers the decode window again', True)
        # The audio chain owns the decode (ggmorse runs at its output rate), so the test enables it
        # exactly like an operator does - and every state the window shows is then real.
        if page.query_selector('#btn-sdr-audio'):
            js_click(page, '#btn-sdr-audio')
        js_click(page, '#btn-decode-window')
        page.wait_for_timeout(2000)
        check('the baseband is streaming before the first decode',
              counter(page, 'blocks') > 0, iq_diag(page)[:90])
        cw_decodes_again(page, 'CW decodes straight after being selected')

        print('C) a digital mode with a companion demodulator, and back', flush=True)
        demod(page, 'ft8')
        page.wait_for_timeout(3000)
        title = page.evaluate("document.getElementById('decode-window-title').textContent")
        check('the window follows the mode (FT8 pane)', title == 'FT8', f'title={title!r}')
        demod(page, 'cw')
        page.wait_for_timeout(3000)
        title = page.evaluate("document.getElementById('decode-window-title').textContent")
        check('the window follows the mode back (CW pane)', title == 'CW', f'title={title!r}')
        cw_decodes_again(page, 'CW decodes after FT8')

        print('D) every other demodulator, each followed by a return to CW', flush=True)
        for mode in [m for m in modes if m not in ('cw', 'ft8')]:
            before = counter(page, 'blocks')
            demod(page, mode)
            page.wait_for_timeout(4000)
            moving = counter(page, 'blocks') > before
            check(f'{mode}: the baseband pipeline keeps running', moving,
                  f'blocks {before} -> {counter(page, "blocks")} | {iq_diag(page)[:90]}')
            demod(page, 'cw')
            page.wait_for_timeout(2500)
            cw_decodes_again(page, f'CW decodes after {mode}', seconds=45)

        print('E) fast switching: clicks with no wait between them', flush=True)
        # The wasm decoder loads asynchronously; a switch during that load must not leave the mode
        # dead. This is the race the generation counter exists for, driven the way an operator does.
        for mode in ('cw', 'am', 'cw', 'ft8', 'cw'):
            demod(page, mode)
        page.wait_for_timeout(3000)
        cw_decodes_again(page, 'CW decodes after a burst of switches', seconds=60)

        print('F) the operator\'s filter is not re-picked on every switch', flush=True)
        js_click(page, '[data-sdr-ifbw="180000"]')
        page.wait_for_timeout(800)
        for mode in ('cw', 'am', 'cw', 'ft8', 'cw'):
            demod(page, mode)
            page.wait_for_timeout(600)
        picked = page.evaluate(
            "() => document.querySelector('[data-sdr-ifbw].active')?.dataset.sdrIfbw")
        check('a chosen filter survives a round of mode switches', picked == '180000',
              f'ifbw={picked}')

        print('G) the window can be closed and reopened across switches', flush=True)
        js_click(page, '#btn-decode-window')          # close
        demod(page, 'am')
        page.wait_for_timeout(1500)
        demod(page, 'cw')
        page.wait_for_timeout(1500)
        js_click(page, '#btn-decode-window')          # reopen
        page.wait_for_timeout(1000)
        shown = page.evaluate("getComputedStyle(document.getElementById('decode-window')).display")
        cw_decodes_again(page, 'CW decodes after the window was closed and reopened')
        check('the window is visible again', shown != 'none', f'display={shown}')

        browser.close()

    print(f'\n{"FAILED (%d): %s" % (len(failures), ", ".join(failures)) if failures else "ALL PASS"}')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
