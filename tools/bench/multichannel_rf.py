#!/usr/bin/env python3
"""Sustained owned SAN90/Pluto RF reception with a known tone in every channel.

Uses the normal service, capture and subscriptions. The artifact directory must
be new; its service and transmitter are closed on failure as well as success.
No unbounded IQ/audio capture is retained.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import subprocess
import sys
import time
import traceback
from pathlib import Path

from eight_channel_bench import resources
from eight_channel_hardware_ui import calibrate_tones, verify_tones
from eight_channel_rf import CENTER, MODES, BenchRadio, DisplayConsumer, file_hash, save
from eight_channel_streams import StreamMonitor
from native_eight_channel_workflow import setup
from native_multichannel import api
from native_multichannel_recording import until
from pluto_radio import install_signal_handlers
from rust_iq_browser import ROOT, free_port, post, start_service, stop


def stable_receivers(snapshot):
    return [{key: receiver[key] for key in ['receiver_uid', 'revision', 'confirmed', 'stream']}
            for receiver in snapshot['receivers']]


def check_receivers(snapshot, baseline):
    assert snapshot['capture'] == baseline['capture'], 'capture geometry/identity changed'
    assert stable_receivers(snapshot) == stable_receivers(baseline), 'receiver identity or configuration changed'
    for receiver in snapshot['receivers']:
        assert receiver['state'] == 'running' and receiver['fault'] is None, receiver
        assert receiver['processing_state'] not in ['failed', 'retiring'], receiver
        assert receiver['processing_fault'] is None, receiver


def remove_tone_samples(directory):
    removed = []
    for path in directory.rglob('tone-*.f32'):
        removed.append({'name': str(path.relative_to(directory)), 'bytes': path.stat().st_size, 'sha256': file_hash(path)})
        path.unlink()
    save(directory / 'sample-cleanup.json', removed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--shim', type=Path, required=True)
    parser.add_argument('--channels', type=int, choices=[1, 3, 8, 16], default=16)
    parser.add_argument('--seconds', type=float, default=600)
    parser.add_argument('--gain', type=float, default=-30)
    parser.add_argument('--reference', type=float, default=-40)
    args = parser.parse_args()
    if not 0 < args.seconds <= 86400:
        parser.error('seconds must be finite and in (0, 86400]')
    if not -89.75 <= args.gain <= 0 or not -130 <= args.reference <= 20:
        parser.error('gain/reference outside hardware range')
    if not args.shim.is_file():
        parser.error('matching SDK shim is required')
    args.artifacts.mkdir(parents=True, exist_ok=False)
    install_signal_handlers()
    frequencies = [CENTER + (slot - (args.channels - 1) / 2) * 20000 for slot in range(args.channels)]
    tones = [400 + slot * 100 for slot in range(args.channels)]
    modes = [MODES[slot % len(MODES)] for slot in range(args.channels)]
    original_env = {key: os.environ.get(key) for key in ['WEBSA_RUST_IQS_SHIM', 'WEBSA_RECORDING_DIR']}
    os.environ['WEBSA_RUST_IQS_SHIM'] = str(args.shim.resolve())
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    process = consumer = None
    result = {'passed': False, 'source_kind': 'hardware', 'channels': args.channels,
              'requested_seconds': args.seconds, 'scope': 'real RF narrow AM/NFM/USB/LSB, every IQ and audio subscription'}
    try:
        with (args.artifacts / 'service.log').open('w') as log:
            process, origin = start_service(free_port(), hardware=True, log=log, startup_timeout=30)
            consumer = DisplayConsumer(origin)
            until(lambda: post(origin, {'cmd': 'STATUS'})['connected'], timeout=30)
            binary = ROOT / 'backend/target/release/websad'
            result.update(pid=process.pid, origin=origin, backend_sha256=file_hash(binary),
                          running_binary_matches=os.stat(binary).st_ino == os.stat(f'/proc/{process.pid}/exe').st_ino,
                          adapter={'path': str(args.shim.resolve()), 'sha256': file_hash(args.shim)})
            assert result['running_binary_matches']
            save(args.artifacts / 'device-before.json', post(origin, {'cmd': 'STATUS'}))
            save(args.artifacts / 'host.json', {'uname': subprocess.check_output(['uname', '-a'], text=True),
                 'cpu': subprocess.check_output(['lscpu'], text=True),
                 'usb': subprocess.check_output(['lsusb'], text=True),
                 'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()})
            with BenchRadio(args.artifacts, gain=args.gain) as tx:
                tx.narrow(frequencies=frequencies, tones=tones, modes=modes)
                snapshot = setup(origin, center=CENTER, frequencies=frequencies,
                                 modes=modes, reference=args.reference, agc=True)
                until(lambda: all(row['state'] == 'running' and row.get('clock')
                      for row in api(origin)['receivers']), timeout=30)
                calibration = args.artifacts / 'calibration'; calibration.mkdir()
                result['initial_tones'] = asyncio.run(calibrate_tones(origin, api(origin), calibration, tx, tones))
                remove_tone_samples(calibration)
                snapshot = api(origin)
                save(args.artifacts / 'measurement-start.json', snapshot)
                with StreamMonitor(origin, snapshot, all_iq=True) as monitor:
                    until(lambda: len(monitor.status()) == 2 * args.channels
                          and all(row['samples'] > 10000 for row in monitor.status().values()), timeout=30)
                    network_start = monitor.status()
                    began = time.monotonic()
                    with (args.artifacts / 'observations.jsonl').open('w') as observations:
                        while True:
                            current = api(origin)
                            check_receivers(current, snapshot)
                            tx.check_stream()
                            network = monitor.status()
                            memory = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
                            available_kib = int(memory['MemAvailable'].split()[0])
                            assert available_kib > 512 * 1024, 'host memory guard stopped this RF run'
                            row = {'elapsed': time.monotonic() - began, 'available_kib': available_kib,
                                   'process': resources(process.pid), 'network': network,
                                   'actual': post(origin, {'cmd': 'STATUS'})['sdr']['actual']}
                            observations.write(json.dumps(row) + '\n'); observations.flush()
                            if row['elapsed'] >= args.seconds:
                                break
                            time.sleep(min(5, args.seconds - row['elapsed']))
                    elapsed = time.monotonic() - began
                    network_end = monitor.status()
                    for key, row in network_end.items():
                        duration = (row['samples'] - network_start[key]['samples']) / row['rate_hz']
                        assert abs(duration - elapsed) < 1, (key, duration, elapsed)
                        assert row['energy'] > network_start[key]['energy'], key
                    save(args.artifacts / 'network.json', {'start': network_start, 'end': network_end, 'elapsed': elapsed})
                    final_tones = args.artifacts / 'final-tones'; final_tones.mkdir()
                    result['final_tones'] = asyncio.run(verify_tones(origin, api(origin), final_tones, tones))
                    remove_tone_samples(final_tones)
                    monitor.check()
                save(args.artifacts / 'measurement-end.json', api(origin))
                save(args.artifacts / 'device-end.json', post(origin, {'cmd': 'STATUS'}))
                result.update(passed=True, elapsed_seconds=elapsed, streams_checked=len(network_end),
                              checks=['all channel RF tones at start and end', 'every IQ exact source cadence',
                                      'every audio sample index', 'unchanged stream/UID/configuration',
                                      'no receive/processing faults', 'bounded host memory observation'])
    except BaseException as error:
        result.update(passed=False, error=repr(error), traceback=traceback.format_exc())
        if process and process.poll() is None:
            try:
                save(args.artifacts / 'failure-state.json', post(origin, {'cmd': 'STATUS'}))
                save(args.artifacts / 'failure-receivers.json', api(origin))
            except Exception as diagnostic_error:
                result['diagnostic_error'] = repr(diagnostic_error)
        raise
    finally:
        cleanup_errors = []
        if consumer:
            try:
                consumer.close()
            except Exception as error:
                cleanup_errors.append(f'display_close: {error!r}')
        try:
            stop(process)
        except Exception as error:
            cleanup_errors.append(f'service_stop: {error!r}')
        result['process_stopped'] = process is None or process.poll() is not None
        result['process_exit_code'] = None if process is None else process.returncode
        # Even failed calibration leaves at most one second of PCM per channel.
        for name in ['calibration', 'final-tones']:
            directory = args.artifacts / name
            try:
                if directory.exists() and list(directory.rglob('tone-*.f32')):
                    remove_tone_samples(directory)
            except Exception as error:
                cleanup_errors.append(f'sample_cleanup ({name}): {error!r}')
        for key, previous in original_env.items():
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous
        if cleanup_errors:
            result.update(passed=False, cleanup_errors=cleanup_errors)

        def write_report():
            try:
                save(args.artifacts / 'report.json', result)
                return True
            except Exception as error:
                cleanup_errors.append(f'report_write: {error!r}')
                result.update(passed=False, cleanup_errors=cleanup_errors)
                return False

        report_written = write_report()
        try:
            print(json.dumps({key: result[key] for key in ['passed', 'channels', 'requested_seconds', 'process_stopped']}))
        except Exception as error:
            cleanup_errors.append(f'summary_print: {error!r}')
            result.update(passed=False, cleanup_errors=cleanup_errors)
            if report_written:
                write_report()
        if cleanup_errors:
            # Reporting can fail because the artifact filesystem or stdout is
            # unavailable. Preserve the measurement exception and try stderr.
            try:
                primary = result.get('error', 'RF run cleanup failed')
                sys.stderr.write(f'{primary}; cleanup: {"; ".join(cleanup_errors)}\n')
            except Exception:
                pass
        if cleanup_errors and 'error' not in result:
            raise RuntimeError('; '.join(cleanup_errors))


if __name__ == '__main__':
    main()
