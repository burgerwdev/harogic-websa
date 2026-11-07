#!/usr/bin/env python3
"""Eight shared recording slots, full-band capture, bounded archive reads and restart recovery."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import platform
import re
import struct
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, contextmanager, nullcontext
from fractions import Fraction
from pathlib import Path

import numpy as np
from eight_channel_bench import fine_busy_fraction, resources
from eight_channel_hardware_ui import calibrate_tones, verify_tones, wait_stable_carrier
from eight_channel_rf import (
    CENTER,
    FREQUENCIES,
    MIXED_MODES,
    MIXED_TONES,
    TONES,
    BenchRadio,
    HardwareSession,
    save,
)
from eight_channel_streams import StreamMonitor, input_stride
from native_eight_channel_workflow import setup
from native_multichannel import api
from native_multichannel_audio import read_journal as read_audio
from native_multichannel_recording import finish, read_archive, until
from native_multichannel_results import read_journal as read_results
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, state, stop
from viewport_baseline import wait_ui_ready

ACTIVE = ['preparing', 'waiting', 'recording', 'finalizing']


def performance_summary(artifacts):
    observations = [json.loads(line) for line in (artifacts / 'observations.jsonl').read_text().splitlines()]
    usage = [json.loads(line) for line in (artifacts / 'resources.jsonl').read_text().splitlines()]
    first, last = observations[0], observations[-1]
    seconds = last['elapsed'] - first['elapsed']
    before, after = first['actual'], last['actual']
    def stage(name):
        return (after[name]['total_ns'] - before[name]['total_ns']) / 1e9 / seconds
    stages = {'fine_and_framing': fine_busy_fraction(before, after, seconds),
        'capture_producer': stage('pfb_submit') + stage('fine_handoff')}
    threads = {row['tid']: row for row in usage[0]['threads']}
    duration = usage[-1]['elapsed'] - usage[0]['elapsed']
    busy = [{'name': row['name'], 'tid': row['tid'],
        'busy_fraction': (row['cpu_seconds'] - threads[row['tid']]['cpu_seconds']) / duration}
        for row in usage[-1]['threads'] if row['tid'] in threads]
    windows = []
    for offset in range(0, max(1, int(last['elapsed'])), 300):
        rows = [row for row in usage if offset <= row['elapsed'] <= offset + 300]
        if rows:
            window = {'start_seconds': offset, 'observations': len(rows),
                'rss_min_kib': min(row['rss_kib'] for row in rows), 'rss_max_kib': max(row['rss_kib'] for row in rows),
                'rss_first_kib': rows[0]['rss_kib'], 'rss_last_kib': rows[-1]['rss_kib']}
            span = rows[-1]['elapsed'] - rows[0]['elapsed']
            window['duration_seconds'] = span
            window['margin_evaluated'] = span >= 30
            if span >= 30:
                start_threads = {row['tid']: row for row in rows[0]['threads']}
                fractions = [{'name': row['name'], 'tid': row['tid'],
                    'busy_fraction': (row['cpu_seconds'] - start_threads[row['tid']]['cpu_seconds']) / span}
                    for row in rows[-1]['threads'] if row['tid'] in start_threads]
                stage_rows = [row for row in observations if offset <= row['elapsed'] <= offset + 300]
                begin, end = stage_rows[0], stage_rows[-1]
                stage_span = end['elapsed'] - begin['elapsed']
                delta = {name: (end['actual'][name]['total_ns'] - begin['actual'][name]['total_ns']) / 1e9 / stage_span
                    for name in ['fine', 'framer', 'pfb_submit', 'fine_handoff']}
                window_stages = {'fine_and_framing': fine_busy_fraction(begin['actual'], end['actual'], stage_span),
                    'capture_producer': delta['pfb_submit'] + delta['fine_handoff']}
                window.update(threads=fractions, stage_busy_fraction=window_stages,
                    twenty_percent_margin=all(row['busy_fraction'] <= .8 for row in fractions)
                        and all(value <= .8 for value in window_stages.values()))
            windows.append(window)
    capture = after['iqs_capture']
    usb_rows = []
    started = json.loads((artifacts / 'measurement.json').read_text())['start_monotonic_seconds'] \
        if (artifacts / 'measurement.json').exists() else None
    if started is not None:
        pattern = r'USB queue: monotonic_ns=(\d+) queued=(\d+) peak=(\d+) age_max_ns=(\d+) capacity=(\d+) delivered=(\d+) failed=(\d+)'
        for match in re.finditer(pattern, (artifacts / 'service.log').read_text()):
            timestamp, queued, peak, age, capacity, delivered, failed = map(int, match.groups())
            elapsed = timestamp / 1e9 - started
            if 0 <= elapsed <= last['elapsed']:
                usb_rows.append({'elapsed': elapsed, 'queued_packets': queued, 'peak_packets': peak,
                    'age_max_ms': age / 1e6, 'capacity_packets': capacity, 'delivered': delivered, 'failed': failed})
    usb_windows = []
    for window in windows:
        offset = window['start_seconds']
        rows = [row for row in usb_rows if offset <= row['elapsed'] < offset + 300]
        if rows and window['margin_evaluated']:
            usb_windows.append({'start_seconds': offset, 'observations': len(rows),
                'queued_min': min(row['queued_packets'] for row in rows),
                'queued_max': max(row['queued_packets'] for row in rows),
                'queued_last': rows[-1]['queued_packets'],
                'returned_near_empty': min(row['queued_packets'] for row in rows) <= rows[-1]['capacity_packets'] / 20})
    return {'seconds': seconds, 'stage_busy_fraction': stages,
        'threads': sorted(busy, key=lambda row: -row['busy_fraction']),
        'twenty_percent_margin': all(value <= .8 for value in stages.values())
            and all(row['busy_fraction'] <= .8 for row in busy)
            and all(row['twenty_percent_margin'] for row in windows if row['margin_evaluated']),
        'rss_windows': windows, 'receiver_workers': after['receiver_workers'],
        'capture_queue_residence_max_ms': capture['queue_residence']['max_ns'] / 1e6,
        'capture_queue_wait_max_ms': capture['queue_wait_max_ms'],
        'capture_queue_full': capture['queue_full'], 'failed_reads': capture['fail_calls'],
        'recoveries': capture['recoveries'], 'source_gaps': after['iqs_stamp_gaps'],
        'usb_handoff_observations': usb_rows,
        'usb_handoff_windows': usb_windows,
        'usb_returns_near_empty': bool(usb_windows) and all(row['returned_near_empty'] for row in usb_windows),
        'recording_queued_bytes_max': max(int(task['queued_bytes']) for row in observations
            for task in row['snapshot']['recordings'])}


def start_eight(origin, key, choices=None):
    snapshot = api(origin)
    uids = [r['receiver_uid'] for r in snapshot['receivers']]
    choices = choices or [(0, 'iq'), (0, 'audio'), (0, 'results'), (1, 'iq'), (1, 'audio'),
        (2, 'audio'), (3, 'audio'), (7, 'iq')]
    requests = [{'type': 'record_start', 'request_id': f'{key}-{index}',
        'receiver_uid': uids[channel], 'kinds': [kind], **({'audio_format': 'wav'} if kind == 'audio' else {})}
        for index, (channel, kind) in enumerate(choices)]
    with ThreadPoolExecutor(max_workers=8) as pool:
        replies = list(pool.map(lambda request: api(origin, payload=request), requests))
    assert all(r['fault'] is None for r in replies), replies
    assert api(origin, payload=requests[0]) == replies[0], 'duplicate request changed its receipt'
    snapshot = api(origin)
    capacity = snapshot['capabilities']['max_recording_tasks']
    assert capacity == snapshot['recording_resources']['max_tasks']
    assert capacity >= len(choices)
    assert snapshot['recording_resources']['active_tasks'] == len(choices)
    assert snapshot['recording_resources']['available_tasks'] == capacity - len(choices)
    before = {r['task_id']: r['accepted_bytes'] for r in snapshot['recordings'] if r['state'] in ACTIVE}
    spare_iq = next(index for index in range(8) if (index, 'iq') not in choices)
    ninth = api(origin, payload={'type': 'record_start', 'request_id': key + '-ninth',
        'receiver_uid': uids[spare_iq], 'kinds': ['iq']})
    if capacity == len(choices):
        assert ninth['fault'] and 'capacity' in ninth['fault']['message'], ninth
    else:
        assert ninth['fault'] is None, ninth
        extra = next(t for t in ninth['snapshot']['recordings'] if t['task_id'] not in before and t['state'] in ACTIVE)
        reply = api(origin, payload={'type': 'record_stop', 'request_id': key + '-probe-stop', 'task_id': extra['task_id']})
        assert reply['fault'] is None, reply
        until(lambda: not any(t['task_id'] == extra['task_id'] and t['state'] in ACTIVE for t in api(origin)['recordings']))
    assert set(before) == {r['task_id'] for r in api(origin)['recordings'] if r['state'] in ACTIVE}
    return snapshot, ninth


def browser_quota(origin, artifacts, spare_audio=4):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1500, 'height': 1100})
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        view = page.locator('[data-view-id=overview-follow]')
        quota = api(origin)['recording_resources']
        expect(view.locator('[data-recording-capacity]').first).to_contain_text(f"{quota['active_tasks']} / {quota['max_tasks']}")
        rows = view.locator('.mc-row')
        rows.nth(spare_audio).locator('[data-action=select]').click()
        view.locator('[data-action=tab-recording]').click()
        spare = view.locator('[data-editor] [data-action=rec-audio]')
        expect(spare).to_be_enabled() if quota['available_tasks'] else expect(spare).to_be_disabled()
        rows.first.locator('[data-action=select]').click()
        button = view.locator('[data-editor] [data-action=rec-iq]')
        expect(button).to_be_enabled(); button.click()
        expect(view.locator('[data-recording-capacity]').first).to_contain_text(f"{quota['active_tasks'] - 1} / {quota['max_tasks']}")
        expect(button).to_have_attribute('aria-pressed', 'false')
        button.click()
        expect(view.locator('[data-recording-capacity]').first).to_contain_text(f"{quota['active_tasks']} / {quota['max_tasks']}")
        page.set_viewport_size({'width': 420, 'height': 900})
        page.screenshot(path=str(artifacts / 'quota-420.png'))
        assert not view.evaluate('host=>host.scrollWidth>host.clientWidth+2')
        page.close(); browser.close()


def checked_files(root, task, *, complete=True):
    track, total, samples, rows = task['tracks'][0], 0, 0, 0
    last = {}
    def ordered(key, sequence, position=None, length=0, step=0):
        if key in last:
            old_seq, old_pos, old_length = last[key]
            assert sequence == old_seq + 1, (key, old_seq, sequence)
            if position is not None:
                assert position == old_pos + old_length * step, (position, old_pos, old_length, step)
        last[key] = sequence, position, length
    if track['kind'] == 'iq':
        for volume in track['volumes']:
            blocks, finished = read_archive(root / task['task_id'] / volume['file'])
            assert finished or not complete
            for index, raw in blocks:
                meta, clock = index['meta'], index['meta']['clock']
                assert meta['service_id'] == task['service_id'] and clock['receiver_uid'] == task['receiver_uid']
                position = Fraction(int(clock['first_input_index'])) + Fraction(int(clock['fraction_num']), int(clock['fraction_den']))
                step = input_stride(clock['input_rate_hz'], clock['actual_rate_hz'])
                ordered((clock['epoch'], clock['channel_id']), int(meta['sequence']), position, clock['samples'], step)
                values = np.frombuffer(raw, dtype='<f4'); assert np.isfinite(values).all()
                total += len(raw); samples += clock['samples']; rows += 1
            del blocks
    elif track['kind'] == 'audio':
        records, _ = read_audio(root / task['task_id'])
        for source, packet in records:
            assert source['service_id'] == task['service_id'] and source['receiver_uid'] == task['receiver_uid']
            assert packet[:4] == b'AUD2'
            seq, count = struct.unpack_from('<II', packet, 8)
            channels = struct.unpack_from('<I', packet, 56)[0]
            position = Fraction(struct.unpack_from('<Q', packet, 40)[0]) + Fraction(*struct.unpack_from('<QQ', packet, 80))
            step = Fraction(struct.unpack_from('<d', packet, 72)[0]) / Fraction(struct.unpack_from('<d', packet, 16)[0])
            # Sequence zero is the exact source start and carries no PCM.
            if count:
                ordered(packet[32:40], seq, position, count, step)
            values = np.frombuffer(packet, dtype='<f4', offset=96)
            assert len(values) == count * channels and np.isfinite(values).all()
            total += len(packet) - 96; samples += count; rows += 1
    else:
        records, raws, _ = read_results(root / task['task_id'])
        for record in records:
            envelope = record['envelope']
            assert envelope['service_id'] == task['service_id'] and envelope['receiver_uid'] == task['receiver_uid']
            ordered((envelope['epoch'], envelope['channel_id']), envelope['seq'])
        total, rows = sum(map(len, raws)), len(records)
    assert total == int(track['raw_bytes']), (task['task_id'], total, track['raw_bytes'])
    assert rows and total and (not complete or task['accepted_bytes'] == track['raw_bytes'])
    return {'task_id': task['task_id'], 'kind': track['kind'], 'raw_bytes': total, 'samples': samples,
        'records': rows, 'checked_streams': len(last), 'complete': complete}


@contextmanager
def synthetic(artifacts):
    os.environ['WEBSA_RECORDING_DIR'] = str((artifacts / 'recordings').resolve())
    with (artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log)
        session = type('Session', (), {})()
        session.process, session.origin = process, origin
        try:
            yield session
        finally:
            stop(session.process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', required=True, type=Path)
    parser.add_argument('--seconds', type=float, default=600)
    parser.add_argument('--hardware', action='store_true')
    parser.add_argument('--expect-service-id')
    parser.add_argument('--skip-browser', action='store_true', help='Use the separately completed browser gate and observe capture only')
    parser.add_argument('--known-source', action='store_true', help='Transmit and verify eight known RF tones with the owned Pluto source')
    parser.add_argument('--cyclic-source', action='store_true',
        help='Use the existing short cyclic analog RF waveform; verify tones before and after without host TX streaming')
    parser.add_argument('--gain', type=float, default=-10)
    parser.add_argument('--lo-correction', type=float, default=-135)
    parser.add_argument('--profile', choices=['analog', 'mixed'], default='analog')
    parser.add_argument('--record-seconds', type=float, help='Stop the eight tasks after this measured interval; keep reception running')
    parser.add_argument('--consume-streams', action='store_true', help='Continuously check all audio, the last IQ stream and digital results with bounded consumers')
    args = parser.parse_args()
    if args.hardware and not args.expect_service_id:
        parser.error('--hardware requires --expect-service-id')
    if args.known_source and not args.hardware:
        parser.error('--known-source requires --hardware')
    mixed = args.profile == 'mixed'
    if args.cyclic_source and (mixed or not args.known_source):
        parser.error('cyclic-source requires known analog RF')
    if mixed and not (args.hardware and args.known_source):
        parser.error('mixed acceptance requires real hardware and its known RF source')
    if args.consume_streams and not args.hardware:
        parser.error('full-band stream-clock checks require raw hardware capture')
    record_seconds = args.record_seconds if args.record_seconds is not None else args.seconds
    if not 0 < record_seconds <= args.seconds:
        parser.error('record-seconds must be positive and no longer than seconds')
    choices = [(0, 'iq'), (0, 'audio'), (1, 'audio'), (2, 'audio'), (3, 'audio'),
        (4, 'audio'), (6, 'results'), (7, 'iq')] if mixed else None
    args.artifacts.mkdir(parents=True, exist_ok=False)
    owner = HardwareSession(args.artifacts, 'http://127.0.0.1:8080', args.expect_service_id) if args.hardware else synthetic(args.artifacts)
    with (owner as session, (BenchRadio(args.artifacts, args.gain, args.lo_correction)
            if args.known_source else nullcontext(None)) as tx,
            ExitStack() as subscriptions):
        origin, root = session.origin, args.artifacts / 'recordings'
        if tx:
            tx.mixed() if mixed else tx.narrow(streaming=not args.cyclic_source)
        setup(origin, center=CENTER if args.hardware else 100e6,
            frequencies=FREQUENCIES if args.hardware else None, reference=-40 if args.hardware else None,
            agc=True, modes=MIXED_MODES if mixed else None)
        tones = []
        if tx:
            if mixed:
                tx.track_carrier(origin, api(origin))
            else:
                # C8's known LSB has a negative complex-IQ tone even while its
                # demodulator publishes positive-frequency listening audio.
                tx.track_carrier(origin, api(origin), reference_hz=-TONES[-1])
            asyncio.run(wait_stable_carrier(origin, api(origin), args.artifacts, MIXED_TONES if mixed else TONES))
            (args.artifacts / 'tones-start').mkdir()
            tones = asyncio.run(calibrate_tones(origin, api(origin), args.artifacts / 'tones-start', tx,
                MIXED_TONES if mixed else TONES))
        _, ninth = start_eight(origin, 'eight', choices)
        if not args.skip_browser:
            browser_quota(origin, args.artifacts, spare_audio=5 if mixed else 4)
        monitor = subscriptions.enter_context(StreamMonitor(origin, api(origin))) if mixed or args.consume_streams else None
        def ready():
            snapshot = api(origin)
            active = [r for r in snapshot['recordings'] if r['state'] in ACTIVE]
            return snapshot if len(active) == 8 and all(r['state'] in ['waiting', 'recording']
                and int(r['tracks'][0]['raw_bytes']) > 0 for r in active) else None
        until(ready, timeout=60 if mixed else 30)
        if monitor:
            try:
                until(lambda: all(row['samples'] > 0 if row['kind'] != 'decode' else row['expected_text_count'] > 0
                    for row in monitor.status().values()), timeout=90)
            finally:
                with monitor.lock:
                    save(args.artifacts / 'network-warmup.json', {'streams': monitor.rows, 'errors': monitor.errors})
        time.sleep(5)
        snapshot = ready()
        assert snapshot, 'recording tasks stopped during warmup'
        ids = [r['task_id'] for r in snapshot['recordings'] if r['state'] in ACTIVE]
        assert len(ids) == 8
        baseline = {r['task_id']: int(r['accepted_bytes']) for r in snapshot['recordings'] if r['task_id'] in ids}
        network_baseline = monitor.status() if monitor else None
        save(args.artifacts / 'initial.json', {'snapshot': snapshot, 'state': state(origin)})
        save(args.artifacts / 'machine.json', {'platform': list(platform.uname()),
            'logical_cpus': os.cpu_count(), 'physical_memory_bytes': os.sysconf('SC_PAGE_SIZE') * os.sysconf('SC_PHYS_PAGES')})
        started, observations, saved = time.monotonic(), 0, None
        save(args.artifacts / 'measurement.json', {'start_monotonic_seconds': started,
            'requested_seconds': args.seconds, 'recording_seconds': record_seconds})
        epoch = snapshot['capture']['epoch']
        with (args.artifacts / 'observations.jsonl').open('w') as output, (args.artifacts / 'resources.jsonl').open('w') as usage:
            while True:
                if tx:
                    tx.check_stream()
                snapshot, actual = api(origin), state(origin)
                if snapshot['capture']['state'] != 'running' or snapshot['capture']['epoch'] != epoch \
                        or any(r['fault'] or r.get('processing_state') == 'failed' for r in snapshot['receivers']):
                    actual = post(origin, {'cmd': 'STATUS'})
                    save(args.artifacts / 'failure.json', {'elapsed': time.monotonic() - started,
                        'snapshot': snapshot, 'state': actual, 'resources': resources(session.process.pid)})
                    raise AssertionError([(r['display_number'], r['fault']) for r in snapshot['receivers']])
                assert snapshot['capture']['usable_bandwidth_hz'] == 50e6 and snapshot['capture']['input_rate_hz'] == 62500000
                assert snapshot['capture']['state'] == 'running' and not any(r['fault'] or r.get('processing_state') == 'failed' for r in snapshot['receivers'])
                assert snapshot['capture']['epoch'] == epoch
                current = [r for r in snapshot['recordings'] if r['task_id'] in ids]
                assert len(current) == 8 and all(r['state'] in (['completed'] if saved else ['waiting', 'recording']) for r in current), current
                assert snapshot['recording_resources']['active_tasks'] == (0 if saved else 8)
                assert all(int(r['queued_bytes']) <= 4 * 1024 * 1024 for r in current)
                network = monitor.status() if monitor else None
                if mixed:
                    digital = network['7-decode']
                    assert time.monotonic() - monitor.started - digital['last_expected_seconds'] < 45, \
                        'known FT8 text has been absent for three source periods'
                output.write(json.dumps({'elapsed': time.monotonic() - started, 'snapshot': snapshot, 'network': network,
                    'actual': {k: v for k, v in actual['sdr']['actual'].items() if k != 'iqs_events'}}) + '\n'); output.flush()
                usage.write(json.dumps({'elapsed': time.monotonic() - started, **resources(session.process.pid)}) + '\n'); usage.flush()
                observations += 1
                if not saved and time.monotonic() - started >= record_seconds:
                    saved = [finish(origin, identity) for identity in ids]
                    save(args.artifacts / 'recording-stopped.json', {'elapsed': time.monotonic() - started, 'tasks': saved})
                remaining = args.seconds - (time.monotonic() - started)
                if remaining <= 0:
                    break
                time.sleep(min(5, remaining))
        measured = time.monotonic() - started
        saved = saved or [finish(origin, identity) for identity in ids]
        assert all(int(task['accepted_bytes']) > baseline[task['task_id']] for task in saved)
        if monitor:
            network = monitor.status()
            for key, row in network.items():
                before = network_baseline[key]
                if row['kind'] != 'decode':
                    duration = (row['samples'] - before['samples']) / row['rate_hz']
                    assert abs(duration - measured) < 1, (key, duration, measured)
                else:
                    assert row['expected_text_count'] - before['expected_text_count'] >= max(1, int(measured / 30))
            save(args.artifacts / 'network.json', {'baseline': network_baseline, 'end': network, 'measurement_seconds': measured})
        if tx:
            (args.artifacts / 'tones-end').mkdir()
            asyncio.run(verify_tones(origin, api(origin), args.artifacts / 'tones-end', MIXED_TONES if mixed else TONES))
        if monitor:
            monitor.close()
        # Eight more active tasks must recover their own checked prefix after a hard stop.
        crashed, _ = start_eight(origin, 'crash', choices)
        until(ready, timeout=60 if mixed else 30)
        time.sleep(1)
        session.process.kill(); session.process.wait(timeout=5)
        with (args.artifacts / 'recovery-service.log').open('w') as log:
            session.process, session.origin = start_service(free_port(), log=log, startup_timeout=120)
            recovered = api(session.origin)
            crash_ids = {r['task_id'] for r in crashed['recordings'] if r['state'] in ACTIVE}
            prefixes = [r for r in recovered['recordings'] if r['task_id'] in crash_ids]
            assert len(prefixes) == 8 and all(r['state'] == 'failed' for r in prefixes)
            assert all(r['service_id'] == crashed['service_id'] for r in prefixes)
            assert recovered['recording_resources']['active_tasks'] == 0
            recovery_files = [checked_files(root, task, complete=False) for task in prefixes]
            files = [checked_files(root, task) for task in saved]
            save(args.artifacts / 'recovered.json', recovered)
        # Sampled statistics are evidence for resource margin; file clock checks
        # above establish exact per-stream continuity independently.
        if args.hardware and args.seconds >= 5:
            performance = performance_summary(args.artifacts)
            save(args.artifacts / 'performance.json', performance)
            # Throughput margin is an observation, never a product/test admission gate.
            # USB backlog windows remain diagnostic; exact continuity and actual
            # queue/read/source failures below determine data integrity.
            assert performance['failed_reads'] == performance['recoveries'] == performance['source_gaps'] == 0
        save(args.artifacts / 'report.json', {'source_kind': 'hardware RF' if tx else 'hardware input, known RF unverified' if args.hardware else 'independent baseband fixture',
            'capture': snapshot['capture'], 'elapsed_seconds': time.monotonic() - started,
            'measurement_seconds': measured, 'requested_seconds': args.seconds, 'observations': observations, 'ninth': ninth['fault'],
            'profile': args.profile, 'recording_seconds': record_seconds, 'network_checked': monitor is not None,
            'files': files, 'recovery_files': recovery_files, 'known_tones': tones, 'known_rf_verified': bool(tx)})
    print('PASS: eight shared recording slots, file clocks/CRC/Zstd and eight-task crash recovery')


if __name__ == '__main__':
    main()
