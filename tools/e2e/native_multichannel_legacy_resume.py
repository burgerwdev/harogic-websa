#!/usr/bin/env python3
"""Owned synthetic gate: Stop all -> main SDR tuning/mode -> real browser PCM.

Always creates an isolated fake service; accepts no existing/hardware origin.
"""
from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def until(origin, predicate):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        snapshot = api(origin)
        if predicate(snapshot):
            return snapshot
        time.sleep(0.05)
    raise AssertionError('service did not confirm the requested change')


def browser_gate(origin, artifacts):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1600, 'height': 1100})
        errors, commands, cases = [], [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        def sent(payload):
            if isinstance(payload, str):
                message = json.loads(payload)
                if isinstance(message, dict) and 'cmd' in message:
                    commands.append(message)
        page.on('websocket', lambda socket: socket.on('framesent', sent))
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        expect(overview).to_be_visible()
        initial = until(origin, lambda s: all(r['stream'] for r in s['receivers']))
        uid = initial['receivers'][0]['receiver_uid']
        for action in ['tune', 'same-frequency', 'center', 'mode', 'same-mode-muted']:
            if not overview.is_visible():
                page.locator('[data-action=sdr-workbench]').click()
            before = api(origin)
            for receiver in before['receivers']:
                if not receiver['confirmed']['enabled']:
                    continue
                overview.locator(f'[data-uid="{receiver["receiver_uid"]}"] [data-action=select]').click()
                overview.locator('[data-action=toggle-receiver]').click()
                expect(overview.locator('[data-action=toggle-receiver]')).to_have_attribute('aria-checked', 'false')
            stopped = until(origin, lambda s: all(not r['confirmed']['enabled'] for r in s['receivers']))
            page.wait_for_function('window.websaAudioStatus().every(r => !r.enabled)')
            # C2 remains the editor target; main controls still explicitly target C1.
            overview.locator('.mc-row').nth(1).locator('[data-action=select]').click()
            overview.locator('[data-monitor-mode]').select_option('follow')
            muted = action == 'same-mode-muted'
            if muted:
                overview.locator('[data-action=master-mute]').click()
            overview.locator('[data-close]').click()
            start = len(commands)
            if action in ['mode', 'same-mode-muted']:
                page.locator('[data-sdr-demod=usb]').click()
            else:
                field = '#input-sdr-center' if action == 'center' else '#input-sdr-listen'
                frequency = 100.01 if action == 'center' else 100.005
                page.locator(field).fill(str(frequency)); page.locator(field).press('Enter')
            try:
                resumed = until(origin, lambda s: s['receivers'][0]['confirmed']['enabled'] and s['receivers'][0]['state'] == 'running')
            except AssertionError:
                (artifacts / 'failure.json').write_text(json.dumps({'action': action, 'commands': commands[start:],
                    'snapshot': api(origin), 'playback': page.evaluate('window.websaAudioStatus()'),
                    'page_errors': errors}, indent=2) + '\n')
                page.screenshot(path=str(artifacts / 'failure.png'))
                raise
            assert resumed['receivers'][0]['receiver_uid'] == uid
            assert resumed['receivers'][0]['stream'] != before['receivers'][0]['stream']
            assert [r['confirmed'] for r in resumed['receivers'][1:]] == [r['confirmed'] for r in stopped['receivers'][1:]]
            page.wait_for_function('''uid => {
              const active = window.websaAudioStatus().filter(r => r.enabled);
              return active.length === 1 && active[0].channel === uid && active[0].owner === 'workbench'
                && !active[0].error && active[0].playing && active[0].playedFrames > 4000;
            }''', arg=uid)
            page.evaluate('window.websaNativeMixLevels()')
            page.wait_for_timeout(250)
            if muted:
                assert max(page.evaluate('window.websaNativeMixLevels()').values()) < 1e-5
                expect(page.locator('#btn-sdr-audio')).to_have_attribute('aria-pressed', 'true')
                page.locator('#btn-sdr-audio').click()
            page.wait_for_function('''() => {
              const levels = window.websaNativeMixLevels(); return levels.left > 0.001 && levels.right > 0.001;
            }''')
            playback = page.evaluate('window.websaAudioStatus()')
            frames = next(row['playedFrames'] for row in playback if row['channel'] == uid)
            page.wait_for_function('([uid,frames]) => window.websaAudioStatus().some(r => r.channel === uid && r.playedFrames > frames + 4000)', arg=[uid, frames])
            edits = [command for command in commands[start:] if command['cmd'].startswith('SET_SDR')]
            assert edits and all('volume' not in command for command in edits), edits
            assert all(command['cmd'] != 'SET_SDR_CHANNEL' or command.get('channel', 1) == 1 for command in edits)
            cases.append({'action': action, 'stopped': stopped, 'resumed': resumed, 'commands': edits,
                          'playback': playback, 'mix_levels': page.evaluate('window.websaNativeMixLevels()')})
            page.screenshot(path=str(artifacts / f'{action}.png'))
        assert not errors, errors
        browser.close()
        return {'source': 'synthetic native IQ/AM; no RF or physical listening', 'cases': cases, 'page_errors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args(); artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=False)
    os.environ['WEBSA_RECORDING_DIR'] = str(artifacts / 'recordings')
    with (artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log)
        try:
            setup(origin)
            report = browser_gate(origin, artifacts)
            (artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: stopped C1 resumes via tune/same-frequency/center/mode with one UID PCM consumer')
        finally:
            stop(process)


if __name__ == '__main__':
    main()
