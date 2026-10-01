#!/usr/bin/env python3
"""Trace-panel reset regression: Preset and page reload must restore factory defaults.

Reported: after a Preset the trace panel kept the last session's widgets - the active tab,
the Mode select, the Freeze state, the Average depth and row, Smooth and the normalize
reference window - while the trace *state* was already reset, so the display and the state
disagreed until a reload. A reload itself must land on the same defaults whatever
localStorage holds (none of the trace options persist, by design).

This drives the real page against the fake backend, pushes every trace control away from
its default, then checks both reset paths against the factory set.

Usage:  python3 tools/e2e/trace_reset.py [--url http://127.0.0.1:8099]
Exit status is non-zero when any check fails, so it can be a CI gate.
"""
from __future__ import annotations

import argparse
import json
import socket
import subprocess
import sys
import time
import urllib.request

from playwright.sync_api import sync_playwright

failures: list[str] = []

#: The factory defaults every reset path must land on.
DEFAULTS = {
    'tab': 'T1',
    'mode': 'CLEAR_WRITE',
    'freeze': False,
    'smooth': '1',
    'refwin': '0',
    'norm': False,
    'avgRow': 'none',
    'avg': '16',
}

SNAP = """() => ({
  tab: document.querySelector('.trace-btn.active')?.textContent || '?',
  mode: document.getElementById('select-trace-mode')?.value || '?',
  freeze: document.getElementById('btn-view-freeze')?.classList.contains('active') || false,
  smooth: document.getElementById('select-smooth')?.value || '?',
  refwin: document.getElementById('select-refwin')?.value || '?',
  norm: document.getElementById('btn-normalize')?.classList.contains('active') || false,
  avgRow: getComputedStyle(document.getElementById('trace-avg-row')).display,
  avg: document.getElementById('select-trace-avg')?.value || '?',
})"""


def check(name: str, ok: bool, detail: str = '') -> None:
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ''))
    if not ok:
        failures.append(name)


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def configure(page) -> None:
    """Push every trace control away from its factory default (T2, Average/64, freeze, 9, 5)."""
    page.evaluate("() => document.querySelectorAll('[data-trace-tab]')[1].click()")
    page.evaluate("""() => {
        const set = (id, value) => {
            const s = document.getElementById(id);
            s.value = value;
            s.dispatchEvent(new Event('change', { bubbles: true }));
        };
        set('select-trace-mode', 'AVERAGE');
        set('select-trace-avg', '64');
        set('select-smooth', '9');
        set('select-refwin', '5');
    }""")
    page.evaluate("() => document.getElementById('btn-view-freeze').click()")
    page.evaluate("() => document.getElementById('btn-normalize').click()")
    page.wait_for_timeout(1200)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def wait_ready(url: str, timeout: float = 20.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f'{url}/api/state', timeout=2) as response:
                if response.status == 200:
                    return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f'backend not ready at {url}')


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default=None, help='an already-running backend (default: fake)')
    args = parser.parse_args()

    server = None
    url = args.url
    if not url:
        import os
        port = free_port()
        url = f'http://127.0.0.1:{port}'
        env = dict(os.environ, WEBSA_FAKE='1', WEBSA_PORT=str(port))
        server = subprocess.Popen([sys.executable, '-m', 'web_sa.main'], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        wait_ready(url)
        with sync_playwright() as p:
            browser = p.chromium.launch()
            ctx = browser.new_context()
            page = ctx.new_page()
            errors: list[str] = []
            page.on('pageerror', lambda e: errors.append(str(e)))
            page.goto(url, wait_until='domcontentloaded')
            page.wait_for_selector('#select-trace-mode', state='attached', timeout=15000)
            page.wait_for_timeout(2500)
            boot = page.evaluate(SNAP)
            check('a fresh page shows the factory trace defaults', boot == DEFAULTS,
                  f'got {boot}, want {DEFAULTS}')

            configure(page)
            away = page.evaluate(SNAP)
            check('the configuration step moved the controls away from the defaults',
                  away != DEFAULTS, f'got {away}')

            # Path 1: Preset resets both the trace state and the panel.
            page.evaluate("() => document.getElementById('btn-preset').click()")
            page.wait_for_timeout(2500)
            check('Preset restores the factory trace panel', page.evaluate(SNAP) == DEFAULTS,
                  f'got {page.evaluate(SNAP)}')

            # Path 2: a reload lands on the same defaults (with whatever localStorage a real
            # session left behind - none of it is trace state, by design).
            configure(page)
            page.evaluate("() => localStorage.setItem('websa-trace-window', 'garbage')")
            page.reload(wait_until='domcontentloaded')
            page.wait_for_selector('#select-trace-mode', state='attached', timeout=15000)
            page.wait_for_timeout(2500)
            check('a reload restores the factory trace panel', page.evaluate(SNAP) == DEFAULTS,
                  f'got {page.evaluate(SNAP)}')

            check('no page errors', not errors, '; '.join(errors[:3]))
            browser.close()
    finally:
        if server:
            server.terminate()
            server.wait(timeout=10)

    print(f"\n{'ALL PASS' if not failures else 'FAILURES: ' + ', '.join(failures)}")
    return 0 if not failures else 1


if __name__ == '__main__':
    raise SystemExit(main())
