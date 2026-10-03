#!/usr/bin/env python3
"""DRM mode UI: the panel offers it, and STATUS's sdr.drm renders as a readout.

Runs against the fake backend (WEBSA_FAKE=1), which publishes the same DRM metadata the real
Dream decoder does, so this covers the browser wiring without PulseAudio or the Dream binary.

Usage:  python3 tools/e2e/drm_mode.py [--url http://127.0.0.1:8180] [--shot PATH]
Exit status is non-zero when any check fails.
"""
from __future__ import annotations

import argparse
import json
import sys
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright  # noqa: E402

REPO = Path(__file__).resolve().parent.parent.parent
failures: list[str] = []


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def check(label: str, ok: bool, detail: str = '') -> None:
    print(f'  {"PASS" if ok else "FAIL"}  {label}' + (f'  <- {detail}' if detail else ''), flush=True)
    if not ok:
        failures.append(label)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8180')
    parser.add_argument('--shot', default=str(REPO / 'screenshots' / 'drm-dream' / 'drm-mode.png'))
    args = parser.parse_args()

    post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am', 'ifbw': 6000})

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        context = browser.new_context(viewport={'width': 1600, 'height': 1000})
        context.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
        page = context.new_page()
        page.goto(args.url, wait_until='domcontentloaded')
        page.wait_for_selector('[data-sdr-demod="drm"]', state='attached', timeout=30000)
        page.wait_for_timeout(2000)

        print('A) the demod group offers DRM and it is enabled', flush=True)
        modes = page.evaluate(
            "() => [...document.querySelectorAll('[data-sdr-demod]')]"
            ".map((b) => ({ id: b.dataset.sdrDemod, disabled: b.disabled }))")
        drm = next((m for m in modes if m['id'] == 'drm'), None)
        check('DRM is in the demod group', drm is not None, f'modes={[m["id"] for m in modes]}')
        check('DRM is enabled', bool(drm) and not drm['disabled'])

        print('B) selecting DRM shows the decoded metadata readout', flush=True)
        page.evaluate("document.querySelector('[data-sdr-demod=\"drm\"]').click()")
        page.wait_for_function(
            "() => { const el = document.getElementById('cur-drm-status');"
            " return el && el.textContent.includes('SAN90 DRM TEST'); }", timeout=20000)
        readout = page.evaluate("document.getElementById('cur-drm-status').textContent")
        row_shown = page.evaluate(
            "() => getComputedStyle(document.getElementById('drm-status-row')).display !== 'none'")
        check('the readout names the station', 'SAN90 DRM TEST' in readout, readout)
        check('the readout shows the robustness mode', 'Mode B' in readout, readout)
        check('the readout shows the bitrate', 'kbps' in readout, readout)
        check('the readout shows sync', 'SYNC' in readout, readout)
        check('the DRM row is visible', row_shown)

        shot = Path(args.shot)
        shot.parent.mkdir(parents=True, exist_ok=True)
        page.screenshot(path=str(shot), full_page=False)
        print(f'  screenshot: {shot}', flush=True)
        browser.close()

    print(f'\n{"ALL CHECKS PASSED" if not failures else "FAILURES: " + ", ".join(failures)}')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
