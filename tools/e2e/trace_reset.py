#!/usr/bin/env python3
"""Exercise trace controls, Preset, family changes and reload via the real page."""
from __future__ import annotations

import argparse

from playwright.sync_api import sync_playwright
from rust_iq_browser import free_port, start_service, stop

failures: list[str] = []

DEFAULTS = {
    'tab': 'T1',
    'mode': 'CLEAR_WRITE',
    'freeze': False,
    'smooth': '1',
    'refwin': '0',
    'norm': False,
    'avgRow': False,
    'avg': '16',
    'avgStatus': '',
}
SDR_DEFAULTS = {**DEFAULTS, 'mode': 'AVERAGE', 'avgRow': True, 'avgStatus': '16'}

SNAP = """() => ({
  tab: document.querySelector('.trace-btn.active')?.textContent.trim() || '?',
  mode: document.getElementById('select-trace-mode')?.value || '?',
  freeze: document.getElementById('btn-view-freeze')?.classList.contains('active') || false,
  smooth: document.getElementById('select-smooth')?.value || '?',
  refwin: document.getElementById('select-refwin')?.value || '?',
  norm: document.getElementById('btn-normalize')?.classList.contains('active') || false,
  avgRow: getComputedStyle(document.getElementById('trace-avg-row')).display !== 'none',
  avg: document.getElementById('select-trace-avg')?.value || '?',
  avgStatus: document.getElementById('trace-avg-status')?.textContent.trim() || '',
})"""


def check(name: str, ok: bool, detail: str = '') -> None:
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ''))
    if not ok:
        failures.append(name)


def expect_panel(page, name: str, expected: dict) -> None:
    page.wait_for_function(
        f"expected => {{ const actual = ({SNAP})(); return Object.entries(expected)"
        ".every(([key, value]) => actual[key] === value); }",
        arg=expected,
    )
    actual = page.evaluate(SNAP)
    check(name, all(actual[key] == value for key, value in expected.items()), f'got {actual}')


def wait_mode(page, mode: str) -> None:
    page.wait_for_function("""mode => {
      const rta = document.getElementById('btn-mode-rta');
      const sdr = document.getElementById('btn-mode-sdr');
      const actual = sdr.classList.contains('active') ? 'sdr'
        : rta.classList.contains('active') ? 'rta' : 'std';
      return actual === mode && !rta.disabled && !sdr.disabled
        && !rta.classList.contains('pending') && !sdr.classList.contains('pending');
    }""", arg=mode)


def wait_data(page, mode: str) -> None:
    # Counters advance only after the frontend accepts data for the current display.
    key = 'swpFrames' if mode == 'std' else 'rtaFrames'
    before = page.eval_on_selector('#spectrum', '(el, key) => Number(el.dataset[key] ?? -1)', key)
    page.wait_for_function("""({key, before}) =>
      Number(document.getElementById('spectrum').dataset[key] ?? -1) > before
    """, arg={'key': key, 'before': before})


def toggle_sdr(page, target: str) -> None:
    page.click('#btn-mode-sdr')
    wait_mode(page, target)
    wait_data(page, target)


def configure(page) -> None:
    """Prove every altered standard-mode control before exercising a reset."""
    wait_mode(page, 'std')
    page.click('[data-trace-tab="1"]')
    expect_panel(page, 'configuration selects T2', {'tab': 'T2'})
    page.select_option('#select-trace-mode', 'AVERAGE')
    page.select_option('#select-trace-avg', '64')
    expect_panel(page, 'configuration shows Average64 row and status',
                 {'mode': 'AVERAGE', 'avg': '64', 'avgRow': True, 'avgStatus': '64'})
    page.select_option('#select-smooth', '9')
    expect_panel(page, 'configuration sets Smooth9', {'smooth': '9'})
    page.select_option('#select-refwin', '5')
    expect_panel(page, 'configuration sets reference window5', {'refwin': '5'})
    wait_data(page, 'std')
    page.click('#btn-normalize')
    expect_panel(page, 'configuration normalizes populated T2', {'norm': True})
    page.click('#btn-view-freeze')
    expect_panel(page, 'configuration freezes normalized T2', {'freeze': True, 'norm': True})


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default=None, help='an already-running native backend (default: native fixture)')
    args = parser.parse_args()

    server = None
    url = args.url
    if not url:
        server, url = start_service(free_port())
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            ctx = browser.new_context()
            page = ctx.new_page()
            page.set_default_timeout(15000)
            errors: list[str] = []
            page.on('pageerror', lambda e: errors.append(str(e)))
            page.goto(url, wait_until='domcontentloaded')
            wait_mode(page, 'std')
            wait_data(page, 'std')
            expect_panel(page, 'fresh standard page shows factory trace defaults', DEFAULTS)

            configure(page)
            page.click('#btn-preset')
            expect_panel(page, 'standard Preset restores the factory trace panel', DEFAULTS)

            configure(page)
            page.reload(wait_until='domcontentloaded')
            wait_mode(page, 'std')
            wait_data(page, 'std')
            expect_panel(page, 'standard reload restores CLEAR_WRITE16', DEFAULTS)

            toggle_sdr(page, 'sdr')
            expect_panel(page, 'first SDR entry shows AVERAGE16', SDR_DEFAULTS)
            for depth, label in (('64', '64'), ('0', 'infinity')):
                page.select_option('#select-trace-avg', depth)
                expected = {'mode': 'AVERAGE', 'avg': depth, 'avgRow': True}
                expect_panel(page, f'SDR accepts custom {label} depth', expected)
                if depth == '64':
                    expect_panel(page, 'SDR64 status matches depth', {'avgStatus': '64'})
                else:
                    page.wait_for_function("() => document.getElementById('trace-avg-status').textContent.startsWith('∞ (')")
                    check('SDR infinity status matches depth',
                          page.locator('#trace-avg-status').inner_text().startswith('∞ ('))
                toggle_sdr(page, 'std')
                expect_panel(page, f'leaving SDR{label} restores standard trace mode',
                             {'mode': 'CLEAR_WRITE', 'avg': '16', 'avgRow': False})
                toggle_sdr(page, 'sdr')
                expect_panel(page, f'SDR {label} depth survives family return', expected)
                if depth == '64':
                    expect_panel(page, 'returned SDR64 status matches depth', {'avgStatus': '64'})
                else:
                    page.wait_for_function("() => document.getElementById('trace-avg-status').textContent.startsWith('∞ (')")
                    check('returned SDR infinity status matches depth',
                          page.locator('#trace-avg-status').inner_text().startswith('∞ ('))

            page.click('[data-trace-tab="1"]')
            expect_panel(page, 'SDR reset setup selects T2', {'tab': 'T2'})
            page.select_option('#select-trace-mode', 'AVERAGE')
            page.select_option('#select-trace-avg', '64')
            expect_panel(page, 'SDR reset setup changes T2 to Average64',
                         {'mode': 'AVERAGE', 'avg': '64', 'avgRow': True, 'avgStatus': '64'})
            page.click('#btn-view-freeze')
            expect_panel(page, 'SDR reset setup freezes T2', {'freeze': True})
            page.click('#btn-preset')
            wait_mode(page, 'sdr')
            expect_panel(page, 'SDR Preset restores T1 CLEAR_WRITE16', DEFAULTS)

            # Preset clears the saved mode; real mode actions establish a saved SDR session.
            toggle_sdr(page, 'std')
            toggle_sdr(page, 'sdr')
            page.wait_for_function("() => localStorage.getItem('web-sa-mode') === 'sdr'")
            check('SDR reload setup saved SDR mode',
                  page.evaluate("localStorage.getItem('web-sa-mode')") == 'sdr')

            page.reload(wait_until='domcontentloaded')
            wait_mode(page, 'sdr')
            wait_data(page, 'sdr')
            expect_panel(page, 'saved SDR reload restores AVERAGE16', SDR_DEFAULTS)
            toggle_sdr(page, 'std')
            page.reload(wait_until='domcontentloaded')
            wait_mode(page, 'std')
            wait_data(page, 'std')
            expect_panel(page, 'saved standard reload restores CLEAR_WRITE16', DEFAULTS)

            check('no page errors', not errors, '; '.join(errors[:3]))
            browser.close()
    finally:
        stop(server)

    print(f"\n{'ALL PASS' if not failures else 'FAILURES: ' + ', '.join(failures)}")
    return 0 if not failures else 1


if __name__ == '__main__':
    raise SystemExit(main())
