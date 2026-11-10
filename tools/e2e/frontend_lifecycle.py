#!/usr/bin/env python3
"""Mixed receiver UI, speaker ownership and clear-all resource cleanup in Chromium.

Defaults to an owned synthetic service. --origin exercises and changes an existing
service; RF quality and device throughput remain separate from these UI assertions.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import tempfile
import time
import uuid
from pathlib import Path

from native_eight_channel_workflow import setup
from native_multichannel import api
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready

PROBE = """{
  const BaseWorker = Worker;
  window.frontendWorkers = []; window.frontendSources = [];
  window.Worker = class extends BaseWorker {
    constructor(...args) {
      super(...args);
      const row = {script: String(args[0]), url: '', terminated: false, frames: 0};
      frontendWorkers.push(row); frontendSources.push(row);
      const post = this.postMessage.bind(this), terminate = this.terminate.bind(this), sources = new Map();
      this.postMessage = (message, ...rest) => {
        if (message.url) row.url = message.url;
        for (const item of message.type === 'source-commands' ? message.items : []) {
          if (item.message.url && !sources.has(item.clientId)) {
            const source = {url: String(item.message.url), terminated: false, frames: 0};
            sources.set(item.clientId, source); frontendSources.push(source);
          }
          if (item.message.type === 'dispose' && sources.has(item.clientId)) sources.get(item.clientId).terminated = true;
        }
        return post(message, ...rest);
      };
      this.terminate = () => { row.terminated = true; return terminate(); };
      this.addEventListener('message', event => {
        if (event.data?.type === 'frames') row.frames++;
        for (const item of event.data?.type === 'source-events' ? event.data.items : []) {
          if (item.message.type === 'frames' && sources.has(item.clientId)) sources.get(item.clientId).frames++;
        }
      });
    }
  };
  const portSend = MessagePort.prototype.postMessage, portClose = MessagePort.prototype.close;
  MessagePort.prototype.postMessage = function(message, ...rest) {
    if (message?.url && !this.frontendSource) {
      const row = this.frontendSource = {url: String(message.url), terminated: false, frames: 0};
      frontendSources.push(row);
      this.addEventListener('message', event => {
        if (event.data?.type === 'frames') row.frames++;
      });
    }
    return portSend.call(this, message, ...rest);
  };
  MessagePort.prototype.close = function() { if (this.frontendSource) this.frontendSource.terminated = true; return portClose.call(this); };
}"""
MODES = ['am', 'ft8', 'nfm', 'usb', 'lsb', 'cw', 'am', 'ft8']


def observe_playback(page, browser, origin, count, seconds, artifacts):
    """Observe actual playback and source continuity, without retaining media."""
    if not seconds:
        return []
    system = browser.new_browser_cdp_session()
    initial = api(origin, '/api/sdr/receivers?detail=live')
    previous = {row['id']: row['cpuTime'] for row in system.send('SystemInfo.getProcessInfo')['processInfo']}
    previous_audio = {row['channel']: row for row in page.evaluate('websaAudioStatus()') if row['enabled']}
    started = sampled = time.monotonic()
    observations = []
    while time.monotonic() - started < seconds:
        page.wait_for_timeout(min(5000, max(1, seconds * 1000 - (time.monotonic() - started) * 1000)))
        now = time.monotonic()
        snapshot = api(origin, '/api/sdr/receivers?detail=live')
        audio = {row['channel']: row for row in page.evaluate('websaAudioStatus()') if row['enabled']}
        processes = {row['id']: row['cpuTime'] for row in system.send('SystemInfo.getProcessInfo')['processInfo']}
        cpu = sum(max(0, value - previous.get(pid, value)) for pid, value in processes.items()) / (now - sampled) * 100
        observations.append({'elapsed_seconds': round(now - started, 2), 'browser_cpu_percent': round(cpu, 2),
                             'playing_channels': sum(bool(row['playing']) for row in audio.values()),
                             'capture_epoch': snapshot['capture']['epoch'], 'capture_state': snapshot['capture']['state'],
                             'underruns': sum(row['underruns'] for row in audio.values()),
                             'audio_errors': [row['error'] for row in audio.values() if row['error']]})
        (artifacts / 'playback-observations.json').write_text(json.dumps(observations, indent=2) + '\n')
        if snapshot['service_id'] != initial['service_id'] or snapshot['capture'] != initial['capture']:
            failure = {'receivers': snapshot, 'audio': list(audio.values()),
                       'state': api(origin, '/api/state'), 'render': page.evaluate('websaRenderStatus()')}
            (artifacts / 'capture-interruption.json').write_text(json.dumps(failure, indent=2) + '\n')
            raise AssertionError(f'Capture continuity changed: {snapshot["capture"]}')
        assert len(snapshot['receivers']) == count and all(row['state'] == 'running' and not row.get('fault')
                                                        for row in snapshot['receivers'])
        assert len(audio) == count and audio.keys() == previous_audio.keys(), audio
        for uid, row in audio.items():
            assert row['playing'] and not row['error'] and row['streamKey'] == previous_audio[uid]['streamKey'], row
            assert row['playedFrames'] > previous_audio[uid]['playedFrames'], row
            assert row['underruns'] == previous_audio[uid]['underruns'], row
        previous, previous_audio, sampled = processes, audio, now
    return observations


def check(origin, count, artifacts, *, seconds=0, mode_set='mixed', spread=False, hardware_gpu=False):
    center = api(origin)['capture']['center_hz']
    if not center or not 1e6 <= center <= 5e9:
        center = 100e6
    modes = ['am'] * count if mode_set == 'am' else (MODES * 2)[:count]
    offsets = [0] + [i * 3e6 for i in range(-8, 9) if i]
    frequencies = [center + offset for offset in offsets[:count]] if spread else [center + i * 20000 for i in range(count)]
    snapshot = setup(origin, center=center, modes=modes, frequencies=frequencies)
    current = next(row['receiver_uid'] for row in snapshot['receivers'] if row['display_number'] == 1)
    errors = []
    with sync_playwright() as playwright:
        launch = {'headless': True, 'args': ['--autoplay-policy=no-user-gesture-required']}
        if hardware_gpu:
            launch.update(executable_path='/usr/bin/chromium',
                          ignore_default_args=['--use-angle=swiftshader-webgl', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'])
            launch['args'].append('--enable-gpu')
        browser = playwright.chromium.launch(**launch)
        gpu = browser.new_browser_cdp_session().send('SystemInfo.getInfo')['gpu']
        if hardware_gpu:
            assert gpu['featureStatus']['gpu_compositing'] == 'enabled', gpu
        page = browser.new_page(viewport={'width': 1600, 'height': 1100})
        page.add_init_script(PROBE)
        page.on('pageerror', lambda error: errors.append(str(error)))
        diagnostics_requests = []
        page.on('request', lambda request: diagnostics_requests.append(request.url)
                if '/api/sdr/receivers?receiver_uid=' in request.url else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-overview]').click()
        cards = page.locator('.mc-overview-card')
        expect(cards).to_have_count(count)
        for uid in [row['receiver_uid'] for row in snapshot['receivers']]:
            button = page.locator(f'.mc-overview-card[data-uid="{uid}"] [data-action=listen]')
            if button.count() and button.get_attribute('aria-pressed') != 'true':
                button.click()
        page.wait_for_function('n => frontendSources.filter(w => !w.terminated && w.url.includes("/ws/decode?") && w.frames).length === n', arg=count)
        assert not page.evaluate('frontendWorkers.some(w => !w.terminated && /\\/ws\\/decode$/.test(w.url))')
        page.wait_for_function('n => websaAudioStatus().filter(r => r.owner === "workbench" && r.enabled && r.playing && r.playedFrames > 2000 && !r.error).length === n', arg=count)
        if seconds:
            page.locator('[data-overview-fit]').click()
        observations = observe_playback(page, browser, origin, count, seconds, artifacts)
        assert not diagnostics_requests, diagnostics_requests
        page.locator(f'.mc-overview-card[data-uid="{current}"] [data-action=detail]').click()
        detail = page.locator(f'.mc-window[data-view-id="detail-{current}"]')
        diagnostic = detail.locator('[data-section=live-diagnostics]')
        with page.expect_response(lambda response: '/api/sdr/receivers?receiver_uid=' in response.url):
            diagnostic.locator('summary').click()
        expect(diagnostic.locator('pre')).to_contain_text('"statistics"')
        diagnostic.locator('summary').click()
        collapsed_requests = len(diagnostics_requests)
        page.wait_for_timeout(2300)
        assert len(diagnostics_requests) == collapsed_requests, diagnostics_requests
        with page.expect_response(lambda response: '/api/sdr/receivers?receiver_uid=' in response.url):
            diagnostic.locator('summary').click()
        detail.locator('[data-close]').click()
        hidden_requests = len(diagnostics_requests)
        page.wait_for_timeout(2300)
        assert len(diagnostics_requests) == hidden_requests, diagnostics_requests
        expect(cards).to_have_count(count)

        recording_ids = []
        last_kind = 'results' if snapshot['receivers'][-1]['confirmed']['purpose'] == 'digital' else 'audio'
        for row, kind in [(snapshot['receivers'][0], 'audio'), (snapshot['receivers'][-1], last_kind)]:
            request = 'frontend-clear-recording-' + str(uuid.uuid4())
            command = {'type': 'record_start', 'request_id': request,
                       'receiver_uid': row['receiver_uid'], 'kinds': [kind]}
            if kind == 'audio':
                command['audio_format'] = 'wav'
            reply = api(origin, payload=command)
            assert reply['fault'] is None, reply['fault']
            recording_ids.append(next(task['task_id'] for task in reply['snapshot']['recordings']
                                      if task.get('request_id') == request))

        # The saved card owns the audible frequency even when Current has different
        # local gain/pan. An off switch must not promote the invisible source.
        if count < snapshot['capabilities']['max_channels']:
            page.locator(f'.mc-overview-card[data-uid="{current}"] [data-action=save-current]').click()
            page.wait_for_function('old => !websaAudioStatus().some(r => r.channel === old && r.enabled)', arg=current)
            expect(cards).to_have_count(count)
            defined = api(origin)['receivers']
            saved = next(row['receiver_uid'] for row in defined if row['receiver_uid'] != current
                         and abs(row['confirmed']['baseband']['frequency_hz'] - center) < .5)
            saved_card = page.locator(f'.mc-overview-card[data-uid="{saved}"]')
            expect(saved_card.locator('[data-action=listen]')).to_have_attribute('aria-pressed', 'true')
            saved_card.locator('[data-action=listen]').click()
            page.wait_for_function('ids => ids.every(id => !websaAudioStatus().some(r => r.channel === id && r.enabled))', arg=[current, saved])
            page.locator('#input-sdr-listen').fill(str((center + 7000) / 1e6))
            page.locator('#input-sdr-listen').press('Enter')
            page.wait_for_function('id => websaAudioStatus().some(r => r.channel === id && r.enabled && r.playing && r.playedFrames > 2000)', arg=current)
            expect(cards).to_have_count(count + 1)
            assert next(row for row in api(origin)['receivers'] if row['receiver_uid'] == saved)['confirmed']['baseband']['frequency_hz'] == center

        clear = page.locator('[data-overview-clear]')
        before = api(origin)
        clear.click(); expect(page.locator('[data-clear-channels-dialog]')).to_be_visible()
        assert api(origin)['revision'] == before['revision']
        page.screenshot(path=str(artifacts / f'clear-confirm-{count}.png'))
        page.locator('[data-clear-cancel]').click()
        assert api(origin)['revision'] == before['revision']
        clear.click(); page.locator('[data-clear-confirm]').click()
        page.wait_for_function('() => !frontendWorkers.some(w => !w.terminated) && !frontendSources.some(w => !w.terminated)')
        page.wait_for_function('() => !websaAudioStatus().some(r => r.owner === "workbench" && r.enabled)')
        page.wait_for_function('() => websaAudioStatus().every(r => r.contextState === "suspended" || r.contextState === "none")')
        expect(cards).to_have_count(1)
        remaining = api(origin)['receivers']
        assert len(remaining) == 1 and remaining[0]['receiver_uid'] == current
        assert not remaining[0]['confirmed']['enabled']
        assert remaining[0]['state'] == 'stopped'
        page.wait_for_function('''async ids => {
          const snapshot = await (await fetch('/api/sdr/receivers')).json();
          return ids.every(id => snapshot.recordings.some(task => task.task_id === id && task.state === 'completed'));
        }''', arg=recording_ids)
        retained_recordings = [task for task in api(origin)['recordings'] if task['task_id'] in recording_ids]
        assert len(retained_recordings) == 2
        assert all(not task.get('fault') and sum(int(track['stored_bytes']) for track in task['tracks']) > 0
                   for task in retained_recordings), retained_recordings
        page.screenshot(path=str(artifacts / f'cleared-{count}.png'))
        retired_workers = page.evaluate('frontendWorkers')

        # Removing every saved source must leave a responsive UI and a usable
        # Current receiver. Its explicit restart gets one fresh speaker/decoder.
        page.locator('#input-sdr-listen').fill(str((center + 11000) / 1e6))
        page.locator('#input-sdr-listen').press('Enter')
        page.wait_for_function('id => websaAudioStatus().some(r => r.channel === id && r.enabled && r.playing && r.playedFrames > 2000 && !r.error)', arg=current)
        active = page.evaluate('frontendWorkers.filter(w => !w.terminated)')
        assert len(active) == 2, active
        assert not errors, errors
        report = {'channels': count, 'modes': modes, 'source': snapshot['capture']['source_kind'],
                  'graphics': {'devices': gpu['devices'], 'features': gpu['featureStatus']},
                  'frequencies_hz': frequencies, 'playback_observations': observations,
                  'retired_workers': retired_workers, 'restarted_workers': active,
                  'retained_recordings': [{'task_id': task['task_id'], 'state': task['state'],
                    'stored_bytes': sum(int(track['stored_bytes']) for track in task['tracks'])}
                    for task in retained_recordings],
                  'errors': errors, 'checks': ['two shared ingress Workers with independent subscriptions per UID',
                    *(['saved card speaker ownership'] if count < snapshot['capabilities']['max_channels'] else []),
                    'diagnostics fetched only while expanded and visible',
                    'clear cancellation', 'clear confirmation', 'zero retained workers after clear',
                    'active recordings completed with files retained',
                    'Current stopped with tuning retained', 'explicit restart after clear']}
        (artifacts / f'frontend-lifecycle-{count}.json').write_text(json.dumps(report, indent=2) + '\n')
        # Leave an existing service with its temporary receiver stopped.
        clear.click(); page.locator('[data-clear-confirm]').click()
        page.wait_for_function('() => !frontendWorkers.some(w => !w.terminated) && !frontendSources.some(w => !w.terminated)')
        browser.close()
        # Only files created by this gate are removed, after retention checks.
        for task_id in recording_ids:
            reply = api(origin, payload={'type': 'record_delete', 'request_id': 'frontend-cleanup-' + str(uuid.uuid4()),
                                         'task_id': task_id})
            assert reply['fault'] is None, reply['fault']
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument('--origin')
    source.add_argument('--hardware', action='store_true', help='Own a hardware service on a private port; requires WEBSA_RUST_IQS_SHIM and exclusive device access')
    parser.add_argument('--channels', type=int, choices=[8, 16], default=16)
    parser.add_argument('--seconds', type=float, default=0, help='Observe all channels playing before the lifecycle checks; retains no media')
    parser.add_argument('--mode-set', choices=['am', 'mixed'], default='mixed')
    parser.add_argument('--spread', action='store_true', help='Spread receivers across the full 50 MHz capture')
    parser.add_argument('--hardware-gpu', action='store_true', help='Use system Chromium with actual GPU compositing; fails if unavailable')
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-frontend-lifecycle'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    if not math.isfinite(args.seconds) or args.seconds < 0:
        parser.error('--seconds must be finite and nonnegative')
    proc = None
    with tempfile.TemporaryDirectory(prefix='websa-frontend-recordings-') as recording_dir, (args.artifacts / 'service.log').open('w') as service_log:
        os.environ['WEBSA_RECORDING_DIR'] = recording_dir
        try:
            if args.origin:
                origin = args.origin
            else:
                proc, origin = start_service(free_port(), hardware=args.hardware, log=service_log)
            report = check(origin, args.channels, args.artifacts, seconds=args.seconds, mode_set=args.mode_set,
                           spread=args.spread, hardware_gpu=args.hardware_gpu)
            print(json.dumps({key: value for key, value in report.items() if 'workers' not in key}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
