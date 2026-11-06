#!/usr/bin/env python3
"""P4 native UID audio/decode and browser mixing gate, using an isolated synthetic/replay service."""
from __future__ import annotations

import argparse
import asyncio
import copy
import json
import math
import re
import struct
from pathlib import Path

from native_multichannel import api, config
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready
from websockets.asyncio.client import connect
from websockets.sync.client import connect as sync_connect


def setup(origin):
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(origin, {'cmd': 'SET_SDR', 'center': 100e6})
    for receiver in api(origin)['receivers']:
        reply = api(origin, payload={'type': 'retire', 'request_id': 'clear-' + receiver['receiver_uid'], 'receiver_uid': receiver['receiver_uid']})
        assert reply['fault'] is None, reply
    for index, mode in enumerate(['am', 'ft8', None]):
        candidate = config(100e6 + index * 10000)
        candidate['name'] = ['AM native', 'FT8 replay', 'IQ native'][index]
        if mode:
            candidate['purpose'] = 'listen' if mode == 'am' else 'digital'
            candidate['baseband'].update(passband_hz=16000, minimum_rate_hz=48000)
            candidate['processors'] = [{'id': 'primary', 'kind': 'demodulator' if mode == 'am' else 'decoder', 'mode': mode,
                'minimum_rate_hz': 0, 'pcm_gain': 0.5, 'agc': False,
                'bandwidth_hz': 6000 if mode == 'am' else 3000, 'squelch_dbm': -160}]
        reply = api(origin, payload={'type': 'create', 'request_id': f'create-{index}', 'config': candidate})
        assert reply['fault'] is None, reply
    return api(origin)


async def protocol(origin, snapshot):
    receiver = snapshot['receivers'][0]
    uid = receiver['receiver_uid']
    endpoint = origin.replace('http', 'ws')
    trace = {'audio_frames': 0, 'audio_samples': 0, 'power': 0.0, 'events': []}
    async with connect(endpoint + '/ws/audio?receiver_uid=' + uid) as audio, connect(endpoint + '/ws/decode?receiver_uid=' + uid) as decode:
        previous = None
        async with asyncio.timeout(10):
            while trace['audio_samples'] < 24000:
                message = await audio.recv()
                assert isinstance(message, bytes) and message[:4] == b'AUD2'
                seq, count = struct.unpack_from('<II', message, 8)
                rate, frequency = struct.unpack_from('<dd', message, 16)
                identity = struct.unpack_from('<II', message, 32)
                index = struct.unpack_from('<Q', message, 64)[0]
                channels = struct.unpack_from('<I', message, 56)[0]
                assert rate == 48000 and frequency == receiver['confirmed']['baseband']['frequency_hz']
                assert len(message) == 96 + count * channels * 4
                if previous:
                    assert seq == previous[0] + 1 and index == previous[1] + previous[2]
                    assert identity == previous[3]
                samples = struct.unpack_from(f'<{count*channels}f', message, 96)
                assert all(math.isfinite(value) for value in samples)
                trace['power'] += sum(value * value for value in samples)
                trace['audio_frames'] += 1; trace['audio_samples'] += count
                previous = seq, index, count, identity
        assert trace['power'] / trace['audio_samples'] > 0.001, (trace, receiver['confirmed'])
        async with asyncio.timeout(10):
            while not any(event['event']['type'] == 'am-info' for event in trace['events']):
                message = await decode.recv()
                assert message[:4] == b'SDE2'
                event = json.loads(message[8:]); trace['events'].append(event)
                assert event['receiver_uid'] == uid and event['service_id'] == snapshot['service_id']
                assert event['baseband_stream'] == receiver['stream']
                assert str(event['epoch']) != receiver['stream']['epoch'], 'processing identity must be independent of IQ'
    for path in ['audio', 'decode']:
        try:
            async with connect(endpoint + '/ws/' + path + '?receiver_uid=' + snapshot['receivers'][2]['receiver_uid']):
                raise AssertionError('IQ-only accepted processed output')
        except Exception as error:
            assert getattr(getattr(error, 'response', None), 'status_code', None) == 409, error
    return trace


def assert_receivers_continue(before, after):
    """Configuration/identities stay fixed while actual sample clocks advance."""
    assert len(before) == len(after)
    for old, current in zip(before, after, strict=True):
        observations = {'clock', 'processing_statistics', 'processing_runtime', 'control_receipts'}
        assert {k: v for k, v in old.items() if k not in observations} == {k: v for k, v in current.items() if k not in observations}, (old, current)
        if old.get('processing_runtime'):
            assert old['processing_runtime']['processing_generation'] == current['processing_runtime']['processing_generation']
        lanes = {row['processing_generation']: row for row in current.get('processing_statistics', [])}
        for previous in old.get('processing_statistics', []):
            present = lanes[previous['processing_generation']]
            assert present['packets']['successful_samples'] >= previous['packets']['successful_samples']
            for counter in ['failed', 'rejected', 'cancelled']:
                assert present['packets'][counter] == previous['packets'][counter], (counter, previous, present)
        a, b = old['clock'], current['clock']
        if a is not None:
            assert b is not None
            for key in ['input_index', 'output_index']:
                assert int(b[key]) >= int(a[key]), (a, b)
            assert {k: v for k, v in a.items() if k not in ['input_index', 'output_index']} == {k: v for k, v in b.items() if k not in ['input_index', 'output_index']}


def browser_gate(origin, snapshot, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1600, 'height': 1100})
        page = context.new_page(); errors = []; commands = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: commands.append(json.loads(request.post_data)) if request.method == 'POST' and '/api/' in request.url and request.post_data else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action="sdr-workbench"]').click()
        overview = page.locator('[data-view-id="overview-follow"]')
        expect(overview).to_be_visible()
        uid = snapshot['receivers'][0]['receiver_uid']; second = snapshot['receivers'][1]['receiver_uid']
        row = overview.locator('.mc-row').nth(0)
        if overview.locator('[data-action=master-mute]').get_attribute('aria-pressed') == 'true':
            overview.locator('[data-action=master-mute]').click()
        if row.locator('[data-action=listen]').get_attribute('aria-pressed') != 'true':
            row.locator('[data-action=listen]').click()
        page.wait_for_function('(uid) => window.websaAudioStatus().some(r => r.channel === uid && r.playedFrames > 2000 && !r.error)', arg=uid)
        expect(overview.locator('.mc-monitor')).to_be_visible()
        # Observe actual post-limiter PCM. Local controls must not POST or change source revisions.
        before = api(origin); count = len(commands)
        def levels():
            return page.evaluate('window.websaNativeMixLevels()')
        levels(); page.wait_for_timeout(200)
        initial = levels(); assert initial['left'] > 0.001 and initial['right'] > 0.001, initial
        for _ in range(35):
            overview.locator('[data-gain]').press('ArrowLeft')
        page.wait_for_timeout(250); quiet = levels()
        assert 0.40 < quiet['left'] / initial['left'] < 0.60, (initial, quiet)
        overview.locator('[data-pan]').press('End')
        page.wait_for_timeout(250); right = levels()
        (artifacts / 'pan-observation.json').write_text(json.dumps({'audio': page.evaluate('window.websaAudioStatus()'), 'control': overview.locator('[data-pan]').input_value(), 'levels': right}, indent=2) + '\n')
        assert right['right'] > 0.001 and right['left'] < 1e-5, right
        overview.locator('[data-action=master-mute]').click(); page.wait_for_timeout(250)
        muted = levels(); assert max(muted.values()) < 1e-5, muted
        overview.locator('[data-action=master-mute]').click()
        page.locator('#btn-sdr-audio').click(); page.wait_for_timeout(200)
        assert max(levels().values()) < 1e-5
        page.locator('#btn-sdr-audio').click()
        # Manual mix, Solo restoration, and follow selection change only browser consumers.
        overview.locator('.mc-row').nth(1).locator('[data-action=listen]').click()
        page.wait_for_function('() => window.websaAudioStatus().filter(r => r.enabled && r.owner === "workbench" && r.playedFrames > 1000).length === 2')
        row.locator('.mc-row-more > summary').click()
        row.locator('[data-action=solo]').click()
        page.wait_for_function('(uid) => window.websaAudioStatus().filter(r => r.enabled).every(r => r.channel === uid)', arg=uid)
        row.locator('[data-action=solo]').click()
        page.wait_for_function('() => window.websaAudioStatus().filter(r => r.enabled).length === 2')
        overview.locator('.mc-row').nth(1).locator('[data-action=listen]').click()
        overview.locator('[data-monitor-mode]').select_option('follow')
        overview.locator('.mc-row').nth(1).locator('[data-action=select]').click()
        page.wait_for_function('(uid) => {const rows=window.websaAudioStatus().filter(r=>r.enabled); return rows.length===1 && rows[0].channel===uid;}', arg=second)
        overview.locator('.mc-row').nth(2).locator('[data-action=select]').click()
        page.wait_for_function('() => window.websaAudioStatus().every(r => !r.enabled)')
        row.locator('[data-action=select]').click()
        overview.locator('[data-monitor-mode]').select_option('manual')
        page.wait_for_function('(uid) => {const rows=window.websaAudioStatus().filter(r=>r.enabled); return rows.length===1 && rows[0].channel===uid;}', arg=uid)
        after = api(origin)
        (artifacts / 'local-controls.json').write_text(json.dumps({'before': before, 'after': after, 'commands': commands[count:], 'initial': initial, 'half_gain': quiet, 'pan_right': right, 'muted': muted}, indent=2) + '\n')
        assert len(commands) == count, commands[count:]
        assert_receivers_continue(before['receivers'], after['receivers'])
        # Two views of one result source keep exactly one decode consumer and audio producer.
        overview.locator('.mc-row').nth(1).locator('.mc-row-more > summary').click()
        overview.locator('.mc-row').nth(1).locator('[data-action=results]').click()
        results = page.locator(f'[data-view-id="results-{second}"]')
        expect(results).to_contain_text(re.compile('CQ|W1ABC'), timeout=35000)
        with page.expect_popup() as popup:
            results.locator('[data-pop]').click()
        child = popup.value
        child.wait_for_function("Array.from(document.querySelectorAll('link[rel=stylesheet]')).every(link=>link.sheet!==null)")
        child.evaluate('document.fonts.ready')
        expect(child.locator('.mc-message-table tbody tr')).not_to_have_count(0)
        playing_before = page.evaluate('(uid) => window.websaAudioStatus().find(r => r.channel === uid).playedFrames', uid)
        dock = child.locator('[data-dock]'); dock.scroll_into_view_if_needed(); box = dock.bounding_box(); assert box
        with child.expect_event('close'):
            child.mouse.click(box['x'] + box['width']/2, box['y'] + box['height']/2)
        overview.locator('.mc-head strong').click()
        row.locator('[data-action=select]').click()
        overview.locator('[data-action=tab-output]').click()
        overview.locator('[data-section=iq-preview] summary').click()
        overview.locator('[data-action=output]').click()
        expect(overview).to_contain_text('active')
        overview.locator('[data-close]').click(); page.locator('[data-action=sdr-workbench]').click()
        page.wait_for_function('([uid,before]) => window.websaAudioStatus().find(r => r.channel === uid).playedFrames > before + 1000', arg=[uid, playing_before])
        assert_receivers_continue(before['receivers'], api(origin)['receivers'])
        assert page.evaluate('window.websaAudioStatus().filter(r => r.enabled).length') == 1
        page.screenshot(path=str(artifacts / 'native-listening-and-ft8.png'))
        # Switch the real native replay source to SSTV and confirm decoded pixels in its UID view.
        target = api(origin)['receivers'][1]; candidate = copy.deepcopy(target['confirmed'])
        candidate['processors'][0].update(mode='sstv', bandwidth_hz=3000)
        candidate['baseband'].update(passband_hz=6000, transition_hz=500, minimum_rate_hz=8000)
        reply = api(origin, payload={'type': 'configure', 'request_id': 'sstv', 'receiver_uid': second,
            'expected_revision': target['revision'], 'config': candidate})
        assert reply['fault'] is None, reply
        expect(results.locator('canvas[role=img]')).to_be_visible(timeout=20000)
        page.wait_for_function("uid => { const canvas = document.querySelector(`[data-view-id=\"results-${uid}\"] canvas[role=img]`); if (!canvas) return false; const d=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data; let count=0; for(let i=0;i<d.length;i+=4) if(d[i]+d[i+1]+d[i+2]>0) count++; return count>16000; }", arg=second, timeout=20000)
        pixels = results.locator('canvas[role=img]').evaluate('(canvas) => { const d = canvas.getContext("2d").getImageData(0,0,canvas.width,canvas.height).data; let nonzero=0; for(let i=0;i<d.length;i+=4) if(d[i]+d[i+1]+d[i+2]>0) nonzero++; return nonzero; }')
        assert pixels > 16000, pixels
        # A new subscriber receives the retained decoded image without restarting reception.
        def image_snapshot():
            with sync_connect(origin.replace('http', 'ws') + '/ws/decode?receiver_uid=' + second) as socket:
                while True:
                    message = socket.recv(timeout=5); envelope = json.loads(message[8:])
                    assert envelope['receiver_uid'] == second
                    if envelope['event']['type'] == 'sstv-image':
                        assert envelope['event']['row'] == 0 and envelope['event']['rows'] > 0
                        return envelope['epoch']
        snapshot_epoch = image_snapshot()
        page.screenshot(path=str(artifacts / 'native-sstv.png'))
        # Configure a real processor through the workbench form and then remove it.
        page.locator('[data-action=sdr-workbench]').click()
        overview.locator('.mc-row').nth(2).locator('[data-action=select]').click()
        overview.locator('[data-action=tab-receive]').click()
        form = overview.locator('form[data-config]')
        form.locator('[name=mode]').select_option('am')
        form.locator('button[type=submit]').click()
        preview = overview.locator('[data-preview] button')
        if preview.count():
            preview.click()
        third = snapshot['receivers'][2]['receiver_uid']
        page.wait_for_function('(uid) => fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.receiver_uid===uid)?.confirmed.processors[0]?.mode==="am")', arg=third)
        overview.locator('.mc-row').nth(2).locator('[data-action=listen]').click()
        page.wait_for_function('(uid) => window.websaAudioStatus().some(r=>r.channel===uid&&r.playedFrames>1000&&!r.error)', arg=third)
        form.locator('[name=mode]').select_option('')
        form.locator('button[type=submit]').click()
        if preview.count():
            preview.click()
        page.wait_for_function('(uid) => fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.receiver_uid===uid)?.confirmed.processors.length===0)', arg=third)
        expect(overview.locator('.mc-monitor')).to_have_count(0)
        final = api(origin)
        assert final['receivers'][0]['stream'] == before['receivers'][0]['stream']
        assert final['receivers'][0]['confirmed'] == before['receivers'][0]['confirmed']
        assert not errors, errors
        result = {'initial': initial, 'half_gain': quiet, 'pan_right': right, 'master_mute': muted,
            'sstv_nonzero_pixels': pixels, 'image_snapshot_epoch': snapshot_epoch, 'audio': page.evaluate('window.websaAudioStatus()'), 'errors': errors,
            'checks': ['actual rendered PCM', 'local gain ratio', 'stereo pan', 'master mute', 'no configuration writes',
                'FT8 replay decoded text', 'UID image pixels', 'Pop out/Dock/close keep playback', 'no duplicate audio', 'manual mix', 'Solo restore', 'follow selection', 'IQ has no audio chain', 'processor form native confirmation', 'remove processor UI', 'unaffected receiver identity']}
        browser.close(); return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-p4-processing'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    with (args.artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        try:
            snapshot = setup(origin)
            report = {'source': 'synthetic AM/baseband and committed FT8/SSTV replay; no RF hardware', 'snapshot': snapshot}
            report['protocol'] = asyncio.run(protocol(origin, snapshot))
            (args.artifacts / 'protocol-report.json').write_text(json.dumps(report, indent=2) + '\n')
            report['browser'] = browser_gate(origin, snapshot, args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: P4 UID audio/decode, actual browser mixing, FT8 text and SSTV pixels (synthetic/replay)')
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
