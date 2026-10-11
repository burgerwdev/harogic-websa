#!/usr/bin/env python3
"""SSTV receive, end to end: fake backend -> IQBF -> browser worker -> decoder -> pane.

Runs against a real service with the fake device replaying the committed Robot 36 transmission.
Asserts: first decode < 2 s, dual transmission (2 completions), history thumbnails populated.
Fault injection: --offset Hz, --scale amplitude.

    python3 tools/e2e/sstv_receive.py
    python3 tools/e2e/sstv_receive.py --offset 800 --scale 0.01
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from playwright.sync_api import sync_playwright  # noqa: E402


def post(url: str, payload: dict) -> None:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        response.read()


def start_backend(port: int, offset: float = 0.0, scale: float = 1.0) -> subprocess.Popen:
    env = dict(os.environ, WEBSA_FAKE='1', WEBSA_PORT=str(port),
               WEBSA_FAKE_SSTV_OFFSET_HZ=str(offset), WEBSA_FAKE_SSTV_SCALE=str(scale))
    proc = subprocess.Popen([sys.executable, '-m', 'web_sa.supervisor'], cwd=ROOT, env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f'http://127.0.0.1:{port}'
    for _ in range(60):
        try:
            urllib.request.urlopen(f'{url}/api/config', timeout=2)
            return proc
        except urllib.error.HTTPError:
            return proc
        except Exception:
            time.sleep(0.5)
    proc.terminate()
    raise SystemExit('backend did not start')


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--port', type=int, default=8125)
    parser.add_argument('--offset', type=float, default=0.0, help='replay frequency offset in Hz')
    parser.add_argument('--scale', type=float, default=1.0, help='replay amplitude scale')
    parser.add_argument('--seconds', type=float, default=90.0, help='how long to wait for 2 pictures')
    args = parser.parse_args()

    backend = start_backend(args.port, offset=args.offset, scale=args.scale)
    failures: list[str] = []
    try:
        post(f'http://127.0.0.1:{args.port}', {'cmd': 'SET_MODE', 'mode': 'sdr'})
        post(f'http://127.0.0.1:{args.port}',
             {'cmd': 'SET_SDR_DEMOD', 'mode': 'sstv', 'ifbw': 3000, 'pitch': 700, 'volume': 0.0,
              'squelch': -140.0, 'agc': True})
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            context = browser.new_context()
            context.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
            page = context.new_page()
            page.goto(f'http://127.0.0.1:{args.port}', wait_until='domcontentloaded')
            page.wait_for_selector('[data-sdr-demod="sstv"]', state='attached', timeout=30000)
            page.wait_for_timeout(2000)
            if page.evaluate("() => document.getElementById('btn-decode-window').disabled") is False:
                page.click('#btn-decode-window')
            page.wait_for_timeout(1000)

            start_mono = time.monotonic()
            deadline = start_mono + args.seconds
            first_decode_at = None
            completions = 0
            last_progress = ''
            # Phase 1: sub-300 ms polling until the first decode (latency gate: < 2 s).
            while time.monotonic() < start_mono + 10.0:
                page.wait_for_timeout(250)
                mode = page.evaluate("() => document.getElementById('sstv-mode-name').textContent")
                if mode and mode != '—':
                    first_decode_at = time.monotonic() - start_mono
                    gate_ok = first_decode_at < 2.0
                    print(f'FIRST DECODE {mode} latency={first_decode_at:.2f}s gate_ok={gate_ok}')
                    if not gate_ok:
                        failures.append(f'first decode latency {first_decode_at:.2f}s >= 2s')
                    break
            else:
                failures.append('no decode within 10s')
            # Phase 2: 3 s polling until 2 completions; assert history thumbnails.
            while time.monotonic() < deadline:
                page.wait_for_timeout(3000)
                progress = page.evaluate("() => document.getElementById('sstv-progress-text').textContent")
                diag = page.evaluate("() => document.getElementById('spectrum').dataset.sdrIq || ''")
                thumbs = page.evaluate(
                    "() => document.querySelectorAll('.decode-sstv-thumb').length")
                if progress == '100%' and last_progress != '100%':
                    completions += 1
                    print(f'COMPLETION #{completions} at t={time.monotonic() - start_mono:.0f}s  thumbs={thumbs}')
                    if completions >= 1 and thumbs < 1:
                        failures.append(f'completion #{completions} but no history thumbnails')
                last_progress = progress
                if completions >= 2:
                    print(f'DUAL TRANSMISSION OK: {completions} completions, thumbs={thumbs}')
                    if thumbs < 1:
                        failures.append('no history thumbnails after 2 completions')
                    break
            if completions < 2:
                failures.append(f'only {completions} completions in {args.seconds:.0f}s')
            if failures:
                print('\nFAIL:')
                for failure in failures:
                    print(f'  {failure}')
                print(f'diag: {diag}')
                browser.close()
                return 1
            print('\nOK: streaming decode passed all gates')
            browser.close()
            return 0
    finally:
        backend.send_signal(signal.SIGTERM)


if __name__ == '__main__':
    raise SystemExit(main())
