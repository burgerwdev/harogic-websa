#!/usr/bin/env python3
"""Playwright UI test for the SDR mode: enters SDR, tunes to a tinySA AM signal,
counts WebSocket binary frames in the browser, and checks the canvas renders."""
from __future__ import annotations

import json
import os
import sys
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from playwright.sync_api import sync_playwright  # noqa: E402
from tinysa import TinySA  # noqa: E402

URL = 'http://127.0.0.1:8080'
CENTER_MHZ = 100.0
LISTEN_MHZ = 100.2


def canvas_sig(page):
    return page.evaluate(
        "() => { const c=document.getElementById('spectrum'); const g=c.getContext('2d');"
        " const d=g.getImageData(0,0,c.width,c.height).data; let h=2166136261>>>0;"
        " for (let i=0;i<d.length;i+=7){ h^=d[i]; h=Math.imul(h,16777619)>>>0; } return h; }")


def main():
    sa = TinySA('/dev/ttyACM0')
    errors = []
    counts = {'rtaf': 0, 'audf': 0, 'other': 0}
    try:
        sa.enter_low_output()
        sa.am(100.2e6, -25, 1000, 50)
        print('tinySA', sa.cmd('sweep'), flush=True)
        with sync_playwright() as p:
            browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
            page = browser.new_page(viewport={'width': 1600, 'height': 950})
            page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None)
            page.on('pageerror', lambda e: errors.append(str(e)))

            def on_ws(ws):
                def on_frame(frame):
                    data = getattr(frame, 'payload', frame)
                    if isinstance(data, str):
                        data = data.encode('latin1')
                    if data and len(data) >= 4:
                        magic = bytes(data[:4])
                        if magic == b'RTAF':
                            counts['rtaf'] += 1
                        elif magic == b'AUDF':
                            counts['audf'] += 1
                        else:
                            counts['other'] += 1
                ws.on('framereceived', on_frame)
            page.on('websocket', on_ws)

            page.goto(URL, wait_until='networkidle')
            page.wait_for_timeout(1500)
            page.click('#btn-mode-sdr')
            page.wait_for_selector('#sdr-settings', state='visible', timeout=10000)
            page.wait_for_function("() => document.body.classList.contains('rta-mode')", timeout=10000)
            page.fill('#input-sdr-center', str(CENTER_MHZ))
            page.click('button[data-action="apply-sdr"]')
            page.wait_for_timeout(1500)
            page.fill('#input-sdr-listen', str(LISTEN_MHZ))
            page.click('button[data-action="apply-sdr-tune"]')
            page.click('button[data-sdr-demod="am"]')
            page.click('button[data-sdr-ifbw="6000"]')
            page.wait_for_timeout(3500)

            st = json.load(urllib.request.urlopen('http://127.0.0.1:8080/api/state', timeout=3))
            print('backend mode:', st.get('mode'), 'actual listen:',
                  (st['sdr'].get('actual') or {}).get('listen'), flush=True)
            sig1 = canvas_sig(page)
            page.wait_for_timeout(1200)
            sig2 = canvas_sig(page)
            active = page.eval_on_selector('#btn-mode-sdr', 'el => el.classList.contains("active")')
            level = page.eval_on_selector('#cur-sdr-level', 'el => el.textContent')
            adm = page.eval_on_selector('#cur-sdr-adm', 'el => el.textContent')
            print(f'frames rtaf={counts["rtaf"]} audf={counts["audf"]} other={counts["other"]}')
            print(f'canvas sig1={sig1} sig2={sig2} changed={sig1 != sig2}')
            print(f'sdr button active={active} level="{level}" adm="{adm}"')
            page.screenshot(path='/tmp/sdr_ui.png')
            print('errors:', errors[:8])
            browser.close()
    finally:
        try:
            sa.off()
        except Exception:
            pass
        sa.ser.close()


if __name__ == '__main__':
    main()
