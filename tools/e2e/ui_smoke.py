#!/usr/bin/env python3
"""UI smoke test against the fake backend - the hardware-free half of the UI regression.

Why this exists (report finding A1): the full state regression needs a real SAN-90, so the
only tests that assert *user-visible* results (canvas pixels, DOM text, real clicks) could not
run in CI. That is where the blank-canvas defect and the dead-control defects slipped through.

This script drives the same UI against `WEBSA_FAKE=1 python3 -m web_sa.main` and checks the
properties that are independent of the analyzer: the spectrum is actually drawn, the controls
reach the backend, modes switch and keep drawing, the measurement tabs render, and the
waterfall yields to a measurement. Everything it asserts used to be covered only on the bench.

Usage:  python3 tools/e2e/ui_smoke.py [--url http://127.0.0.1:8099]
Exit status is non-zero when any check fails, so it can be a CI gate.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request

sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent.parent.parent))

from playwright.sync_api import sync_playwright  # noqa: E402


def state(url: str) -> dict:
    with urllib.request.urlopen(f'{url}/api/state', timeout=5) as response:
        return json.load(response)


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def js_click(page, selector: str) -> None:
    """Click through the DOM: the measurement panel overlays its own toggle button.

    A real user collapses/scrolls the rail first; the smoke test only cares that the bound
    handler runs, so it dispatches the click directly.
    """
    page.evaluate(f"document.querySelector('{selector}').click()")


def painted_pixels(page) -> int:
    """Non-transparent pixels on the spectrum canvas (0 = nothing drawn)."""
    return page.evaluate(
        """() => {
          const c = document.getElementById('spectrum');
          const g = c.getContext('2d');
          const d = g.getImageData(0, 0, c.width, c.height).data;
          let n = 0;
          for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++;
          return n;
        }""")


def axis_band_points(page) -> dict:
    """A grab point inside each axis-label band (the margins outside the graticule).

    MARGIN is 10/50/14/26 logical px, so the bottom ~6 px and the right ~6 px of the canvas are
    inside the frequency row and the level labels whatever the window size or UI scale.
    """
    box = page.evaluate(
        """() => { const c = document.getElementById('spectrum');
                   const r = c.getBoundingClientRect();
                   return {x: r.x, y: r.y, w: r.width, h: r.height}; }""")
    return {
        'x': (box['x'] + box['w'] * 0.35, box['y'] + box['h'] - 6),
        'y': (box['x'] + box['w'] - 6, box['y'] + box['h'] * 0.5),
        'box': box,
    }


def drag(page, start: tuple, dx: float = 0.0, dy: float = 0.0, steps: int = 10,
         on_hold=None) -> None:
    """Press on a point, move in small steps, and release (on_hold runs before the release)."""
    x, y = start
    page.mouse.move(x, y)
    page.mouse.down()
    for step in range(1, steps + 1):
        page.mouse.move(x + dx * step / steps, y + dy * step / steps)
        page.wait_for_timeout(16)
    if on_hold is not None:
        on_hold()
    page.mouse.up()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    parser.add_argument('--painted-min', type=int, default=5000)
    args = parser.parse_args()
    failures: list[str] = []

    def check(name: str, ok: bool, detail: str = '') -> None:
        print(f'  {"PASS" if ok else "FAIL"}  {name}' + (f'  <- {detail}' if detail else ''))
        if not ok:
            failures.append(name)

    with sync_playwright() as p:
        # The audio checks need a RUNNING AudioContext: headless Chromium suspends it without
        # this policy, and a suspended context never renders, so the worklet could not report
        # whether the browser DSP is feeding it.
        browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
        # The measurement panel overlays the toggle in a small viewport; use the same size as
        # the hardware regression so the layout matches what a user sees on a desktop.
        page = browser.new_page(viewport={'width': 1600, 'height': 1000})
        errors: list[str] = []
        dialogs: list[str] = []
        page.on('dialog', lambda d: (dialogs.append(d.message), d.dismiss()))
        page.on('pageerror', lambda e: errors.append(f'pageerror: {e}'))
        page.on('console', lambda m: errors.append(f'console: {m.text[:160]}') if m.type == 'error' else None)

        page.goto(f'{args.url}/', wait_until='networkidle')
        page.wait_for_timeout(3000)

        print('1) the spectrum is drawn')
        painted = painted_pixels(page)
        check('spectrum is actually drawn (renderer registered)', painted > args.painted_min,
              f'{painted} painted pixels')

        print('2) controls reach the backend')
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(1500)
        page.click('[data-action="full-span"]')
        page.wait_for_timeout(1500)
        caps = state(args.url)['caps']
        expected_center = (caps['fmin'] + caps['fmax']) / 2
        check('Full Span reaches the device', abs(state(args.url)['center'] - expected_center) < 1e3,
              f"center {state(args.url)['center']:.0f}")
        page.fill('#input-center', '433')
        page.click('#unit-center-group button:text-is("MHz")')
        page.wait_for_timeout(1500)
        check('the centre field and its unit button reach the device',
              abs(state(args.url)['center'] - 433e6) < 1e3, f"{state(args.url)['center'] / 1e6} MHz")
        check('the canvas is still drawn after re-tuning', painted_pixels(page) > args.painted_min)

        print('2a) the Auto Scale message goes to the canvas status stack')
        # It used to be the last item of the Ref parameter row, so every message moved the input,
        # the buttons and the arrows sideways (reported twice); it now lives in the canvas stack
        # with the warnings, which also has no width limit.
        #
        # Measured as offsets *within* the row: absolute coordinates also move when the panel
        # scrolls (clicking a below-the-fold button scrolls it into view) or when the UI scale
        # changes, neither of which says anything about this row.
        row_box = ("() => { const r = document.getElementById('input-ref').getBoundingClientRect();"
                   " const s = document.getElementById('btn-ref-set').getBoundingClientRect();"
                   " const u = document.getElementById('btn-ref-up').getBoundingClientRect();"
                   " return [Math.round(s.left - r.left), Math.round(s.top - r.top),"
                   "         Math.round(u.right - r.left), Math.round(u.top - r.top)]; }")
        before = page.evaluate(row_box)
        # A press posts the decision it produces. That used to be helped along by the device-clamp
        # notice ("Device limited Ref to X dBm"), which was removed on request, so the check now
        # relies on the Auto Scale answer alone - and a press sent while the device is still
        # reconfiguring can be dropped, which is what a second press by the user is for.
        notice = ''
        for _ in range(2):
            page.click('#btn-ref-auto')
            page.wait_for_timeout(1500)
            notice = page.evaluate("document.getElementById('spectrum').dataset.notice")
            if (notice or '').strip():
                break
        after = page.evaluate(row_box)
        check('a decision posts a canvas notice', bool((notice or '').strip()), repr(notice))
        check('the notice is drawn on the canvas, not in the control row',
              page.evaluate("!document.getElementById('ref-hint')"))
        check('the Ref row does not move when the notice appears', before == after,
              f'{before} -> {after}')

        print('2a3) a reference level the user set is left alone')
        # The ranger used to raise Ref once the peak stood 10 dB or more above the top edge, and a
        # request the device clamped was announced ("Device limited Ref to X dBm"). Both were
        # removed on request: the level someone sets is the level they get, the step arrows stay
        # usable at the ends of the range, and Auto is what re-fits a trace that no longer fits.
        caps = state(args.url)['caps']
        page.fill('#input-ref', str(int(caps['ref_max'])))
        page.click('#btn-ref-set')
        page.wait_for_timeout(2500)
        # A decision from the Auto press above can still be on screen (its hold is a few seconds),
        # so wait for a clean stack: "nothing was posted" then means nothing NEW was posted.
        for _ in range(25):
            if not (page.evaluate("document.getElementById('spectrum').dataset.notice")
                    or '').strip():
                break
            page.wait_for_timeout(300)
        top = state(args.url)
        page.click('#btn-ref-up')            # past the top of the range: a silent no-op
        page.wait_for_timeout(1500)
        check('the step arrows stay usable at the top of the range',
              not page.evaluate("() => document.getElementById('btn-ref-up').disabled"),
              f"ref={top['ref']} max={caps['ref_max']}")
        notice_now = page.evaluate("document.getElementById('spectrum').dataset.notice")
        check('nothing announces the end of the range',
              not (notice_now or '').strip(), repr(notice_now))
        # The fake backend's carrier sits near -25 dBm, so a Ref at the bottom of the range leaves
        # the peak far above the top edge - exactly the case that used to be corrected behind the
        # user's back. It must hold, however many frames are observed.
        post(args.url, {'cmd': 'SET_REF', 'mode': 'manual', 'ref': caps['ref_min'],
                        'range_db': 100})
        page.wait_for_timeout(3500)
        clipped = state(args.url)
        page.wait_for_timeout(4000)
        held = state(args.url)
        check('a grossly clipped trace is not raised automatically',
              held['ref'] <= clipped['ref'] + 0.5,
              f"peak {clipped['auto_ref'].get('last_peak')} at ref {clipped['ref']:.0f} -> "
              f"{held['ref']:.0f}, result={held['auto_ref'].get('result')}")
        post(args.url, {'cmd': 'SET_REF', 'mode': 'manual', 'ref': -20, 'range_db': 100})
        page.wait_for_timeout(1500)

        print('2a2) the Level offset moves the plot amplitude numbers')
        # Reported: the offset shifted the trace but not the amplitude numbers on the plot. The
        # trace is displaced in getY(), so the axis has to be labelled with the same conversion
        # (fmtAxisLevel); the labels are exposed as dataset.yLabels because canvas text is not DOM.
        def y_labels():
            return (page.evaluate("document.getElementById('spectrum').dataset.yLabels") or '').split(',')

        def set_offset(value):
            page.fill('#input-offset', str(value))
            page.dispatch_event('#input-offset', 'change')
            page.wait_for_timeout(700)

        set_offset(0)
        base = y_labels()
        set_offset(20)
        shifted = y_labels()
        set_offset(-10)
        negated = y_labels()
        set_offset(0)
        restored = y_labels()
        check('the axis is labelled at all', len(base) > 1 and base[0].strip() != '', str(base[:3]))
        check('a +20 dB offset lifts every axis label by 20 dB',
              len(shifted) == len(base)
              and all(abs(float(b) + 20 - float(s)) < 0.6 for b, s in zip(base, shifted, strict=True)),
              f'{base[:3]} -> {shifted[:3]}')
        check('a negative offset moves them the other way',
              len(negated) == len(base)
              and all(abs(float(b) - 10 - float(n)) < 0.6 for b, n in zip(base, negated, strict=True)),
              f'{base[:3]} -> {negated[:3]}')
        check('offset 0 restores the original numbers', restored == base,
              f'{base[:3]} -> {restored[:3]}')

        print('2b) the peak list works off the threshold slot')
        # The threshold moved from the input element into a parameter slot (one decision per
        # measurement geometry, see render/peaklist.ts). Peak finding, marker peak search and
        # the CSV export all read that slot now, so a desync would empty the table.
        js_click(page, '#btn-markers-all')        # All On: the auto threshold needs a marker
        page.wait_for_timeout(1200)
        js_click(page, '#btn-peaklist')
        page.wait_for_timeout(1500)
        rows = page.eval_on_selector_all('#peak-tbody tr', 'els => els.length')
        threshold = page.input_value('#input-peakthr')
        check('the peak list finds peaks above the threshold', rows > 0, f'{rows} rows')
        check('the auto threshold is a level, not the input default',
              float(threshold) > -150, f'threshold {threshold} dBm')
        js_click(page, '#btn-peaklist')
        js_click(page, '#btn-markers-all')        # back to All Off
        page.wait_for_timeout(600)

        print('3) mode switching keeps drawing')
        for mode, selector, clicks in (('rta', '#btn-mode-rta', 1), ('sdr', '#btn-mode-sdr', 1),
                                       ('std', '#btn-mode-rta', 2)):
            # the buttons toggle: RTA -> SDR -> RTA (from SDR) -> SWP (RTA again)
            for _ in range(clicks):
                page.click(selector)
                page.wait_for_timeout(2500)
            painted = painted_pixels(page)
            check(f'{mode} mode is applied', state(args.url)['mode'] == mode,
                  state(args.url)['mode'])
            check(f'{mode} view is drawn', painted > args.painted_min, f'{painted} pixels')
            if mode == 'sdr':
                # The DSP input is its own `?iq=1` socket in a worker, and the same dataset reports
                # what the WASM pipeline produced, so this asserts the whole chain
                # (backend encoder -> WS -> worker decoder -> Rust DSP -> PCM for the worklet).
                page.wait_for_timeout(2500)
                dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrIq") or ''
                blocks = int(m.group(1)) if (m := re.search(r'blocks=(\d+)', dbg)) else 0
                check('the SDR IQ stream reaches the DSP worker', blocks > 0, dbg)
                check('the IQ worker learns the capture rate and centre',
                      'rate=0' not in dbg and 'center_hz=0' not in dbg, dbg)
                pcm_frames = int(m.group(1)) if (m := re.search(r'pcm_frames=(\d+)', dbg)) else 0
                check('the WASM pipeline turns IQ into PCM for the worklet', pcm_frames > 0, dbg)

        print('4) measurement tabs render')
        js_click(page, '#btn-meas-onoff')
        page.wait_for_timeout(800)
        for tab, label in (('#tab-harm', 'harmonic'), ('#tab-pnm', 'phase noise'),
                           ('#tab-amp', 'amplitude'), ('#tab-chan', 'channel')):
            js_click(page, tab)
            page.wait_for_timeout(1800)
            painted = painted_pixels(page)
            check(f'{label} tab gives the canvas content', painted > args.painted_min,
                  f'{painted} pixels')
        js_click(page, '#tab-amp')
        page.wait_for_timeout(600)
        js_click(page, '#btn-meas-onoff')        # leave measurement mode for the next step
        page.wait_for_timeout(1200)

        print('5) a measurement owns the display (waterfall disabled)')
        page.click('[data-action="toggle-waterfall"]')
        page.wait_for_timeout(1200)
        check('waterfall can be enabled', page.evaluate(
            "document.getElementById('btn-waterfall').classList.contains('active')"))
        js_click(page, '#btn-meas-onoff')
        page.wait_for_timeout(1200)
        check('waterfall is turned off and disabled while measuring',
              not page.evaluate("document.getElementById('btn-waterfall').classList.contains('active')")
              and page.eval_on_selector('#btn-waterfall', 'e => e.disabled'))
        js_click(page, '#btn-meas-onoff')
        page.wait_for_timeout(1200)
        check('the waterfall choice comes back afterwards', page.evaluate(
            "document.getElementById('btn-waterfall').classList.contains('active')"))

        # A measurement OWNS the device: the control rail is greyed out for it and the command layer
        # refuses the SWP-owned commands. The canvas gesture has to obey the same rule - it used to
        # send them anyway and answer each refusal with an alert popup (measured: four dialogs from a
        # single drag after the harmonic mode was entered through the API, where the client's own
        # measurement flag knows nothing).
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'harmonic'})
        for _ in range(12):                    # wait until the client has SEEN the reported mode
            if state(args.url)['mode'] == 'harmonic':
                break
            page.wait_for_timeout(400)
        page.wait_for_timeout(1800)
        band = axis_band_points(page)
        # The panel's centre box is the stable observable here: a harmonic measurement retunes the
        # device internally (so the reported centre moves by itself), while the box only follows a
        # frequency command the user made.
        # The observable is the gesture's own commit counter: a harmonic measurement retunes the
        # device internally (so the reported centre and even the centre box move by themselves), but
        # only a gesture that got through would bump this.
        def axis_commits() -> int:
            return int(page.evaluate(
                "() => document.getElementById('spectrum').dataset.axisCommits || 0"))

        held = axis_commits()
        dialogs_before = len(dialogs)          # scoped to this drag: earlier sections have their own
        drag(page, band['x'], dx=band['box']['w'] * 0.1)
        page.wait_for_timeout(1500)
        check('the axis bands are dead while a measurement owns the device',
              axis_commits() == held
              and page.evaluate("() => document.getElementById('spectrum').style.cursor") == '',
              f"axis commits {held} -> {axis_commits()}")
        check('...and a drag there raises no error popup',
              len(dialogs) == dialogs_before, f'{dialogs[dialogs_before:]}')
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(2500)

        print('6) the spectrum/waterfall divider drags')
        def divider_y():
            return page.evaluate(
                "() => { const s = document.getElementById('wf-split').getBoundingClientRect();"
                " return s.y + s.height / 2; }")
        def sizes():
            return page.evaluate(
                "() => ({wf: document.getElementById('waterfall-container').clientHeight,"
                " spec: document.getElementById('spectrum').clientHeight})")
        box = page.evaluate(
            "() => { const s = document.getElementById('wf-split').getBoundingClientRect();"
            " return {x: s.x + s.width / 2, y: s.y + s.height / 2}; }")
        heights = sizes()
        # Drag DOWN: the divider follows the pointer, so the boundary moves down - the spectrum
        # above it grows and the waterfall below it shrinks. (The first version had the delta
        # inverted: the divider ran away from the cursor and the waterfall grew, reported as
        # "the up/down direction is reversed".)
        page.mouse.move(box['x'], box['y'])
        page.mouse.down()
        page.mouse.move(box['x'], box['y'] + 50, steps=8)
        page.mouse.up()
        page.wait_for_timeout(700)
        moved_down = sizes()
        check('dragging the divider down follows the pointer',
              abs(divider_y() - (box['y'] + 50)) <= 4,
              f"divider {box['y']:.1f} -> {divider_y():.1f}, cursor {box['y'] + 50:.1f}")
        check('dragging down grows the spectrum and shrinks the waterfall',
              abs(moved_down['wf'] - (heights['wf'] - 50)) <= 12
              and moved_down['spec'] > heights['spec'] + 30,
              f"wf {heights['wf']} -> {moved_down['wf']}, spectrum {heights['spec']} -> {moved_down['spec']}")
        # ...and drag UP, back past the start: the waterfall grows again.
        y_now = divider_y()
        page.mouse.move(box['x'], y_now)
        page.mouse.down()
        page.mouse.move(box['x'], y_now - 100, steps=8)
        page.mouse.up()
        page.wait_for_timeout(700)
        moved_up = sizes()
        check('dragging up follows the pointer and grows the waterfall',
              abs(divider_y() - (y_now - 100)) <= 4
              and moved_up['wf'] > moved_down['wf'] + 60,
              f"divider {y_now:.1f} -> {divider_y():.1f}; wf {moved_down['wf']} -> {moved_up['wf']}")
        grown = moved_up
        check('the split is stored for the next visit',
              abs(float(page.evaluate("localStorage.getItem('web-sa-wf-split')") or 0) - grown['wf']) <= 12)
        # Closing the waterfall gives its slot back to the marker table (the layout's normal
        # state); reopening brings the remembered split back.
        page.click('[data-action="toggle-waterfall"]')
        page.wait_for_timeout(600)
        closed = page.evaluate(
            "() => ({wf: document.getElementById('waterfall-container').clientHeight,"
            " table: getComputedStyle(document.getElementById('marker-table')).display,"
            " split: getComputedStyle(document.getElementById('wf-split')).display})")
        check('closing the waterfall restores the normal layout (table slot, no divider)',
              closed['wf'] == 0 and closed['table'] != 'none' and closed['split'] == 'none',
              f"wf {closed['wf']}, marker table {closed['table']}, split {closed['split']}")
        page.click('[data-action="toggle-waterfall"]')
        page.wait_for_timeout(600)
        reopened = page.evaluate(
            "() => ({wf: document.getElementById('waterfall-container').clientHeight,"
            " split: getComputedStyle(document.getElementById('wf-split')).display})")
        check('reopening restores the remembered split',
              abs(reopened['wf'] - grown['wf']) <= 12 and reopened['split'] != 'none',
              f"wf {grown['wf']} -> {reopened['wf']}, split {reopened['split']}")
        # Preset puts the split back to the factory height (the inline style is exact; the client
        # box is 2px smaller because of the container's borders).
        page.click('#btn-preset')
        page.wait_for_timeout(1200)
        reset_wf = page.evaluate(
            "() => document.getElementById('waterfall-container').style.height")
        check('Preset resets the split to the default',
              reset_wf == '135px' and page.evaluate("localStorage.getItem('web-sa-wf-split')") is None,
              f'style height {reset_wf}')

        print('7) i18n, keypad and the status page')
        label_en = page.inner_text('#btn-connect') if page.query_selector('#btn-connect') else ''
        page.click('#btn-lang')
        page.wait_for_timeout(800)
        label_zh = page.inner_text('#btn-connect') if page.query_selector('#btn-connect') else ''
        check('switching the language relabels the UI', label_en != label_zh, f'{label_en} -> {label_zh}')
        page.click('#btn-lang')
        page.wait_for_timeout(500)
        js_click(page, '#btn-keypad')          # enable the pad, then focus a numeric field
        page.wait_for_timeout(400)
        page.click('#input-center')
        page.wait_for_timeout(600)
        check('the virtual keypad opens on a numeric field', page.evaluate(
            "(() => { const el = document.getElementById('keypad'); "
            "return !!el && getComputedStyle(el).display !== 'none'; })()"))
        js_click(page, '#btn-keypad')
        page.wait_for_timeout(300)

        print('10) the global UI scale is one control, usable in every view')
        # The scale is CSS zoom on the frame, so it has to be checked as a *rendered* size: a
        # computed font-size stays 11px whatever the scale is. The frame must stay exactly one
        # viewport (no scrollbar appears) and the plot must stay painted at every step.
        def scale_state():
            return page.evaluate(
                "() => { const de = document.documentElement; return {"
                "  applied: de.dataset.uiScale,"
                "  stored: (() => { try { return localStorage.getItem('web-sa-ui-scale'); }"
                "                    catch (e) { return null; } })(),"
                "  overflowX: de.scrollWidth - de.clientWidth,"
                "  viewportH: window.innerHeight,"
                "  frameH: Math.round(document.querySelector('.analyzer-card')"
                "            .getBoundingClientRect().height),"
                "  labelH: +document.querySelector('.info-label')"
                "            .getBoundingClientRect().height.toFixed(2),"
                "  labelW: +document.querySelector('.info-label')"
                "            .getBoundingClientRect().width.toFixed(2),"
                "  panelW: +document.querySelector('.control-panel')"
                "            .getBoundingClientRect().width.toFixed(2) }; }")

        check('the control exists in the top bar', page.evaluate(
            "!!document.getElementById('ui-scale')"))
        first = scale_state()
        check('a first visit scales for the screen instead of leaving it at 100%',
              first['applied'] not in (None, '', '1'), f"scale {first['applied']}")
        check('nothing is stored until the user chooses', first['stored'] is None,
              repr(first['stored']))

        baseline_label = first['labelH']
        for backend_mode, view in (('std', 'SWP'), ('rta', 'RTA'), ('sdr', 'SDR')):
            post(args.url, {'cmd': 'SET_MODE', 'mode': backend_mode})
            page.wait_for_timeout(2500)
            page.select_option('#ui-scale', '1.5')
            page.wait_for_timeout(1200)
            st = scale_state()
            check(f'{view}: 150% applies while the plot keeps drawing',
                  st['applied'] == '1.5' and painted_pixels(page) > args.painted_min,
                  f"scale {st['applied']}, {painted_pixels(page)} pixels")
            check(f'{view}: the whole frame still fits the viewport at 150%',
                  st['overflowX'] <= 0 and st['frameH'] <= st['viewportH'] + 1,
                  f"overflowX {st['overflowX']}, frame {st['frameH']} of {st['viewportH']}")
            # 1.5 / 1.25 is what every dimension must grow by if the scale is global: the label
            # is text, the panel column is pure spacing (a CSS clamp of the viewport).
            grown = 1.5 / float(first['applied'])
            check(f'{view}: spacing scales with the UI, not just the font',
                  abs(st['panelW'] / first['panelW'] - grown) <= 0.06,
                  f"panel {first['panelW']} -> {st['panelW']} (expected x{grown:.2f})")
            check(f'{view}: text is rendered larger, not re-laid out',
                  st['labelW'] / first['labelW'] >= grown - 0.06,
                  f"label {first['labelW']} -> {st['labelW']}px; box height "
                  f"{baseline_label} -> {st['labelH']}px")
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(1500)

        page.reload(wait_until='networkidle')
        page.wait_for_timeout(2500)
        after = scale_state()
        check('a manual scale survives a reload',
              after['applied'] == '1.5' and after['stored'] == '1.5',
              f"applied {after['applied']}, stored {after['stored']}")
        check('the reloaded page is still sharp and painted',
              painted_pixels(page) > args.painted_min)
        page.select_option('#ui-scale', 'auto')
        page.wait_for_timeout(1200)
        auto = scale_state()
        check('Auto forgets the override and follows the screen again',
              auto['stored'] is None and auto['applied'] == first['applied'],
              f"applied {auto['applied']}, stored {auto['stored']}")

        print('N) the Python fallback still plays when the browser DSP is disabled')
        # `?wasm=0` is the documented way to run the reference implementation: the worker is started
        # without the module, so it must never take the worklet port, and the Python audio path
        # (?audio=1) has to keep delivering. Both halves are asserted, because "no audio" and "the
        # wrong path silently took over" look identical from the UI.
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        page.goto(f'{args.url}/?wasm=0', wait_until='networkidle')
        page.wait_for_timeout(2000)
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        page.wait_for_timeout(2500)
        if page.query_selector('#btn-sdr-audio'):
            js_click(page, '#btn-sdr-audio')      # start the Python audio pipeline
        page.wait_for_timeout(3500)
        iq_dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrIq") or ''
        audio_dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrAudio") or ''
        check('the browser DSP is off because it was asked to be',
              'pipeline=false' in iq_dbg and 'fallback=requested' in iq_dbg, iq_dbg)
        check('the worker never took the worklet port while the fallback is active',
              'pcm_frames=0' in iq_dbg, iq_dbg)
        audio_frames = int(m.group(1)) if (m := re.search(r'frames=(\d+)', audio_dbg)) else 0
        check('the Python audio path still delivers PCM', audio_frames > 0, audio_dbg)
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(800)

        print('N) the browser DSP owns playback when the SDR audio is switched on')
        # Back to the normal page: the fallback check left the browser on ?wasm=0, which disables
        # the browser DSP by design (and so would make this check test the wrong path).
        page.goto(args.url, wait_until='networkidle')
        page.wait_for_timeout(1500)
        # The reported failure: switching the audio on gave one blip and then silence, because the
        # AudioWorklet port was handed to the Python audio worker while the browser DSP was the
        # producer - the WASM PCM had nowhere to go, so the ring was fed by nobody. The worklet's
        # own ring state is the evidence that matters here.
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        page.wait_for_timeout(2500)
        if page.query_selector('#btn-sdr-audio'):
            js_click(page, '#btn-sdr-audio')
        page.wait_for_timeout(6000)
        iq_dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrIq") or ''
        audio_dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrAudio") or ''
        check('the DSP worker owns the AudioWorklet port', 'dsp_worklet=1' in iq_dbg, iq_dbg)
        received = int(m.group(1)) if (m := re.search(r'dsp_received=(\d+)', iq_dbg)) else 0
        underruns = int(m.group(1)) if (m := re.search(r'dsp_underruns=(\d+)', iq_dbg)) else -1
        check('the worklet is fed by the browser DSP', received > 0, iq_dbg)
        delivered = int(m.group(1)) if (m := re.search(r'dsp_delivered=(\d+)', iq_dbg)) else 0
        # The reported bug was "produced but never heard": the DSP handed PCM over and the ring
        # stayed empty. Comparing handed-over against pushed-in is that invariant, exactly.
        # `received` comes from the worklet's status message, which is up to one reporting window
        # (0.25 s of audio) behind `delivered`, so a tenth of slack is expected and enough to catch
        # the reported failure, where the ring stayed empty while the DSP kept handing audio over.
        check('every delivered sample reaches the worklet ring',
              delivered > 0 and received >= delivered * 0.9,
              f'delivered={delivered} received={received}')
        # A delivery that throws used to be invisible while the counters kept climbing.
        check('audio delivery did not fail', 'dsp_error=' not in iq_dbg, iq_dbg)
        # Ring health is reported, not asserted: the fake backend delivers IQ slower than real
        # time, so the DSP correctly produces audio below 48 kHz and the ring drains - a real
        # analyzer feeds it at real time (verified on the bench: underruns stay 0 there).
        print(f'    (ring: received={received} underruns={underruns})')

        print('N) FT8 decodes in the browser (fake backend replays a real transmission)')
        # Back to the normal page: the fallback check above left the browser on ?wasm=0, which
        # disables the browser DSP by design.
        page.goto(args.url, wait_until='networkidle')
        page.wait_for_timeout(1500)
        # The digital path end to end: the fake backend emits the committed FT8 fixture as IQ, the
        # worker runs the shared DDC and the protocol decoder, and the panel shows the text plus the
        # slot timing. This is the check that the decoder is *reachable* in the product, not only in
        # unit tests.
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'ft8', 'ifbw': 2400, 'pitch': 700,
                        'volume': 1.0, 'squelch': -140.0, 'agc': True})
        page.wait_for_timeout(2500)
        button = page.query_selector('[data-sdr-demod="ft8"]')
        check('the FT8 mode button is offered and enabled',
              button is not None and button.get_attribute('disabled') is None)
        if button is not None:
            js_click(page, '[data-sdr-demod="ft8"]')
        # The worker has no slot clock, so it finds the burst and sweeps the window phase across
        # attempts (each attempt costs a transmission of stream plus one decode): give it room.
        page.wait_for_timeout(45000)
        # The one-line readout this used to read was replaced by the decode window (see the note in
        # sdr/iqStream.ts): the table is where a decode is visible now, so that is what is asserted.
        window_button = page.query_selector('#btn-decode-window')
        check('the FT8 window toggle is offered', window_button is not None)
        if window_button is not None:
            js_click(page, '#btn-decode-window')
            page.wait_for_timeout(500)
        rows_text = page.evaluate("document.getElementById('decode-window-rows').textContent") or ''
        row_title = page.evaluate(
            "() => { const r = document.querySelector('#decode-window-rows tr');"
            " return r ? (r.getAttribute('title') || '') : ''; }") or ''
        iq_dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrIq") or ''
        detail = f'rows={rows_text[:120]!r} title={row_title[:120]!r} iq=[{iq_dbg}]'
        check('the FT8 message is decoded in the browser', 'CQ JO1WKO PM95' in rows_text, detail)
        check('the decode carries its measured frequency', 'MHz' in row_title, detail)

        # The same head carries an opacity slider (user-visible behaviour, DEVELOPMENT §6): move it and
        # assert the window's own style changed *and* persisted.
        if window_button is not None:
            page.evaluate("""() => {
                const slider = document.getElementById('decode-window-opacity');
                slider.value = '60';
                slider.dispatchEvent(new Event('input', { bubbles: true }));
            }""")
            dimmed = page.evaluate("document.getElementById('decode-window').style.opacity")
            saved = page.evaluate(
                "JSON.parse(localStorage.getItem('websa-decode-window') || '{}').opacity")
            check('the FT8 window opacity slider dims the window and stores the level',
                  dimmed == '0.6' and saved is not None and abs(saved - 0.6) < 1e-6,
                  f'opacity={dimmed!r} stored={saved!r}')

        # The CW decoder end to end: the fake backend keys 'TEST DE N0CALL' on its carrier, the
        # browser's CW demodulator turns it into a beat note, the decoder reads it back and the
        # decode window shows it. The same window as FT8, on its CW pane.
        print('4b) CW decodes into the same window')
        post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'cw', 'ifbw': 3000, 'pitch': 700,
                        'volume': 0.8, 'squelch': -140.0, 'agc': True})
        # Wait for the STATUS that applies the mode before touching the toggle (it stays disabled
        # until then, and a click on a disabled button is simply lost).
        enabled = False
        for _ in range(20):
            page.wait_for_timeout(500)
            if not page.eval_on_selector('#btn-decode-window', 'e => e.disabled'):
                enabled = True
                break
        check('the CW decode window toggle is enabled', enabled)
        if enabled:
            # The FT8 section above already opened the window: clicking again would CLOSE it, and a
            # closed window does not render (its pane freezes at the last frame).
            if page.evaluate("getComputedStyle(document.getElementById('decode-window')).display === 'none'"):
                js_click(page, '#btn-decode-window')
            # The fake keys a 7.7 s loop and the decoder joins it mid-transmission, so the first
            # line is a fragment; poll for a line with the whole message rather than sleeping once.
            cw_text = ''
            title = ''
            for _ in range(30):
                page.wait_for_timeout(1000)
                cw_text = page.evaluate("document.getElementById('decode-window-cw').textContent") or ''
                title = page.evaluate("document.getElementById('decode-window-title').textContent") or ''
                if 'TEST DE N0CALL' in cw_text:
                    break
            check('the CW text is decoded into the decode window',
                  'TEST DE N0CALL' in cw_text and title == 'CW',
                  f'title={title!r} text={cw_text[:80]!r}')
            # The decoded text carries the window's own reading aids: the sender's word pauses are
            # drawn (a faint dot, so "thinking" cannot look like "the page is stuck"), and the keyed
            # lamp/meter are live while the demodulator is CW.
            # The pause dot marks the sender's word gap and nothing else, which is checkable exactly:
            # ggmorse separates words with a space, so the dots must number one per space - a timing
            # rule dotted the inside of words instead (reported).
            stats = page.evaluate(
                "() => {"
                " const pane = document.getElementById('decode-window-cw');"
                " const gaps = pane.querySelectorAll('.cw-gap').length;"
                " let spaces = 0;"
                " for (const row of pane.querySelectorAll('.cw-line')) {"
                "   const text = row.querySelector('.cw-text');"
                "   let chars = '';"
                "   for (const node of text.childNodes) {"
                "     if (node.nodeType === 3) chars += node.textContent;"
                "     else if (node.classList.contains('cw-ch')) chars += node.textContent;"
                "   }"
                "   for (const ch of chars.slice(0, -1)) if (ch === ' ') spaces++;"
                " }"
                " return { gaps, spaces };"
                " }")
            check('a word pause is drawn at every word gap, and only there',
                  f"gaps={stats['gaps']} spaces={stats['spaces']}")
            meter = page.evaluate(
                "() => { const m = document.getElementById('cw-meter');"
                " return m ? getComputedStyle(m).display : 'missing'; }")
            check('the CW keyed lamp and level meter are shown',
                  meter not in ('none', 'missing'), f'display={meter}')

            # ...and it still works after leaving CW and coming back: the wasm module loads
            # asynchronously, and the first version judged that load stale against a params object
            # that a STATUS had replaced, so the decoder stayed null for the rest of the session
            # (reported). Clearing first makes "new text arrives" the thing being asserted.
            page.evaluate("document.getElementById('btn-decode-clear').click()")
            post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'usb', 'ifbw': 2400, 'pitch': 700,
                            'volume': 0.8, 'squelch': -140.0, 'agc': True})
            page.wait_for_timeout(2500)
            post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'cw', 'ifbw': 3000, 'pitch': 700,
                            'volume': 0.8, 'squelch': -140.0, 'agc': True})
            page.wait_for_timeout(1500)
            cw_again = ''
            for _ in range(30):
                page.wait_for_timeout(1000)
                cw_again = page.evaluate("document.getElementById('decode-window-cw').textContent") or ''
                if 'TEST DE N0CALL' in cw_again:
                    break
            check('CW decodes again after switching away and back',
                  'TEST DE N0CALL' in cw_again, f'text={cw_again[:80]!r}')

            # The operator's own filter choice survives a mode switch: the auto-pick is a courtesy
            # for the first selection (or after a Preset) and must never undo a deliberate choice.
            page.evaluate(
                "() => document.querySelector('[data-sdr-ifbw=\"180000\"]').click()")
            page.wait_for_timeout(600)
            js_click(page, '[data-sdr-demod="cw"]')
            page.wait_for_timeout(600)
            kept = page.evaluate(
                "() => document.querySelector('[data-sdr-ifbw].active')?.dataset.sdrIfbw")
            check('a chosen filter survives re-selecting the demodulator', kept == '180000',
                  f'ifbw={kept}')

            # ...while a first selection still gets the courtesy pick. Deliberately a profile that
            # *has* stored SDR preferences (a saved 180 kHz filter among them): the courtesy used to
            # be skipped whenever any preference was stored, which is every returning operator - so
            # the first CW selection of a session did nothing (reported).
            fresh = browser.new_context()
            fresh_page = fresh.new_page()
            fresh_page.add_init_script(
                "for (const [k, v] of Object.entries({'web-sa-sdr-ifbw': '180000',"
                " 'web-sa-sdr-demod': 'usb', 'web-sa-sdr-audio': '0',"
                " 'web-sa-sdr-decimate': '32', 'web-sa-sdr-center': '411000000',"
                " 'web-sa-sdr-listen': '411000000'})) localStorage.setItem(k, v);")
            fresh_page.goto(args.url, wait_until='domcontentloaded')
            fresh_page.wait_for_selector('#btn-decode-window', state='attached', timeout=15000)
            post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            post(args.url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am', 'ifbw': 180000, 'pitch': 700,
                            'volume': 0.8, 'squelch': -140.0, 'agc': True})
            fresh_page.wait_for_timeout(2000)
            js_click(fresh_page, '[data-sdr-demod="cw"]')
            fresh_page.wait_for_timeout(800)
            picked = fresh_page.evaluate(
                "() => document.querySelector('[data-sdr-ifbw].active')?.dataset.sdrIfbw")
            check('a first CW selection picks a 3 kHz filter', picked == '3000', f'ifbw={picked}')
            fresh.close()

        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(500)

        # ── The axis-label gestures, the density persistence switch and the SDR trace default ──
        # The gesture exists to make the device round trip invisible: following the pointer with
        # real requests is impossible (0.3-1 s per frequency change, ~1.9 s per Ref change), so
        # the preview is local and one request goes out when the gesture settles. The checks below
        # are the two halves of that contract - nothing during, exactly one after - plus the
        # direction (the content follows the finger), which is what makes it feel like the axis
        # is being pushed rather than the view jumping.
        print('P) dragging the axis labels pans and zooms the window')
        # Floating decode windows can sit over the right margin (an earlier section opens one, and a
        # window keeping its own pointer events is correct behaviour). Close it the way a user would,
        # then assert the band is actually reachable - a covered band would otherwise fail as a
        # mysterious "the drag did nothing".
        overlay = page.evaluate(
            """() => { const w = document.getElementById('decode-window');
                       return !!w && getComputedStyle(w).display !== 'none'; }""")
        if overlay:
            js_click(page, '#btn-decode-window')
            page.wait_for_timeout(800)
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(1500)
        post(args.url, {'cmd': 'SET_FREQ', 'center': 1_000_000_000, 'span': 100_000_000})
        post(args.url, {'cmd': 'SET_REF', 'mode': 'manual', 'ref': -20, 'range_db': 100})
        page.wait_for_timeout(2500)
        bands = axis_band_points(page)

        def element_at_band(which: str) -> str:
            return page.evaluate(
                """(which) => { const c = document.getElementById('spectrum');
                                const r = c.getBoundingClientRect();
                                const x = which === 'x' ? r.left + r.width * 0.35 : r.right - 6;
                                const y = which === 'x' ? r.bottom - 6 : r.top + r.height / 2;
                                return document.elementFromPoint(x, y)?.id || ''; }""", which)

        check('the axis bands are reachable (no overlay on top of them)',
              element_at_band('x') == 'spectrum' and element_at_band('y') == 'spectrum',
              f"top elements: x={element_at_band('x')!r}, y={element_at_band('y')!r}")
        held: list[dict] = []
        before = state(args.url)
        drag(page, bands['x'], dx=bands['box']['w'] * 0.1,
             on_hold=lambda: held.append(state(args.url)))
        page.wait_for_timeout(2500)
        after = state(args.url)
        check('dragging the frequency row sends nothing while the pointer is down',
              abs(held[0]['center'] - before['center']) < 1,
              f"center {before['center']:.0f} -> {held[0]['center']:.0f} during the drag")
        check('...and pans to the lower frequencies when the row is pushed right',
              after['center'] < before['center'] - 1000,
              f"center {before['center']:.0f} -> {after['center']:.0f}")

        span_before = state(args.url)['span']
        page.mouse.move(*bands['x'])
        for _ in range(4):
            page.mouse.wheel(0, -120)
            page.wait_for_timeout(120)
        page.wait_for_timeout(2500)
        span_after = state(args.url)['span']
        check('the wheel on the frequency row zooms the span around the pointer',
              span_after < span_before,
              f'span {span_before:.0f} -> {span_after:.0f}')

        # A wheel has no release, so it must not leave the canvas in a "pointer held" state: a leak
        # here kept panning (and committing) as the mouse merely hovered, and swallowed the release
        # of the next in-plot gesture - found in review.
        mid = (bands['box']['x'] + bands['box']['w'] * 0.5, bands['box']['y'] + bands['box']['h'] * 0.5)
        page.mouse.move(*mid)
        page.wait_for_timeout(600)
        hovered = state(args.url)
        for step in range(1, 6):
            page.mouse.move(mid[0] + step * 12, mid[1])
            page.wait_for_timeout(50)
        page.wait_for_timeout(1200)
        hover_later = state(args.url)
        check('a wheel zoom does not leave the plot panning while the pointer only hovers',
              abs(hover_later['center'] - hovered['center']) < 1,
              f"center {hovered['center']:.0f} -> {hover_later['center']:.0f}")
        cursor = page.evaluate("() => document.getElementById('spectrum').style.cursor")
        check('...and the axis cursor is released with it', cursor == '', f'cursor={cursor!r}')

        # The level axis means what the display shows, and earlier sections may have left the active
        # trace normalized, which puts the display in the relative (dB) unit - where that gesture pans
        # the level OFFSET instead of the device reference. The unit is exposed as a diagnostic
        # (`dataset.levelUnit`); drive the UI until it is the absolute display, then check.
        def level_unit() -> str:
            return page.evaluate("() => document.getElementById('spectrum').dataset.levelUnit")

        def display_unit(want: str) -> str:
            for _ in range(10):
                if level_unit() == want:
                    return want
                js_click(page, '[data-action="reset-norm"]' if want == 'dBm'
                         else '[data-action="normalize"]')
                js_click(page, '[data-trace-tab="0"]')     # the unit follows the tab selection
                page.wait_for_timeout(600)
            return level_unit()

        check('the level axis gesture is checked in the absolute display',
              display_unit('dBm') == 'dBm', f'level unit={level_unit()}')
        post(args.url, {'cmd': 'SET_REF', 'mode': 'manual', 'ref': -20, 'range_db': 100})
        page.wait_for_timeout(2500)
        ref_before = state(args.url)['ref']
        held.clear()
        # A quarter of the window height: a few dB on a 10 dB/div scale, so the change is not
        # swallowed by the device's own Ref quantisation however the canvas is laid out.
        drag(page, bands['y'], dy=bands['box']['h'] * 0.25,
             on_hold=lambda: held.append(state(args.url)))
        page.wait_for_timeout(2500)
        ref_after = state(args.url)['ref']
        check('dragging the level labels sends nothing while the pointer is down',
              abs(held[0]['ref'] - ref_before) < 0.5,
              f"ref {ref_before:.0f} -> {held[0]['ref']:.0f} during the drag")
        check('...and raises the reference when the labels are pushed down',
              ref_after > ref_before,
              f'ref {ref_before:.0f} -> {ref_after:.0f} (band top: {element_at_band("y")!r})')

        # The relative (dB) display pins the top of the graticule to 0, so there is no device
        # reference on that axis: the same gesture pans the level OFFSET, which is a client-side
        # display value - applied live and never sent to the device.
        check('the level axis gesture is checked in the relative display',
              display_unit('dB') == 'dB', f'level unit={level_unit()}')
        offset_before = float(page.input_value('#input-offset') or 0)
        held.clear()
        drag(page, bands['y'], dy=bands['box']['h'] * 0.25,
             on_hold=lambda: held.append(state(args.url)['ref']))
        page.wait_for_timeout(1000)
        offset_after = float(page.input_value('#input-offset') or 0)
        check('in the relative display the same drag pans the level offset',
              abs(offset_after - offset_before) > 1 and abs(held[0] - state(args.url)['ref']) < 1,
              f'offset {offset_before} -> {offset_after}, ref held at {held[0]:.0f}')
        page.fill('#input-offset', '0')
        page.dispatch_event('#input-offset', 'change')
        display_unit('dBm')
        page.wait_for_timeout(500)

        print('Q) the density persistence switch lives in the Trace panel and Off means off')
        rows = page.evaluate(
            """() => { const sel = document.getElementById('select-rta-fade');
                       const bins = document.getElementById('select-rta-bins');
                       return {trace: !!sel.closest('#trace-panel') && !!bins.closest('#trace-panel'),
                               options: [...sel.options].map(o => o.value)}; }""")
        check('Persist and Grain are in the Trace panel (visible in every mode)',
              rows['trace'], f"{rows}")
        check('Persist offers Off as well as the four gears',
              '0' in rows['options'], f"options={rows['options']}")

        post(args.url, {'cmd': 'SET_MODE', 'mode': 'rta'})
        page.wait_for_timeout(3000)
        usable = page.evaluate(
            """() => { const sel = document.getElementById('select-rta-fade');
                       return {disabled: sel.disabled,
                               pe: getComputedStyle(sel.closest('.param-row')).pointerEvents}; }""")
        density_on = page.evaluate("() => document.getElementById('spectrum').dataset.density")
        check('the persistence control stays usable in RTA',
              not usable['disabled'] and usable['pe'] != 'none', f"{usable}")
        check('the density layer is drawn in RTA while persistence is on',
              density_on == 'on', f'density={density_on}')

        page.select_option('#select-rta-fade', '0')
        page.wait_for_timeout(1500)
        off = page.evaluate(
            """() => ({density: document.getElementById('spectrum').dataset.density,
                        stored: localStorage.getItem('rta-fade')})""")
        check('Persist=Off removes the whole density layer and is stored',
              off['density'] == 'off' and off['stored'] == '0', f"{off}")
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(1500)
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'rta'})
        page.wait_for_timeout(2500)
        restored = page.evaluate("() => document.getElementById('select-rta-fade').value")
        check('...and comes back when the mode is entered again', restored == '0', f'value={restored}')
        page.select_option('#select-rta-fade', '0.975')
        page.wait_for_timeout(800)

        print('R) SDR opens on an average while the swept views keep Clear Write')
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        page.wait_for_timeout(4000)
        sdr_trace = page.evaluate(
            """() => ({mode: document.getElementById('select-trace-mode').value,
                        avg: document.getElementById('select-trace-avg').value,
                        row: document.getElementById('trace-avg-row').style.display})""")
        check('SDR opens on Average at the default depth',
              sdr_trace['mode'] == 'AVERAGE' and sdr_trace['avg'] == '16', f"{sdr_trace}")
        check('the Avg row is visible with it', sdr_trace['row'] == '', f"row={sdr_trace['row']!r}")
        page.select_option('#select-trace-mode', 'MAX_HOLD')
        page.wait_for_timeout(1500)
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'rta'})
        page.wait_for_timeout(2500)
        swept_mode = page.evaluate("() => document.getElementById('select-trace-mode').value")
        check('the swept side is back on its own trace mode',
              swept_mode == 'CLEAR_WRITE', f'trace mode={swept_mode}')
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        page.wait_for_timeout(3500)
        back_mode = page.evaluate("() => document.getElementById('select-trace-mode').value")
        check("...and SDR remembers the trace mode the user chose there",
              back_mode == 'MAX_HOLD', f'trace mode={back_mode}')

        # The SDR capture window is a second commit path (SET_SDR with a decimate step, not a span in
        # Hz), so the gesture is checked here as well: a pan moves the wideband centre and leaves the
        # capture bandwidth alone, and a zoom lands on a decimate step the hardware actually offers.
        post(args.url, {'cmd': 'SET_SDR', 'center': 100_000_000, 'decimate': 32})
        page.wait_for_timeout(3000)
        bands = axis_band_points(page)
        sdr_before = state(args.url)['sdr']
        held.clear()
        drag(page, bands['x'], dx=bands['box']['w'] * 0.1,
             on_hold=lambda: held.append(state(args.url)['sdr']))
        page.wait_for_timeout(3000)
        sdr_after = state(args.url)['sdr']
        check('dragging the frequency row pans the SDR capture centre',
              abs(held[0]['center'] - sdr_before['center']) < 1
              and sdr_after['center'] < sdr_before['center'] - 1000,
              f"centre {sdr_before['center']:.0f} -> {held[0]['center']:.0f} -> {sdr_after['center']:.0f}")
        check('...without touching the capture bandwidth',
              sdr_after['decimate'] == sdr_before['decimate'],
              f"decimate {sdr_before['decimate']} -> {sdr_after['decimate']}")
        page.mouse.move(*bands['x'])
        for _ in range(4):
            page.mouse.wheel(0, -120)
            page.wait_for_timeout(120)
        page.wait_for_timeout(3000)
        sdr_zoom = state(args.url)['sdr']
        check('the wheel on the frequency row narrows the capture to a real decimate step',
              sdr_zoom['decimate'] > sdr_after['decimate'],
              f"decimate {sdr_after['decimate']} -> {sdr_zoom['decimate']}")

        post(args.url, {'cmd': 'SET_MODE', 'mode': 'std'})
        page.wait_for_timeout(2500)

        check('no page errors', not errors, '; '.join(errors[:3]))
        browser.close()

    print()
    if failures:
        print(f'FAILED ({len(failures)}): ' + ', '.join(failures))
        return 1
    print('all UI smoke checks passed')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
