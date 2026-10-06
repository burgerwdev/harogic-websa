#!/usr/bin/env python3
"""Capture the README screenshots on the bench (SAN-90 + signal sources).

The committed images under `screenshots/` are produced by this script, so they can be
re-made when the UI changes instead of being one-off captures.

  * Analyzer-side shots (spectrum, markers, measurements, RTA/waterfall) need the analyzer
    and a CW source on the tinySA (`--tinysa-port`; the level comes from the smoke test's
    table).
  * SDR shots (FT8 / CW decoding) need a PlutoSDR transmitting: start
    `tools/pluto/pluto_ft8_tx.py --lo 411e6` or `tools/pluto/pluto_cw_tx.py --lo 411e6` first and pass
    `--only sdr_ft8` / `--only sdr_cw`. The decode window fills over a few FT8 slots, so
    those two wait longer than the rest.

Usage:
  python3 tools/e2e/readme_shots.py --tinysa-port /dev/ttyACM0
  python3 tools/e2e/readme_shots.py --only sdr_ft8,sdr_cw      # Pluto already transmitting
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT))

from playwright.sync_api import sync_playwright  # noqa: E402

CARRIER_HZ = 100.2e6                 # tinySA CW, the frequency the smoke test uses
SPAN_HZ = 2e6
PNM_HZ = 1e9                         # phase noise / harmonic source (a clean 1 GHz carrier)
PNM_LEVEL_DBM = -30.0
HARM_SPAN_HZ = 100e6                 # the widest per-harmonic sweep the panel offers
SDR_HZ = 411e6                       # Pluto FT8/CW (tools/pluto/pluto_*_tx.py --lo 411e6)
RTA_CENTER_HZ = 2.425e9               # RTA + waterfall: ambient WiFi / base stations (2.4G antenna)
RTA_SPAN_HZ = 50.78125e6             # the widest RTA span the device offers (~50 MHz)
VIEWPORT = {'width': 1380, 'height': 900}    # the size the committed shots use

ANALYZER_SHOTS = ['main_dark', 'main_zh', 'main_light', 'marker_peaks',
                  'measure_amplitude', 'measure_harmonic', 'measure_phasenoise',
                  'waterfall_rta']
SDR_SHOTS = ['sdr_ft8', 'sdr_cw']


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def painted_pixels(page) -> int:
    return page.evaluate(
        """() => {
          const c = document.getElementById('spectrum');
          const g = c.getContext('2d');
          const d = g.getImageData(0, 0, c.width, c.height).data;
          let n = 0;
          for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++;
          return n;
        }""")


def set_theme(page, theme: str) -> None:
    for _ in range(3):
        if page.evaluate("document.documentElement.dataset.theme") == theme:
            return
        page.click('#btn-theme')
        page.wait_for_timeout(400)
    raise RuntimeError(f'the UI would not switch to the {theme} theme')


def set_lang(page, lang: str) -> None:
    expected = 'zh-CN' if lang == 'zh' else lang      # setLang() writes a full tag
    for _ in range(3):
        if page.evaluate("document.documentElement.lang") == expected:
            return
        page.click('#btn-lang')
        page.wait_for_timeout(400)
    raise RuntimeError(f'the UI would not switch to {lang}')


def settle(page, ms: int = 2500, min_pixels: int = 5000) -> None:
    """Wait out the render, and fail loudly on an empty canvas (a blank shot is worthless)."""
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        page.wait_for_timeout(ms)
        if painted_pixels(page) > min_pixels:
            return
    raise RuntimeError('the spectrum canvas stayed empty')


def set_source(source, hz: float, level_dbm: float) -> None:
    """Point the tinySA CW source at `hz` / `level_dbm` (no-op without a source)."""
    if source is None:
        return
    source.enable_cw(hz, 'normal')          # enables the output at the band's safe level
    source.command(f'level {level_dbm:g}')
    print(f'  source: {hz / 1e6:g} MHz at {level_dbm:g} dBm')


def bind_sweep(page, url: str, hz: float = CARRIER_HZ) -> None:
    """A clean swept view of the CW source: 2 MHz around it, signal framed."""
    post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
    post(url, {'cmd': 'SET_FREQ', 'center': hz, 'span': SPAN_HZ})
    post(url, {'cmd': 'SET_RBW', 'mode': 'auto'})
    post(url, {'cmd': 'SET_POINTS', 'points': 1001})
    page.wait_for_timeout(1200)
    post(url, {'cmd': 'AUTO_SCALE', 'range_db': 100})
    settle(page)


def capture(page, out: Path, name: str) -> None:
    path = out / f'{name}.png'
    page.screenshot(path=str(path))
    print(f'  {name}: {path.stat().st_size // 1024} KB')


def button_on(page, button_id: str) -> bool:
    """True when a text-toggle button (Off/On) reports its on state."""
    return page.evaluate(
        f"() => document.getElementById('{button_id}').textContent.trim().toLowerCase() === 'on'")


def ensure_button(page, button_id: str, on: bool) -> None:
    for _ in range(3):
        if button_on(page, button_id) == on:
            return
        page.click(f'#{button_id}')
        page.wait_for_timeout(600)
    raise RuntimeError(f'{button_id} would not switch to {"On" if on else "Off"}')


def ensure_active(page, selector: str, on: bool = True) -> None:
    """Click a toggle that marks its state with the `active` class, only when needed."""
    for _ in range(3):
        is_on = page.evaluate(
            f"() => document.querySelector('{selector}')?.classList.contains('active')")
        if bool(is_on) == on:
            return
        page.click(selector)
        page.wait_for_timeout(600)
    raise RuntimeError(f'{selector} would not switch to {on}')


def analyzer_shots(page, url: str, out: Path, names: list[str], source) -> None:
    bind_sweep(page, url)
    if 'main_dark' in names:
        set_lang(page, 'en')
        set_theme(page, 'dark')
        settle(page)
        capture(page, out, 'main_dark')
    if 'main_zh' in names:
        set_lang(page, 'zh')
        settle(page)
        capture(page, out, 'main_zh')
    if 'main_light' in names:
        set_theme(page, 'light')
        set_lang(page, 'en')
        settle(page)
        capture(page, out, 'main_light')
        set_theme(page, 'dark')

    if 'marker_peaks' in names:
        page.select_option('#select-trace-mode', 'MAX_HOLD')
        page.select_option('#select-smooth', '1')      # unsmoothed: the raw trace reads best here
        ensure_active(page, '#btn-markers-all')
        ensure_active(page, '#btn-marker-tracking')
        settle(page, 4000)
        capture(page, out, 'marker_peaks')

    if 'measure_amplitude' in names:
        ensure_button(page, 'btn-meas-onoff', True)
        ensure_button(page, 'btn-peaklist', True)
        page.click('[data-action="peakthr-auto"]')
        page.click('[data-meas-tab="amp"]')
        settle(page, 3000)
        capture(page, out, 'measure_amplitude')

    # The differential measurements want a clean, strong carrier at 1 GHz: switch the source
    # over (and the analyzer with it) before either of them runs.
    if {'measure_harmonic', 'measure_phasenoise'} & set(names):
        set_source(source, PNM_HZ, PNM_LEVEL_DBM)
        bind_sweep(page, url, PNM_HZ)
    if 'measure_harmonic' in names:
        # The panel's apply functions return early unless the measurement is switched on.
        ensure_button(page, 'btn-meas-onoff', True)
        page.select_option('#select-smooth', '1')      # unsmoothed trace, as requested
        page.click('[data-meas-tab="harm"]')
        page.fill('#input-harm-f0', f'{PNM_HZ / 1e6:g}')
        page.fill('#input-harm-count', '5')
        page.select_option('#select-harm-span', str(int(HARM_SPAN_HZ)))
        page.click('#btn-harm-set')
        settle(page, 6000)
        page.wait_for_timeout(10000)          # let the server walk H1..H5
        capture(page, out, 'measure_harmonic')
    if 'measure_phasenoise' in names:
        ensure_button(page, 'btn-meas-onoff', True)
        page.click('[data-meas-tab="pnm"]')
        page.fill('#input-pnm', f'{PNM_HZ / 1e6:g}')
        page.click('[data-action="meas-pnm"]')
        settle(page, 12000)
        page.wait_for_timeout(10000)
        capture(page, out, 'measure_phasenoise')

    if 'waterfall_rta' in names:
        ensure_button(page, 'btn-meas-onoff', False)   # the panel would cover the plot
        # Ambient signals (WiFi / base stations) around 2.425 GHz: a real wide-band view with no
        # signal source attached. Connect a 2.4 GHz antenna to the analyzer before this shot.
        post(url, {'cmd': 'SET_MODE', 'mode': 'std'})
        post(url, {'cmd': 'SET_FREQ', 'center': RTA_CENTER_HZ, 'span': 10e6})
        page.wait_for_timeout(1500)
        page.click('#btn-mode-rta')
        page.wait_for_timeout(2500)
        page.fill('#input-rta-center', f'{RTA_CENTER_HZ / 1e6:g}')
        page.select_option('#select-rta-span', str(int(RTA_SPAN_HZ)))
        page.select_option('#select-rta-fade', '0.995')   # slowest fade: bursty signals accumulate
        page.click('[data-action="apply-rta"]')
        page.wait_for_timeout(2000)
        ensure_active(page, '[data-action="toggle-waterfall"]')
        settle(page, 2500)
        page.wait_for_timeout(30000)                   # let the intermittent traffic build up
        capture(page, out, 'waterfall_rta')


def wait_decodes(page, mode: str, timeout_s: float, want: int) -> int:
    """Wait until the decode window really holds content; raise when nothing arrives.

    An empty floating window is worse than no screenshot, so the FT8 count and the CW log are
    polled instead of guessed at with a fixed sleep.
    """
    deadline = time.monotonic() + timeout_s
    count = 0
    while True:
        count = page.evaluate(
            """(mode) => {
              if (mode === 'cw') return document.querySelectorAll('#decode-window-cw .cw-line').length;
              const c = document.getElementById('decode-window-count');
              return c ? parseInt(c.textContent || '0', 10) || 0 : 0;
            }""", mode)
        if count >= want or time.monotonic() > deadline:
            break
        page.wait_for_timeout(2000)
    if count == 0:
        raise RuntimeError(
            f'the {mode} decode window stayed empty after {timeout_s:.0f}s; '
            'is the signal source transmitting (tools/pluto/pluto_*_tx.py)?')
    return count


def sdr_shots(page, url: str, out: Path, names: list[str]) -> None:
    page.click('#btn-mode-sdr')
    page.wait_for_timeout(2500)
    # The UI re-applies the persisted SDR preferences when the mode is entered, so the tuning
    # has to go through the panel fields: a REST `SET_SDR` is overwritten by that sync (that is
    # why an earlier run decoded nothing - the analyzer was still parked at 1 GHz).
    page.fill('#input-sdr-center', f'{SDR_HZ / 1e6:g}')
    page.click('[data-action="apply-sdr"]')
    page.wait_for_timeout(2000)
    page.fill('#input-sdr-listen', f'{SDR_HZ / 1e6:g}')
    page.click('[data-action="apply-sdr-tune"]')
    page.wait_for_timeout(2000)
    # The bench recipe for the decoders: a -40 dBm reference (it also sets the device's
    # attenuation) with the trace averaged, so the channel sits well inside the window.
    page.fill('#input-ref', '-40')
    page.click('#btn-ref-set')
    page.select_option('#select-trace-mode', 'AVERAGE')
    page.select_option('#select-trace-avg', '16')
    page.wait_for_timeout(2500)
    # (name, demod, filter preset, decodes wanted before the shot, how long to keep waiting)
    for name, mode, ifbw, want, wait_s in (('sdr_ft8', 'ft8', '3000', 2, 180),
                                           ('sdr_cw', 'cw', '3000', 3, 90)):
        if name not in names:
            continue
        page.click(f'[data-sdr-demod="{mode}"]')     # through the panel, like a user
        page.wait_for_timeout(1500)
        page.click(f'[data-sdr-ifbw="{ifbw}"]')      # the panel's Filter preset
        page.wait_for_timeout(1500)
        if page.evaluate("() => document.getElementById('btn-decode-window').textContent.trim().toLowerCase() === 'off'"):
            page.click('#btn-decode-window')
        settle(page, 3000)
        count = wait_decodes(page, mode, wait_s, want)
        window_text = page.evaluate(
            """(mode) => mode === 'cw'
              ? Array.from(document.querySelectorAll('#decode-window-cw .cw-line'))
                  .map(e => e.textContent.replace(/\\s+/g, ' ').trim()).join(' | ')
              : Array.from(document.querySelectorAll('#decode-window-rows tr'))
                  .map(e => e.textContent.replace(/\\s+/g, ' ').trim()).join(' | ')""", mode)
        print(f'  {name}: {count} decode(s): {window_text[:160]}')
        page.wait_for_timeout(2000)
        capture(page, out, name)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--out', default=str(ROOT / 'screenshots'))
    parser.add_argument('--tinysa-port', default='')
    parser.add_argument('--only', default='', help='comma-separated shot names (default: all)')
    args = parser.parse_args()

    names = [n.strip() for n in args.only.split(',') if n.strip()] or ANALYZER_SHOTS + SDR_SHOTS
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    source = None
    if args.tinysa_port and set(names) & set(ANALYZER_SHOTS):
        from tools.bench.hardware_smoke import TinySaSource

        source = TinySaSource(args.tinysa_port)
        print('tinySA:', source.identify().splitlines()[1] if source.identify() else '?')
        set_source(source, CARRIER_HZ, -25.0)

    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
            page = browser.new_page(viewport=VIEWPORT)
            page.goto(f'{args.url}/', wait_until='networkidle')
            page.wait_for_timeout(3000)
            if set(names) & set(ANALYZER_SHOTS):
                analyzer_shots(page, args.url, out, names, source)
            if set(names) & set(SDR_SHOTS):
                sdr_shots(page, args.url, out, names)
            browser.close()
    finally:
        if source is not None:
            source.close(disable_output=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
