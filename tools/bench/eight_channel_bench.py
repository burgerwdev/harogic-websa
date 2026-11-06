#!/usr/bin/env python3
"""Measure the full-band product pipeline separately from builds and browser gates.

The default raw ADC adapter exercises capture ownership, PFB, fine filtering and
native receivers at 62.5 MSps. It never opens a radio and cannot certify RF reception.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def resources(pid):
    path = Path('/proc') / str(pid)
    status = dict(line.split(':', 1) for line in (path / 'status').read_text().splitlines() if ':' in line)
    stat = (path / 'stat').read_text().split(') ', 1)[1].split()
    ticks = os.sysconf('SC_CLK_TCK')
    threads = []
    for task in (path / 'task').iterdir():
        try:
            text = (task / 'stat').read_text()
            name, fields = text.split('(', 1)[1].rsplit(') ', 1)
            values = fields.split()
            threads.append({'name': name, 'tid': int(task.name), 'cpu_seconds': (int(values[11]) + int(values[12])) / ticks,
                'processor': int(values[36])})
        except (FileNotFoundError, ProcessLookupError):
            pass
    machine = {}
    for pattern in ['devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq',
            'devices/system/cpu/cpu[0-9]*/thermal_throttle/*_throttle_count',
            'class/thermal/thermal_zone*/temp']:
        for sensor in Path('/sys').glob(pattern):
            try:
                machine[str(sensor)] = int(sensor.read_text().strip())
            except (OSError, ValueError):
                pass
    return {'cpu_seconds': (int(stat[11]) + int(stat[12])) / ticks,
        'rss_kib': int(status['VmRSS'].split()[0]), 'peak_rss_kib': int(status['VmHWM'].split()[0]),
        'swap_kib': int(status['VmSwap'].split()[0]), 'major_faults': int(stat[9]),
        'pressure': {kind: (Path('/proc/pressure') / kind).read_text()
            for kind in ['cpu', 'memory', 'io']},
        'threads': threads, 'machine_counters': machine}


def fine_busy_fraction(before, after, seconds):
    """A parallel sum is not a serial-stage utilization measurement."""
    if 'fine_owners' not in after:
        return sum(after[key]['total_ns'] - before[key]['total_ns']
            for key in ['fine', 'framer']) / 1e9 / seconds
    old = {(row['channel_id'], row['epoch']): row
        for row in before.get('fine_owners', {}).get('channels', [])}
    fractions = [sum(row[key]['total_ns'] - old[(row['channel_id'], row['epoch'])][key]['total_ns']
        for key in ['fine', 'framer']) / 1e9 / seconds
        for row in after['fine_owners']['channels'] if (row['channel_id'], row['epoch']) in old]
    return max(fractions, default=0.0)


def summarize(artifacts):
    report = json.loads((artifacts / 'pipeline.json').read_text())
    if 'snapshots' not in report:
        return {'passed': False, 'errors': report.get('errors', ['pipeline failed before capture'])}
    before = report['snapshots']['window_start']['actual']
    after = report['snapshots']['window_end']['actual']
    seconds = report['measurement']['elapsed_seconds']
    duration = max(seconds, 1e-9)
    def delta(section):
        return (after[section]['total_ns'] - before[section]['total_ns']) / 1e9
    stages = {
        'fine_and_framing': fine_busy_fraction(before, after, duration),
        'capture_producer': (delta('pfb_submit') + delta('fine_handoff')) / duration,
        'coarse_per_worker': (after['pool_timing']['process']['total_ns'] - before['pool_timing']['process']['total_ns']) / 4e9 / duration,
    }
    start = report['warmup']['receiver_workers']['workers']
    stop = report['measurement']['receiver_workers']['workers']
    for old, new in zip(start, stop, strict=True):
        stages[f"receiver_{new['worker']}_{new.get('role', 'realtime')}"] = (new['busy_ns'] - old['busy_ns']) / 1e9 / duration
    resources = [json.loads(line) for line in (artifacts / 'resources.jsonl').read_text().splitlines()]
    windows = []
    for offset in range(0, int(seconds), 300):
        # Align the process observations to the end of the measured pipeline.
        end = resources[-1]['elapsed'] - report['stop_seconds'] - seconds + offset
        rows = [row for row in resources if end <= row['elapsed'] < end + 300]
        if rows:
            windows.append({'measurement_start_seconds': offset, 'rss_min_kib': min(r['rss_kib'] for r in rows),
                'rss_max_kib': max(r['rss_kib'] for r in rows), 'observations': len(rows)})
    if not resources:
        raise RuntimeError('missing process observations')
    measured = [row for row in resources if row['elapsed'] >= resources[-1]['elapsed'] - seconds]
    first, last = measured[0], measured[-1]
    thread_start = {row['tid']: row for row in first['threads']}
    thread_fractions = [{'name': row['name'], 'tid': row['tid'],
        'busy_fraction': (row['cpu_seconds'] - thread_start[row['tid']]['cpu_seconds']) / max(last['elapsed'] - first['elapsed'], 1e-9)}
        for row in last['threads'] if row['tid'] in thread_start]
    margin = all(value <= 0.8 for value in stages.values()) and all(row['busy_fraction'] <= 0.8 for row in thread_fractions)
    return {'passed': report['passed'], 'source_kind': report['source_kind'],
        'measurement_seconds': seconds, 'stage_busy_fraction': stages, 'individual_threads': thread_fractions, 'twenty_percent_margin': margin,
        'process_rss_windows': windows, 'errors': report['errors'],
        'receiver_queue_max_wait_ns': max(r['queue_wait_max_ns'] for r in stop),
        'receiver_peak_queued_samples': [r['peak_queued_samples'] for r in stop],
        'capture_queue_residence_max_ns': after['iqs_capture']['queue_residence']['max_ns'],
        'scope': 'raw ADC fixture is separate from hardware, transport/browser and recording acceptance'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--channels', type=int, choices=[1, 3, 8, 16], required=True)
    parser.add_argument('--seconds', type=float, required=True)
    parser.add_argument('--warmup', type=float, default=5)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--shim', type=Path)
    parser.add_argument('--source-kind', choices=['raw-fixture', 'hardware'], default='raw-fixture')
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=False)
    shim = args.shim
    if shim is None:
        if args.source_kind == 'hardware':
            parser.error('hardware requires an explicit existing shim')
        shim = args.artifacts / 'capture.so'
        subprocess.run(['c++', '-O3', '-std=c++17', '-Wall', '-Wextra', '-shared', '-fPIC', '-pthread',
            f'-DSDR_BENCH_CHANNELS={max(8, args.channels)}',
            str(ROOT / 'tools/bench/eight_channel_capture.cpp'), '-o', str(shim)], check=True)
    command = [str(ROOT / 'backend/target/release/websad'), '--sdr-pipeline-bench',
        '--product-channels', 'true', '--workload', 'narrow', '--channels', str(args.channels),
        '--seconds', str(args.seconds), '--warmup', str(args.warmup), '--source-kind', args.source_kind,
        '--shim', str(shim.resolve()), '--center', '100000000', '--output', str((args.artifacts / 'pipeline.json').resolve())]
    (args.artifacts / 'command.json').write_text(json.dumps({'command': command, 'scope': args.source_kind,
        'binary_sha256': hashlib.sha256((ROOT / 'backend/target/release/websad').read_bytes()).hexdigest(),
        'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'machine': subprocess.check_output(['lscpu'], text=True)}, indent=2) + '\n')
    began = time.monotonic()
    with (args.artifacts / 'service.log').open('w') as log, (args.artifacts / 'resources.jsonl').open('w') as observations:
        process = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=log)
        try:
            while process.poll() is None:
                try:
                    observations.write(json.dumps({'elapsed': time.monotonic() - began, **resources(process.pid)}) + '\n')
                    observations.flush()
                except (FileNotFoundError, ProcessLookupError, KeyError):
                    pass
                time.sleep(1)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    summary = summarize(args.artifacts)
    (args.artifacts / 'verification.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary))
    raise SystemExit(process.returncode if process.returncode else (0 if summary['passed'] else 1))


if __name__ == '__main__':
    main()
