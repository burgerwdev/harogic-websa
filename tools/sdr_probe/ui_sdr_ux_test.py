#!/usr/bin/env python3
"""Playwright test for the modern SDR interaction: auto-scale, click-to-tune,
keyboard tuning, demod/filter/band buttons, no JS errors."""
from __future__ import annotations

import json
import urllib.request

from playwright.sync_api import sync_playwright

URL = 'http://127.0.0.1:8080'


def backend():
    return json.load(urllib.request.urlopen(f'{URL}/api/state', timeout=5))


def main():
    errors = []
    with sync_playwright() as p:
        browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
        page = browser.new_page(viewport={'width': 1600, 'height': 950})
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None)
        page.on('pageerror', lambda e: errors.append(str(e)))
        page.goto(URL, wait_until='networkidle')
        page.wait_for_timeout(1500)
        page.click('#btn-mode-sdr')
        page.wait_for_selector('#sdr-settings', state='visible', timeout=10000)
        page.wait_for_function("() => document.body.classList.contains('rta-mode')", timeout=10000)

        # center 101.7 via Enter
        page.fill('#input-sdr-center', '101.7')
        page.press('#input-sdr-center', 'Enter')
        page.wait_for_timeout(1800)
        s = backend()['sdr']
        print('after center set: center=%.3fMHz listen=%.3fMHz' % (s['center']/1e6, s['listen']/1e6))

        # click-to-tune near the middle of the canvas
        box = page.eval_on_selector('#spectrum', 'el => { const r = el.getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height}; }')
        cx = box['x'] + box['w'] * 0.5
        cy = box['y'] + box['h'] * 0.5
        page.mouse.click(cx, cy)
        page.wait_for_timeout(1200)
        s = backend()['sdr']
        print('after click-to-tune: listen=%.4fMHz' % (s['listen']/1e6))

        # keyboard: ArrowRight x5 (+1 kHz each)
        before = backend()['sdr']['listen']
        for _ in range(5):
            page.keyboard.press('ArrowRight')
            page.wait_for_timeout(120)
        page.wait_for_timeout(800)
        after = backend()['sdr']['listen']
        print('keyboard tune: %.4f -> %.4f MHz (delta %.1f Hz)' % (before/1e6, after/1e6, after-before))

        # demod + filter buttons
        page.click('[data-sdr-demod="fm"]')
        page.wait_for_timeout(900)
        page.click('[data-sdr-ifbw="180000"]')
        page.wait_for_timeout(900)
        s = backend()['sdr']
        print('buttons: demod=%s if_bw=%s' % (s['demod'], s['if_bw']))

        # band preset
        page.click('[data-sdr-band="fm"]')
        page.wait_for_timeout(1500)
        s = backend()['sdr']
        print('band FM preset: center=%.3fMHz decimate=%s demod=%s if_bw=%s' %
              (s['center']/1e6, s['decimate'], s['demod'], s['if_bw']))

        page.wait_for_timeout(1500)
        page.screenshot(path='/tmp/sdr_ux.png')
        print('errors:', errors[:6])
        browser.close()


if __name__ == '__main__':
    main()
