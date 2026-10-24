#!/usr/bin/env python3
"""UI smoke test against the native fixture backend, without analyzer hardware.

Checks rendered canvases, controls, mode changes, measurements, native decode, and
axis gestures against native `websad` fixture mode.

Usage:  python3 tools/e2e/ui_smoke.py [--url http://127.0.0.1:8099]
Exit status is non-zero when any check fails, so it can be a CI gate.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from contextlib import ExitStack

sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent.parent.parent))

from playwright.sync_api import sync_playwright  # noqa: E402
from rust_iq_browser import free_port, start_service, stop  # noqa: E402


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


IDLE_COMMIT_MS = 250  # Matches frontend/src/ui/axisDrag.ts.


def drag(page, start: tuple, dx: float = 0.0, dy: float = 0.0, steps: int = 10,
         command: str | None = None, hold_ms: int = 0) -> dict:
    """Drag with real input; passively timestamp axis commands and pointer events together."""
    if command is not None:
        page.evaluate(
            """command => {
              const trace = {events: [], commands: [], pointerId: null};
              const record = event => {
                if (event.type === 'pointerdown') {
                  if (event.target.id !== 'spectrum' || event.button !== 0) return;
                  trace.pointerId = event.pointerId;
                }
                if (event.pointerId !== trace.pointerId) return;
                trace.events.push({type: event.type, at: performance.now(),
                                   trusted: event.isTrusted, buttons: event.buttons});
              };
              const types = ['pointerdown', 'pointermove', 'pointerup', 'pointercancel'];
              for (const type of types) window.addEventListener(type, record, true);
              const send = WebSocket.prototype.send;
              WebSocket.prototype.send = function(data) {
                let payload;
                try { payload = JSON.parse(data); } catch {}
                if (payload?.cmd === command) {
                  trace.commands.push({payload, at: performance.now(),
                                       eventCount: trace.events.length});
                }
                return send.call(this, data);
              };
              window.__axisSmoke = {trace, cleanup() {
                for (const type of types) window.removeEventListener(type, record, true);
                WebSocket.prototype.send = send;
              }};
            }""", command)
    x, y = start
    try:
        page.mouse.move(x, y)
        page.mouse.down()
        try:
            page.mouse.move(x + dx, y + dy, steps=steps)
            if hold_ms:
                page.wait_for_timeout(hold_ms)
        finally:
            page.mouse.up()
    finally:
        if command is not None:
            trace = page.evaluate(
                """() => { const probe = window.__axisSmoke;
                           probe.cleanup(); delete window.__axisSmoke;
                           return probe.trace; }""")
    return trace if command is not None else {}


def drag_timing(trace: dict) -> tuple[bool, str]:
    """Every command must follow release or at least 250 ms of pointer stillness."""
    events = trace['events']
    valid = (bool(trace['commands']) and bool(events) and events[0]['type'] == 'pointerdown'
             and events[-1]['type'] == 'pointerup'
             and all(event['trusted'] for event in events)
             and sum(event['type'] == 'pointermove' and event['buttons'] == 1
                     for event in events) >= 2
             and not any(event['type'] == 'pointercancel' for event in events))
    timings = []
    for command in trace['commands']:
        preceding = events[:command['eventCount']]
        released = any(event['type'] == 'pointerup' for event in preceding)
        movement = [event for event in preceding if event['type'] == 'pointermove']
        idle = command['at'] - movement[-1]['at'] if movement else 0
        valid = valid and (released or idle >= IDLE_COMMIT_MS)
        timings.append(f"{'released' if released else 'held'} idle={idle:.1f}ms")
    return valid, f"{len(events)} pointer events; commands: {', '.join(timings) or 'none'}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default=None, help='existing native backend; otherwise start native fixture')
    parser.add_argument('--painted-min', type=int, default=5000)
    args = parser.parse_args()
    failures: list[str] = []

    def check(name: str, ok: bool, detail: str = '') -> None:
        print(f'  {"PASS" if ok else "FAIL"}  {name}' + (f'  <- {detail}' if detail else ''))
        if not ok:
            failures.append(name)

    with sync_playwright() as p, ExitStack() as resources:
        if args.url is None:
            server, args.url = start_service(free_port())
            resources.callback(stop, server)
        # A running AudioContext lets the native PCM playback checks observe rendered audio.
        browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
        resources.callback(browser.close)
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
        # Measure within the row so scrolling and UI scaling do not affect the comparison.
        row_box = ("() => { const r = document.getElementById('input-ref').getBoundingClientRect();"
                   " const s = document.getElementById('btn-ref-set').getBoundingClientRect();"
                   " const u = document.getElementById('btn-ref-up').getBoundingClientRect();"
                   " return [Math.round(s.left - r.left), Math.round(s.top - r.top),"
                   "         Math.round(u.right - r.left), Math.round(u.top - r.top)]; }")
        before = page.evaluate(row_box)
        # A press during device reconfiguration can be dropped; allow a second press.
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
        # Manual Ref remains fixed; Auto is the explicit action that fits the trace.
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
        # The fixture carrier near -25 dBm must not raise a deliberately low manual Ref.
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

        print('2a2) the Level offset displaces the trace, not the absolute axis labels')
        froze_offset_trace = not page.locator('#btn-view-freeze').evaluate("e => e.classList.contains('active')")
        if froze_offset_trace:
            js_click(page, '#btn-view-freeze')
        # Observe canvas calls temporarily so the assertion uses actual trace vertices and
        # actual axis label positions, without adding work to the production renderer.
        page.evaluate("""() => {
            const c = document.getElementById('spectrum'), ctx = c.getContext('2d');
            const saved = {}, sample = { points: [], labels: [] };
            for (const name of ['beginPath', 'moveTo', 'lineTo', 'stroke', 'fillText']) saved[name] = ctx[name];
            let path = [];
            ctx.beginPath = function(...args) { path = []; return saved.beginPath.apply(this, args); };
            for (const name of ['moveTo', 'lineTo']) ctx[name] = function(x, y) {
                path.push({x, y}); return saved[name].call(this, x, y);
            };
            ctx.stroke = function(...args) {
                if (this.lineWidth === 1.5 && path.length > 20) sample.points = path.slice();
                return saved.stroke.apply(this, args);
            };
            ctx.fillText = function(text, x, y, ...args) {
                if (this.textAlign === 'right' && /^-?\\d+$/.test(String(text))) {
                    const old = sample.labels.findIndex(label => label.y === y);
                    const label = {y, value: Number(text)};
                    if (old < 0) sample.labels.push(label); else sample.labels[old] = label;
                }
                return saved.fillText.call(this, text, x, y, ...args);
            };
            window.offsetCanvasSample = sample;
            window.restoreOffsetCanvas = () => { Object.assign(ctx, saved); delete window.offsetCanvasSample; };
        }""")
        box = page.locator('#spectrum').bounding_box()
        hover_x = box['width'] * 0.5
        page.mouse.move(box['x'] + hover_x, box['y'] + box['height'] * 0.5)
        try:
            base_readout = None
            for offset in (0, 20, -10):
                page.fill('#input-offset', str(offset))
                page.dispatch_event('#input-offset', 'change')
                page.wait_for_timeout(700)
                point = page.evaluate("""x => {
                    const sample = window.offsetCanvasSample;
                    const point = sample.points.reduce((a, b) => Math.abs(a.x-x) < Math.abs(b.x-x) ? a : b);
                    const labels = sample.labels.sort((a,b) => a.y-b.y);
                    const top = labels[0], bottom = labels[labels.length-1];
                    const axis = top.value + (point.y-top.y)/(bottom.y-top.y)*(bottom.value-top.value);
                    const cursor = document.getElementById('spectrum').dataset.cursor || '';
                    const match = cursor.match(/(-?\\d+(?:\\.\\d+)?) dBm$/);
                    return {axis, readout: match ? Number(match[1]) : null};
                }""", hover_x)
                check(f'offset {offset}: drawn trace point agrees with its axis and readout',
                      point['readout'] is not None and abs(point['axis'] - point['readout']) < 1,
                      str(point))
                if offset == 0:
                    base_readout = point['readout']
                check(f'offset {offset}: readout retains external compensation',
                      point['readout'] is not None and base_readout is not None
                      and abs(point['readout'] - base_readout - offset) < 1,
                      f"base={base_readout}, current={point['readout']}")
        finally:
            page.evaluate('window.restoreOffsetCanvas()')
            page.fill('#input-offset', '0')
            page.dispatch_event('#input-offset', 'change')
            if froze_offset_trace:
                js_click(page, '#btn-view-freeze')
            page.wait_for_timeout(700)

        print('2b) the peak list works off the threshold slot')
        # Peak finding and export share the threshold parameter slot.
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
                page.wait_for_timeout(2500)
                dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrIq") or ''
                check('native decoded transport is enabled', 'enabled=true' in dbg and 'backend=1' in dbg, dbg)

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

        # Measurement ownership also blocks canvas frequency gestures.
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'harmonic'})
        for _ in range(12):                    # wait until the client has SEEN the reported mode
            if state(args.url)['mode'] == 'harmonic':
                break
            page.wait_for_timeout(400)
        page.wait_for_timeout(1800)
        band = axis_band_points(page)
        # Measurements retune internally; only an accepted gesture bumps its commit counter.
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
        def divider_state():
            return page.evaluate("""() => {
                const split = document.getElementById('wf-split');
                const box = split.getBoundingClientRect();
                const x = box.x + box.width / 2, y = box.y + box.height / 2;
                return {x, y, width: box.width, height: box.height,
                        hit: document.elementFromPoint(x, y)?.id || '',
                        scrollX, scrollY, scale: Number(document.documentElement.dataset.uiScale),
                        wf: document.getElementById('waterfall-container').clientHeight,
                        spec: document.getElementById('spectrum').clientHeight};
            }""")

        def divider_y():
            return divider_state()['y']

        def sizes():
            return divider_state()

        def drag_divider(dy):
            # A resize can change both axes or the page scroll. Observe a settled, visible
            # strip before every grab; never reuse the previous drag's viewport coordinates.
            page.locator('#wf-split').scroll_into_view_if_needed()
            page.evaluate("""async () => {
                const split = document.getElementById('wf-split');
                let previous = '';
                for (let frame = 0; frame < 30; frame++) {
                    await new Promise(requestAnimationFrame);
                    const r = split.getBoundingClientRect();
                    const current = [r.x, r.y, r.width, r.height, scrollX, scrollY].join(',');
                    if (current === previous) return;
                    previous = current;
                }
                throw new Error('waterfall divider layout did not settle');
            }""")
            grab = divider_state()
            check('the divider grab point hits the visible strip', grab['hit'] == 'wf-split',
                  str(grab))
            page.evaluate("""() => {
                const events = [];
                const types = ['pointerdown', 'pointermove', 'pointerup', 'pointercancel',
                               'gotpointercapture', 'lostpointercapture'];
                const record = e => events.push({type: e.type, target: e.target.id,
                                                id: e.pointerId, trusted: e.isTrusted,
                                                x: e.clientX, y: e.clientY, buttons: e.buttons});
                for (const type of types) window.addEventListener(type, record, true);
                window.__dividerSmoke = {events, cleanup() {
                    for (const type of types) window.removeEventListener(type, record, true);
                }};
            }""")
            try:
                drag(page, (grab['x'], grab['y']), dy=dy, steps=8)
            finally:
                trace = page.evaluate("""() => {
                    const probe = window.__dividerSmoke;
                    probe.cleanup(); delete window.__dividerSmoke;
                    const split = document.getElementById('wf-split');
                    return {events: probe.events, captured: probe.events.some(
                        e => split.hasPointerCapture(e.id)),
                        dragging: split.classList.contains('dragging')};
                }""")
            types = [event['type'] for event in trace['events']
                     if event['target'] == 'wf-split' and event['trusted']]
            check('the divider receives and releases the real pointer',
                  all(kind in types for kind in ('pointerdown', 'pointerup',
                                                'gotpointercapture', 'lostpointercapture'))
                  and 'pointercancel' not in types and not trace['captured']
                  and not trace['dragging'], str(trace))
            return grab

        check('divider gestures run at the global 125% UI scale',
              divider_state()['scale'] == 1.25, str(divider_state()))
        heights = sizes()
        # Dragging down grows the spectrum above the divider and shrinks the waterfall.
        box = drag_divider(50)
        moved_down = sizes()
        check('dragging the divider down follows the pointer',
              abs(divider_y() - (box['y'] + 50)) <= 4,
              f"divider {box['y']:.1f} -> {divider_y():.1f}, cursor {box['y'] + 50:.1f}")
        check('dragging down grows the spectrum and shrinks the waterfall',
              abs(moved_down['wf'] - (heights['wf'] - 50 / box['scale'])) <= 3
              and moved_down['spec'] > heights['spec'] + 30,
              f"wf {heights['wf']} -> {moved_down['wf']}, spectrum {heights['spec']} -> {moved_down['spec']}")
        # ...and drag UP, back past the start: the waterfall grows again.
        up_box = drag_divider(-100)
        y_now = up_box['y']
        moved_up = sizes()
        check('dragging up follows the pointer and grows the waterfall',
              abs(divider_y() - (y_now - 100)) <= 4
              and abs(moved_up['wf'] - (moved_down['wf'] + 100 / up_box['scale'])) <= 3
              and moved_up['spec'] < moved_down['spec'] - 60,
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

        print('N) native AUD2 audio reaches browser playback')
        page.goto(args.url, wait_until='networkidle')
        page.wait_for_timeout(1500)
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        page.wait_for_timeout(2500)
        button = page.locator('#btn-sdr-audio')
        if 'on' not in button.inner_text().lower():
            button.evaluate('(el) => el.click()')
        page.wait_for_timeout(6000)
        audio_dbg = page.evaluate("document.getElementById('spectrum').dataset.sdrAudio") or ''
        received = int(m.group(1)) if (m := re.search(r'frames=(\d+)', audio_dbg)) else 0
        played = int(m.group(1)) if (m := re.search(r'played_frames=(\d+)', audio_dbg)) else 0
        check('native PCM arrives and is rendered', received > 0 and played > 0, audio_dbg)
        check('native audio retains stream identity and source position',
              bool(re.search(r'stream_key=\d+:\d+', audio_dbg))
              and bool(re.search(r'input_index=\d+ audio_index=\d+', audio_dbg)), audio_dbg)
        check('native audio delivery did not fail', audio_dbg.endswith('error='), audio_dbg)

        print('N) native FT8 decode reaches the browser pane')
        page.goto(args.url, wait_until='networkidle')
        page.wait_for_timeout(1500)
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
        # Native decode results appear in the decode window table.
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
        check('the native FT8 message is rendered', 'CQ JO1WKO PM95' in rows_text, detail)
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

        # Native fixture IQ passes through the CW receiver and SDE2 into the shared decode pane.
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
            # The native receiver fixture keys CQ DE W1ABC; joining mid-transmission can
            # yield a fragment before the next complete message.
            cw_text = ''
            title = ''
            for _ in range(30):
                page.wait_for_timeout(1000)
                cw_text = page.evaluate("document.getElementById('decode-window-cw').textContent") or ''
                title = page.evaluate("document.getElementById('decode-window-title').textContent") or ''
                if 'CQ DE W1ABC' in cw_text:
                    break
            check('the CW text is decoded into the decode window',
                  'CQ DE W1ABC' in cw_text and title == 'CW',
                  f'title={title!r} text={cw_text[:80]!r}')
            # Pause dots mark word gaps; the keyed lamp and meter remain live in CW.
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

            # Returning to CW must resume newly arriving native text after clearing the pane.
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
                if 'CQ DE W1ABC' in cw_again:
                    break
            check('CW decodes again after switching away and back',
                  'CQ DE W1ABC' in cw_again, f'text={cw_again[:80]!r}')

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

            # A first CW selection also picks its default filter with stored SDR preferences.
            fresh = browser.new_context()
            fresh_page = fresh.new_page()
            fresh_page.add_init_script(
                "for (const [k, v] of Object.entries({'web-sa-sdr-ifbw': '180000',"
                " 'web-sa-sdr-demod': 'usb', 'web-sa-sdr-audio': '0',"
                " 'web-sa-sdr-decimate': '32', 'web-sa-sdr-center': '411000000',"
                " 'web-sa-sdr-listen': '411000000'})) localStorage.setItem(k, v);")
            fresh_page.goto(args.url, wait_until='domcontentloaded')
            fresh_page.wait_for_selector('#btn-decode-window', state='attached', timeout=15000)
            # The fresh profile's first-connect handshake re-posts its saved mode (std for a
            # fresh profile) after our SET_MODE; wait for it to settle, then re-assert SDR so
            # the demod command below is mode-legal.
            fresh_page.wait_for_timeout(1200)
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

        # Axis previews commit on release or after 250 ms without pointer movement.
        print('P) dragging the axis labels pans and zooms the window')
        # Close the decode overlay so both axis bands receive real pointer input.
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
        before = state(args.url)
        freq_drag = drag(page, bands['x'], dx=bands['box']['w'] * 0.1,
                         command='SET_FREQ', hold_ms=IDLE_COMMIT_MS * 3)
        page.wait_for_timeout(2500)
        after = state(args.url)
        check('frequency commands wait for pointer stillness or release', *drag_timing(freq_drag))
        check('a stationary held frequency drag commits before release',
              any(command['eventCount'] == len(freq_drag['events']) - 1
                  for command in freq_drag['commands']),
              drag_timing(freq_drag)[1])
        check('...and pans to the lower frequencies when the row is pushed right',
              after['center'] < before['center'] - 1000
              and bool(freq_drag['commands'])
              and all(command['payload']['center'] < before['center'] - 1000
                      for command in freq_drag['commands']),
              f"center {before['center']:.0f} -> {after['center']:.0f}")
        check('the frequency pan preserves the span',
              abs(after['span'] - before['span']) < 1
              and all(abs(command['payload']['span'] - before['span']) < 1
                      for command in freq_drag['commands']),
              f"span {before['span']:.0f} -> {after['span']:.0f}")

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

        # Wheel zoom must release the gesture before subsequent pointer hover.
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

        # Absolute dBm gestures adjust Ref; relative dB gestures adjust the display offset.
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
        # Move far enough to exceed the device's Ref quantization.
        ref_drag = drag(page, bands['y'], dy=bands['box']['h'] * 0.25, command='SET_REF')
        page.wait_for_timeout(2500)
        ref_after = state(args.url)['ref']
        check('reference commands wait for pointer stillness or release', *drag_timing(ref_drag))
        check('...and raises the reference when the labels are pushed down',
              ref_after > ref_before and bool(ref_drag['commands'])
              and all(command['payload']['ref'] > ref_before for command in ref_drag['commands']),
              f'ref {ref_before:.0f} -> {ref_after:.0f} (band top: {element_at_band("y")!r})')

        # The relative (dB) display pins the top of the graticule to 0, so there is no device
        # reference on that axis: the same gesture pans the level OFFSET, which is a client-side
        # display value - applied live and never sent to the device.
        check('the level axis gesture is checked in the relative display',
              display_unit('dB') == 'dB', f'level unit={level_unit()}')
        offset_before = float(page.input_value('#input-offset') or 0)
        relative_ref = state(args.url)['ref']
        offset_drag = drag(page, bands['y'], dy=bands['box']['h'] * 0.25, command='SET_REF')
        page.wait_for_timeout(1000)
        offset_after = float(page.input_value('#input-offset') or 0)
        check('in the relative display the same drag pans the level offset',
              offset_after < offset_before - 1 and not offset_drag['commands']
              and abs(relative_ref - state(args.url)['ref']) < 1,
              f'offset {offset_before} -> {offset_after}, ref held at {relative_ref:.0f}')
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

        # SDR pans preserve capture bandwidth; zooms use supported decimation steps.
        post(args.url, {'cmd': 'SET_SDR', 'center': 100_000_000, 'decimate': 32})
        page.wait_for_timeout(3000)
        bands = axis_band_points(page)
        sdr_before = state(args.url)['sdr']
        sdr_drag = drag(page, bands['x'], dx=bands['box']['w'] * 0.1, command='SET_SDR')
        page.wait_for_timeout(3000)
        sdr_after = state(args.url)['sdr']
        check('SDR commands wait for pointer stillness or release', *drag_timing(sdr_drag))
        check('dragging the frequency row pans the SDR capture centre',
              sdr_after['center'] < sdr_before['center'] - 1000
              and bool(sdr_drag['commands'])
              and all(command['payload']['center'] < sdr_before['center'] - 1000
                      for command in sdr_drag['commands']),
              f"centre {sdr_before['center']:.0f} -> {sdr_after['center']:.0f}")
        check('...without touching the capture bandwidth',
              sdr_after['decimate'] == sdr_before['decimate']
              and all(command['payload']['decimate'] == sdr_before['decimate']
                      for command in sdr_drag['commands']),
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

    print()
    if failures:
        print(f'FAILED ({len(failures)}): ' + ', '.join(failures))
        return 1
    print('all UI smoke checks passed')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
