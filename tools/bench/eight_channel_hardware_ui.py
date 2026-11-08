#!/usr/bin/env python3
"""Eight known RF tones through a 50 MHz N90 capture and the real workbench UI."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import struct
import threading
import time
from contextlib import contextmanager, nullcontext
from pathlib import Path

import numpy as np
from eight_channel_bench import resources
from eight_channel_rf import CENTER, FREQUENCIES, TONES, BenchRadio, HardwareSession, save
from native_eight_channel_workflow import browser_gate, setup
from native_multichannel import api
from native_multichannel_recording import until
from rust_iq_browser import post
from websockets.asyncio.client import connect


@contextmanager
def observe_ui(session, artifacts):
    """Observe the UI gate separately from the sustained recording benchmark."""
    finished = threading.Event()
    started = time.monotonic()
    def observe():
        with (artifacts / 'ui-resources.jsonl').open('w') as output:
            while not finished.is_set():
                began = time.monotonic()
                try:
                    processes, pending = [], [os.getpid()]
                    for pid in pending:
                        try:
                            path = Path('/proc') / str(pid)
                            fields = (path / 'stat').read_text().rsplit(') ', 1)[1].split()
                            processes.append({'pid': pid, 'name': (path / 'comm').read_text().strip(),
                                'cpu_seconds': (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK'),
                                'rss_kib': int(fields[21]) * os.sysconf('SC_PAGE_SIZE') // 1024})
                            pending.extend(int(child) for child in (path / 'task' / str(pid) / 'children').read_text().split())
                        except (FileNotFoundError, ProcessLookupError):
                            pass
                    row = {'elapsed': time.monotonic() - started, 'backend': resources(session.process.pid),
                        'processes': processes, 'state': post(session.origin, {'cmd': 'STATUS'}),
                        'snapshot': api(session.origin)}
                    row['observer_seconds'] = time.monotonic() - began
                except Exception as error:
                    row = {'elapsed': time.monotonic() - started, 'error': str(error)}
                output.write(json.dumps(row) + '\n'); output.flush()
                finished.wait(1)
    thread = threading.Thread(target=observe, daemon=True)
    thread.start()
    try:
        yield
    finally:
        finished.set(); thread.join(timeout=5)


async def measure_tones(origin, snapshot, artifacts, expected_tones=TONES):
    async def receive(receiver, expected):
        uid = receiver['receiver_uid']
        async with connect(origin.replace('http', 'ws') + '/ws/audio?receiver_uid=' + uid,
                max_size=4 * 1024 * 1024) as socket:
            chunks, count, last, identity = [], 0, None, None
            while count < 96000:
                packet = await asyncio.wait_for(socket.recv(), 10)
                assert packet[:4] == b'AUD2'
                sequence, frames = struct.unpack_from('<II', packet, 8)
                if frames == 0:
                    chunks, count, last = [], 0, None
                    identity = packet[32:40]
                    continue
                if identity is None:
                    identity = packet[32:40]
                assert packet[32:40] == identity
                if last is not None:
                    assert sequence == last + 1, (uid, last, sequence)
                last = sequence
                rate = struct.unpack_from('<d', packet, 16)[0]
                channels = struct.unpack_from('<I', packet, 56)[0]
                values = np.frombuffer(packet, dtype='<f4', offset=96).reshape(-1, channels).mean(axis=1)
                assert np.isfinite(values).all()
                chunks.append(values); count += len(values)
            samples = np.concatenate(chunks)[-48000:]
            spectrum = np.abs(np.fft.rfft((samples - samples.mean()) * np.hanning(len(samples))))
            spectrum[:100] = 0
            peak = int(np.argmax(spectrum))
            tone = peak * rate / len(samples)
            rms = float(np.sqrt(np.mean(samples ** 2)))
            result = {'receiver_uid': uid, 'expected_hz': expected, 'peak_hz': tone, 'rms': rms,
                'peak_above_median_db': float(10 * np.log10(spectrum[peak] ** 2 / np.median(spectrum[100:4000] ** 2))),
                'rate_hz': rate, 'stream_hex': identity.hex(), 'last_sequence': last}
            save(artifacts / f'tone-{receiver["display_number"]}.json', result)
            samples.astype('<f4').tofile(artifacts / f'tone-{receiver["display_number"]}.f32')
            assert rms > 1e-5 and result['peak_above_median_db'] > 20, result
            return result
    results = await asyncio.gather(*(receive(r, tone) for r, tone in zip(snapshot['receivers'], expected_tones, strict=True)
        if tone is not None), return_exceptions=True)
    errors = [str(result) for result in results if isinstance(result, BaseException)]
    save(artifacts / 'tone-checks.json', {'errors': errors})
    assert not errors, errors
    return results


async def verify_tones(origin, snapshot, artifacts, expected_tones=TONES):
    results = await measure_tones(origin, snapshot, artifacts, expected_tones)
    assert all(abs(row['peak_hz'] - row['expected_hz']) < 25 for row in results), results
    return results


async def calibrate_tones(origin, snapshot, artifacts, tx, expected_tones=TONES):
    """Measure the source's common carrier error before starting acceptance."""
    attempts = []
    for index in range(4):
        directory = artifacts / f'calibration-{index + 1}'
        directory.mkdir()
        results = await measure_tones(origin, snapshot, directory, expected_tones)
        modes = {receiver['receiver_uid']: receiver['confirmed']['processors'][0]['mode']
            for receiver in snapshot['receivers'] if receiver['confirmed']['processors']}
        error = float(np.median([(row['peak_hz'] - row['expected_hz'])
            * (1 if modes[row['receiver_uid']] == 'usb' else -1) for row in results
            if modes[row['receiver_uid']] in ['usb', 'lsb']]))
        attempts.append({'tx_lo_hz': tx.sdr.tx_lo, 'carrier_error_hz': error, 'tones': results})
        save(artifacts / 'source-calibration.json', {'attempts': attempts, 'verified': False})
        if all(abs(row['peak_hz'] - row['expected_hz']) < 25 for row in results):
            save(artifacts / 'source-calibration.json', {'attempts': attempts, 'verified': True})
            return results
        assert abs(error) < 2000, 'The measured source is outside the carrier calibration range'
        tx.sdr.tx_lo = round(tx.sdr.tx_lo - error)
    raise AssertionError('The RF source did not stabilize within the original 25 Hz tone threshold')


async def wait_stable_carrier(origin, snapshot, artifacts, expected_tones=TONES):
    """Wait for measured RF stability before testing a 6.25 Hz-spaced waveform.

    The acceptance clock has not started. Do not chase the oscillator with live
    retunes: require three measurements spanning a minute to agree within 2 Hz.
    """
    directory = artifacts / 'source-stability'
    directory.mkdir()
    modes = {receiver['receiver_uid']: receiver['confirmed']['processors'][0]['mode']
        for receiver in snapshot['receivers'] if receiver['confirmed']['processors']}
    rows, started = [], time.monotonic()
    for index in range(29):
        attempt = directory / str(index + 1)
        attempt.mkdir()
        tones = await measure_tones(origin, snapshot, attempt, expected_tones)
        error = float(np.median([(row['peak_hz'] - row['expected_hz'])
            * (1 if modes[row['receiver_uid']] == 'usb' else -1) for row in tones
            if modes[row['receiver_uid']] in ['usb', 'lsb']]))
        assert np.isfinite(error) and abs(error) < 2000
        rows.append({'elapsed_seconds': time.monotonic() - started, 'carrier_error_hz': error})
        recent = rows[-3:]
        stable = len(recent) == 3 and recent[-1]['elapsed_seconds'] - recent[0]['elapsed_seconds'] >= 60 \
            and max(row['carrier_error_hz'] for row in recent) - min(row['carrier_error_hz'] for row in recent) <= 2
        save(directory / 'report.json', {'verified': stable, 'observations': rows})
        if stable:
            return
        await asyncio.sleep(30)
    raise AssertionError('RF carrier did not reach the measured stability threshold before acceptance')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', required=True, type=Path)
    parser.add_argument('--original-origin', default='http://127.0.0.1:8080')
    parser.add_argument('--expect-service-id', required=True)
    parser.add_argument('--gain', type=float, default=-30)
    parser.add_argument('--reference', type=float, default=-40)
    parser.add_argument('--lo-correction', type=float, default=0)
    parser.add_argument('--ui-only', action='store_true', help='Do not transmit or claim known RF content; test the hardware UI with the current input')
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=False)
    with (HardwareSession(args.artifacts, args.original_origin, args.expect_service_id) as session,
            nullcontext(None) if args.ui_only else BenchRadio(args.artifacts, gain=args.gain, lo_correction=args.lo_correction) as tx):
        if tx:
            tx.narrow()
        snapshot = setup(session.origin, center=CENTER, frequencies=FREQUENCIES, reference=args.reference, agc=True)
        snapshot = until(lambda: s if (s := api(session.origin))['capture']['state'] == 'running'
            and all(r['state'] == 'running' for r in s['receivers']) else None)
        save(args.artifacts / 'configured.json', snapshot)
        with observe_ui(session, args.artifacts):
            try:
                tones = [] if args.ui_only else asyncio.run(calibrate_tones(session.origin, snapshot, args.artifacts, tx))
                report = browser_gate(session.origin, snapshot, args.artifacts,
                    source_kind='hardware, known RF unverified' if args.ui_only else 'hardware RF')
                report['known_tones'] = tones
                save(args.artifacts / 'report.json', report)
            finally:
                save(args.artifacts / 'final-receivers.json', api(session.origin))
                save(args.artifacts / 'final-state.json', post(session.origin, {'cmd': 'STATUS'}))
    print('PASS: 50 MHz hardware browser workflow; original states restored; '
        + ('known RF content remains UNVERIFIED' if args.ui_only else 'eight known RF tones verified'))


if __name__ == '__main__':
    main()
