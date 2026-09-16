#!/usr/bin/env python3
"""Resolution baseline: what the 1366x768-only layout actually does on other screens.

Why this exists: the front-end was laid out against one laptop panel (1366x768), so the
version under test has a fixed 1280px card, an 860px minimum for the spectrum column, a
360px control panel, `height: calc(100vh - 96px)` and a canvas whose backing store is
pinned at 860x480 x DPR. Nothing here asserts what "adaptive" should look like - that is
the goal. This script only *measures* the current state so the change can be argued with
numbers, and so the same metrics can become the gate later.

Reported per viewport:
  css/backing   canvas CSS box vs backing store, and the stretch factor (backing < CSS*DPR
                means the browser scales the bitmap up = blur)
  overflow      document scrollWidth/clientWidth and the outermost elements hanging past
                the right edge
  boxes/overlap bounding boxes of the layout landmarks plus pairwise overlaps
  text          computed font sizes as a share of viewport height (px per 1000 logical px),
                i.e. "how big does 11px look on a 1440-tall screen"
  fixed sizes   every px width/height left in src/style.css, with file:line so it greps

Usage:
  python3 tools/e2e/viewport_baseline.py --url http://127.0.0.1:8080
  python3 tools/e2e/viewport_baseline.py --check          # non-zero exit on any finding
  python3 tools/e2e/viewport_baseline.py --screens        # real monitors: grim screenshots
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
STYLE_CSS = ROOT / 'frontend/modern/src/style.css'
SHOT_DIR = ROOT / 'screenshots/resolution-baseline'

# (label, CSS width, CSS height, device pixel ratio)
#
# The last two are not synthetic: 1280x960@1.6 is exactly the attached 2048x1536 panel at
# the compositor's 1.6 scale (what the browser reports), and 1280x720@2 is what a 2560x1440
# screen looks like to the page at 200% browser zoom.
VIEWPORTS = [
    ('laptop-1366x768', 1366, 768, 1.0),
    ('desktop-1600x1000', 1600, 1000, 1.0),
    ('fhd-1920x1080', 1920, 1080, 1.0),
    ('qhd-2560x1440', 2560, 1440, 1.0),
    ('uhd-3840x2160', 3840, 2160, 1.0),
    ('external-1280x960-dpr1.6', 1280, 960, 1.6),
    ('zoom200-1280x720-dpr2', 1280, 720, 2.0),
    # The two viewports the real browser actually reports on the two attached outputs. Wayland
    # hands the page the compositor scale multiplied by GNOME's text-scaling-factor (0.9091 on
    # this machine), so the real DPR is 0.906 on the laptop panel and 1.450 on the 2K one - the
    # page gets MORE css pixels than the panel's nominal logical size, and dpr can be below 1.
    ('real-laptop-1481x708-dpr0.906', 1481, 708, 0.906),
    ('real-2k-1386x920-dpr1.45', 1386, 920, 1.45),
    # Below the 1280 design width the flex row has to wrap, and the container is a fixed
    # 100vh-96 box with overflow hidden - these are the widths where the current layout
    # actually breaks rather than merely wasting space.
    ('narrow-1200x800', 1200, 800, 1.0),
    ('narrow-1024x768', 1024, 768, 1.0),
    ('narrow-800x600', 800, 600, 1.0),
    ('narrow-480x800', 480, 800, 1.0),
]

# UI mode -> the buttons that enter it (empty = the SWP default after page load). The
# criterion covers SWP/RTA/SDR, and the waterfall only exists once it is toggled on.
MODES = [('swp', []),
         ('rta+waterfall', ['#btn-mode-rta', '#btn-waterfall']),
         ('sdr', ['#btn-mode-sdr'])]

# Layout landmarks that must stay visible and must not collide with each other.
LANDMARKS = [
    ('info-bar', '.info-bar'),
    ('spectrum-area', '.spectrum-area'),
    ('canvas#spectrum', '#spectrum'),
    ('waterfall', '#waterfall-container'),
    ('marker-table', '#marker-table'),
    ('rail', '#control-rail'),
    ('panel', '.control-panel'),
    ('dev-bar', '#dev-bar'),
]

PROBE = """(landmarks) => {
  const out = {};
  const de = document.documentElement;
  const vw = window.innerWidth, vh = window.innerHeight;
  const dpr = window.devicePixelRatio || 1;
  // The global UI scale (core/uiScale.ts) is CSS `zoom` on the frame, so a canvas in it covers
  // scale times more device pixels than its local px suggest.
  const ui = parseFloat(document.documentElement.dataset.uiScale || '1') || 1;
  out.viewport = { w: vw, h: vh, dpr: dpr, uiScale: ui,
                   zoom: window.visualViewport ? window.visualViewport.scale : null,
                   svw: screen.width, svh: screen.height };
  out.doc = { scrollW: de.scrollWidth, clientW: de.clientWidth,
              scrollH: de.scrollHeight, clientH: de.clientHeight };
  const card = document.querySelector('.analyzer-card');
  if (card) {
    const r = card.getBoundingClientRect();
    out.card = { w: +r.width.toFixed(1), h: +r.height.toFixed(1), left: +r.left.toFixed(1) };
  }
  // Canvas: CSS content box vs backing store. `stretch` ~1 is sharp; >1 means an upscaled
  // bitmap. The bitmap is painted into the content box (borders excluded), so that is the box
  // the backing store has to match - `expectedW/H` is round(box x ratio) with the same policy
  // core/store.ts uses (ratio = DPR x UI scale, capped at 4, reduced only past an 8M
  // device-pixel budget, never below 1).
  out.canvases = [];
  for (const c of document.querySelectorAll('canvas')) {
    const r = c.getBoundingClientRect();
    const cssW = c.clientWidth > 0 ? c.clientWidth : r.width;
    const cssH = c.clientHeight > 0 ? c.clientHeight : r.height;
    let ratio = Math.max(1, Math.min(4, dpr * ui));
    if (cssW * cssH * ratio * ratio > 8000000) ratio = Math.max(1, Math.sqrt(8000000 / (cssW * cssH)));
    let lit = null, sampled = 0;
    const g = c.getContext('2d');
    if (g && c.width && c.height) {
      try {
        const d = g.getImageData(0, 0, c.width, c.height).data;
        const step = Math.max(1, Math.floor(Math.sqrt((c.width * c.height) / 4000)));
        for (let y = 0; y < c.height; y += step) {
          for (let x = 0; x < c.width; x += step) {
            sampled++;
            if (d[(y * c.width + x) * 4 + 3] > 0) lit = (lit || 0) + 1;
          }
        }
      } catch (e) { /* tainted or no 2d context */ }
    }
    out.canvases.push({
      id: c.id || '(anon)', cssW: +cssW.toFixed(1), cssH: +cssH.toFixed(1),
      screenW: +r.width.toFixed(1), screenH: +r.height.toFixed(1),
      backingW: c.width, backingH: c.height,
      stretch: cssW > 0 ? +((cssW * dpr * ui) / c.width).toFixed(3) : null,
      ratio: +ratio.toFixed(3),
      expectedW: Math.round(cssW * ratio), expectedH: Math.round(cssH * ratio),
      litRatio: sampled ? +((lit || 0) / sampled).toFixed(4) : null, sampled: sampled,
    });
  }
  // Landmark boxes, overlaps (DOM-containment pairs excluded) and clipping.
  //
  // A bounding rect lies about a wrapped flex row inside an overflow:hidden box: the element
  // still reports its full size while none of it is reachable. So every box is intersected
  // with its overflow:hidden/clip ancestors first, and overlaps use those reachable rects.
  const clip = (el) => {
    const r = el.getBoundingClientRect();
    const box = { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
    let p = el.parentElement;
    while (p && p !== document.documentElement) {
      const st = getComputedStyle(p);
      const pr = p.getBoundingClientRect();
      if (st.overflowX === 'hidden' || st.overflowX === 'clip') {
        box.left = Math.max(box.left, pr.left); box.right = Math.min(box.right, pr.right);
      }
      if (st.overflowY === 'hidden' || st.overflowY === 'clip') {
        box.top = Math.max(box.top, pr.top); box.bottom = Math.min(box.bottom, pr.bottom);
      }
      p = p.parentElement;
    }
    return box;
  };
  const area = (b) => Math.max(0, b.right - b.left) * Math.max(0, b.bottom - b.top);
  out.boxes = {};
  const rects = [];
  for (const [name, sel] of landmarks) {
    const el = document.querySelector(sel);
    if (!el) { out.boxes[name] = null; continue; }
    const r = el.getBoundingClientRect(), c = clip(el);
    const full = r.width * r.height;
    const box = { x: +r.left.toFixed(1), y: +r.top.toFixed(1),
                  w: +r.width.toFixed(1), h: +r.height.toFixed(1),
                  visible: !!(el.offsetParent || el.getClientRects().length),
                  visibleFrac: full > 0 ? +(area(c) / full).toFixed(3) : 1 };
    out.boxes[name] = box;
    if (area(c) > 0) rects.push([name, el, c]);
  }
  out.overlaps = [];
  for (let i = 0; i < rects.length; i++) {
    for (let j = i + 1; j < rects.length; j++) {
      const [an, ae, ar] = rects[i], [bn, be, br] = rects[j];
      if (ae.contains(be) || be.contains(ae)) continue;
      const w = Math.min(ar.right, br.right) - Math.max(ar.left, br.left);
      const h = Math.min(ar.bottom, br.bottom) - Math.max(ar.top, br.top);
      if (w > 0 && h > 0 && w * h > 4) {
        out.overlaps.push({ a: an, b: bn, area: Math.round(w * h) });
      }
    }
  }
  // Outermost elements reaching past the left/right edge of the viewport, measured on the
  // reachable rect (inside a clipped ancestor they are cut off, not overflowing). An element
  // inside a horizontally scrollable ancestor is reachable by scrolling that ancestor, so it is
  // not an offender.
  const scrollableInX = (el) => {
    let p = el.parentElement;
    while (p && p !== document.documentElement) {
      const ox = getComputedStyle(p).overflowX;
      if (ox === 'auto' || ox === 'scroll') return true;
      p = p.parentElement;
    }
    return false;
  };
  const clipped = [];
  for (const el of document.querySelectorAll('body *')) {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none' || st.opacity === '0') continue;
    const c = clip(el);
    if (area(c) <= 0) continue;
    if (c.right <= vw + 1 && c.left >= -1) continue;
    if (scrollableInX(el)) continue;
    clipped.push([el, c]);
  }
  out.clipped = [];
  for (const [el, c] of clipped) {
    // keep only the outermost offender so a nested text node does not spam the list
    let p = el.parentElement, seen = false;
    while (p && p !== document.body) {
      if (clipped.some(([o]) => o === p)) { seen = true; break; }
      p = p.parentElement;
    }
    if (seen) continue;
    const sel = el.tagName.toLowerCase() + (el.id ? '#' + el.id : '')
      + (el.className && typeof el.className === 'string'
         ? '.' + el.className.trim().split(/\\s+/)[0] : '');
    out.clipped.push({ el: sel, right: Math.round(c.right), left: Math.round(c.left),
                       w: Math.round(c.right - c.left) });
  }
  // Below-the-fold clipping inside the fixed-height container.
  out.docBottom = vh < de.scrollHeight - 1 ? de.scrollHeight - vh : 0;
  const fonts = {};
  for (const [k, sel] of Object.entries({ body: 'body', label: '.info-label',
                                          value: '.info-value', groupTitle: '.group-title',
                                          paramLabel: '.param-row label', input: 'input, select' })) {
    const el = document.querySelector(sel);
    if (el) fonts[k] = +parseFloat(getComputedStyle(el).fontSize).toFixed(1);
  }
  out.fonts = fonts;
  return out;
}"""


def style_inventory() -> list[str]:
    """Every px width/height still written into src/style.css, as file:line lines."""
    rows: list[str] = []
    decl_re = re.compile(r'(?<![\w-])((?:min-|max-)?(?:width|height))\s*:\s*([^;}]+)')
    for lineno, line in enumerate(STYLE_CSS.read_text(encoding='utf-8').splitlines(), 1):
        for prop, value in decl_re.findall(line):
            if 'px' in value:
                rows.append(f'  frontend/modern/src/style.css:{lineno}: {prop}: {value.strip()}')
    return rows


# The four structural locks the goal names explicitly; each is greppable by its pattern.
LOCKS = [
    ('fixed card width', r'\.analyzer-card\s*\{[^}]*width:\s*(\d+)px', 'width: 1280px'),
    ('spectrum column floor', r'\.spectrum-area\s*\{[^}]*min-width:\s*(\d+)px', 'min-width: 860px'),
    ('control panel width', r'\.control-panel\s*\{[^}]*width:\s*(\d+)px', 'width: 360px'),
    ('container height', r'\.screen-container\s*\{[^}]*height:\s*calc\(100vh - (\d+)px\)',
     'calc(100vh - 96px)'),
]


def lock_report() -> list[str]:
    css = STYLE_CSS.read_text(encoding='utf-8')
    out: list[str] = []
    for label, pattern, shown in LOCKS:
        m = re.search(pattern, css)
        if not m:
            out.append(f'  {label}: gone ({shown} is no longer in the file)')
            continue
        lineno = css[:m.start()].count('\n') + 1
        out.append(f'  {label}: {shown}  (frontend/modern/src/style.css:{lineno})')
    return out


def fmt(metrics: dict) -> list[str]:
    """One viewport's report lines plus its findings."""
    lines: list[str] = []
    findings: list[str] = []
    vp, doc = metrics['viewport'], metrics['doc']
    lines.append(f"viewport {vp['w']}x{vp['h']} css, dpr {vp['dpr']}"
                 + (f", ui scale {vp['uiScale']}" if vp.get('uiScale', 1) != 1 else '')
                 + (f", visualViewport.scale {vp['zoom']}" if vp['zoom'] not in (None, 1) else ''))
    ov_x = doc['scrollW'] - doc['clientW']
    ov_y = doc['scrollH'] - doc['clientH']
    lines.append(f"  overflow      : x={ov_x}px  y={ov_y}px  "
                 f"(scrollW {doc['scrollW']} / clientW {doc['clientW']})")
    if ov_x > 1:
        findings.append(f'horizontal page overflow {ov_x}px')
    card = metrics.get('card')
    if card:
        used = card['w'] / vp['w'] * 100
        lines.append(f"  card          : {card['w']}x{card['h']} at x={card['left']}"
                     f"  -> {100 - used:.1f}% of the width unused")
        if used < 80:
            findings.append(f"card covers only {used:.0f}% of the viewport width")
    for c in metrics['canvases']:
        if c['cssW'] <= 0 or c['cssH'] <= 0:
            lines.append(f"  canvas {c['id']:<10}: css {c['cssW']}x{c['cssH']} (not rendered)")
            continue
        lines.append(f"  canvas {c['id']:<10}: local box {c['cssW']}x{c['cssH']}"
                     f"  screen {c['screenW']}x{c['screenH']}"
                     f"  backing {c['backingW']}x{c['backingH']}"
                     f"  expected {c['expectedW']}x{c['expectedH']} at ratio {c['ratio']}"
                     f"  stretch {c['stretch']}x  lit {c['litRatio']}")
        if (abs(c['backingW'] - c['expectedW']) > 2 or abs(c['backingH'] - c['expectedH']) > 2):
            findings.append(f"canvas {c['id']} backing {c['backingW']}x{c['backingH']} != "
                            f"round(box x ratio) {c['expectedW']}x{c['expectedH']}")
        elif c['backingW'] != c['expectedW'] or c['backingH'] != c['expectedH']:
            # Within 2 device px: a zoomed box is rounded to the device grid, and a canvas sized
            # from that same box one frame earlier can land a pixel off. Not a blur - the
            # stretch check below is what would catch a real mismatch.
            lines.append(f"    (backing differs from the box by "
                         f"{c['backingW'] - c['expectedW']}x{c['backingH'] - c['expectedH']} "
                         f"device px: zoomed-box rounding)")
        if c['stretch'] and c['stretch'] > 1.02:
            findings.append(f"canvas {c['id']} bitmap stretched {c['stretch']}x (blur)")
        if c['litRatio'] == 0:
            findings.append(f"canvas {c['id']} has no painted pixels")
    fonts = metrics['fonts']
    if fonts:
        ui = vp.get('uiScale', 1)
        label = fonts.get('label', fonts.get('body'))
        # The UI scale is CSS zoom, so a computed font size is local px while the viewport is
        # screen px: what the eye sees is fontSize x scale against the screen height.
        per_1000 = label * ui / vp['h'] * 1000
        lines.append(f"  text          : body {fonts.get('body')}px, labels {label}px, "
                     f"inputs {fonts.get('input')}px -> {per_1000:.1f} px per 1000 px of "
                     f"viewport height (local {label} x scale {ui} = "
                     f"{label * ui:.1f} screen px of {vp['h']}, dpr {vp['dpr']})")
        if per_1000 < 10:
            findings.append(f'label text shrinks to {per_1000:.1f}px per 1000px of height')
    boxes = {k: v for k, v in metrics['boxes'].items()}
    shown = '  '.join(
        f"{k} {v['w']}x{v['h']}@{v['y']}"
        + (f" vis {v['visibleFrac']:.0%}" if v['visibleFrac'] < 0.999 else '')
        for k, v in boxes.items() if v)
    lines.append(f"  boxes         : {shown}")
    for name, b in boxes.items():
        if not b or not b['visible']:
            continue
        if b['w'] <= 0 or b['h'] <= 0:
            findings.append(f'{name} collapsed ({b["w"]}x{b["h"]})')
        elif b['visibleFrac'] <= 0.001:
            findings.append(f'{name} is clipped away (unreachable)')
        elif b['visibleFrac'] < 0.98:
            findings.append(f"{name} is {(1 - b['visibleFrac']) * 100:.0f}% clipped")
    if metrics['overlaps']:
        lines.append('  overlaps      : ' + ', '.join(
            f"{o['a']}/{o['b']} ({o['area']}px2)" for o in metrics['overlaps']))
        findings.append(f"{len(metrics['overlaps'])} landmark overlap(s)")
    if metrics['clipped']:
        total = len(metrics['clipped'])
        lines.append(f"  past right edge: {total} outermost element(s): " + ', '.join(
            f"{c['el']} right={c['right']} (w {c['w']})" for c in metrics['clipped'][:6]))
        findings.append(f'{total} element(s) past the right edge')
    if not metrics.get('settled', True):
        lines.append('  canvas sizes changed again 700ms later (layout feedback loop)')
        findings.append('canvas size never settles')
    if metrics['docBottom']:
        lines.append(f"  page taller than the viewport by {metrics['docBottom']}px")
    return lines, findings


def collect(browser, url: str, label: str, w: int, h: int, dpr: float,
            mode: str = 'swp', mode_clicks: list[str] | None = None,
            shot_path: Path | None = None, ui_scale: float | None = None) -> dict:
    ctx = browser.new_context(viewport={'width': w, 'height': h}, device_scale_factor=dpr)
    if ui_scale is not None:
        # Seed the stored override the way a returning user would have it, so the same sweep
        # can run at several UI scales (the control itself is exercised by tools/e2e/ui_smoke.py).
        ctx.add_init_script(
            f"try {{ localStorage.setItem('web-sa-ui-scale', '{ui_scale}') }} catch (e) {{}}")
    page = ctx.new_page()
    errors: list[str] = []
    page.on('pageerror', lambda e: errors.append(f'pageerror: {e}'))
    page.goto(f'{url}/', wait_until='networkidle')
    page.wait_for_timeout(2500)
    for selector in (mode_clicks or []):
        page.click(selector)
        page.wait_for_timeout(3000)
    metrics = page.evaluate(PROBE, LANDMARKS)
    # A backing store derived from the box must settle: if the size keeps changing, the layout
    # is feeding the bitmap back into itself (the trap the canvas CSS now avoids).
    before = [[c['backingW'], c['backingH']] for c in metrics['canvases']]
    page.wait_for_timeout(700)
    after = page.evaluate("() => [...document.querySelectorAll('canvas')]"
                          '.map((c) => [c.width, c.height])')
    metrics['settled'] = before == after
    if shot_path is not None:
        page.screenshot(path=str(shot_path), full_page=False)
        print(f"  screenshot    : {shot_path.relative_to(ROOT)}")
    metrics['label'] = label
    metrics['mode'] = mode
    metrics['uiScale'] = ui_scale if ui_scale is not None else 1
    metrics['errors'] = errors
    ctx.close()
    return metrics


def environment() -> list[str]:
    """The output/scale environment the numbers below depend on (facts, not assumptions)."""
    rows = []
    for m in monitors():
        rows.append(f"  output {m['name']}: {m['lw']}x{m['lh']} logical "
                    f"(compositor scale {m['scale']}, at {m['x']},{m['y']})")
    try:
        ts = subprocess.run(['gsettings', 'get', 'org.gnome.desktop.interface',
                             'text-scaling-factor'], capture_output=True, text=True, timeout=5)
        factor = ts.stdout.strip() or '(unknown)'
    except Exception:                                                      # noqa: BLE001
        factor = '(gsettings unavailable)'
    rows.append(f"  GNOME text-scaling-factor = {factor}: Chromium multiplies its device scale "
                'factor by it, so the real browser reports dpr = compositor scale x this value')
    rows.append('  measured in the real browser (screenshots/resolution-baseline/*.json): '
                'eDP-1 dpr 0.906 over a 1481x708 css viewport; '
                'HDMI-A-2 dpr 1.450 over a 1386x920 css viewport')
    return rows


def monitors() -> list[dict]:
    out = subprocess.run(['hyprctl', 'monitors', '-j'], capture_output=True, text=True,
                         check=True).stdout
    rows = []
    for m in json.loads(out):
        rows.append({'name': m['name'], 'x': m['x'], 'y': m['y'],
                     'lw': round(m['width'] / m['scale']), 'lh': round(m['height'] / m['scale']),
                     'scale': m['scale'], 'focused': m['focused']})
    return rows


def hyprctl(args: list[str]) -> str:
    out = subprocess.run(['hyprctl', *args], capture_output=True, text=True)
    return out.stdout.strip()


def screens(url: str, port: int = 9411) -> int:
    """Real-session capture: the actual browser on the actual output, grabbed by the compositor.

    Wayland does not let a client place its own window, so the sequence is: focus the target
    output, start the system Chromium (it picks up the user's Wayland flags from
    ~/.config/chromium-flags.conf, so this is the real rendering path), maximise that window
    through the compositor, measure the page as it really is (real DPR, real viewport), then
    grim the window's own region. Only the window this script started is ever touched - the
    window is located by the browser process id, never by focus, so a mis-targeted dispatch
    cannot move or fullscreen anything of the user's.
    """
    from playwright.sync_api import sync_playwright
    SHOT_DIR.mkdir(parents=True, exist_ok=True)
    mons = monitors()
    previous_focus = next((m['name'] for m in mons if m['focused']), None)
    rc = 0
    with sync_playwright() as p:
        rc = screens_for_monitors(p, mons, url, port)
    if previous_focus:
        hyprctl(['dispatch', f'hl.dsp.focus({{monitor="{previous_focus}"}})'])
    return rc


def screens_for_monitors(p, mons: list[dict], url: str, port: int) -> int:
    rc = 0
    for mon in mons:
        print(f"--- {mon['name']} {mon['lw']}x{mon['lh']} @{mon['scale']} "
              f"logical ({mon['x']},{mon['y']})")
        profile = f'/tmp/websa-baseline-{mon["name"]}'
        shutil.rmtree(profile, ignore_errors=True)
        hyprctl(['dispatch', f'hl.dsp.focus({{monitor="{mon["name"]}"}})'])
        time.sleep(0.5)
        with open(f'/tmp/websa-baseline-{mon["name"]}.log', 'w') as log:
            proc = subprocess.Popen(
                ['chromium', f'--remote-debugging-port={port}', f'--user-data-dir={profile}',
                 '--new-window', f'{url}/'],
                stdout=log, stderr=log, start_new_session=True)
            try:
                page = connect_page(p, port, url)
                if page is None:
                    print('    WARN: the browser never exposed the page')
                    rc = 1
                    continue
                # Maximise this window (located by browser pid) and let the layout settle.
                addr = next((c['address'] for c in clients()
                             if c.get('pid') == proc.pid or c.get('class') == 'chromium'), '')
                if addr:
                    hyprctl(['dispatch', f'hl.dsp.window.focus({{window="address:{addr}"}})'])
                    hyprctl(['dispatch',
                             f'hl.dsp.window.fullscreen({{mode="maximized", '
                             f'window="address:{addr}"}})'])
                page.wait_for_timeout(3000)
                measured = page.evaluate(PROBE, LANDMARKS)
                page.screenshot(path=str(SHOT_DIR / f"{mon['name']}-baseline-page.png"))
                window = next((c for c in clients() if c['address'] == addr), {}) if addr else {}
                geo = {k: window.get(k) for k in ('address', 'at', 'size', 'monitor')}
                # Capture while the window is still on screen, or the region shows what is behind it.
                shot = SHOT_DIR / f"{mon['name']}-baseline.png"
                subprocess.run(['grim', '-g',
                                f"{mon['x']},{mon['y']} {mon['lw']}x{mon['lh']}", str(shot)],
                               check=True)
                (SHOT_DIR / f"{mon['name']}-baseline.json").write_text(
                    json.dumps({'monitor': mon, 'url': url, 'window': geo, 'metrics': measured},
                               indent=2, ensure_ascii=False), encoding='utf-8')
                vp = measured['viewport']
                card = measured.get('card') or {}
                print(f"    browser reported dpr {vp['dpr']:.3f}, viewport "
                      f"{vp['w']}x{vp['h']} css on a {vp['svw']}x{vp['svh']} logical screen")
                print(f"    window {geo.get('at')} {geo.get('size')}")
                for c in measured['canvases']:
                    print(f"    canvas {c['id']}: css {c['cssW']}x{c['cssH']} "
                          f"backing {c['backingW']}x{c['backingH']} stretch {c['stretch']}")
                print(f"    card {card.get('w')}x{card.get('h')} -> "
                      f"{100 - card.get('w', 0) / vp['w'] * 100:.1f}% of the width unused")
                print(f"    screenshot: {shot.relative_to(ROOT)}"
                      f" + {mon['name']}-baseline-page.png (browser-side capture)")
                if not shot.exists() or shot.stat().st_size == 0:
                    print('    WARN: screenshot is empty')
                    rc = 1
            finally:
                proc.terminate()
                time.sleep(1)
                subprocess.run(['pkill', '-f', profile], capture_output=True)
    return rc


def clients() -> list[dict]:
    try:
        return json.loads(hyprctl(['clients', '-j']) or '[]')
    except json.JSONDecodeError:
        return []


def connect_page(p, port: int, url: str, timeout: float = 40.0):
    """The page in the browser we just started, once its CDP endpoint is up."""
    host = url.split('//')[1].rstrip('/')
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            browser = p.chromium.connect_over_cdp(f'http://127.0.0.1:{port}')
            found = [x for c in browser.contexts for x in c.pages if host in x.url]
            if found:
                return found[-1]
        except Exception:                                                  # noqa: BLE001
            pass
        time.sleep(1)
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    parser.add_argument('--json', default='')
    parser.add_argument('--check', action='store_true',
                        help='exit non-zero when a viewport reports a finding')
    parser.add_argument('--screens', action='store_true',
                        help='also grab one real-session screenshot per attached monitor')
    parser.add_argument('--screens-only', action='store_true')
    parser.add_argument('--ui-scales', default='',
                        help='comma list of UI scales to sweep (e.g. 1,1.5,2); default: the page decides')
    parser.add_argument('--modes', default='swp,rta+waterfall',
                        help='comma list of UI modes to measure '
                             f"({','.join(m[0] for m in MODES)})")
    parser.add_argument('--shots', default='',
                        help='comma list of viewport labels to screenshot during the sweep')
    parser.add_argument('--shot-suffix', default='current',
                        help='filename suffix for those screenshots')
    parser.add_argument('--no-inventory', action='store_true')
    parser.add_argument('--no-restore-std', dest='restore_std', action='store_false',
                        help='do not POST SET_MODE std after measuring (the last mode wins)')
    args = parser.parse_args()

    if args.screens_only:
        return screens(args.url)

    wanted = [m.strip() for m in args.modes.split(',') if m.strip()]
    selected = [(name, sels) for name, sels in MODES if name in wanted]
    scales = [float(s) for s in args.ui_scales.split(',') if s.strip()] or [None]
    shot_labels = [s.strip() for s in args.shots.split(',') if s.strip()]
    if not selected:
        print(f'no mode matches {args.modes!r}; known: {[m[0] for m in MODES]}')
        return 1
    print('=== environment (the outputs and scales the numbers depend on) ===')
    try:
        for line in environment():
            print(line)
    except Exception as exc:                                               # noqa: BLE001
        print(f'  WARN: could not read the compositor environment: {exc}')

    print('=== fixed sizes still in src/style.css (grep-able) ===')
    print('--- structural locks the goal names:')
    for line in lock_report():
        print(line)
    if not args.no_inventory:
        print(f'--- all px width/height declarations ({STYLE_CSS.relative_to(ROOT)}):')
        for line in style_inventory():
            print(line)

    from playwright.sync_api import sync_playwright
    results = []
    total_findings = 0
    with sync_playwright() as p:
        browser = p.chromium.launch()
        for label, w, h, dpr in VIEWPORTS:
            for ui_scale in scales:
                for mode, mode_clicks in selected:
                    title = f'{label} [{mode}]' + (f' ui{ui_scale}' if ui_scale else '')
                    print(f"\n=== {title} ===")
                    shot = None
                    if label in shot_labels:
                        SHOT_DIR.mkdir(parents=True, exist_ok=True)
                        safe = title.replace(' [', '-').replace(']', '').replace('+', '-')
                        safe = safe.replace(' ', '')
                        shot = SHOT_DIR / f'{safe}-{args.shot_suffix}.png'
                    m = collect(browser, args.url, label, w, h, dpr, mode, mode_clicks,
                                shot, ui_scale)
                    lines, findings = fmt(m)
                    for line in lines:
                        print(line)
                    for e in m['errors']:
                        print(f"  JS ERROR      : {e}")
                        findings.append('JS error')
                    if findings:
                        print('  FINDINGS      : ' + '; '.join(findings))
                    else:
                        print('  FINDINGS      : none')
                    total_findings += len(findings)
                    results.append({'label': title, 'mode': mode, 'findings': findings,
                                'metrics': m})
        browser.close()

    print('\n=== summary ===')
    print(f"  {'viewport [mode]':<32} {'card':>6} {'canvas local':>12} {'backing':>12} "
          f"{'stretch':>8} {'unused':>7} {'findings':>8}")
    for r in results:
        m = r['metrics']
        card = m.get('card') or {'w': 0}
        c = m['canvases'][0] if m['canvases'] else {}
        unused = f"{100 - card['w'] / m['viewport']['w'] * 100:.0f}%" if card['w'] else '-'
        print(f"  {r['label']:<32} {card['w']:>6} "
              f"{str(c.get('cssW')) + 'x' + str(c.get('cssH')):>12} "
              f"{str(c.get('backingW')) + 'x' + str(c.get('backingH')):>12} "
              f"{str(c.get('stretch')):>8} {unused:>7} {len(r['findings']):>8}")
    print(f'  total findings: {total_findings}')

    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2, ensure_ascii=False),
                                   encoding='utf-8')
        print(f'  json: {args.json}')

    if args.restore_std:
        # leave the instrument in sweep mode, whichever mode the last viewport switched to
        try:
            import urllib.request
            req = urllib.request.Request(
                f'{args.url}/api/config',
                data=json.dumps({'cmd': 'SET_MODE', 'mode': 'std'}).encode(),
                headers={'Content-Type': 'application/json'})
            urllib.request.urlopen(req, timeout=5).read()
            print('  restored the instrument to sweep (SET_MODE std)')
        except Exception as exc:                                   # noqa: BLE001
            print(f'  WARN: could not restore sweep mode: {exc}')

    if args.screens:
        print('\n=== real-session screenshots ===')
        rc = screens(args.url)
        if rc:
            total_findings += 1

    if args.check and total_findings:
        print(f'\nFAIL: {total_findings} finding(s)')
        return 1
    print('\nOK: measurement only (no thresholds applied)' if not args.check else '\nOK')
    return 0


if __name__ == '__main__':
    sys.exit(main())
