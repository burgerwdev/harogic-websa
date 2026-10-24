#!/usr/bin/env python3
"""Native websad + tinySA squelch bench; leaves RF off and restores initial mode."""
import argparse
import json
import sys
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from sdr_network_diagnostics import command, state  # noqa: E402

from tools.sdr_probe.tinysa import TinySA  # noqa: E402


def diag(page):
    return page.evaluate("document.getElementById('spectrum').dataset.sdrAudio")


url = 'http://127.0.0.1:8080'
parser = argparse.ArgumentParser()
parser.add_argument('--modulation', choices=('am', 'fm'), default='am')
parser.add_argument('--url', default=url, help='running native websad hardware service')
args = parser.parse_args()
url = args.url
mode = 'am' if args.modulation == 'am' else 'wfm'
initial = state(url)
source = None
with sync_playwright() as pw:
    browser = pw.chromium.launch(headless=True, args=['--autoplay-policy=no-user-gesture-required'])
    page = browser.new_page(viewport={'width': 1440, 'height': 900})
    try:
        page.goto(url, wait_until='networkidle')
        page.evaluate("document.getElementById('btn-mode-sdr').click()")
        page.wait_for_timeout(800)
        command(url, {'cmd': 'SET_SDR', 'center': 100e6, 'decimate': 16})
        command(url, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                      'freq': 100e6, 'mode': mode, 'ifbw': 12000 if mode == 'am' else 180000,
                      'squelch': -60, 'agc': True, 'volume': 0.8})
        button = page.locator('#btn-sdr-audio')
        if 'on' not in button.inner_text().lower():
            button.evaluate('(el) => el.click()')
        page.wait_for_timeout(4000)
        source = TinySA('/dev/ttyACM0')
        print('tinySA:', source.am(100e6, -40, 1000, 60) if mode == 'am'
              else source.fm(100e6, -40, 1000, 25000), flush=True)
        page.wait_for_timeout(4000)
        print(f'{mode} -40:', json.dumps({'audio': diag(page), 'sdr': state(url)['sdr']}), flush=True)
        command(url, {'cmd': 'SET_SDR_DEMOD', 'squelch': -20})
        page.wait_for_timeout(3000)
        print(f'{mode} threshold -20:', json.dumps({'audio': diag(page), 'sdr': state(url)['sdr']}), flush=True)
        command(url, {'cmd': 'SET_SDR_DEMOD', 'squelch': -60})
        source.off()
        page.wait_for_timeout(5000)
        print('noise after signal:', json.dumps({'audio': diag(page), 'sdr': state(url)['sdr']}), flush=True)
    finally:
        if source:
            source.close()
        browser.close()
        old = initial['sdr']
        command(url, {'cmd': 'SET_SDR', 'center': old['center'], 'decimate': old['decimate']})
        command(url, {'cmd': 'SET_SDR_TUNE', 'listen': old['listen']})
        command(url, {'cmd': 'SET_SDR_DEMOD', 'mode': old['demod'], 'ifbw': old['if_bw'],
                      'squelch': old['squelch'], 'agc': old['agc'], 'volume': old['volume']})
        if initial['mode'] != 'sdr':
            command(url, {'cmd': 'SET_MODE', 'mode': initial['mode']})
