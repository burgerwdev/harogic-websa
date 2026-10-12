"""UI e2e for the SDR channel rows (ux-3/ux-4): listen switching, PEP meter, FT8 source.

Drives the real page against the fake backend: frequency commit, row-click listen
switch, keyboard 1/2/3, PEP/S readouts and the FT8 background socket. The backend-side
semantics (config_version invariance, IQAX routing) are covered by tests/test_sdr_aux.py.

Usage:  python3 tools/e2e/sdr_channels.py --url http://127.0.0.1:8099
"""
from __future__ import annotations

import argparse
import sys

from playwright.sync_api import sync_playwright


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    args = parser.parse_args()
    failures: list[str] = []

    def check(name: str, ok: bool, detail: str = '') -> None:
        print(f'  {"PASS" if ok else "FAIL"}  {name}' + (f'  <- {detail}' if detail else ''))
        if not ok:
            failures.append(name)

    with sync_playwright() as p:
        browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
        page = browser.new_page(viewport={'width': 1440, 'height': 900})
        page.goto(f'{args.url}/', wait_until='domcontentloaded')
        page.wait_for_timeout(2500)
        page.click('#btn-mode-sdr')
        page.wait_for_timeout(2000)

        # Frequency commit on ch2 (Enter) re-enables/tunes the channel.
        page.fill('#chan-freq-2', '88.0000')
        page.press('#chan-freq-2', 'Enter')
        page.wait_for_timeout(800)
        freq2 = page.input_value('#chan-freq-2')
        check('ch2 frequency committed', freq2.startswith('88.0'), freq2)

        # Row click (on the tag, not the input) switches the listen channel.
        page.click('#chan-row-2 .chan-tag')
        page.wait_for_timeout(1500)
        seg2 = page.evaluate("() => document.querySelector('#listen-seg [data-chan=\"2\"]').className")
        row2 = page.evaluate("() => document.getElementById('chan-row-2').className")
        check('row click switches listen (seg CH2 on)', 'on' in (seg2 or ''), seg2 or '')
        check('listen row highlighted', 'listen' in (row2 or ''), row2 or '')

        # Keyboard 1 returns to CH1.
        page.click('body')
        page.keyboard.press('1')
        page.wait_for_timeout(1200)
        seg1 = page.evaluate("() => document.querySelector('#listen-seg [data-chan=\"1\"]').className")
        check('keyboard 1 returns to CH1', 'on' in (seg1 or ''), seg1 or '')

        # PEP meter renders a real readout + S badge (fake backend publishes peak_dbm).
        page.wait_for_timeout(1500)
        pep = page.text_content('#pep-val') or ''
        peps = page.text_content('#pep-s') or ''
        ticks = page.evaluate("() => document.querySelectorAll('#pep-bar .pep-tick').length")
        check('PEP readout present', pep.strip().endswith('dBm'), pep)
        check('S badge present', peps.startswith('S'), peps)
        check('PEP S-scale ticks mounted', ticks >= 7, str(ticks))

        # Activity LED renders a class (on/off — the fake toggles it every ~10 ticks).
        led3 = page.evaluate("() => document.getElementById('chan-led-3').className")
        check('activity LED element present', 'chan-led' in (led3 or ''), led3 or '')

        # FT8 background: selecting CH2 opens the decoder's ?iqaux=1 socket.
        aux: list[str] = []
        page.on('websocket', lambda ws: aux.append(ws.url))
        page.select_option('#ft8-source', '2')
        page.wait_for_timeout(2500)
        check('FT8 aux socket opened (?iqaux=1)', any('iqaux=1' in u for u in aux),
              str([u.split('?')[-1] for u in aux]))

        browser.close()

    print(f'{"OK" if not failures else "FAILED"}: sdr channels e2e ({len(failures)} failures)')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
