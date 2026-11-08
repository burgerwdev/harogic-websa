#!/usr/bin/env python3
"""P3 native API/IQ and product-window gate on a dedicated synthetic-source service.

Requires built websad/frontend, websockets and Playwright. Owns only its child
service. This validates service transport, not RF hardware/filter performance;
the native Rust multichannel tests separately exercise PFB/fine DSP.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import re
import struct
import urllib.error
import urllib.request
from fractions import Fraction
from pathlib import Path

from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready
from websockets.asyncio.client import connect


def api(origin, path='/api/sdr/receivers', payload=None):
    request = urllib.request.Request(origin + path, data=None if payload is None else json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def config(frequency):
    return {'name': 'Native IQ', 'color': '#4cc99b', 'tags': [], 'enabled': True, 'purpose': 'iq', 'processors': [],
            'baseband': {'frequency_hz': frequency, 'passband_hz': 3000, 'transition_hz': 1000, 'minimum_rate_hz': 6000}}


def saved_receivers(snapshot):
    return [row for row in snapshot['receivers'] if row['display_number'] != 1]


def reset_receivers(origin):
    """Stop Current and retire saved receivers on the owned fixture service."""
    for row in api(origin)['receivers']:
        if row['display_number'] == 1:
            candidate = json.loads(json.dumps(row['confirmed']))
            candidate['enabled'] = False
            command = {'type': 'configure', 'request_id': 'stop-' + row['revision'] + '-' + row['receiver_uid'],
                'receiver_uid': row['receiver_uid'], 'expected_revision': row['revision'], 'config': candidate}
        else:
            command = {'type': 'retire', 'request_id': 'clear-' + row['receiver_uid'], 'receiver_uid': row['receiver_uid']}
        reply = api(origin, payload=command)
        assert reply['fault'] is None, reply['fault']
    return api(origin)


async def protocol(origin, channels=3, *, raw_capture=False, center=100e6):
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(origin, {'cmd': 'SET_SDR', 'center': center, **({'decimate': 1} if raw_capture else {})})
    initial = api(origin)
    if not 3 <= channels <= initial['capabilities']['max_channels']:
        raise ValueError('This middle-slot reuse scenario needs 3..max_channels receivers')
    current = reset_receivers(origin)['receivers'][0]
    receivers = []
    for i in range(channels):
        request = {'type': 'create', 'request_id': f'create-{i}', 'config': config(center + i * 10000)}
        if i == 0:
            request.update(type='configure', receiver_uid=current['receiver_uid'], expected_revision=current['revision'])
        reply = api(origin, payload=request)
        assert reply['fault'] is None, reply
        assert api(origin, payload=request) == reply
        receivers = reply['snapshot']['receivers']
    assert len({r['receiver_uid'] for r in receivers}) == channels
    assert all(r['confirmed']['processors'] == [] for r in receivers)
    if channels == initial['capabilities']['max_channels']:
        before_full = api(origin)
        full = api(origin, payload={'type': 'create', 'request_id': 'over-capacity', 'config': config(center)})
        assert full['fault'] is not None
        if raw_capture:
            fields = ['receiver_uid', 'revision', 'confirmed', 'stream']
            assert [{key: r[key] for key in fields} for r in full['snapshot']['receivers']] == [
                {key: r[key] for key in fields} for r in before_full['receivers']]
        else:
            assert full['snapshot']['receivers'] == before_full['receivers']
    service_id = reply['snapshot']['service_id']
    disabled = json.loads(json.dumps(receivers[0]['confirmed']))
    disabled['enabled'] = False
    disabled['baseband']['frequency_hz'] = -1
    invalid = api(origin, payload={'type': 'configure', 'request_id': 'invalid-disabled',
        'receiver_uid': receivers[0]['receiver_uid'], 'expected_revision': receivers[0]['revision'], 'config': disabled})
    assert invalid['fault'] is not None
    assert invalid['snapshot']['receivers'][0]['confirmed'] == receivers[0]['confirmed']
    assert invalid['snapshot']['receivers'][0]['stream'] == receivers[0]['stream']
    traces = [{'events': [], 'identities': [], 'frames': 0, 'samples': 0, 'last': None, 'ended': False} for _ in receivers]
    async def consume(socket, trace):
        async for message in socket:
            if isinstance(message, str):
                event = json.loads(message); trace['events'].append(event)
                if event['type'] == 'stream_start':
                    clock = event['clock']
                    for key in ['input_index', 'output_index', 'fraction_num', 'fraction_den']:
                        assert isinstance(clock[key], str)
                    assert clock['actual_rate_hz'] > 0
                    if raw_capture:
                        assert clock['input_rate_hz'] == 62500000
                        trace['step'] = round(clock['input_rate_hz'] / clock['actual_rate_hz'])
                        assert abs(trace['step'] * clock['actual_rate_hz'] - clock['input_rate_hz']) < .01
                    else:
                        assert clock['actual_rate_hz'] == 976562.5 / 162
                        assert clock['input_rate_hz'] == clock['actual_rate_hz'], 'fixture clock must describe its actual baseband source'
                        trace['step'] = 1
                    trace['identities'].append((event['stream']['channel_id'], event['stream']['epoch']))
                    trace['last'] = None
                elif event['type'] == 'stream_boundary' and event['next'] is None:
                    assert event['drained'] is True; trace['ended'] = True
                continue
            magic, version, sequence, count, rate, frequency, channel, epoch, position, delay = struct.unpack_from('<4sIIIddIIQd', message)
            assert magic in [b'IQBF', b'IQAX'] and version == 2 and len(message) == 56 + count * 8
            assert (str(channel), str(epoch)) == trace['identities'][-1]
            previous = trace['last']
            if previous:
                assert sequence == previous[0] + 1, (sequence, previous)
                assert position == previous[1] + previous[2] * trace['step'], (position, previous)
            if count:
                samples = struct.unpack_from(f'<{count*2}f', message, 56)
                assert all(math.isfinite(v) for v in samples)
                power = sum(samples[i]**2 + samples[i+1]**2 for i in range(0, len(samples), 2)) / count
                if not raw_capture:
                    assert abs(power - 0.0625) < 1e-7, power
            trace['last'] = sequence, position, count
            trace['frames'] += 1; trace['samples'] += count
    sockets = [await connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + r['receiver_uid']) for r in receivers]
    tasks = [asyncio.create_task(consume(s, t)) for s, t in zip(sockets, traces, strict=True)]
    async def until(predicate):
        async with asyncio.timeout(12):
            while not predicate():
                for task in tasks:
                    if task.done() and task.exception():
                        raise task.exception()
                await asyncio.sleep(0.03)
    await until(lambda: all(t['samples'] >= 2000 for t in traces))
    before = api(origin)
    target = before['receivers'][channels - 1]; candidate = json.loads(json.dumps(target['confirmed']))
    candidate['baseband']['frequency_hz'] += 2e6
    changed = await asyncio.to_thread(api, origin, '/api/sdr/receivers', {'type': 'configure', 'request_id': 'cross-bin',
        'service_id': service_id, 'receiver_uid': target['receiver_uid'], 'expected_revision': target['revision'], 'config': candidate})
    assert changed['fault'] is None, changed
    await until(lambda: len(traces[channels - 1]['identities']) == 2 and traces[channels - 1]['samples'] >= 4000)
    assert all(len(t['identities']) == 1 for t in traces[:-1])
    assert any(e['type'] == 'stream_boundary' and e['reason'] == 'reconfigured' and e['drained'] for e in traces[channels - 1]['events'])
    rejected = api(origin, payload={'type': 'configure', 'request_id': 'late', 'receiver_uid': target['receiver_uid'],
        'expected_revision': target['revision'], 'config': target['confirmed']})
    assert rejected['fault'] is not None
    assert rejected['snapshot']['receivers'][channels - 1]['stream'] == changed['snapshot']['receivers'][channels - 1]['stream']
    candidate['baseband']['frequency_hz'] = 200e6
    invalid = api(origin, payload={'type': 'configure', 'request_id': 'out-of-range', 'receiver_uid': target['receiver_uid'],
        'expected_revision': changed['snapshot']['receivers'][channels - 1]['revision'], 'config': candidate})
    assert invalid['fault'] is not None
    assert invalid['snapshot']['receivers'][channels - 1]['stream'] == changed['snapshot']['receivers'][channels - 1]['stream']
    # Stop via the legacy path too: both interfaces must drain native subscribers.
    post(origin, {'cmd': 'SET_SDR_CHANNEL', 'channel': channels, 'enabled': False})
    await until(lambda: traces[channels - 1]['ended'])
    await asyncio.wait_for(tasks[channels - 1], 5)
    counts = [t['samples'] for t in traces[:-1]]
    await until(lambda: all(t['samples'] > n + 1000 for t, n in zip(traces[:-1], counts, strict=True)))
    try:
        async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + target['receiver_uid']):
            raise AssertionError('stopped receiver accepted a subscription')
    except Exception as error:
        assert getattr(getattr(error, 'response', None), 'status_code', None) == 409, error
    for socket in sockets:
        await socket.close()
    await asyncio.gather(*tasks)
    current = api(origin)
    stopped = current['receivers'][channels - 1]
    resumed_config = json.loads(json.dumps(stopped['confirmed']))
    resumed_config['enabled'] = True
    resumed = api(origin, payload={'type': 'configure', 'request_id': 'resume-last',
        'receiver_uid': stopped['receiver_uid'], 'expected_revision': stopped['revision'], 'config': resumed_config})
    assert resumed['fault'] is None
    assert resumed['snapshot']['receivers'][channels - 1]['receiver_uid'] == stopped['receiver_uid']
    assert resumed['snapshot']['receivers'][channels - 1]['stream'] != stopped['stream']
    middle = resumed['snapshot']['receivers'][1]
    retired = api(origin, payload={'type': 'retire', 'request_id': 'retire-middle', 'receiver_uid': middle['receiver_uid']})
    assert retired['fault'] is None
    reused = api(origin, payload={'type': 'create', 'request_id': 'reuse-middle', 'config': middle['confirmed']})
    assert reused['fault'] is None
    assert reused['snapshot']['receivers'][1]['display_number'] == middle['display_number']
    assert reused['snapshot']['receivers'][1]['receiver_uid'] != middle['receiver_uid']
    for index in range(channels):
        if index != 1:
            if raw_capture:
                for key in ['receiver_uid', 'revision', 'confirmed', 'stream', 'state', 'fault']:
                    assert reused['snapshot']['receivers'][index][key] == resumed['snapshot']['receivers'][index][key]
            else:
                assert reused['snapshot']['receivers'][index] == resumed['snapshot']['receivers'][index]
    try:
        async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + middle['receiver_uid']):
            raise AssertionError('retired identity accepted a subscription')
    except Exception as error:
        assert getattr(getattr(error, 'response', None), 'status_code', None) == 404, error
    return {'source_kind': before['capture']['source_kind'], 'service_id': service_id, 'receivers': receivers,
            'streams': traces, 'snapshot': api(origin), 'known_content_checked': not raw_capture,
            'checks': [f'{channels} UIDs', 'finite IQ samples' if raw_capture else 'IQ samples/power', 'actual source clocks',
            'cross-bin boundary', 'unaffected sequence/position continuity', 'revision rejection', 'RF rejection', 'disabled frequency validation', 'unified command stop drain', 'stopped subscription rejection', 'resume keeps UID', 'middle number reused with fresh UID', 'retired UID rejection']}


def browser_gate(origin, artifacts, channels=3):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1600, 'height': 1000})
        page = context.new_page(); errors = []; commands = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: commands.append(json.loads(request.post_data)) if request.method == 'POST' and '/api/' in request.url and request.post_data else None)
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action="sdr-workbench"]').click()
        overview = page.locator('[data-view-id="overview-follow"]')
        expect(overview).to_be_visible(); expect(overview.locator('.mc-row')).to_have_count(channels)
        expect(overview.locator('[data-capture-status]')).to_contain_text(re.compile('Simulated window|模拟窗口'))
        overview.locator('[data-action="tab-output"]').first.click()
        overview.locator('[data-section=iq-preview] summary').click()
        overview.locator('[data-action="output"]').click()
        expect(overview).to_contain_text('active')
        page.wait_for_function("() => /active · [1-9][0-9]* samples/.test(document.querySelector('[data-view-id=overview-follow]').textContent)")
        before = api(origin)
        with page.expect_popup() as popup:
            overview.locator('[data-pop]').click()
        child = popup.value
        expect(child.locator('[data-view-id="overview-follow"]')).to_contain_text('active')
        child.screenshot(path=str(artifacts / 'native-popout.png'))
        child.locator('[data-dock]').click(no_wait_after=True)
        expect(overview).to_be_visible(); overview.locator('[data-close]').click()
        page.wait_for_timeout(650)
        page.locator('[data-action="sdr-workbench"]').click(); expect(overview).to_contain_text('active')
        page.screenshot(path=str(artifacts / 'native-workbench.png'))
        after = api(origin)
        assert [(r['receiver_uid'], r['confirmed'], r['stream']) for r in before['receivers']] == [(r['receiver_uid'], r['confirmed'], r['stream']) for r in after['receivers']]
        assert not any(c.get('type') in ['configure', 'retire'] or (c.get('cmd') == 'SET_SDR_CHANNEL' and c.get('enabled') is False) for c in commands), commands
        assert not errors, errors
        # Closing this page closes only its output subscriptions.
        context.close(); browser.close()
    return {'checks': ['native workbench', 'real IQ counters', 'Pop out/Dock', 'close/recall', 'no receiver stop or reconfiguration'], 'commands': commands}


async def processed_protocol(origin, channels):
    snapshot = api(origin)
    for receiver in snapshot['receivers']:
        candidate = json.loads(json.dumps(receiver['confirmed']))
        candidate.update(purpose='listen', enabled=True)
        candidate['baseband'].update(passband_hz=16000, transition_hz=1000, minimum_rate_hz=48000)
        candidate['processors'] = [{'id': 'primary', 'kind': 'demodulator', 'mode': 'am',
            'minimum_rate_hz': 0, 'bandwidth_hz': 6000, 'pcm_gain': 0.8, 'agc': False, 'squelch_dbm': -200}]
        reply = api(origin, payload={'type': 'configure', 'request_id': 'listen-' + receiver['receiver_uid'],
            'receiver_uid': receiver['receiver_uid'], 'expected_revision': receiver['revision'], 'config': candidate})
        assert reply['fault'] is None, reply
    snapshot = api(origin)
    traces = [{'channel': r['display_number'], 'audio_samples': 0, 'events': 0} for r in snapshot['receivers']]
    async def audio(receiver, trace):
        endpoint = origin.replace('http', 'ws') + '/ws/audio?receiver_uid=' + receiver['receiver_uid']
        previous = None
        async with connect(endpoint) as socket:
            async for frame in socket:
                assert isinstance(frame, bytes) and frame[:4] == b'AUD2'
                seq, count = struct.unpack_from('<II', frame, 8)
                rate, frequency = struct.unpack_from('<dd', frame, 16)
                identity, epoch, source = struct.unpack_from('<IIQ', frame, 32)
                audio_channels, flags, index, input_rate, num, den = struct.unpack_from('<IIQdQQ', frame, 56)
                assert audio_channels in (1, 2) and len(frame) == 96 + count * audio_channels * 4
                assert frequency == receiver['confirmed']['baseband']['frequency_hz']
                assert den and num < den and rate == 48000
                position = Fraction(source) + Fraction(num, den)
                if previous:
                    pseq, pcount, pindex, pposition, pidentity = previous
                    assert seq == pseq + 1 and index == pindex + pcount
                    assert (identity, epoch) == pidentity
                    assert position == pposition + pcount * Fraction(input_rate) / Fraction(rate)
                else:
                    assert seq == 0 and count == 0
                previous = seq, count, index, position, (identity, epoch)
                trace['audio_samples'] += count
    async def decode(receiver, trace):
        endpoint = origin.replace('http', 'ws') + '/ws/decode?receiver_uid=' + receiver['receiver_uid']
        previous = None
        async with connect(endpoint) as socket:
            async for frame in socket:
                assert isinstance(frame, bytes) and frame[:4] == b'SDE2'
                event = json.loads(frame[8:])
                assert event['receiver_uid'] == receiver['receiver_uid'] and event['service_id'] == snapshot['service_id']
                assert event['channel'] == receiver['display_number']
                assert event['baseband_stream'] == receiver['stream']
                assert isinstance(event['input_index'], str)
                if previous is not None:
                    assert event['seq'] == previous + 1
                else:
                    assert event['seq'] == 0
                assert event['event']['type'] != 'error', event
                previous = event['seq']
                trace['events'] += 1
    tasks = [asyncio.create_task(run(receiver, trace)) for receiver, trace in zip(snapshot['receivers'], traces, strict=True)
             for run in (audio, decode)]
    try:
        async with asyncio.timeout(12):
            while not all(t['audio_samples'] >= 12000 and t['events'] >= 2 for t in traces):
                for task in tasks:
                    if task.done():
                        task.result()
                        raise AssertionError('processed stream closed before required output')
                await asyncio.sleep(0.03)
    finally:
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
    assert len(traces) == channels
    return {'source_kind': 'fixture', 'streams': traces,
        'checks': ['all processed audio and result subscriptions', 'exact audio source clocks', 'logical UID and service fencing', f'{channels} result channel numbers']}


async def capture_boundary(origin):
    receiver = next(r for r in api(origin)['receivers'] if r['confirmed']['enabled'])
    async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + receiver['receiver_uid']) as socket:
        async with asyncio.timeout(10):
            while True:
                message = await socket.recv()
                if isinstance(message, str) and json.loads(message)['type'] == 'stream_start':
                    break
            post(origin, {'cmd': 'SET_MODE', 'mode': 'std'})
            while True:
                message = await socket.recv()
                if isinstance(message, str):
                    event = json.loads(message)
                    if event['type'] == 'stream_boundary':
                        assert event['next'] is None and event['drained'] is False
                        assert event['reason'] == 'capture_unavailable'
                        return event


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--channels', type=int, default=3)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-p3-native'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        try:
            report = asyncio.run(protocol(origin, args.channels))
            (args.artifacts / 'protocol-report.json').write_text(json.dumps(report, indent=2) + '\n')
            report['browser'] = browser_gate(origin, args.artifacts, args.channels)
            report['processed'] = asyncio.run(processed_protocol(origin, args.channels))
            report['capture_boundary'] = asyncio.run(capture_boundary(origin))
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: P3 native service IQ protocol and browser lifecycle (synthetic RF source)')
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
