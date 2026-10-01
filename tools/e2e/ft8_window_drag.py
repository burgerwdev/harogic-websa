#!/usr/bin/env python3
"""FT8 window drag regression: the cursor must stay anchored to the title bar.

The window used to live inside the zoomed frame (`.analyzer-card` carries `zoom: var(--ui)`), so
its `style.left/top` rendered in zoomed coordinates while pointer `clientX/Y` stayed viewport
coordinates - on a scaled UI the window jumped away from the cursor by the zoom factor the moment
the head was grabbed (reported on a high-resolution display, where the auto scale is 1.5/2).

This drives the real UI at a forced UI scale with real mouse events (the browser's own pointer
plumbing, not synthetic PointerEvents) and asserts the invariant the operator feels: the grabbed
point of the head stays under the cursor for the whole drag, at every scale, and the geometry
that survives a reload is the same viewport-pixel box.

Usage:  python3 tools/e2e/ft8_window_drag.py
Exit status is non-zero when any check fails. Requires the frontend built (./build.sh) - the
backend serves frontend/modern/dist.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request

from playwright.sync_api import sync_playwright

failures: list[str] = []


def check(name: str, ok: bool, detail: str = '') -> None:
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ''))
    if not ok:
        failures.append(name)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


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


def drag_check(page, scale: float) -> None:
    """Resize from any edge, then grab the head and assert the grab point follows the cursor."""
    win = page.locator('#decode-window')
    head = page.locator('#decode-window-head')

    def drag_handle(dir_: str, dx: float, dy: float) -> None:
        handle = page.locator(f'[data-decode-rz="{dir_}"]')
        hb = handle.bounding_box()
        assert hb, f'the {dir_} resize handle must exist'
        cx, cy = hb['x'] + hb['width'] / 2, hb['y'] + hb['height'] / 2
        page.mouse.move(cx, cy)
        page.mouse.down()
        page.mouse.move(cx + dx, cy + dy, steps=6)
        page.mouse.up()

    # Resize from the edges and corners first, while the window sits at its start position with
    # room on every side: a single-corner-only window fails here (the west/north/corner handles do
    # not exist or do not move the edge they belong to).
    start = win.bounding_box()
    assert start, 'the FT8 window must be visible'
    drag_handle('e', 60, 0)
    grown = win.bounding_box()
    check(f'scale {scale}: the east edge grows the width in place',
          abs((grown['width'] - start['width']) - 60) <= 3 and abs(grown['x'] - start['x']) <= 1.5
          and abs(grown['y'] - start['y']) <= 1.5,
          f"{start['width']:.1f}x{start['height']:.1f} -> {grown['width']:.1f}x{grown['height']:.1f} "
          f"at ({start['x']:.1f},{start['y']:.1f}) -> ({grown['x']:.1f},{grown['y']:.1f})")
    drag_handle('w', -30, 0)
    wider = win.bounding_box()
    check(f'scale {scale}: the west edge moves the origin left and grows the width',
          abs(wider['x'] - (grown['x'] - 30)) <= 3
          and abs((wider['width'] - grown['width']) - 30) <= 3,
          f"x {grown['x']:.1f} -> {wider['x']:.1f}, w {grown['width']:.1f} -> {wider['width']:.1f}")
    drag_handle('n', 0, -40)
    taller = win.bounding_box()
    check(f'scale {scale}: the north edge moves the top and grows the height',
          abs(taller['y'] - (wider['y'] - 40)) <= 3
          and abs((taller['height'] - wider['height']) - 40) <= 6,
          f"y {wider['y']:.1f} -> {taller['y']:.1f}, h {wider['height']:.1f} -> {taller['height']:.1f}")
    drag_handle('se', 20, 20)
    corner = win.bounding_box()
    check(f'scale {scale}: the south-east corner grows both axes',
          abs((corner['width'] - taller['width']) - 20) <= 3
          and abs((corner['height'] - taller['height']) - 20) <= 6,
          f"{taller['width']:.1f}x{taller['height']:.1f} -> {corner['width']:.1f}x{corner['height']:.1f}")

    box = win.bounding_box()
    head_box = head.bounding_box()
    assert box and head_box, 'the FT8 window must be visible to drag'
    size_before = (box['width'], box['height'])
    # Grab a point inside the head away from its buttons and the opacity slider.
    grab_x = head_box['x'] + 40
    grab_y = head_box['y'] + head_box['height'] / 2
    grab_dx = grab_x - box['x']
    grab_dy = grab_y - box['y']

    page.mouse.move(grab_x, grab_y)
    page.mouse.down()
    try:
        # A tour: a plain move, a long move, and near the viewport edges (clamping must still
        # keep the head under the cursor within the clamp allowance).
        for target_x, target_y, allowance in (
            (grab_x + 120, grab_y + 60, 1.5),
            (grab_x - 260, grab_y - 140, 1.5),
            (page.viewport_size['width'] - 30, page.viewport_size['height'] - 30, 200.0),
        ):
            page.mouse.move(target_x, target_y, steps=8)
            box = win.bounding_box()
            dx = abs(box['x'] + grab_dx - target_x)
            dy = abs(box['y'] + grab_dy - target_y)
            check(f'scale {scale}: the head stays under the cursor at ({target_x:.0f},{target_y:.0f})',
                  dx <= allowance and dy <= allowance,
                  f'window at ({box["x"]:.1f},{box["y"]:.1f}), grab offset ({grab_dx:.1f},{grab_dy:.1f}), '
                  f'cursor ({target_x:.0f},{target_y:.0f}), deviation ({dx:.1f},{dy:.1f}) px')
    finally:
        page.mouse.up()

    # A drag is a move, not a resize: the window must come back exactly its starting size (a
    # border/box-sizing round-trip through getBoundingClientRect used to grow it ~2 px per
    # pointermove - "the window gets bigger while I drag it").
    box = win.bounding_box()
    check(f'scale {scale}: dragging does not resize the window',
          abs(box['width'] - size_before[0]) <= 1.0 and abs(box['height'] - size_before[1]) <= 1.0,
          f'before {size_before}, after ({box["width"]:.1f},{box["height"]:.1f})')

    # What the drag persisted must be the same box the DOM shows (viewport px), so a reload
    # restores the same spot.
    box = win.bounding_box()
    stored = json.loads(page.evaluate("localStorage.getItem('websa-decode-window')") or '{}')
    check(f'scale {scale}: the stored geometry is the viewport-pixel box',
          abs(stored.get('x', -1) - box['x']) <= 1.5 and abs(stored.get('y', -1) - box['y']) <= 1.5,
          f'dom ({box["x"]:.1f},{box["y"]:.1f}) vs stored ({stored.get("x")},{stored.get("y")})')

    page.reload(wait_until='domcontentloaded')
    page.wait_for_selector('#decode-window', state='attached')
    page.wait_for_function(
        "() => { const b = document.getElementById('btn-decode-window');"
        " return b && !b.disabled; }", timeout=15000)
    page.evaluate("document.getElementById('btn-decode-window').click()")
    page.wait_for_function(
        "() => getComputedStyle(document.getElementById('decode-window')).display !== 'none'")
    restored = win.bounding_box()
    check(f'scale {scale}: a reload restores the window where it was left',
          abs(restored['x'] - box['x']) <= 1.5 and abs(restored['y'] - box['y']) <= 1.5,
          f'before ({box["x"]:.1f},{box["y"]:.1f}) after reload ({restored["x"]:.1f},{restored["y"]:.1f})')


def open_ft8_window(url: str, page) -> None:
    post(url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'ft8', 'ifbw': 2400, 'pitch': 700,
               'volume': 1.0, 'squelch': -140.0, 'agc': True})
    page.wait_for_function(
        "() => { const b = document.getElementById('btn-decode-window');"
        " return b && !b.disabled; }", timeout=15000)
    page.evaluate("document.getElementById('btn-decode-window').click()")
    page.wait_for_function(
        "() => getComputedStyle(document.getElementById('decode-window')).display !== 'none'")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default=None,
                        help='an already-running backend (default: start a fake one)')
    args = parser.parse_args()

    env = dict(os.environ, WEBSA_FAKE='1')
    server = None
    url = args.url
    if not url:
        port = free_port()
        url = f'http://127.0.0.1:{port}'
        env['WEBSA_PORT'] = str(port)
        server = subprocess.Popen([sys.executable, '-m', 'web_sa.main'], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        wait_ready(url)
        errors: list[str] = []
        with sync_playwright() as p:
            browser = p.chromium.launch()
            for scale in (1, 2, 1.5):
                page = browser.new_page(viewport={'width': 1280, 'height': 800})
                page.on('pageerror', lambda e: errors.append(str(e)))
                # Force the UI scale before the app boots (core/uiScale.ts reads this key at
                # startup); a high-resolution display picks 1.5/2 on its own, this pins it.
                page.add_init_script(
                    f"localStorage.setItem('web-sa-ui-scale', '{scale}')")
                page.goto(url, wait_until='domcontentloaded')
                page.wait_for_selector('#btn-decode-window', state='attached', timeout=15000)
                applied = page.evaluate("document.documentElement.dataset.uiScale")
                check(f'scale {scale}: the UI scale is applied by the app',
                      applied == str(scale), f'dataset.uiScale={applied!r}')
                open_ft8_window(url, page)
                drag_check(page, scale)
                page.close()
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
