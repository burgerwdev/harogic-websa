#!/usr/bin/env python3
"""Check independent neural NR, gain and AGC on an owned synthetic service."""
from __future__ import annotations

import argparse
import asyncio
import copy
import json
import math
import os
import struct
import time
import urllib.request
from pathlib import Path

from rust_iq_browser import free_port, start_service, stop
from websockets.asyncio.client import connect


def api(origin, payload=None, path='/api/sdr/receivers'):
    request = urllib.request.Request(origin + path,
        data=None if payload is None else json.dumps(payload).encode(),
        headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        result = json.load(response)
    assert not result.get('fault') and result.get('cmd') != 'ERROR', result
    return result


def config(algorithm, gain, agc):
    return {'name': algorithm, 'color': '#4cc99b', 'tags': [], 'enabled': True, 'purpose': 'listen',
        'baseband': {'frequency_hz': 100e6, 'passband_hz': 6000, 'transition_hz': 1000, 'minimum_rate_hz': 8000},
        'processors': [{'id': 'primary', 'kind': 'demodulator', 'mode': 'am', 'minimum_rate_hz': 0,
            'bandwidth_hz': 6000, 'pcm_gain': gain, 'agc': agc, 'squelch_dbm': -160,
            'nr': True, 'nr_algorithm': algorithm, 'nr_strength': 0.6, 'nr_atten_db': 12}]}


async def listen(origin, receiver, seconds=2):
    trace = {'frames': 0, 'samples': 0, 'energy': 0.0, 'nr': None}
    query = '?receiver_uid=' + receiver['receiver_uid']
    endpoint = origin.replace('http://', 'ws://')
    async with connect(endpoint + '/ws/audio' + query) as audio, connect(endpoint + '/ws/decode' + query) as decode:
        async def pcm():
            previous = None
            async with asyncio.timeout(max(30, seconds * 2)):
                while trace['samples'] < round(48000 * seconds):
                    frame = await audio.recv()
                    assert isinstance(frame, bytes) and frame[:4] == b'AUD2'
                    seq, count = struct.unpack_from('<II', frame, 8)
                    channels = struct.unpack_from('<I', frame, 56)[0]
                    index = struct.unpack_from('<Q', frame, 64)[0]
                    identity = struct.unpack_from('<II', frame, 32)
                    assert struct.unpack_from('<d', frame, 16)[0] == 48000
                    assert len(frame) == 96 + count * channels * 4
                    if previous:
                        assert (seq, index, identity) == (previous[0] + 1, previous[1] + previous[2], previous[3])
                    previous = seq, index, count, identity
                    samples = struct.unpack_from(f'<{count * channels}f', frame, 96)
                    assert all(math.isfinite(sample) for sample in samples)
                    trace['energy'] += sum(sample * sample for sample in samples)
                    trace['frames'] += 1
                    trace['samples'] += count
            trace['successful_media_seconds'] = trace['samples'] / 48000
            trace['collection_completed_monotonic'] = time.monotonic()
            assert trace['energy'] > 1e-6, trace

        async def status():
            async with asyncio.timeout(20):
                while trace['nr'] is None:
                    frame = await decode.recv()
                    assert isinstance(frame, bytes) and frame[:4] == b'SDE2'
                    event = json.loads(frame[8:])['event']
                    if event['type'] == 'nr-status':
                        expected = receiver['confirmed']['processors'][0]['nr_algorithm']
                        assert event['requested'] == expected and event['effective'] == expected, event
                        assert event['state'] == 'ready', event
                        trace['nr'] = event
        await asyncio.gather(pcm(), status())
    return trace


def run(origin, seconds=2, include_multi=False):
    api(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'}, '/api/config')
    api(origin, {'cmd': 'SET_SDR', 'center': 100e6}, '/api/config')
    results = []
    for algorithm in ['dfn', 'gtcrn', 'ul-unas', 'fastenhancer']:
        for receiver in api(origin)['receivers']:
            api(origin, {'type': 'retire', 'request_id': f'clear-{algorithm}-{receiver["receiver_uid"]}',
                'receiver_uid': receiver['receiver_uid']})
        for index, requested in enumerate([config('dfn', 0.4, False), config(algorithm, 0.8, True)]):
            reply = api(origin, {'type': 'create', 'request_id': f'add-{algorithm}-{index}', 'config': requested})
            assert all(reply['snapshot']['receivers'][index]['confirmed']['processors'][0][key] == value for key, value in requested['processors'][0].items())
        snapshot = api(origin)
        assert len(snapshot['receivers']) == 2
        before = copy.deepcopy(snapshot['receivers'][0])
        traces = asyncio.run(collect(origin, snapshot['receivers'], seconds))
        current = api(origin)
        second = current['receivers'][1]
        changed = copy.deepcopy(second['confirmed'])
        changed['processors'][0].update(pcm_gain=0.3, agc=False)
        reply = api(origin, {'type': 'configure', 'request_id': f'gain-agc-{algorithm}',
            'receiver_uid': second['receiver_uid'], 'expected_revision': second['revision'], 'config': changed})
        first = reply['snapshot']['receivers'][0]
        for field in ['receiver_uid', 'revision', 'confirmed', 'stream']:
            assert first[field] == before[field], (field, first, before)
        assert all(not receiver['fault'] for receiver in reply['snapshot']['receivers'])
        results.append({'second_algorithm': algorithm, 'resources': current['processing_resources'], 'audio': traces})
    if include_multi:
        for count in [4, 8]:
            for receiver in api(origin)['receivers']:
                api(origin, {'type': 'retire', 'request_id': f'multi-clear-{count}-{receiver["receiver_uid"]}', 'receiver_uid': receiver['receiver_uid']})
            algorithms = ['dfn', 'gtcrn', 'ul-unas', 'fastenhancer'] * (count // 4)
            for index, algorithm in enumerate(algorithms):
                api(origin, {'type': 'create', 'request_id': f'multi-add-{count}-{index}', 'config': config(algorithm, 0.5, True)})
            snapshot = api(origin)
            traces = asyncio.run(collect(origin, snapshot['receivers'], seconds, report_overload=True))
            current = api(origin)
            assert current['capture']['state'] == 'running'
            # All legal choices are accepted; sustained ability is observed, not promised.
            for row in current['receivers']:
                assert row['confirmed']['processors'][0]['nr_algorithm'] == algorithms[row['display_number'] - 1]
            results.append({'case': f'{count} independent selected models', 'second_algorithm': f'multi-{count}',
                'requested_seconds': seconds, 'audio': traces, 'snapshot': current})
    return results


async def collect(origin, receivers, seconds=2, report_overload=False):
    async def measured(receiver):
        began = time.monotonic()
        try:
            trace = await listen(origin, receiver, seconds)
            trace['collection_wall_seconds'] = trace.pop('collection_completed_monotonic') - began
            trace['wall_seconds_including_socket_close'] = time.monotonic() - began
            return trace
        except Exception as error:
            if not report_overload:
                raise
            return {'error': str(error), 'wall_seconds': time.monotonic() - began,
                'receiver_uid': receiver['receiver_uid']}
    return await asyncio.gather(*(measured(receiver) for receiver in receivers))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-independent-neural-nr'))
    parser.add_argument('--seconds', type=float, default=2)
    parser.add_argument('--include-multi', action='store_true')
    args = parser.parse_args()
    if not math.isfinite(args.seconds) or args.seconds <= 0:
        parser.error('--seconds must be finite and positive')
    args.artifacts.mkdir(parents=True, exist_ok=True)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log, startup_timeout=30)
        try:
            results = run(origin, args.seconds, args.include_multi)
            (args.artifacts / 'result.json').write_text(json.dumps(results, indent=2) + '\n')
            passed = all('error' not in stream for result in results for stream in result['audio'])
            print(json.dumps({'passed': passed, 'algorithms': [result['second_algorithm'] for result in results],
                'artifacts': str(args.artifacts)}))
            if not passed:
                raise SystemExit(1)
        finally:
            stop(process)


if __name__ == '__main__':
    main()
