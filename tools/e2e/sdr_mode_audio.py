#!/usr/bin/env python3
"""Owned fixture gate: retained page -> service restart -> RTA/SDR -> speaker PCM."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def playback(page):
    page.wait_for_function('''() => {
      const active = window.websaAudioStatus().filter(row => row.enabled);
      return active.length === 1 && active[0].playing && active[0].playedFrames > 8000
        && active[0].rms > .001 && !active[0].error && active[0].contextState === 'running';
    }''')
    return page.evaluate('window.websaAudioStatus().find(row => row.enabled)')


def speakers(page):
    page.wait_for_function('''() => {
      const levels = window.websaNativeMixLevels();
      return levels.left > .001 && levels.right > .001;
    }''')


def enter_sdr(page):
    page.locator('#btn-mode-rta').click()
    wait_ui_ready(page, 'rta')
    page.locator('#btn-mode-sdr').click()
    wait_ui_ready(page, 'sdr')
    page.locator('#input-sdr-center').fill('101.7')
    page.locator('#input-sdr-center').press('Enter')
    page.wait_for_function('''async () => {
      const snapshot = await (await fetch('/api/sdr/receivers?detail=live')).json();
      const current = snapshot.receivers.find(row => row.display_number === 1);
      return snapshot.capture.center_hz === 101700000
        && current?.confirmed.baseband.frequency_hz === 101700000 && current.state === 'running';
    }''')
    page.locator('[data-sdr-demod=wfm]').click()
    page.wait_for_function('''async () => {
      const snapshot = await (await fetch('/api/sdr/receivers?detail=live')).json();
      const current = snapshot.receivers.find(row => row.display_number === 1);
      const audio = window.websaAudioStatus().find(row => row.enabled && row.channel === current?.receiver_uid);
      return current?.confirmed.processors[0]?.mode === 'wfm' && current.stream
        && audio?.streamKey === `${current.stream.epoch}:${current.stream.channel_id}`
        && audio.playing && audio.playedFrames > 8000;
    }''')
    return playback(page)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', default='/tmp/websa-sdr-mode-audio')
    args = parser.parse_args()
    artifacts = Path(args.artifacts)
    artifacts.mkdir(parents=True, exist_ok=True)
    port = free_port()
    proc, origin = start_service(port)
    cases, errors = [], []
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            page = browser.new_page(viewport={'width': 1600, 'height': 1100})
            page.add_init_script("localStorage.setItem('web-sa-sdr-audio', '1')")
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.goto(origin)
            wait_ui_ready(page, 'std')
            first = enter_sdr(page)
            speakers(page)
            cases.append({'case': 'fresh-rta-sdr-wfm', 'audio': first})
            for muted in [False, True]:
                if muted:
                    page.locator('#btn-sdr-audio').click()
                expect(page.locator('#btn-sdr-audio')).to_have_attribute('aria-pressed', str(muted).lower())
                previous = playback(page)
                stop(proc)
                page.wait_for_function("!document.querySelector('#connDot.on')")
                proc, _ = start_service(port)
                wait_ui_ready(page, 'std')
                current = enter_sdr(page)
                assert current['channel'] != previous['channel'], (previous, current)
                assert current['masterMuted'] == muted, current
                expect(page.locator('#btn-sdr-audio')).to_have_attribute('aria-pressed', str(muted).lower())
                cases.append({'case': 'restart-retained-page', 'muted': muted, 'audio': current})
                if muted:
                    page.locator('#btn-sdr-audio').click()
                speakers(page)
            # Exercise both the same-mode return and rapid superseding requests.
            for _ in range(3):
                page.locator('#btn-mode-rta').click()
                page.locator('#btn-mode-sdr').click()
                wait_ui_ready(page, 'sdr')
                playback(page)
            page.locator('#btn-mode-rta').click()
            wait_ui_ready(page, 'rta')
            page.locator('#btn-mode-rta').click()
            wait_ui_ready(page, 'std')
            cases.append({'case': 'round-trips', 'audio': enter_sdr(page)})
            speakers(page)
            assert not errors, errors
            browser.close()
    finally:
        stop(proc)
        (artifacts / 'result.json').write_text(json.dumps({'cases': cases, 'page_errors': errors}, indent=2) + '\n')
    print(json.dumps({'ok': True, 'cases': len(cases), 'artifacts': str(artifacts)}))


if __name__ == '__main__':
    main()
