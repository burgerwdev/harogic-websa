#!/usr/bin/env python3
"""Owned hardware control/geometry and mixed-browser gates, separate from long observation."""
from __future__ import annotations

import argparse
import asyncio
import copy
import time
from pathlib import Path

from eight_channel_hardware_ui import calibrate_tones, observe_ui, wait_stable_carrier
from eight_channel_recording import checked_files
from eight_channel_rf import (
    CENTER,
    FREQUENCIES,
    MIXED_MODES,
    MIXED_TONES,
    BenchRadio,
    HardwareSession,
    save,
)
from eight_channel_streams import StreamMonitor
from native_eight_channel_workflow import setup
from native_multichannel import api, protocol
from native_multichannel_recording import finish, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import post
from viewport_baseline import wait_ui_ready


def stable(snapshot):
    return [{key: receiver[key] for key in ['receiver_uid', 'revision', 'confirmed', 'stream']}
        for receiver in snapshot['receivers']]


def configure(origin, number, config, key):
    before = api(origin)
    target = next(receiver for receiver in before['receivers'] if receiver['display_number'] == number)
    reply = api(origin, payload={'type': 'configure', 'request_id': key,
        'receiver_uid': target['receiver_uid'], 'expected_revision': target['revision'], 'config': config})
    if reply['fault']:
        assert stable(reply['snapshot']) == stable(before)
        assert reply['snapshot']['capture'] == before['capture']
    else:
        for old, new in zip(stable(before), stable(reply['snapshot']), strict=True):
            if old['receiver_uid'] != target['receiver_uid']:
                assert new == old
    return reply


def capture_choices(origin):
    # Geometry checks use legal narrow IQ definitions; they do not claim known
    # content at every display span. Mixed content is checked at full bandwidth.
    rows = []
    for power in range(12):
        display = 1 << power
        span = 50e6 / display
        for receiver in api(origin)['receivers']:
            candidate = copy.deepcopy(receiver['confirmed'])
            candidate.update(purpose='iq', processors=[])
            candidate['baseband'].update(frequency_hz=CENTER + (receiver['display_number'] - 4.5) * min(span / 10, 8000),
                passband_hz=500, transition_hz=500, minimum_rate_hz=2000)
            assert configure(origin, receiver['display_number'], candidate, f'geometry-{display}-{receiver["display_number"]}')['fault'] is None
        post(origin, {'cmd': 'SET_SDR', 'decimate': display})
        def running():
            value = api(origin)
            capture = value['capture']
            return value if capture['state'] == 'running' and all(receiver['state'] == 'running'
                and receiver.get('clock') and receiver['clock']['input_rate_hz'] == capture['input_rate_hz']
                for receiver in value['receivers']) else None
        snapshot = until(running, timeout=30)
        counts = [int(receiver['clock']['output_index']) for receiver in snapshot['receivers']]
        until(lambda counts=counts: all(int(receiver['clock']['output_index']) > count for receiver, count in
            zip(api(origin)['receivers'], counts, strict=True)), timeout=10)
        target = snapshot['receivers'][-1]
        outside = copy.deepcopy(target['confirmed'])
        outside['baseband']['frequency_hz'] = CENTER + snapshot['capture']['usable_bandwidth_hz'] / 2 - 250
        rejected = configure(origin, 8, outside, f'geometry-edge-{display}')
        assert rejected['fault'] is not None
        state = post(origin, {'cmd': 'STATUS'})
        assert state['req']['sdr']['pfb_taps_per_phase'] == 16
        rows.append({'display_decimate': display, 'display_bandwidth_hz': span,
            'capture': snapshot['capture'], 'receivers': snapshot['receivers'], 'edge_rejection': rejected['fault']})
    post(origin, {'cmd': 'SET_SDR', 'decimate': 1})
    until(lambda: api(origin)['capture']['input_rate_hz'] == 62500000)
    return rows


def processor_config(original, mode, *, nr=False):
    candidate = copy.deepcopy(original)
    candidate['purpose'] = 'digital' if mode == 'ft8' else 'listen'
    bandwidth = 180000 if mode == 'wfm' else 3000 if mode == 'ft8' else 6000
    passband = 6400 if mode == 'ft8' else bandwidth
    candidate['baseband'].update(passband_hz=passband, transition_hz=passband / 8,
        minimum_rate_hz=300000 if mode == 'wfm' else 12000)
    candidate['processors'] = [{'id': 'primary', 'kind': 'decoder' if mode == 'ft8' else 'demodulator',
        'mode': mode, 'bandwidth_hz': bandwidth, 'minimum_rate_hz': 0, 'pcm_gain': .8, 'agc': True,
        'squelch_dbm': -200, 'nr': nr, 'nr_algorithm': 'fastenhancer'}]
    return candidate


def processing_combinations(origin):
    originals = [receiver['confirmed'] for receiver in api(origin)['receivers']]
    reports = []
    wide = configure(origin, 8, processor_config(originals[7], 'wfm'), 'one-wide')
    reports.append({'case': 'one WFM and seven IQ', 'fault': wide['fault'], 'resources': wide['snapshot']['processing_resources']})
    if not wide['fault']:
        time.sleep(.5)
        with StreamMonitor(origin, api(origin)) as monitor:
            until(lambda: monitor.status()['8-audio']['samples'] > 96000)
            before_audio = monitor.status()['8-audio']
            started = time.monotonic()
            observations = []
            for _ in range(12):
                time.sleep(5)
                observation = api(origin)
                assert observation['capture']['state'] == 'running'
                assert observation['capture']['input_rate_hz'] == 62500000
                assert all(row.get('processing_state') != 'failed' for row in observation['receivers'])
                observations.append({'snapshot': observation, 'network': monitor.status()})
            seconds = time.monotonic() - started
            network = monitor.status()
            audio = network['8-audio']
            assert audio['channels'] == 2 and audio['energy'] > before_audio['energy']
            assert abs((audio['samples'] - before_audio['samples']) / audio['rate_hz'] - seconds) < 1
            reports[-1].update(network=network, measured_seconds=seconds, observations=observations,
                scope='continuous stereo PCM from real ADC input; broadcast RF programme content unverified')
        assert not any(receiver['fault'] for receiver in api(origin)['receivers'])
        assert configure(origin, 8, originals[7], 'restore-wide')['fault'] is None
    for number in [7, 8]:
        reply = configure(origin, number, processor_config(originals[number - 1], 'ft8'), f'search-{number}')
        assert reply['fault'] is None, reply['fault']
    before = api(origin)
    third = configure(origin, 6, processor_config(originals[5], 'ft8'), 'third-search')
    assert third['fault'] is None, third['fault']
    time.sleep(45)
    after = api(origin)
    search_rows = [row for row in after['receivers'] if row['display_number'] in [6, 7, 8]]
    assert all(row['processing_state'] != 'failed' for row in search_rows), search_rows
    assert all(sum(lane['searches']['succeeded'] for lane in row['processing_statistics']) >= 1 for row in search_rows), search_rows
    reports.append({'case': 'three independent searches accepted and executed', 'before': before,
        'after': after, 'third_fault': third['fault']})
    for number in [6, 7, 8]:
        assert configure(origin, number, originals[number - 1], f'restore-search-{number}')['fault'] is None
    for number in [1, 2]:
        neural = configure(origin, number, processor_config(originals[number - 1], 'am', nr=True), f'neural-{number}')
        assert neural['fault'] is None, neural['fault']
    neural_observations = []
    for _ in range(12):
        time.sleep(5)
        observation = api(origin)
        assert all(row.get('processing_state') != 'failed' for row in observation['receivers']), observation
        neural_observations.append(observation)
    actual = api(origin)
    for number in [1, 2]:
        row = next(receiver for receiver in actual['receivers'] if receiver['display_number'] == number)
        assert row['processing_state'] != 'failed', row
        assert any(lane['packets']['successful_samples'] > 0 and lane['receiver'].get('nr_effective') == 'fastenhancer'
            for lane in row['processing_statistics']), row
    reports.append({'case': 'two independently selected neural reducers output for 60 seconds', 'snapshot': actual, 'observations': neural_observations})
    for number in [1, 2]:
        assert configure(origin, number, originals[number - 1], f'restore-neural-{number}')['fault'] is None
    assert api(origin)['capture']['usable_bandwidth_hz'] == 50e6
    return reports


def mixed_browser(origin, snapshot, artifacts):
    uids = [receiver['receiver_uid'] for receiver in snapshot['receivers']]
    reply = api(origin, payload={'type': 'record_start', 'request_id': 'mixed-ui-audio', 'receiver_uid': uids[0], 'kinds': ['audio']})
    assert reply['fault'] is None
    task_id = next(task['task_id'] for task in reply['snapshot']['recordings'] if task['request_id'] == 'mixed-ui-audio')
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1600, 'height': 1100})
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')
        expect(view.locator('.mc-row')).to_have_count(8)
        if view.locator('[data-action=master-mute]').get_attribute('aria-pressed') == 'true':
            view.locator('[data-action=master-mute]').click()
        for uid in uids[:7]:
            button = view.locator(f'.mc-row[data-uid="{uid}"] [data-action=listen]')
            if button.get_attribute('aria-pressed') != 'true':
                button.click()
        def playing(count):
            try:
                page.wait_for_function('count=>window.websaAudioStatus().filter(r=>r.enabled&&r.owner==="workbench"&&r.playedFrames>2000&&!r.error).length===count', arg=count)
            except Exception:
                save(artifacts / 'ui-audio-failure.json', page.evaluate('window.websaAudioStatus()'))
                page.screenshot(path=str(artifacts / 'ui-audio-failure.png'))
                raise
        playing(7)
        eighth = view.locator(f'.mc-row[data-uid="{uids[7]}"]')
        expect(eighth.locator('[data-action=listen]')).to_have_count(0)
        expect(eighth.locator('[data-action=channel-view]')).to_be_enabled()
        eighth.locator('[data-action=select]').click()
        expect(view).to_have_attribute('data-receiver-uid', uids[7])
        for number in range(2, 9):
            page.locator('[data-action=sdr-channel-next]').click()
            expect(page.locator('#sdr-current-channel')).to_have_text(f'C{number}')
        expect(page.locator('#sdr-current-channel')).to_have_text('C8')
        digital = view.locator(f'.mc-row[data-uid="{uids[6]}"]')
        digital.locator('.mc-row-more > summary').click()
        digital.locator('[data-action=results]').click()
        results = page.locator('[data-view-id^=results]').first
        expect(results).to_contain_text('CQ WE0BSA PM95', timeout=90000)
        before = int(next(task for task in api(origin)['recordings'] if task['task_id'] == task_id)['accepted_bytes'])
        page.evaluate('window.websaNativeMixLevels()')
        page.wait_for_timeout(250)
        audible = page.evaluate('window.websaNativeMixLevels()')
        assert max(audible.values()) > 1e-4, audible
        view.locator('[data-action=master-mute]').click()
        playing(7)
        page.wait_for_timeout(250)
        muted = page.evaluate('window.websaNativeMixLevels()')
        assert max(muted.values()) < 1e-5, muted
        assert all(row['masterMuted'] for row in page.evaluate('window.websaAudioStatus()') if row['enabled'])
        until(lambda: int(next(task for task in api(origin)['recordings'] if task['task_id'] == task_id)['accepted_bytes']) > before)
        view.locator('[data-action=master-mute]').click(); playing(7)
        page.wait_for_timeout(250)
        resumed = page.evaluate('window.websaNativeMixLevels()')
        assert max(resumed.values()) > 1e-4, resumed
        # Hide the overlapping overview with its real control before operating
        # the results window. Listening belongs to the session, not this view.
        view.locator('[data-close]').click()
        playing(7)
        with page.expect_popup() as popup:
            results.locator('[data-pop]').click()
        child = popup.value
        child.set_viewport_size({'width': 420, 'height': 900})
        fixed = child.locator('[data-view-id^=results]').first
        expect(fixed).to_contain_text('CQ WE0BSA PM95')
        child.screenshot(path=str(artifacts / 'mixed-results-420.png'))
        dock = fixed.locator('[data-dock]')
        dock.scroll_into_view_if_needed()
        box = dock.bounding_box()
        assert box
        with child.expect_event('close'):
            child.mouse.click(box['x'] + box['width'] / 2, box['y'] + box['height'] / 2)
        playing(7)
        page.screenshot(path=str(artifacts / 'mixed-desktop.png'))
        audio = page.evaluate('window.websaAudioStatus()')
        browser.close()
    saved = finish(origin, task_id)
    recording = checked_files(artifacts / 'recordings', saved)
    return {'audio': audio, 'mix': {'audible': audible, 'muted': muted, 'resumed': resumed},
        'recording': recording, 'checks': ['seven playing routes', 'eighth IQ selection',
        'seventh live FT8 text', 'local mute preserves recording', '420px results Pop out/Dock']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', required=True, type=Path)
    parser.add_argument('--expect-service-id', required=True)
    parser.add_argument('--gate', choices=['controls', 'processing', 'mixed-ui'], required=True)
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=False)
    with HardwareSession(args.artifacts, 'http://127.0.0.1:8080', args.expect_service_id) as session:
        if args.gate in ['controls', 'processing']:
            controls = asyncio.run(protocol(session.origin, 8, raw_capture=True, center=CENTER))
            save(args.artifacts / 'controls.json', controls)
            choices = capture_choices(session.origin) if args.gate == 'controls' else []
            if choices:
                save(args.artifacts / 'capture-choices.json', choices)
            combinations = processing_combinations(session.origin)
            save(args.artifacts / 'report.json', {'controls': controls, 'capture_choices': choices, 'processing': combinations,
                'scope': 'Real ADC geometry/control and processing admission. Known RF content and sustained workloads are separate.'})
        else:
            with BenchRadio(args.artifacts, gain=-10, lo_correction=-100) as source:
                source.mixed()
                snapshot = setup(session.origin, center=CENTER, frequencies=FREQUENCIES, reference=-40, agc=True, modes=MIXED_MODES)
                source.track_carrier(session.origin, snapshot)
                with StreamMonitor(session.origin, api(session.origin)) as monitor:
                    try:
                        asyncio.run(wait_stable_carrier(session.origin, snapshot, args.artifacts, MIXED_TONES))
                        tones = asyncio.run(calibrate_tones(session.origin, snapshot, args.artifacts, source, MIXED_TONES))
                        until(lambda: monitor.status()['7-decode']['expected_text_count'] > 0, timeout=90)
                        warmup = monitor.status()
                    finally:
                        with monitor.lock:
                            save(args.artifacts / 'network-final.json', {'streams': monitor.rows, 'errors': monitor.errors})
                with observe_ui(session, args.artifacts):
                    report = mixed_browser(session.origin, snapshot, args.artifacts)
                    source.check_stream()
                    report.update(known_tones=tones, warmup_network=warmup, capture=api(session.origin)['capture'])
                    save(args.artifacts / 'report.json', report)
    print('PASS: owned eight-channel ' + args.gate + ' gate; original hardware states restored')


if __name__ == '__main__':
    main()
