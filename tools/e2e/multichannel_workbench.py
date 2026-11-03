#!/usr/bin/env python3
"""Browser lifecycle gate against the isolated Vite fixture entry; never touches websad.

Start `npm --prefix frontend run dev:workbench`, then run this with a Playwright Python.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from playwright.sync_api import Error, expect, sync_playwright


def quick_listen_check(browser, url: str, artifacts: Path) -> None:
    context = browser.new_context(viewport={'width': 1440, 'height': 1000})
    page = context.new_page()
    errors: list[str] = []
    requests: list[str] = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.on('request', lambda request: requests.append(request.url) if '/api/' in request.url else None)
    page.goto(url)
    overview = page.locator('[data-view-id="overview-follow"]')
    overview.locator('[data-uid="fixture-c1"] [data-action="rec-iq"]').click()
    overview.locator('[data-uid="fixture-c3"] [data-action="rec-iq"]').click()
    expect(overview.locator('[data-uid="fixture-c3"]')).to_contain_text('记录中')
    overview.locator('[data-uid="fixture-c2"] [data-action="select"]').click()
    overview.locator('[data-close]').click()
    page.locator('[data-fault]').select_option('reject')
    page.locator('[data-inject]').click()
    waterfall = page.locator('.mc-waterfall')
    box = waterfall.bounding_box()
    assert box
    waterfall.click(button='right', position={'x': box['width'] * 0.58, 'y': box['height'] - 35})
    quick = page.locator('[data-quick-listen]')
    expect(quick).to_be_visible()
    expect(quick.locator('[name="frequency"]')).to_have_value('7.12')
    expect(quick.locator('[name="target"]')).to_have_value('fixture-c1')
    expect(quick.locator('[name="target"] option[value="new"]')).to_have_attribute('disabled', '')
    expect(quick).to_contain_text('记录继续')
    quick.locator('[name="unit"]').select_option('kHz')
    expect(quick.locator('[name="frequency"]')).to_have_value('7120')
    quick.locator('[name="unit"]').select_option('MHz')
    quick.locator('[name="mode"]').select_option('am')
    page.screenshot(path=str(artifacts / 'quick-listen-preview.png'))
    quick.locator('[data-listen-submit]').click()
    expect(quick).to_contain_text('FIXTURE_REJECTED')
    expect(page.locator('[data-marker-uid="fixture-c1"]')).not_to_contain_text('监听')
    quick.locator('[data-listen-submit]').click()
    expect(quick).not_to_be_visible()
    expect(page.locator('[data-listen-status]')).to_contain_text('7.12 MHz → C1')
    page.locator('[data-action="workbench"]').click()
    expect(overview).to_have_attribute('data-receiver-uid', 'fixture-c2')
    expect(overview.locator('[data-uid="fixture-c1"]')).to_contain_text('7.120000 MHz')
    expect(overview.locator('[data-uid="fixture-c1"]')).to_contain_text('记录中')
    expect(overview.locator('[data-uid="fixture-c3"]')).to_contain_text('7.150000 MHz')
    expect(overview.locator('[data-uid="fixture-c3"]')).to_contain_text('记录中')
    overview.locator('[data-close]').click()
    page.locator('[data-marker-uid="fixture-c1"]').click(button='right')
    expect(quick).to_contain_text('不重启接收')
    quick.locator('[data-dismiss]').click()
    page.locator('[data-action="quick-listen"]').click()
    quick.locator('[name="frequency"]').fill('8')
    expect(quick.locator('[data-listen-submit]')).to_be_disabled()
    expect(quick).to_contain_text('RF_RANGE')
    quick.locator('[name="frequency"]').fill('7.121')
    expect(quick.locator('[data-listen-submit]')).to_be_enabled()
    quick.press('Escape')
    expect(quick).not_to_be_visible()
    page.screenshot(path=str(artifacts / 'quick-listen-active.png'))
    assert not errors, errors
    assert not requests, requests
    context.close()
    # A free slot creates an independent receiver instead of retuning a digital/IQ channel.
    context = browser.new_context(viewport={'width': 1440, 'height': 1000})
    page = context.new_page()
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(url)
    overview = page.locator('[data-view-id="overview-follow"]')
    overview.locator('[data-uid="fixture-c3"] [data-action="select"]').click()
    overview.locator('[data-section=management] summary').click()
    overview.locator('[data-action="retire"]').click()
    expect(overview.locator('.mc-row')).to_have_count(2)
    overview.locator('[data-close]').click()
    waterfall = page.locator('.mc-waterfall')
    box = waterfall.bounding_box()
    assert box
    waterfall.click(button='right', position={'x': box['width'] * 0.58, 'y': box['height'] - 35})
    quick = page.locator('[data-quick-listen]')
    expect(quick.locator('[name="target"]')).to_have_value('new')
    expect(quick.locator('[data-listen-submit]')).to_have_text('新增并监听')
    quick.locator('[data-listen-submit]').click()
    expect(quick).not_to_be_visible()
    expect(page.locator('[data-listen-status]')).to_contain_text('7.12 MHz')
    page.locator('[data-action="workbench"]').click()
    expect(overview.locator('.mc-row')).to_have_count(3)
    expect(overview.locator('[data-uid="fixture-c1"]')).to_contain_text('7.100000 MHz')
    expect(overview.locator('[data-uid="fixture-c2"]')).to_contain_text('7.074000 MHz')
    expect(overview).to_have_attribute('data-receiver-uid', 'fixture-c1')
    assert not errors, errors
    context.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:5186/static/dist/workbench.html')
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-multichannel-browser'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1440, 'height': 1000})
        page = context.new_page()
        errors: list[str] = []
        context.on('page', lambda p: p.on('pageerror', lambda e: errors.append(str(e))))
        page.on('pageerror', lambda e: errors.append(str(e)))
        network: list[str] = []
        page.on('websocket', lambda ws: network.append(ws.url))
        page.on('request', lambda req: network.append(req.url) if '/api/' in req.url else None)
        page.goto(args.url)
        overview = page.locator('[data-view-id="overview-follow"]')
        expect(overview).to_be_visible()
        expect(overview.locator('.mc-row')).to_have_count(3)
        row1 = overview.locator('[data-uid="fixture-c1"]')
        row2 = overview.locator('[data-uid="fixture-c2"]')
        row1.locator('[data-action="rec-iq"]').click()
        expect(row1).to_contain_text('记录中')
        row1.locator('[data-action="detail"]').click()
        pinned = page.locator('[data-view-id="detail-fixture-c1"]')
        expect(pinned).to_be_visible()
        pinned.locator('[data-close]').click()
        expect(row1).to_contain_text('记录中')
        overview.locator('[data-action="follow-results"]').click()
        follow = page.locator('[data-view-id="results-follow"]')
        follow.locator('[data-close]').click()
        row2.locator('[data-action="select"]').click()
        expect(pinned).to_have_attribute('data-receiver-uid', 'fixture-c1')
        expect(follow).to_have_attribute('data-receiver-uid', 'fixture-c2')
        row2.locator('[data-action="results"]').click()
        results = page.locator('[data-view-id="results-fixture-c2"]')
        expect(results).to_contain_text('FIXTURE CQ TEST AA00')
        # Two desktop children coexist and synchronize independently.
        with page.expect_popup() as first:
            results.locator('[data-pop]').click()
        result_child = first.value
        with page.expect_popup() as second:
            overview.locator('[data-pop]').click()
        overview_child = second.value
        page.locator('[data-language]').select_option('en')
        page.locator('#fixture-controls [data-theme]').select_option('light')
        page.locator('[data-scale]').select_option('1.25')
        for child in [result_child, overview_child]:
            expect(child.locator('html')).to_have_attribute('data-theme', 'light')
            expect(child.locator('html')).to_have_attribute('lang', 'en')
            expect(child.locator('html')).to_have_attribute('data-ui-scale', '1.25')
        expect(result_child.locator('[data-view-id]')).to_have_attribute('data-receiver-uid', 'fixture-c2')
        overview_child.locator('[data-uid="fixture-c3"] [data-action="select"]').click()
        expect(result_child.locator('[data-view-id]')).to_have_attribute('data-receiver-uid', 'fixture-c2')
        expect(overview_child.locator('[data-editor]')).not_to_contain_text('Local monitor routing')
        overview_child.screenshot(path=str(args.artifacts / 'popout-light.png'))
        result_child.screenshot(path=str(args.artifacts / 'decode-popout-light.png'))
        overview_child.close()
        expect(overview).to_be_visible()
        expect(overview.locator('[data-uid="fixture-c1"]')).to_contain_text('Recording')
        try:
            result_child.locator('[data-dock]').click(no_wait_after=True)
        except Error:
            # Dock closes the clicked document after adopting its live root.
            if not result_child.is_closed():
                raise
        expect(results).to_be_visible()
        results.locator('[data-close]').click()
        # Drag and resize at non-unit UI scale use viewport pixels.
        start = overview.bounding_box()
        assert start
        head = overview.locator('.mc-head strong').bounding_box()
        assert head
        x, y = head['x'] + 30, head['y'] + head['height'] / 2
        page.mouse.move(x, y)
        page.mouse.down()
        page.mouse.move(x + 50, y + 30, steps=5)
        page.mouse.up()
        moved = overview.bounding_box()
        assert moved and abs(moved['x'] - start['x'] - 50) < 3
        handle = overview.locator('[data-resize="e"]').bounding_box()
        assert handle
        page.mouse.move(handle['x'] + 2, handle['y'] + 20)
        page.mouse.down()
        page.mouse.move(handle['x'] + 62, handle['y'] + 20, steps=5)
        page.mouse.up()
        grown = overview.bounding_box()
        assert grown and abs(grown['width'] - moved['width'] - 60) < 3
        # Rejection leaves confirmed frequency; a new stream is required after an injected gap.
        page.locator('[data-fault]').select_option('reject')
        page.locator('[data-inject]').click()
        editor = overview.locator('[data-editor]')
        old_hz = editor.locator('[name="frequency"]').input_value()
        editor.locator('[name="frequency"]').fill(str(int(old_hz) + 1000))
        editor.locator('button[type="submit"]').click()
        editor.locator('[data-preview] button').click()
        expect(editor).to_contain_text('FIXTURE_REJECTED')
        expect(editor.locator('[name="frequency"]')).to_have_value(old_hz)
        page.locator('[data-fault]').select_option('gap')
        page.locator('[data-inject]').click()
        expect(editor).to_contain_text('Interrupted')
        page.locator('[data-fault]').select_option('recover')
        page.locator('[data-inject]').click()
        expect(editor).to_contain_text('Receiving')
        # Recording error scoped to C1, keeps C2 healthy.
        overview.locator('[data-uid="fixture-c1"] [data-action="select"]').click()
        page.locator('[data-fault]').select_option('disk')
        page.locator('[data-inject]').click()
        expect(overview.locator('[data-uid="fixture-c1"]')).to_contain_text('Failed')
        expect(overview.locator('[data-uid="fixture-c2"]')).to_contain_text('Receiving')
        page.locator('[data-scale]').select_option('1')
        page.locator('[data-language]').select_option('zh')
        page.locator('#fixture-controls [data-theme]').select_option('dark')
        page.screenshot(path=str(args.artifacts / 'desktop-dark.png'), full_page=True)
        # Reload restores dock geometry, never automatically reopens desktop popups.
        before = overview.bounding_box()
        page.reload()
        expect(overview).to_be_visible()
        restored = overview.bounding_box()
        assert before and restored and abs(before['x'] - restored['x']) < 3
        assert len(context.pages) == 1
        page.set_viewport_size({'width': 800, 'height': 720})
        overview.locator('[data-close]').click()
        page.get_by_role('button', name='重置布局').click()
        expect(overview).to_be_visible()
        page.screenshot(path=str(args.artifacts / 'compact-dark.png'), full_page=True)
        # Vite's own HMR socket is expected; no native receiver/socket traffic is allowed.
        assert not [url for url in network if '/api/' in url or '/ws?' in url], network
        assert not errors, errors
        quick_listen_check(browser, args.url, args.artifacts)
        browser.close()
    print('PASS: fixture workflows, two popups, Dock/child close, themes/languages/scale, drag/resize, rejection/gap/disk, layout restore, right-click/manual quick listening, unit conversion, full capacity, recording isolation; no receiver connections')


if __name__ == '__main__':
    main()
