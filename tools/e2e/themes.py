#!/usr/bin/env python3
"""Exercise appearance selection, persistence, contrast and dense instrument layouts.

Uses the existing native fixture binary; does not build or affect a hardware service.
Run: .venv/bin/python tools/e2e/themes.py
"""
from __future__ import annotations

import argparse
import base64
import re
from contextlib import ExitStack
from pathlib import Path

from browser_preferences import select_theme
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready

THEMES = ['dark', 'light', 'liquid-glass', 'liquid-glass-dark', 'minimalist-flat', 'neumorphism', 'mecha',
          'pixel-anime', 'sci-fi-hud', 'brutalist-web', 'neon-tokyo', 'e-ink']
DARK = {'dark', 'liquid-glass-dark', 'mecha', 'pixel-anime', 'sci-fi-hud', 'neon-tokyo'}
GLASS = {'liquid-glass', 'liquid-glass-dark'}

PALETTE = """() => {
  const style = getComputedStyle(document.documentElement);
  const read = name => style.getPropertyValue(name).trim();
  const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
  const ctx = canvas.getContext('2d');
  const rgb = color => {
    ctx.clearRect(0, 0, 1, 1); ctx.fillStyle = color; ctx.fillRect(0, 0, 1, 1);
    return Array.from(ctx.getImageData(0, 0, 1, 1).data).slice(0, 3);
  };
  const luminance = color => rgb(color).map(v => v / 255)
    .map(v => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4)
    .reduce((sum, v, i) => sum + v * [.2126, .7152, .0722][i], 0);
  const contrast = (a, b) => {
    const x = luminance(read(a)), y = luminance(read(b));
    return (Math.max(x, y) + .05) / (Math.min(x, y) + .05);
  };
  const traces = [1, 2, 3, 4].map(i => read('--sa-trace' + i));
  return {scheme: style.colorScheme, bg: rgb(read('--sa-bg')),
    text: contrast('--text', '--panel'), label: contrast('--label', '--panel-2'),
    input: contrast('--text', '--input-bg'), active: contrast('--text', '--btn-active'),
    traces, traceRgb: rgb(traces[0]), traceContrast: traces.map((_, i) => contrast('--sa-trace' + (i + 1), '--sa-bg'))};
}"""


def close_icon_is_centered(page) -> None:
    button = page.locator('.theme-picker-close').bounding_box()
    cross = page.locator('.theme-picker-close path').bounding_box()
    assert button and cross
    dx = abs(button['x'] + button['width'] / 2 - cross['x'] - cross['width'] / 2)
    dy = abs(button['y'] + button['height'] / 2 - cross['y'] - cross['height'] / 2)
    assert dx <= .5 and dy <= .5, (page.locator('html').get_attribute('data-theme'), dx, dy)


def check_glass_optics(page) -> None:
    # A stable striped scene distinguishes real displacement from blur/tint alone.
    page.evaluate("""() => {
      const scene = document.createElement('div'); scene.dataset.glassProbe = '';
      scene.style.cssText = 'position:fixed;z-index:999;width:280px;height:170px;left:20px;top:80px;background:repeating-linear-gradient(90deg,#20384d 0 2px,#fff 2px 12px)';
      const lens = document.createElement('div'); lens.className = 'control-group';
      lens.dataset.glassLens = '';
      lens.style.cssText = 'position:absolute;left:20px;top:20px;width:240px;height:130px;border-radius:22px;--theme-surface-bg:transparent;--liquid-frost:.35px;box-sizing:border-box';
      scene.append(lens); document.body.append(scene);
    }""")
    lens = page.locator('[data-glass-probe] .control-group')
    try:
        page.wait_for_function("""() => document.querySelector('[data-glass-probe] .control-group')
          ?.style.getPropertyValue('--liquid-refraction').includes('url')""")
        page.wait_for_timeout(200)
        refracted = base64.b64encode(lens.screenshot()).decode()
        lens.evaluate("e => e.style.setProperty('--liquid-refraction', 'blur(0px)')")
        plain = base64.b64encode(lens.screenshot()).decode()
        difference = page.evaluate("""async ([a, b]) => {
          const read = async data => {
            const img = new Image(); img.src = 'data:image/png;base64,' + data; await img.decode();
            const c = document.createElement('canvas'); c.width = img.width; c.height = img.height;
            const ctx = c.getContext('2d'); ctx.drawImage(img, 0, 0);
            return {pixels: ctx.getImageData(0, 0, c.width, c.height).data, w: c.width, h: c.height};
          };
          const [x, y] = await Promise.all([read(a), read(b)]);
          let edge = 0, centre = 0, ne = 0, nc = 0;
          for (let row = 0; row < x.h; row++) for (let col = 0; col < x.w; col++) {
            const at = (row * x.w + col) * 4;
            const delta = Math.abs(x.pixels[at] - y.pixels[at]);
            if (col > 24 && col < x.w - 24 && row > 24 && row < x.h - 24) { centre += delta; nc++; }
            else if (col > 4 && col < x.w - 4 && row > 4 && row < x.h - 4) { edge += delta; ne++; }
          }
          return {edge: edge / ne, centre: centre / nc};
        }""", [refracted, plain])
        assert difference['edge'] > 3 and difference['edge'] > difference['centre'] * 2, difference
        print(f'PASS glass bends the rim while keeping its centre clear: {difference}', flush=True)
        # Check the regular material's actual frosting, not just its CSS value.
        # Fine backdrop stripes should disappear through the default blur.
        lens.evaluate("e => e.style.removeProperty('--liquid-frost')")
        frosted = base64.b64encode(lens.screenshot()).decode()
        contrast = page.evaluate("""async ([a, b]) => {
          const range = async data => {
            const img = new Image(); img.src = 'data:image/png;base64,' + data; await img.decode();
            const c = document.createElement('canvas'); c.width = img.width; c.height = img.height;
            const ctx = c.getContext('2d'); ctx.drawImage(img, 0, 0);
            const p = ctx.getImageData(40, 40, c.width - 80, c.height - 80).data;
            const values = []; for (let i = 0; i < p.length; i += 4) values.push(p[i]);
            values.sort((x, y) => x - y);
            return values[Math.floor(values.length * .9)] - values[Math.floor(values.length * .1)];
          };
          return {clear: await range(a), frost: await range(b)};
        }""", [plain, frosted])
        assert contrast['frost'] < contrast['clear'] * .3, contrast
        print(f'PASS regular glass blurs fine backdrop detail: {contrast}', flush=True)
    finally:
        page.locator('[data-glass-probe]').evaluate('e => e.remove()')


def check_glass_motion(page) -> None:
    button = page.locator('.unit-btn.active:visible').first
    button.scroll_into_view_if_needed()
    before = button.bounding_box()
    button.hover(); page.mouse.down()
    try:
        page.wait_for_timeout(130)
        assert button.evaluate("e => getComputedStyle(e, '::before').transform") != 'none'
        assert button.bounding_box() == before, 'only the glass layer should stretch'
    finally:
        page.mouse.up()
    page.wait_for_timeout(500)
    assert button.evaluate("e => getComputedStyle(e, '::before').transform") == 'none'
    page.emulate_media(reduced_motion='reduce')
    button.hover(); page.mouse.down()
    try:
        assert button.evaluate("e => getComputedStyle(e, '::before').transform") == 'none'
    finally:
        page.mouse.up(); page.emulate_media(reduced_motion='no-preference')
    print('PASS glass pressure/rebound keeps labels still and respects reduced motion', flush=True)


def check_glass_sidebar_scroll(page) -> None:
    # Rim and shadow must stay at the viewport edge while only the controls
    # scroll. A pseudo-element on the scroller draws a false bottom mid-panel.
    panel = page.locator('.control-panel')
    frame = page.locator('.control-panel-surface')
    expect(frame).to_have_class(re.compile(r'liquid-panel'))
    expect(panel).not_to_have_class(re.compile(r'liquid-panel'))
    page.wait_for_function("document.querySelector('.control-panel-surface').style.getPropertyValue('--liquid-refraction').includes('url')")
    panel.evaluate('e => e.scrollTop = 0')
    page.mouse.move(1, 1)
    rect = frame.bounding_box()
    before = base64.b64encode(frame.screenshot()).decode()
    for end in [False, True]:
        panel.evaluate('(e, end) => e.scrollTop = end ? e.scrollHeight : 200', end)
        assert panel.evaluate('e => e.scrollTop > 0')
        assert frame.bounding_box() == rect
        assert frame.evaluate('e => e.scrollTop') == 0
    after = base64.b64encode(frame.screenshot()).decode()
    delta = page.evaluate("""async ([a, b]) => {
      const pixels = async data => {
        const img = new Image(); img.src = 'data:image/png;base64,' + data; await img.decode();
        const c = document.createElement('canvas'); c.width = img.width; c.height = img.height;
        const ctx = c.getContext('2d'); ctx.drawImage(img, 0, 0);
        return {data: ctx.getImageData(0, 0, c.width, c.height).data, w: c.width, h: c.height};
      };
      const [x, y] = await Promise.all([pixels(a), pixels(b)]);
      let sum = 0, n = 0;
      for (let row = 0; row < x.h; row++) for (let col = 0; col < x.w; col++) {
        if (Math.min(row, col, x.h - row - 1, x.w - col - 1) >= 3) continue;
        for (let c = 0; c < 3; c++) { const i = (row * x.w + col) * 4 + c; sum += Math.abs(x.data[i] - y.data[i]); n++; }
      }
      return sum / n;
    }""", [before, after])
    assert delta < 1, ('sidebar rim moved with contents', delta)
    page.locator('#panel-vis-toggle').click()
    expect(frame).not_to_be_visible()
    page.locator('#panel-vis-toggle').click()
    expect(frame).to_be_visible()
    panel.evaluate('e => e.scrollTop = 0')
    print(f'PASS glass sidebar scrolls independently of its rim/shadow (border difference {delta:.3f})', flush=True)


def check_window_opacity(page, popup, artifacts: Path) -> None:
    # The popup adopts the real window; updates must still reach all main-page
    # tools, including the old Decode pane and the smaller detail popovers.
    slider = popup.locator('.mc-head .window-opacity')
    popup.locator('.mc-window').evaluate('e => e.dataset.opacityGeometry = JSON.stringify(e.getBoundingClientRect().toJSON())')
    box = slider.bounding_box()
    assert box
    popup.mouse.move(box['x'] + box['width'] / 2, box['y'] + box['height'] / 2)
    popup.mouse.down()
    popup.mouse.move(box['x'] + box['width'] / 3, box['y'] + box['height'] / 2, steps=4)
    popup.mouse.up()
    assert popup.locator('.mc-window').evaluate('e => e.dataset.opacityGeometry === JSON.stringify(e.getBoundingClientRect().toJSON())')
    slider.evaluate("e => { e.value = '65'; e.dispatchEvent(new Event('input', {bubbles:true})); }")
    expect(page.locator('#decode-window-opacity')).to_have_value('65')
    expect(page.locator('#device-popover .window-opacity')).to_have_value('65')
    assert page.locator('[data-window-opacity]').evaluate_all("nodes => nodes.every(e => getComputedStyle(e).opacity === '0.65')")
    assert page.evaluate("localStorage.getItem('web-sa-window-opacity')") == '0.65'
    for theme in THEMES:
        select_theme(page, theme)
        page.locator('#decode-window-opacity').evaluate("e => { e.value = '100'; e.dispatchEvent(new Event('input', {bubbles:true})); }")
        expect(slider).to_have_value('100')
        for target in [page, popup]:
            assert target.locator('[data-window-opacity]').evaluate_all("""nodes => nodes.every(e => {
              const s = getComputedStyle(e), canvas = document.createElement('canvas');
              canvas.width = canvas.height = 1;
              const c = canvas.getContext('2d'); c.fillStyle = s.backgroundColor; c.fillRect(0, 0, 1, 1);
              return s.opacity === '1' && s.backdropFilter === 'none' && c.getImageData(0, 0, 1, 1).data[3] === 255;
            })"""), theme
        if theme in GLASS:
            slider.evaluate("e => { e.value = '80'; e.dispatchEvent(new Event('input', {bubbles:true})); }")
            popup.screenshot(path=str(artifacts / f'floating-{theme}.png'))
            slider.evaluate("e => { e.value = '100'; e.dispatchEvent(new Event('input', {bubbles:true})); }")
    print('PASS global opacity, slider drag, popup synchronization and opaque endpoints in all themes', flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default=None)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-themes'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p, ExitStack() as resources:
        if args.url is None:
            server, args.url = start_service(free_port())
            resources.callback(stop, server)
        browser = p.chromium.launch()
        resources.callback(browser.close)
        context = browser.new_context(viewport={'width': 1600, 'height': 1000})
        resources.callback(context.close)
        page = context.new_page()
        errors: list[str] = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(args.url, wait_until='domcontentloaded')
        wait_ui_ready(page)
        page.evaluate('document.fonts.ready')
        page.locator('#btn-theme').click()
        choices = page.locator('[data-theme-choice]')
        assert choices.evaluate_all('(nodes) => nodes.map(n => n.dataset.themeChoice)') == THEMES
        expect(page.locator('[data-theme-choice="dark"]')).to_be_focused()
        page.keyboard.press('End')
        expect(page.locator('[data-theme-choice="e-ink"]')).to_be_focused()
        page.keyboard.press('Home')
        page.keyboard.press('ArrowRight')
        expect(page.locator('[data-theme-choice="light"]')).to_be_focused()
        page.keyboard.press('Escape')
        expect(page.locator('#theme-picker')).not_to_be_visible()
        expect(page.locator('#btn-theme')).to_be_focused()
        assert page.locator('html').get_attribute('data-theme') == 'dark'
        page.locator('#btn-theme').click()
        page.mouse.click(2, 2)
        expect(page.locator('#theme-picker')).not_to_be_visible()

        backgrounds = set()
        materials = set()
        for theme in THEMES:
            select_theme(page, theme)
            expect(page.locator('html')).to_have_attribute('data-theme', theme)
            expect(page.locator('#theme-picker')).not_to_be_visible()
            assert page.evaluate("localStorage.getItem('web-sa-theme')") == theme
            palette = page.evaluate(PALETTE)
            assert palette['scheme'] == ('dark' if theme in DARK else 'light'), (theme, palette)
            backgrounds.add(tuple(palette['bg']))
            if theme not in {'dark', 'light'}:
                assert min(palette[key] for key in ['text', 'label', 'input', 'active']) >= 4.5, (theme, palette)
                assert min(palette['traceContrast']) >= 3, (theme, palette)
                assert len(set(palette['traces'])) == 4, (theme, palette)
                selected = page.locator('.unit-btn.active:visible').first
                before = selected.evaluate('e => [getComputedStyle(e).color, getComputedStyle(e).backgroundColor]')
                selected.hover()
                after = selected.evaluate('e => [getComputedStyle(e).color, getComputedStyle(e).backgroundColor]')
                assert before == after, (theme, 'hover must retain selected contrast', before, after)
                page.mouse.move(3, 3)
            if theme in GLASS:
                check_glass_optics(page)
                check_glass_motion(page)
                check_glass_sidebar_scroll(page)
            if theme == 'mecha':
                assert page.locator('.control-group').first.evaluate('e => getComputedStyle(e).clipPath') == 'none'
                assert page.locator('.group-title').first.evaluate('e => getComputedStyle(e).textTransform') == 'none'
            page.wait_for_function("""trace => {
              const canvas = document.getElementById('spectrum');
              const pixels = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
              for (let at = 0; at < pixels.length; at += 4) {
                if (pixels[at + 3] && trace.every((v, i) => v === pixels[at + i])) return true;
              }
              return false;
            }""", arg=palette['traceRgb'])
            page.screenshot(path=str(args.artifacts / f'{theme}.png'))
            if theme not in {'dark', 'light'}:
                # Exclude colours: each new theme must have distinct element construction.
                materials.add(tuple(page.evaluate("""() => {
                  const panel = getComputedStyle(document.querySelector('.control-group'));
                  const head = getComputedStyle(document.querySelector('.group-head'));
                  const button = getComputedStyle(document.querySelector('#btn-mode-rta'));
                  const field = getComputedStyle(document.querySelector('.param-row input'));
                  return [panel.clipPath, panel.borderRadius, panel.borderTopWidth,
                    head.borderBottomStyle, head.borderBottomWidth, button.clipPath,
                    button.borderRadius, button.borderTopStyle, button.borderTopWidth,
                    field.borderRadius, field.borderTopStyle];
                }""")))
            page.locator('#btn-theme').click()
            close_icon_is_centered(page)
            page.locator('#theme-picker').screenshot(path=str(args.artifacts / f'picker-{theme}.png'))
            page.locator('.theme-picker-close').click()
            page.reload(wait_until='domcontentloaded')
            wait_ui_ready(page)
            expect(page.locator('html')).to_have_attribute('data-theme', theme)
            print(f'PASS {theme}: selection, palette, canvas repaint and persistence', flush=True)
        assert len(backgrounds) >= 9, backgrounds
        assert len(materials) == 9, materials
        assert page.locator('.liquid-glass-defs').count() == 0, 'switching away must release the glass filters'
        print('PASS nine distinct component constructions, independent of palette', flush=True)

        # Picker previews have their own palette even when the workspace is dark.
        select_theme(page, 'dark')
        page.locator('#btn-theme').click()
        preview = page.locator('[data-theme-preview="e-ink"]')
        assert preview.evaluate("e => getComputedStyle(e).getPropertyValue('--sa-trace1').trim()") == '#20211e'
        page.screenshot(path=str(args.artifacts / 'theme-picker.png'))
        page.locator('.theme-picker-close').click()
        page.locator('#btn-lang').click()
        select_theme(page, 'neon-tokyo')
        expect(page.locator('#btn-theme')).to_have_text('霓虹')
        page.locator('#btn-lang').click()
        expect(page.locator('#btn-theme')).to_have_text('Neon')
        page.locator('#btn-markers-all').click()
        expect(page.locator('#btn-theme')).to_have_text('Neon')
        page.locator('#btn-markers-all').click()

        page.locator('#btn-mode-rta').click()
        wait_ui_ready(page, 'rta')
        page.locator('#btn-waterfall').click()
        expect(page.locator('#waterfall-container')).to_be_visible()
        for theme in THEMES:
            select_theme(page, theme)
            wait_ui_ready(page, 'rta')
            page.screenshot(path=str(args.artifacts / f'rta-{theme}.png'))
        print('PASS all themes in RTA with live waterfall', flush=True)

        page.locator('#btn-mode-sdr').click()
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id="overview-follow"]')
        expect(overview).to_be_visible()
        with page.expect_popup() as opened:
            overview.locator('[data-pop]').click()
        popup = opened.value
        popup.wait_for_load_state('domcontentloaded')
        popup.set_viewport_size({'width': 420, 'height': 760})
        check_window_opacity(page, popup, args.artifacts)
        for theme in THEMES:
            select_theme(page, theme)
            expect(popup.locator('html')).to_have_attribute('data-theme', theme)
            scheme = 'dark' if theme in DARK else 'light'
            expect(popup.locator('.mc-window')).to_have_css('color-scheme', scheme)
            assert popup.locator('.mc-body').evaluate('e => e.scrollWidth <= e.clientWidth + 1'), theme
            if theme in GLASS:
                expect(popup.locator('.mc-window')).to_have_class(re.compile(r'liquid-panel'))
                popup.wait_for_function("document.querySelector('.liquid-glass-defs feImage') !== null")
                check_glass_optics(popup)
        popup.screenshot(path=str(args.artifacts / 'sdr-popout-e-ink.png'))
        popup.locator('[data-dock]').click()
        expect(overview).to_be_visible()
        overview.locator('[data-close]').click()
        print('PASS all themes synchronize SDR floating and popped-out windows', flush=True)

        # New typography and materials must not introduce page overflow at existing densities.
        for width, height in [(1366, 768), (1024, 768), (480, 800), (390, 844)]:
            page.set_viewport_size({'width': width, 'height': height})
            for theme in THEMES:
                select_theme(page, theme)
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth + 1'), (theme, width)
                page.locator('#btn-theme').click()
                close_icon_is_centered(page)
                rect = page.locator('#theme-picker').bounding_box()
                assert rect and rect['x'] >= 0 and rect['y'] >= 0, (theme, width, rect)
                assert rect['x'] + rect['width'] <= width + 1, (theme, width, rect)
                assert rect['y'] + rect['height'] <= height + 1, (theme, width, rect)
                page.keyboard.press('Escape')
            print(f'PASS {width}×{height}: all theme layouts and picker bounds', flush=True)
        for scale in ['0.8', '1.5', '2']:
            page.select_option('#ui-scale', scale)
            for theme in THEMES:
                select_theme(page, theme)
                page.locator('#btn-theme').click()
                close_icon_is_centered(page)
                rect = page.locator('#theme-picker').bounding_box()
                assert rect and rect['x'] >= 0 and rect['y'] >= 0, (scale, theme, rect)
                assert rect['x'] + rect['width'] <= 391 and rect['y'] + rect['height'] <= 845, (scale, theme, rect)
                page.keyboard.press('Escape')
        print('PASS appearance picker remains usable at manual UI scales', flush=True)
        assert not errors, errors
    print(f'PASS appearance checks; screenshots: {args.artifacts}')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
