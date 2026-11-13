#!/usr/bin/env python3
"""4K plot changes must preserve eight native audio routes on an owned fixture service."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory

from native_eight_channel_workflow import setup
from native_multichannel import api, assert_receivers_continue
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def check(origin, snapshot, vulkan):
    flags = ['--use-angle=vulkan', '--enable-features=Vulkan', '--disable-vulkan-surface'] if vulkan else []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(args=flags)
        try:
            page = browser.new_page(viewport={'width': 3840, 'height': 2050})
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.goto(origin)
            wait_ui_ready(page, 'sdr')
            page.locator('[data-action=sdr-workbench]').click()
            view = page.locator('[data-view-id=overview-follow]')
            expect(view.locator('.mc-row')).to_have_count(8)
            if view.locator('[data-action=master-mute]').get_attribute('aria-pressed') == 'true':
                view.locator('[data-action=master-mute]').click()
            for receiver in snapshot['receivers']:
                uid = receiver['receiver_uid']
                button = view.locator(f'.mc-row[data-uid="{uid}"] [data-action=listen]')
                if button.get_attribute('aria-pressed') != 'true':
                    button.click()
            page.wait_for_function('''() => window.websaAudioStatus().filter(
                row => row.enabled && row.owner === 'workbench' && row.playedFrames > 2000 && !row.error).length === 8''')
            before = api(origin)
            played = page.evaluate('''() => window.websaAudioStatus().filter(row => row.enabled && row.owner === 'workbench')
                .map(row => ({channel: row.channel, played: row.playedFrames}))''')
            for size, layers, fade in [(3840, 'both', '2'), (1920, 'density', '0.2'),
                                       (3840, 'trace', '0'), (3840, 'both', '0.2')]:
                page.set_viewport_size({'width': size, 'height': 2050 if size == 3840 else 1000})
                page.locator('#select-rta-fade').select_option(fade, force=True)
                page.locator('#select-rta-dens-layers').select_option(layers, force=True)
                box = page.locator('#spectrum').bounding_box()
                page.mouse.move(box['x'] + box['width'] * .3, box['y'] + box['height'] * .5)
                page.mouse.move(box['x'] + box['width'] * .7, box['y'] + box['height'] * .5, steps=12)
            page.wait_for_function('''before => before.every(old => window.websaAudioStatus().some(
                row => row.channel === old.channel && row.enabled && !row.error && row.playedFrames > old.played + 24000))''', arg=played)
            after = api(origin)
            assert_receivers_continue(before['receivers'], after['receivers'])
            assert after['capture']['state'] == 'running', after['capture']
            expect(view.locator('.mc-row')).to_have_count(8)
            if vulkan:
                expect(page.locator('#spectrum')).to_have_attribute('data-density-renderer', 'webgl2')
                expect(page.locator('#spectrum')).to_have_attribute('data-trace-renderer', 'webgl2')
            assert not errors, errors
            return page.evaluate('''() => ({render: window.websaRenderStatus(),
                audio: window.websaAudioStatus().filter(row => row.enabled && row.owner === 'workbench')
                    .map(row => ({channel: row.channel, played: row.playedFrames, underruns: row.underruns, error: row.error}))})''')
        finally:
            browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vulkan', action='store_true')
    args = parser.parse_args()
    with TemporaryDirectory(prefix='websa-render-channels-') as temporary, (Path(temporary) / 'service.log').open('w') as log:
        os.environ['WEBSA_RECORDING_DIR'] = str(Path(temporary) / 'recordings')
        process, origin = start_service(free_port(), log=log)
        try:
            result = check(origin, setup(origin), args.vulkan)
            print(json.dumps(result))
            print('PASS: eight audio routes and receiver generations survive 4K spectrum/density changes')
        finally:
            stop(process)


if __name__ == '__main__':
    main()
