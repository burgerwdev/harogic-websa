#!/usr/bin/env python3
"""Backend IQ archives, browser controls and restart recovery on an isolated synthetic service.

The independent reader uses libzstd and CRC32; no browser-owned samples are recorded.
This is not a hardware RF or sustained disk-throughput acceptance test.
"""
from __future__ import annotations

import argparse
import asyncio
import ctypes
import ctypes.util
import json
import os
import resource
import signal
import struct
import time
import zlib
from pathlib import Path

from native_multichannel import api, config, reset_receivers
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import ROOT, WEBSAD, free_port, post, start_owned_service, start_service, stop
from viewport_baseline import wait_ui_ready
from websockets.asyncio.client import connect


def until(test, timeout=12):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        result = test()
        if result:
            return result
        time.sleep(0.04)
    raise AssertionError('condition did not become true')


def task(origin, task_id):
    return next(t for t in api(origin)['recordings'] if t['task_id'] == task_id)


def start(origin, uid, key):
    reply = api(origin, payload={'type': 'record_start', 'request_id': key, 'receiver_uid': uid, 'kinds': ['iq']})
    assert reply['fault'] is None, reply
    assert api(origin, payload={'type': 'record_start', 'request_id': key, 'receiver_uid': uid, 'kinds': ['iq']}) == reply
    return next(t['task_id'] for t in reply['snapshot']['recordings'] if t['receiver_uid'] == uid and t['state'] in ['preparing', 'recording'])


def finish(origin, task_id):
    reply = api(origin, payload={'type': 'record_stop', 'request_id': 'stop-' + task_id, 'task_id': task_id})
    assert reply['fault'] is None, reply
    value = until(lambda: (t if (t := task(origin, task_id))['state'] in ['completed', 'failed'] else None))
    assert value['state'] == 'completed', value
    assert value['accepted_bytes'] == value['tracks'][0]['raw_bytes'], value
    return value


def read_archive(path):
    """Validate wire format independently of the Rust writer/reader implementation."""
    zstd = ctypes.CDLL(ctypes.util.find_library('zstd'))
    zstd.ZSTD_decompress.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t]
    zstd.ZSTD_decompress.restype = ctypes.c_size_t
    data = path.read_bytes()
    assert data[:8] == b'WBIQ0002'
    position, blocks, page, digest, complete = 8, [], [], 0, False
    while position < len(data):
        offset = position
        if position + 16 > len(data):
            break
        kind, meta_size, stored, crc = struct.unpack_from('<4sIII', data, position)
        assert meta_size <= 262144 and stored <= 1048576
        end = position + 16 + meta_size + stored
        if end > len(data):
            break
        encoded = data[position+16:position+16+meta_size]
        payload = data[position+16+meta_size:end]
        assert zlib.crc32(data[position:position+12] + encoded + payload) == crc
        header = json.loads(encoded)
        if kind != b'END2':
            digest = zlib.crc32(struct.pack('<I', crc), digest)
        if kind == b'BLK2':
            raw_len = header['meta']['clock']['samples'] * 8
            if header['codec'] == 'zstd':
                output = ctypes.create_string_buffer(raw_len)
                size = zstd.ZSTD_decompress(output, raw_len, payload, len(payload))
                assert size == raw_len
                raw = output.raw
            else:
                assert header['codec'] == 'raw'
                raw = payload
            assert len(raw) == raw_len and zlib.crc32(raw) == header['raw_crc32']
            index = {'offset': str(offset), **header, 'raw_bytes': raw_len, 'stored_bytes': stored}
            page.append(index)
            blocks.append((index, raw))
        elif kind == b'IDX2':
            assert not payload and header == page
            page = []
        elif kind == b'END2':
            assert not page and not payload
            assert header == {'blocks': str(len(blocks)), 'samples': str(sum(i['meta']['clock']['samples'] for i, _ in blocks)), 'records_crc32': digest}
            assert end == len(data)
            complete = True
        else:
            assert kind == b'EVT2' and not payload
        position = end
    return blocks, complete


async def protocol(origin, root):
    snapshot = api(origin)
    uid, healthy = [r['receiver_uid'] for r in snapshot['receivers'][:2]]
    received = {}
    async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + uid) as socket:
        async def consume():
            async for frame in socket:
                if isinstance(frame, bytes) and len(frame) > 56:
                    _, _, sequence, count, rate, frequency, channel, epoch, position, delay = struct.unpack_from('<4sIIIddIIQd', frame)
                    received[(str(epoch), str(channel), str(sequence))] = (frame[56:], position, rate, frequency, delay, count)
        consumer = asyncio.create_task(consume())
        task_id = await asyncio.to_thread(start, origin, uid, 'protocol-start')
        await asyncio.to_thread(until, lambda: int(task(origin, task_id)['tracks'][0]['raw_bytes']) > 20000)
        before = api(origin)['receivers'][1]
        receiver = api(origin)['receivers'][0]
        candidate = json.loads(json.dumps(receiver['confirmed']))
        candidate['baseband']['frequency_hz'] += 2000000
        candidate['baseband']['minimum_rate_hz'] *= 2
        reply = await asyncio.to_thread(api, origin, '/api/sdr/receivers', {'type': 'configure', 'request_id': 'record-retune',
            'receiver_uid': uid, 'expected_revision': receiver['revision'], 'config': candidate})
        assert reply['fault'] is None, reply
        await asyncio.to_thread(until, lambda: len(task(origin, task_id)['tracks'][0]['segments']) == 2)
        await asyncio.to_thread(until, lambda: int(task(origin, task_id)['tracks'][0]['segments'][1]['samples']) > 3000)
        saved = await asyncio.to_thread(finish, origin, task_id)
        assert api(origin)['receivers'][1]['stream'] == before['stream']
        assert api(origin)['receivers'][1]['confirmed'] == before['confirmed']
        await asyncio.sleep(0.2)
        await socket.close()
        await consumer
    indexes = []
    for volume in saved['tracks'][0]['volumes']:
        blocks, complete = read_archive(root / task_id / volume['file'])
        assert complete
        persisted = [json.loads(line) for line in (root / task_id / volume['index']).read_text().splitlines()]
        assert persisted == [index for index, _ in blocks]
        for index, raw in blocks:
            meta, clock = index['meta'], index['meta']['clock']
            original = received[(clock['epoch'], clock['channel_id'], meta['sequence'])]
            assert raw == original[0], 'CF32 bytes differ from actual backend network publication'
            assert clock['first_input_index'] == str(original[1])
            assert clock['actual_rate_hz'] == original[2] and meta['frequency_hz'] == original[3]
            assert clock['delay_seconds'] == original[4] / original[2]
            assert clock['receiver_uid'] == uid and meta['service_id'] == snapshot['service_id']
            assert clock['sample_format'] == 'cf32_le' and clock['amplitude_scale'] == 1
            indexes.append(index)
    indexed = api(origin, '/api/sdr/recordings/' + task_id + '/index')['entries']
    assert indexed == indexes[:128]
    assert sum(i['raw_bytes'] for i in indexes) == int(saved['tracks'][0]['raw_bytes'])
    # Stopping the receiver drains its IQ tail and ends the backend task without a browser.
    stopped_id = start(origin, healthy, 'record-source-stop')
    until(lambda: int(task(origin, stopped_id)['tracks'][0]['raw_bytes']) > 10000)
    post(origin, {'cmd': 'SET_SDR_CHANNEL', 'channel': 2, 'enabled': False})
    stopped = until(lambda: t if (t := task(origin, stopped_id))['state'] == 'completed' else None)
    assert stopped['accepted_bytes'] == stopped['tracks'][0]['raw_bytes']
    assert stopped['tracks'][0]['segments'][-1]['end_reason'] == 'receiver_stopped'
    return {'task': saved, 'verified_blocks': len(indexes), 'receiver_stop': stopped}


def held_click(page, locator):
    # Locator.click retries attachment/scrolling before real pointer down. Once pressed,
    # polling must leave the target alive until pointer up, including in another document.
    locator.click(delay=1100)


def browser_gate(origin, root, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1366, 'height': 900})
        page = context.new_page()
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        uid = api(origin)['receivers'][0]['receiver_uid']
        row = overview.locator(f'.mc-row[data-uid="{uid}"]')
        row.locator('.mc-row-more > summary').click()
        held_click(page, row.locator('[data-action=rec-iq]'))
        task_id = until(lambda: next((t['task_id'] for t in api(origin)['recordings'] if t['receiver_uid'] == uid and t['state'] == 'recording'), None))
        before = int(task(origin, task_id)['tracks'][0]['raw_bytes'])
        held_click(page, overview.locator('[data-action=tab-recording]'))
        expect(overview.locator(f'[data-task-id="{task_id}"]')).to_be_visible()
        expect(overview.locator('[data-action=rec-audio],[data-action=rec-results]')).to_have_count(0)
        page.screenshot(path=str(artifacts / 'backend-iq-recording.png'))
        details = overview.locator(f'[data-task-id="{task_id}"] details')
        held_click(page, details.locator('summary'))
        page.wait_for_timeout(700)
        expect(details).to_have_attribute('open', '')
        page.screenshot(path=str(artifacts / 'archive-details.png'))
        held_click(page, details.locator('summary'))
        expect(details).not_to_have_attribute('open')
        with page.expect_popup() as popup:
            overview.locator('[data-pop]').click()
        child = popup.value
        child.on('pageerror', lambda error: errors.append(str(error)))
        child.set_viewport_size({'width': 420, 'height': 760})
        compact = child.locator('[data-view-id=overview-follow]')
        held_click(child, compact.locator('.mc-editor [data-action=rec-iq]'))
        until(lambda: task(origin, task_id)['state'] == 'completed')
        held_click(child, compact.locator('.mc-editor [data-action=rec-iq]'))
        new_id = until(lambda: next((t['task_id'] for t in api(origin)['recordings'] if t['receiver_uid'] == uid and t['state'] == 'recording'), None))
        expect(compact.locator(f'[data-task-id="{new_id}"][data-task-state=recording]')).to_be_visible()
        child.screenshot(path=str(artifacts / 'compact-recording.png'))
        compact.locator('[data-dock]').click()
        expect(overview.locator('[data-task-state=recording]')).to_be_visible()
        overview.locator('[data-close]').click()
        until(lambda: int(task(origin, new_id)['tracks'][0]['raw_bytes']) > 20000)
        page.reload()
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview.locator('[data-action=tab-recording]').click()
        expect(overview.locator(f'[data-task-id="{new_id}"][data-task-state=recording]')).to_be_visible()
        current = int(task(origin, new_id)['tracks'][0]['raw_bytes'])
        context.close()
        browser.close()
    # No browser, sockets, view or audio consumer remains. The service owns this data.
    until(lambda: int(task(origin, new_id)['tracks'][0]['raw_bytes']) > current + 10000)
    saved = finish(origin, new_id)
    assert before > 0 and not errors, errors
    assert (root / new_id / 'iq-000000.wbiq').is_file()
    return {'first_task': task_id, 'closed_browser_task': saved, 'pageerrors': errors, 'held_click_ms': 1100}


def write_failure_gate(artifacts):
    """Real OS file-size exhaustion on this child only; no shared disk is filled."""
    root = (artifacts / 'write-failure-recordings').resolve()
    env = {**os.environ, 'WEBSA_RECORDING_DIR': str(root), 'WEBSA_PORT': str(free_port()), 'WEBSA_HOST': '127.0.0.1'}
    env.pop('WEBSA_RUST_IQS_SHIM', None)
    origin = 'http://127.0.0.1:' + env['WEBSA_PORT']
    def limit():
        signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
        resource.setrlimit(resource.RLIMIT_FSIZE, (65536, 65536))
    proc = None
    with (artifacts / 'write-failure-service.log').open('w') as log:
        try:
            proc, origin = start_owned_service([str(WEBSAD)], port=int(env['WEBSA_PORT']), cwd=ROOT,
                env=env, stdout=log, stderr=log, preexec_fn=limit, startup_timeout=12)
            setup(origin)
            before = api(origin)
            uid, healthy = [r['receiver_uid'] for r in before['receivers'][::2]]
            async def consume():
                async with connect(origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + healthy) as socket:
                    task_id = await asyncio.to_thread(start, origin, uid, 'write-failure')
                    failed = await asyncio.to_thread(until, lambda: t if (t := task(origin, task_id))['state'] == 'failed' else None)
                    assert failed['fault']['code'] == 'RECORDING_IO' and 'File too large' in failed['fault']['message'], failed
                    frames = 0
                    async with asyncio.timeout(12):
                        while frames < 12:
                            message = await socket.recv()
                            if isinstance(message, bytes) and len(message) > 56:
                                frames += 1
                    assert api(origin)['receivers'][2]['stream'] == before['receivers'][2]['stream']
                    return task_id, failed, frames
            task_id, failed, frames = asyncio.run(consume())
            blocks, complete = read_archive(root / task_id / 'iq-000000.wbiq')
            assert blocks and not complete
            stop(proc)
            proc, origin = start_owned_service([str(WEBSAD)], port=int(env['WEBSA_PORT']), cwd=ROOT,
                env=env, stdout=log, stderr=log, startup_timeout=12)
            recovered = task(origin, task_id)
            assert recovered['state'] == 'failed'
            assert int(recovered['tracks'][0]['raw_bytes']) == sum(i['raw_bytes'] for i, _ in blocks)
            return {'os_error': 'EFBIG / RLIMIT_FSIZE', 'failed': failed, 'recovered': recovered,
                    'healthy_frames_after_failure': frames, 'checked_prefix_blocks': len(blocks)}
        finally:
            stop(proc)


def setup(origin):
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(origin, {'cmd': 'SET_SDR', 'center': 100e6})
    current = reset_receivers(origin)['receivers'][0]
    for i in range(3):
        command = {'type': 'create', 'request_id': f'iq-create-{current["revision"]}-{i}', 'config': config(100e6 + i * 10000)}
        if i == 0:
            command.update(type='configure', receiver_uid=current['receiver_uid'], expected_revision=current['revision'])
        reply = api(origin, payload=command)
        assert reply['fault'] is None, reply


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-p5-recording'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    root = (args.artifacts / 'recordings').resolve()
    root.mkdir(exist_ok=True)
    os.environ['WEBSA_RECORDING_DIR'] = str(root)
    port = free_port()
    proc = None
    with (args.artifacts / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(port, log=log)
            setup(origin)
            report = {'source_kind': 'synthetic native baseband', 'protocol': asyncio.run(protocol(origin, root))}
            report['browser'] = browser_gate(origin, root, args.artifacts)
            report['write_failure'] = write_failure_gate(args.artifacts)
            uid = api(origin)['receivers'][0]['receiver_uid']
            interrupted_id = start(origin, uid, 'interrupted-start')
            until(lambda: int(task(origin, interrupted_id)['tracks'][0]['raw_bytes']) > 10000)
            prior = api(origin)
            proc.kill()
            proc.wait(timeout=3)
            proc, origin = start_service(port, log=log)
            restored = task(origin, interrupted_id)
            assert restored['state'] == 'failed' and restored['fault']['code'] == 'SERVICE_INTERRUPTED', restored
            assert restored['receiver_uid'] == uid and restored['service_id'] == prior['service_id']
            assert api(origin)['service_id'] != prior['service_id']
            assert not any(t['state'] in ['preparing', 'recording', 'finalizing'] for t in api(origin)['recordings'])
            assert int(restored['tracks'][0]['raw_bytes']) >= int(next(t for t in prior['recordings'] if t['task_id'] == interrupted_id)['tracks'][0]['raw_bytes'])
            report['recovered'] = restored
            # A successful start receipt already owns durable preparing metadata and files.
            setup(origin)
            prepared_uid = api(origin)['receivers'][2]['receiver_uid']
            prepared_id = start(origin, prepared_uid, 'prepared-ack')
            assert (root / prepared_id / 'task.json').is_file()
            assert (root / prepared_id / 'iq-000000.wbiq').is_file()
            proc.kill()
            proc.wait(timeout=3)
            proc, origin = start_service(port, log=log)
            prepared = task(origin, prepared_id)
            assert prepared['state'] == 'failed' and prepared['fault']['code'] == 'SERVICE_INTERRUPTED', prepared
            assert prepared['receiver_uid'] == prepared_uid
            report['prepared_ack_recovery'] = prepared
            report['checks'] = ['backend bytes equal actual IQ network bytes', 'retune and rate segments', 'healthy lane identity/config',
                                'source stop drains', '1100ms mouse clicks across refresh', 'compression disclosure survives polling', '420px Pop out start/stop', 'Dock',
                                'page refresh', 'closed browser keeps recording', 'SIGKILL history recovery', 'durable start acknowledgement survives immediate SIGKILL', 'no UID or service rebinding', 'real EFBIG write failure preserves prefix and healthy IQ']
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({'ok': True, 'artifacts': str(args.artifacts), 'checks': report['checks']}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
