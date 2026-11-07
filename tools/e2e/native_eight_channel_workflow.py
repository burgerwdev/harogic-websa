#!/usr/bin/env python3
"""Eight-channel workbench gate on an owned synthetic service, separate from RF acceptance."""
from __future__ import annotations

import argparse
import json
import os
import struct
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import numpy as np
from native_multichannel import api, config
from native_multichannel_audio import finish_audio, read_journal
from native_multichannel_processing import assert_receivers_continue
from native_multichannel_recording import task, until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready

MODES = ['am', 'am', 'nfm', 'nfm', 'usb', 'usb', 'lsb', 'lsb']


def setup(origin, *, center=100e6, frequencies=None, reference=None, agc=False, modes=None):
    modes = modes or MODES
    frequencies = frequencies or [center + index * 20000 for index in range(8)]
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(origin, {'cmd': 'SET_SDR', 'center': center, 'listen': center, 'decimate': 1})
    until(lambda: (capture := api(origin)['capture'])['state'] == 'running'
        and capture['center_hz'] == center and capture['input_rate_hz'] == 62500000, timeout=30)
    if reference is not None:
        post(origin, {'cmd': 'SET_REF', 'ref': reference})
        until(lambda: api(origin)['capture']['state'] == 'running', timeout=30)
    for receiver in api(origin)['receivers']:
        reply = api(origin, payload={'type': 'retire', 'request_id': 'clear-' + receiver['receiver_uid'],
            'receiver_uid': receiver['receiver_uid']})
        assert reply['fault'] is None, reply['fault']
    for index, mode in enumerate(modes):
        candidate = config(frequencies[index])
        candidate.update(name=f'Channel {index + 1}', purpose='listen')
        candidate['baseband'].update(passband_hz=16000, transition_hz=1000, minimum_rate_hz=48000)
        candidate['processors'] = [{'id': 'primary', 'kind': 'demodulator', 'mode': mode, 'minimum_rate_hz': 0,
            'bandwidth_hz': 12000 if mode == 'nfm' else 2400 if mode in ['usb', 'lsb'] else 6000,
            'pcm_gain': 0.8, 'agc': agc, 'squelch_dbm': -200}]
        if mode == 'ft8':
            candidate['purpose'] = 'digital'
            candidate['processors'][0].update(kind='decoder', bandwidth_hz=3000)
        elif mode == 'iq':
            candidate.update(purpose='iq', processors=[])
        reply = api(origin, payload={'type': 'create', 'request_id': f'eight-create-{index}', 'config': candidate})
        assert reply['fault'] is None, reply['fault']
    snapshot = api(origin)
    assert snapshot['capture']['usable_bandwidth_hz'] == 50e6 and snapshot['capture']['input_rate_hz'] == 62500000
    return snapshot


def browser_gate(origin, snapshot, artifacts, *, source_kind='fixture'):
    uids = [receiver['receiver_uid'] for receiver in snapshot['receivers']]
    started = api(origin, payload={'type': 'record_start', 'request_id': 'local-isolation-audio',
        'receiver_uid': uids[0], 'kinds': ['audio'], 'audio_format': 'wav'})
    assert started['fault'] is None, started['fault']
    recording = next(row['task_id'] for row in started['snapshot']['recordings'] if row['request_id'] == 'local-isolation-audio')
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1600, 'height': 1100})
        context.add_init_script('''{
            const NativeWorker = window.Worker;
            const probe = window.eightWorkerProbe = {rows: [], overflow: false};
            window.Worker = class extends NativeWorker {
                constructor(...args) {
                    super(...args);
                    const row = {script: String(args[0]), url: null, terminated: false,
                        connected: 0, closed: 0, ended: 0, errors: 0};
                    if (probe.rows.length < 256) probe.rows.push(row); else probe.overflow = true;
                    const send = this.postMessage.bind(this), terminate = this.terminate.bind(this);
                    this.postMessage = (message, ...rest) => {
                        if (message?.type === 'start' && typeof message.url === 'string') row.url = message.url;
                        return send(message, ...rest);
                    };
                    this.terminate = () => {row.terminated = true; return terminate();};
                    this.addEventListener('message', event => {
                        const type = event.data?.type;
                        if (['connected', 'closed', 'ended'].includes(type)) row[type]++;
                        if (type === 'error') row.errors++;
                    });
                }
            };
        }''')
        page = context.new_page()
        errors, commands, sockets, wire_commands, socket_lifecycle = [], [], [], [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: commands.append(json.loads(request.post_data))
            if request.method == 'POST' and '/api/' in request.url and request.post_data else None)
        def websocket(socket):
            sockets.append(socket.url)
            lifecycle = {'url': socket.url, 'closed': False}
            socket_lifecycle.append(lifecycle)
            socket.on('close', lambda *_: lifecycle.update(closed=True))
            def sent(payload):
                try:
                    command = json.loads(payload)
                    if isinstance(command, dict) and command.get('cmd'):
                        wire_commands.append(command)
                except (ValueError, TypeError, UnicodeDecodeError):
                    pass
            socket.on('framesent', sent)
        page.on('websocket', websocket)
        context.on('page', lambda opened: opened.on('websocket', websocket))
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')
        expect(view.locator('.mc-row')).to_have_count(8)

        def row(uid):
            return view.locator(f'.mc-row[data-uid="{uid}"]')

        def select(uid):
            row(uid).locator('[data-action=select]').click()
            expect(view).to_have_attribute('data-receiver-uid', uid)

        def playing(count):
            page.wait_for_function('count=>window.websaAudioStatus().filter(r=>r.enabled&&r.owner==="workbench"&&r.playedFrames>2000&&!r.error).length===count', arg=count)

        if view.locator('[data-action=master-mute]').get_attribute('aria-pressed') == 'true':
            view.locator('[data-action=master-mute]').click()
        for uid in uids:
            button = row(uid).locator('[data-action=listen]')
            if button.get_attribute('aria-pressed') != 'true':
                button.click()
        playing(8)
        baseline = api(origin)
        local_start = len(commands)
        for index in range(7):
            page.locator('[data-action=sdr-channel-next]').click()
            expect(page.locator('#sdr-current-channel')).to_have_text(f'C{index + 2}')
        eighth_frequency = snapshot['receivers'][7]['confirmed']['baseband']['frequency_hz']
        expect(page.locator('#input-sdr-listen')).to_have_value(f'{eighth_frequency / 1e6:.6f}')
        playing(8)
        select(uids[7])
        view.locator('[name=name]').fill('Draft eighth channel')
        select(uids[0])
        select(uids[7])
        expect(view.locator('[name=name]')).to_have_value('Draft eighth channel')
        row(uids[7]).locator('.mc-row-more > summary').click()
        row(uids[7]).locator('[data-action=solo]').click()
        playing(1)
        assert page.evaluate('window.websaAudioStatus().filter(r=>r.enabled).map(r=>r.channel)') == [uids[7]]
        page.evaluate('window.websaNativeMixLevels()')
        page.wait_for_timeout(200)
        initial = page.evaluate('window.websaNativeMixLevels()')
        assert initial['left'] > 0.001 and initial['right'] > 0.001, initial
        view.locator('[data-pan]').press('End')
        page.wait_for_timeout(250)
        right = page.evaluate('window.websaNativeMixLevels()')
        assert right['right'] > 0.001 and right['left'] < 1e-5, right
        view.locator('[data-action=master-mute]').click()
        page.wait_for_timeout(250)
        assert max(page.evaluate('window.websaNativeMixLevels()').values()) < 1e-5
        muted_bytes = int(task(origin, recording)['tracks'][0]['raw_bytes'])
        until(lambda: int(task(origin, recording)['tracks'][0]['raw_bytes']) > muted_bytes)
        view.locator('[data-action=master-mute]').click()
        view.locator('[data-pan]').press('Home')
        row(uids[7]).locator('[data-action=solo]').click()
        playing(8)
        assert commands[local_start:] == [], commands[local_start:]
        assert_receivers_continue(baseline['receivers'], api(origin)['receivers'])

        # Tune the eighth through the main panel; every submitted command keeps its own actor.
        tuned_frequency = eighth_frequency + 100
        page.locator('#input-sdr-listen').fill(str(tuned_frequency / 1e6))
        page.locator('#input-sdr-listen').press('Enter')
        page.locator('[data-action=sdr-channel-prev]').click()
        expect(page.locator('#sdr-current-channel')).to_have_text('C7')
        until(lambda: api(origin)['receivers'][7]['confirmed']['baseband']['frequency_hz'] == tuned_frequency)
        expect(page.locator('#input-sdr-listen')).to_have_value(
            f'{snapshot["receivers"][6]["confirmed"]["baseband"]["frequency_hz"] / 1e6:.6f}')
        page.locator('[data-action=sdr-channel-next]').click()
        expect(page.locator('#sdr-current-channel')).to_have_text('C8')
        # The HTTP snapshot can confirm tuning before the browser receives its
        # status update. Center the frequency actually confirmed in the panel.
        expect(page.locator('#input-sdr-listen')).to_have_value(f'{tuned_frequency / 1e6:.6f}')
        after_tune = api(origin)
        assert_receivers_continue(baseline['receivers'][:7], after_tune['receivers'][:7])
        assert any(command.get('cmd') == 'SET_SDR_CHANNEL' and command.get('channel') == 8
            and command.get('freq') == tuned_frequency for command in wire_commands)
        # Selected-channel centering moves only the capture; C1 keeps its frequency.
        page.locator('[data-action=sdr-to-center]').click()
        until(lambda: api(origin)['capture']['center_hz'] == tuned_frequency)
        after_center = api(origin)
        assert [r['confirmed']['baseband']['frequency_hz'] for r in after_center['receivers']] == [r['confirmed']['baseband']['frequency_hz'] for r in after_tune['receivers']]
        assert after_center['capture']['usable_bandwidth_hz'] == 50e6
        assert any(c.get('cmd') == 'SET_SDR' and c.get('center') == tuned_frequency and 'listen' not in c for c in wire_commands)
        playing(8)

        # Views retain drafts and reuse the page's single speaker/decoder owner.
        # Tuning and recentering legitimately replace processing sources. Count
        # active subscriptions, and forbid any new owner during view-only actions.
        owner_observations = []

        def decoder_uid(url):
            parsed = urlsplit(url or '')
            return parse_qs(parsed.query).get('receiver_uid', [None])[0] if parsed.path == '/ws/decode' else None

        def owner_state():
            probes = [opened.evaluate('window.eightWorkerProbe') for opened in context.pages if not opened.is_closed()]
            assert all(probe is not None and not probe['overflow'] for probe in probes), probes
            workers = [worker for probe in probes for worker in probe['rows']]
            return {uid: {'workers': [worker for worker in workers if decoder_uid(worker['url']) == uid],
                'sockets': [dict(item) for item in socket_lifecycle if decoder_uid(item['url']) == uid]} for uid in uids}

        def stable_owners():
            current = owner_state()
            return current if all(sum(not worker['terminated'] for worker in lane['workers']) == 1
                and all(worker['terminated'] or worker['connected'] > 0 for worker in lane['workers'])
                and sum(not item['closed'] for item in lane['sockets']) == 1 for lane in current.values()) else None

        try:
            before_views = until(stable_owners, timeout=5)
        except AssertionError:
            (artifacts / 'decoder-owner-observation.json').write_text(json.dumps(owner_state(), indent=2) + '\n')
            raise
        owner_observations.append({'stage': 'after source changes', 'owners': before_views})

        def views_kept_owners(stage):
            current = owner_state()
            owner_observations.append({'stage': stage, 'owners': current})
            (artifacts / 'decoder-owner-observation.json').write_text(json.dumps(owner_observations, indent=2) + '\n')
            for uid, lane in current.items():
                assert sum(not worker['terminated'] for worker in lane['workers']) == 1, {'uid': uid, 'workers': lane['workers']}
                assert sum(not item['closed'] for item in lane['sockets']) == 1, {'uid': uid, 'sockets': lane['sockets']}
                assert len(lane['workers']) == len(before_views[uid]['workers']), f'{stage} created another decoder owner for {uid}'
                assert len(lane['sockets']) == len(before_views[uid]['sockets']), f'{stage} opened another decoder subscription for {uid}'
        audio_before = page.evaluate('window.websaAudioStatus().filter(r=>r.enabled&&r.owner==="workbench").map(r=>({channel:r.channel,played:r.playedFrames}))')
        with page.expect_popup() as popup:
            view.locator('[data-pop]').click()
        child = popup.value
        compact = child.locator('[data-view-id=overview-follow]')
        expect(compact.locator('[name=name]')).to_have_value('Draft eighth channel')
        child.set_viewport_size({'width': 420, 'height': 900})
        expect(compact.locator('.mc-row')).to_have_count(8)
        compact.locator('.mc-row').last.locator('[data-action=select]').click()
        expect(compact).to_have_attribute('data-receiver-uid', uids[7])
        child.screenshot(path=str(artifacts / 'eight-420.png'))
        page.locator('#btn-lang').click()
        expect(compact.locator('h1')).to_contain_text('信道工作台')
        child.screenshot(path=str(artifacts / 'eight-420-zh.png'))
        page.locator('#btn-lang').click()
        expect(compact.locator('h1')).to_contain_text('Channel workbench')
        overflow = compact.evaluate('(host)=>host.scrollWidth>host.clientWidth+2')
        assert not overflow, 'narrow workbench overflows horizontally'
        views_kept_owners('pop out, resize and language changes')
        dock = child.locator('[data-dock]')
        dock.scroll_into_view_if_needed()
        box = dock.bounding_box()
        assert box
        with child.expect_event('close'):
            child.mouse.click(box['x'] + box['width'] / 2, box['y'] + box['height'] / 2)
        expect(view.locator('[name=name]')).to_have_value('Draft eighth channel')
        playing(8)
        page.wait_for_function('rows=>rows.every(old=>window.websaAudioStatus().some(r=>r.channel===old.channel&&r.playedFrames>old.played))', arg=audio_before)
        view.locator('[data-close]').click()
        playing(8)
        page.locator('[data-action=sdr-workbench]').click()
        expect(view).to_be_visible()
        views_kept_owners('dock, close and reopen')
        page.screenshot(path=str(artifacts / 'eight-desktop.png'))
        assert not errors, errors
        audio = page.evaluate('window.websaAudioStatus()')
        report = {'source_kind': source_kind, 'capture': after_center['capture'], 'audio': audio,
            'decoder_owners': owner_observations,
            'mix': {'initial_solo': initial, 'pan_right': right}, 'commands': commands, 'wire_commands': wire_commands, 'sockets': sockets,
            'checks': ['eight main selections', 'eight playing routes', 'editing preserves manual mix', 'in-flight tuning actor',
                'draft switching', 'eighth tuning', 'capture-only selected centering', 'solo and master mute',
                'recording independent of local mute', '420px sidebar scrolling', 'Pop out/Dock', 'one decoder owner per receiver']}
        # The workbench selects a surviving definition. The main tuning slot stays
        # vacant so an unsubmitted C8 edit cannot silently target another receiver.
        retired = api(origin, payload={'type': 'retire', 'request_id': 'ui-retire-eighth', 'receiver_uid': uids[7]})
        assert retired['fault'] is None, retired['fault']
        expect(page.locator('#sdr-current-channel')).to_have_text('C8 · Stopped')
        expect(page.locator('#sdr-audio-rate-row')).to_have_count(0)
        expect(view.locator('.mc-row')).to_have_count(7)
        expect(view).to_have_attribute('data-receiver-uid', uids[0])
        playing(7)
        replacement = api(origin, payload={'type': 'create', 'request_id': 'ui-replace-eighth',
            'config': snapshot['receivers'][7]['confirmed']})
        assert replacement['fault'] is None, replacement['fault']
        fresh = next(r['receiver_uid'] for r in replacement['snapshot']['receivers'] if r['display_number'] == 8)
        assert fresh != uids[7]
        expect(view.locator('.mc-row')).to_have_count(8)
        row(fresh).locator('[data-action=listen]').click()
        playing(8)
        page.locator('[data-action=sdr-channel-prev]').click()
        expect(page.locator('#sdr-current-channel')).to_have_text('C7')
        page.locator('[data-action=sdr-channel-next]').click()
        expect(page.locator('#sdr-current-channel')).to_have_text('C8')
        expect(view).to_have_attribute('data-receiver-uid', fresh)
        expect(view.locator('[name=name]')).to_have_value('Channel 8')
        assert uids[7] not in page.evaluate('window.websaAudioStatus().map(r=>r.channel)')
        report['replacement'] = {'retired': uids[7], 'fresh': fresh}
        report['checks'].append('selected deletion and fresh eighth owner')
        context.close()
        browser.close()
    saved = finish_audio(origin, recording)
    records, codecs = read_journal(artifacts / 'recordings' / recording)
    pcm = [payload for metadata, payload in records if len(payload) > 96]
    # The independent direct-WAV/timeline reader checks hashes and original frames.
    # Accepted PCM bytes must all reach disk.
    assert pcm and int(saved['tracks'][0]['raw_bytes']) > 0
    energy, samples = 0.0, 0
    for metadata, payload in records:
        assert metadata['receiver_uid'] == uids[0] and payload[:4] == b'AUD2'
        frames, channels = struct.unpack_from('<I', payload, 12)[0], struct.unpack_from('<I', payload, 56)[0]
        values = np.frombuffer(payload, dtype='<f4', offset=96)
        assert len(values) == frames * channels and np.isfinite(values).all()
        energy += float(np.dot(values, values)); samples += len(values)
    rms = (energy / samples) ** 0.5
    assert rms > 0.001, rms
    report['recording'] = {'task': saved, 'journal_records': len(records), 'pcm_records': len(pcm), 'rms': rms, 'codecs': codecs}
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=False)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log)
        try:
            snapshot = setup(origin)
            report = browser_gate(origin, snapshot, args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: eight-channel synthetic workbench, playback and local-route isolation; RF remains separate')
        finally:
            stop(process)


if __name__ == '__main__':
    main()
