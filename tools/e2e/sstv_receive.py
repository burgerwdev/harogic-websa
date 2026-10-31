#!/usr/bin/env python3
"""SSTV -> SDE2 -> browser decoder pane.

Requires the native websad release binary and built frontend assets.
Asserts: completed pictures, partial progress and populated thumbnails.
The fixture path also checks first decode < 2 s.
Use --url for an already configured hardware service and external RF transmitter.

    python3 tools/e2e/sstv_receive.py
"""
from __future__ import annotations

import argparse
import json
import signal
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
    cancelled = False

    def interrupted(_signum, _frame):
        nonlocal cancelled
        cancelled = True

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, interrupted)
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--port', type=int, default=8125)
    parser.add_argument('--seconds', type=float, default=90.0, help='overall receive timeout')
    parser.add_argument('--url', help='observe an existing SSTV service; do not configure or stop it')
    parser.add_argument('--mode', default='Robot 36', help='expected mode display name')
    parser.add_argument('--pictures', type=int, default=2)
    parser.add_argument('--screenshot', type=Path, help='save the final decoder pane')
    args = parser.parse_args()
    if args.pictures < 1:
        parser.error('--pictures must be positive')
    if not args.url and args.mode != 'Robot 36':
        parser.error('the fixture sends Robot 36; other modes require --url')
    modes = json.loads((ROOT / 'tests/fixtures/sstv/modes.json').read_text())
    expected = next((mode for mode in modes['modes'] if mode['display'] == args.mode), None)
    if expected is None:
        parser.error('unknown --mode display name')
    origin = args.url.rstrip('/') if args.url else f'http://127.0.0.1:{args.port}'

    backend = None if args.url else start_backend(args.port)
    failures: list[str] = []
    try:
        if not args.url:
            post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            post(origin,
                 {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                  'freq': 100_000_000, 'mode': 'sstv', 'ifbw': 3000, 'pitch': 700,
                  'volume': 0.0, 'squelch': -140.0, 'agc': True})
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            try:
                context = browser.new_context()
                context.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
                page = context.new_page()
                page.goto(origin, wait_until='domcontentloaded')
                page.wait_for_selector('[data-sdr-demod="sstv"]', state='attached', timeout=30000)
                page.wait_for_timeout(2000)
                if page.evaluate("() => document.getElementById('btn-decode-window').disabled") is False:
                    page.click('#btn-decode-window')
                page.wait_for_timeout(1000)

                print(f'READY: waiting for {args.pictures} {args.mode} pictures', flush=True)
                start_mono = time.monotonic()
                deadline = start_mono + args.seconds
                first_decode_at = None
                completed_ids: set[str] = set()
                saw_progress = False
                diag = ''
                # Phase 1: sub-300 ms polling until the first decode (latency gate: < 2 s).
                while not cancelled and time.monotonic() < min(deadline, start_mono + (args.seconds if args.url else 10.0)):
                    page.wait_for_timeout(250)
                    mode = page.evaluate("() => document.getElementById('sstv-mode-name').textContent")
                    if mode == args.mode:
                        first_decode_at = time.monotonic() - start_mono
                        gate_ok = first_decode_at < 2.0
                        gate = '' if args.url else f' gate_ok={gate_ok}'
                        print(f'FIRST DECODE {mode} latency={first_decode_at:.2f}s{gate}', flush=True)
                        if not args.url and not gate_ok:
                            failures.append(f'first decode latency {first_decode_at:.2f}s >= 2s')
                        break
                else:
                    failures.append(f'no {args.mode} decode before timeout')
                if cancelled:
                    return 130
                # Track the actual picture identity; progress can finish between polls.
                while not cancelled and time.monotonic() < deadline:
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
                    if snapshot['mode'] != args.mode:
                        continue
                    percent = float((snapshot['progress'] or '0').rstrip('%'))
                    saw_progress |= 0 < percent < 100
                    if percent == 100 and snapshot['id'] and snapshot['id'] not in completed_ids:
                        completed_ids.add(snapshot['id'])
                        print(f'COMPLETION #{len(completed_ids)}: {diag}', flush=True)
                        if (snapshot['width'], snapshot['height']) != (expected['width'], expected['height']) or not snapshot['nonzero']:
                            failures.append('picture is empty or has incorrect dimensions')
                        if snapshot['thumbs'] < len(completed_ids):
                            failures.append('completed pictures missing history thumbnails')
                    if len(completed_ids) >= args.pictures:
                        break
                if cancelled:
                    return 130
                if not saw_progress:
                    failures.append('no partial picture progress observed')
                if len(completed_ids) < args.pictures:
                    failures.append(f'only {len(completed_ids)} completions in {args.seconds:.0f}s')
                if args.screenshot:
                    page.screenshot(path=str(args.screenshot))
                if failures:
                    print('\nFAIL:')
                    for failure in failures:
                        print(f'  {failure}')
                    print(f'diag: {diag}')
                    return 1
                print('\nOK: streaming decode passed all gates')
                return 0
            finally:
                browser.close()
    finally:
        stop(backend)


if __name__ == '__main__':
    raise SystemExit(main())
