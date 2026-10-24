#!/usr/bin/env python3
"""Native Robot36 fixture -> SDE2 -> browser decoder pane.

Requires the native websad release binary and built frontend assets.
Asserts: first decode < 2 s, two distinct completed pictures, populated thumbnails.

    python3 tools/e2e/sstv_receive.py
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from playwright.sync_api import sync_playwright  # noqa: E402

from tools.e2e.rust_iq_browser import start_service, stop  # noqa: E402


def post(url: str, payload: dict) -> None:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        response.read()


def start_backend(port: int) -> subprocess.Popen:
    proc, _ = start_service(port)
    return proc


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--port', type=int, default=8125)
    parser.add_argument('--seconds', type=float, default=90.0, help='how long to wait for 2 pictures')
    args = parser.parse_args()

    backend = start_backend(args.port)
    failures: list[str] = []
    try:
        post(f'http://127.0.0.1:{args.port}', {'cmd': 'SET_MODE', 'mode': 'sdr'})
        post(f'http://127.0.0.1:{args.port}',
             {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
              'freq': 100_000_000, 'mode': 'sstv', 'ifbw': 3000, 'pitch': 700,
              'volume': 0.0, 'squelch': -140.0, 'agc': True})
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
            completed_ids: set[str] = set()
            saw_progress = False
            diag = ''
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
            # Track the actual picture identity; progress can finish between polls.
            while time.monotonic() < deadline:
                page.wait_for_timeout(250)
                snapshot = page.evaluate("""() => {
                    const picture = document.querySelector('.decode-sstv-thumb.viewed');
                    const canvas = document.getElementById('decode-sstv-canvas');
                    const pixels = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
                    let nonzero = false;
                    for (let i = 0; i < pixels.length; i += 4) {
                        if (pixels[i] || pixels[i + 1] || pixels[i + 2]) { nonzero = true; break; }
                    }
                    return {id: picture?.dataset.id, progress: document.getElementById('sstv-progress-text').textContent,
                        mode: document.getElementById('sstv-mode-name').textContent,
                        width: canvas.width, height: canvas.height, nonzero,
                        thumbs: document.querySelectorAll('.decode-sstv-thumb canvas').length};
                }""")
                diag = json.dumps(snapshot)
                percent = float((snapshot['progress'] or '0').rstrip('%'))
                saw_progress |= 0 < percent < 100
                if percent == 100 and snapshot['id'] and snapshot['id'] not in completed_ids:
                    completed_ids.add(snapshot['id'])
                    print(f'COMPLETION #{len(completed_ids)}: {diag}')
                    if 'robot' not in snapshot['mode'].lower() or '36' not in snapshot['mode']:
                        failures.append(f'unexpected fixture mode: {snapshot["mode"]}')
                    if (snapshot['width'], snapshot['height']) != (320, 240) or not snapshot['nonzero']:
                        failures.append('Robot36 picture is empty or has incorrect dimensions')
                    if snapshot['thumbs'] < len(completed_ids):
                        failures.append('completed pictures missing history thumbnails')
                if len(completed_ids) >= 2:
                    break
            if not saw_progress:
                failures.append('no partial picture progress observed')
            if len(completed_ids) < 2:
                failures.append(f'only {len(completed_ids)} completions in {args.seconds:.0f}s')
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
        stop(backend)


if __name__ == '__main__':
    raise SystemExit(main())
