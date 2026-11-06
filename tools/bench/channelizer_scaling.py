#!/usr/bin/env python3
"""Compare two binaries on identical full-band PFB/fine inputs, without a radio.

This measures channelization and IQ delivery, not demodulator/model or product
channel capacity. Failed runs remain evidence; every child has a finite deadline.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import time
from pathlib import Path

from eight_channel_bench import resources

ROOT = Path(__file__).resolve().parents[2]


def save(path, data):
    path.write_text(json.dumps(data, indent=2) + '\n')


def measure(binary, directory, shim, channels, seconds):
    directory.mkdir()
    command = [str(binary), '--sdr-pipeline-bench', '--channels', str(channels),
        '--seconds', str(seconds), '--warmup', '3', '--taps', '16',
        '--display-decimate', '1', '--fine-decimate', '24',
        '--source-kind', 'raw-fixture', '--shim', str(shim),
        '--center', '100000000', '--output', str(directory / 'pipeline.json')]
    with binary.open('rb') as executable:
        digest = hashlib.file_digest(executable, 'sha256').hexdigest()
    save(directory / 'command.json', {'command': command, 'binary_sha256': digest})
    began = time.monotonic()
    rows = []
    timed_out = False
    with (directory / 'service.log').open('w') as log:
        process = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=log)
        try:
            while process.poll() is None:
                try:
                    rows.append({'elapsed': time.monotonic() - began, **resources(process.pid)})
                except (OSError, KeyError, ValueError):
                    pass
                if time.monotonic() - began > seconds + 180:
                    timed_out = True
                    break
                time.sleep(1)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    save(directory / 'resources.json', rows)
    path = directory / 'pipeline.json'
    report = json.loads(path.read_text()) if path.exists() else {}
    window = report.get('snapshots', {})
    start = window.get('window_start', {}).get('actual', {})
    end = window.get('window_end', {}).get('actual', {})
    elapsed = report.get('measurement', {}).get('elapsed_seconds', 0)
    def delta(key):
        return (end[key]['total_ns'] - start[key]['total_ns']) / 1e9 / elapsed if elapsed and key in end and key in start else None
    measured = rows[-max(2, int(seconds)):]
    cpu = None
    if len(measured) > 1:
        cpu = (measured[-1]['cpu_seconds'] - measured[0]['cpu_seconds']) / (measured[-1]['elapsed'] - measured[0]['elapsed'])
    return {'binary': str(binary), 'channels': channels, 'passed': bool(report.get('passed')),
        'exit_code': process.returncode, 'timed_out': timed_out, 'errors': report.get('errors', []),
        'measurement_seconds': elapsed, 'process_cpu_cores_approx': cpu,
        'rss_peak_kib': max((row['rss_kib'] for row in rows), default=None),
        'fine_wall_seconds_per_second': delta('fine'), 'framer_wall_seconds_per_second': delta('framer'),
        'producer_fine_handoff_seconds_per_second': delta('fine_handoff'),
        'publisher_handoff_seconds_per_second': delta('pipeline_output_send'),
        'scope': 'raw fixture, PFB/fine/IQ only; not RF, models or product capacity'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, default=ROOT / 'backend/target/release/websad')
    parser.add_argument('--channels', type=int, nargs='+', default=[8, 32])
    parser.add_argument('--seconds', type=float, default=20)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    if args.seconds < 10 or any(count < 1 or count > 128 for count in args.channels):
        parser.error('use at least 10 seconds and 1..128 channelizer lanes')
    destination = args.artifacts.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    shim = destination / 'capture.so'
    subprocess.run(['c++', '-O3', '-std=c++17', '-shared', '-fPIC', '-pthread',
        str(ROOT / 'tools/bench/eight_channel_capture.cpp'), '-o', str(shim)], check=True)
    results = []
    for count in args.channels:
        for name, binary in [('baseline', args.baseline), ('candidate', args.candidate)]:
            result = measure(binary.resolve(strict=True), destination / f'{name}-{count}', shim, count, args.seconds)
            results.append(result)
            save(destination / 'comparison.json', results)
            print(json.dumps(result), flush=True)
    raise SystemExit(0 if all(row['passed'] for row in results if row['binary'] == str(args.candidate.resolve())) else 1)


if __name__ == '__main__':
    main()
