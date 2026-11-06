#!/usr/bin/env python3
"""Run owned receiver tests under affinity and inherited fractional cgroup quotas."""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path


def nested(binary: str, selected: str) -> None:
    group = next(line.removeprefix('0::') for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    parent = Path('/sys/fs/cgroup') / group.lstrip('/')
    child = parent / 'receiver-test'
    child.mkdir()
    (child / 'cgroup.procs').write_text(str(os.getpid()))
    (parent / 'cgroup.subtree_control').write_text('+cpu')
    (child / 'cpu.max').write_text('50000 100000')
    allowed = sorted(os.sched_getaffinity(0))[:4]
    os.sched_setaffinity(0, allowed)
    print(json.dumps({'nested_parent_cpu_max': (parent / 'cpu.max').read_text().strip(),
        'child_cpu_max': (child / 'cpu.max').read_text().strip(), 'affinity': allowed}), flush=True)
    os.execv(binary, [binary, selected, '--test-threads=1', '--nocapture'])


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test-binary', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path)
    parser.add_argument('--filter', default='resource_')
    parser.add_argument('--inside-nested', action='store_true')
    args = parser.parse_args()
    binary = str(args.test_binary.resolve())
    if args.inside_nested:
        nested(binary, args.filter)
        return
    if args.artifacts is None:
        parser.error('--artifacts required')
    args.artifacts.mkdir(parents=True, exist_ok=False)
    allowed = sorted(os.sched_getaffinity(0))
    tests = [binary, args.filter, '--test-threads=1', '--nocapture']
    report = []
    for mode in ['normal', 'single', 'dual', 'half_quota', 'nested_quota']:
        unit = f'websa-parity-{mode.replace("_", "-")}-{os.getpid()}'
        if mode == 'normal':
            command = tests
        elif mode in ['single', 'dual']:
            cpus = allowed[:1 if mode == 'single' else 2]
            command = ['taskset', '-c', ','.join(map(str, cpus)), *tests]
        else:
            common = ['systemd-run', '--user', '--wait', '--pipe', '--collect', '--quiet',
                '--unit', unit, '--working-directory', str(Path.cwd()), '-p', 'MemoryMax=2G']
            if mode == 'half_quota':
                command = [*common, '-p', 'CPUQuota=50%', '--', 'taskset', '-c',
                    ','.join(map(str, allowed[:4])), *tests]
            else:
                command = [*common, '-p', 'CPUQuota=150%', '-p', 'Delegate=cpu', '--',
                    sys.executable, str(Path(__file__).resolve()), '--inside-nested',
                    '--test-binary', binary, '--filter', args.filter]
        began = time.monotonic()
        try:
            with (args.artifacts / f'{mode}.log').open('w') as log:
                process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=90)
            row = {'mode': mode, 'command': command, 'returncode': process.returncode,
                'wall_seconds': time.monotonic() - began}
        except subprocess.TimeoutExpired:
            row = {'mode': mode, 'command': command, 'error': 'owned test exceeded 90 seconds'}
        finally:
            if mode in ['half_quota', 'nested_quota']:
                # Only the transient unit created above; never stop other services.
                subprocess.run(['systemctl', '--user', 'stop', unit], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        report.append(row)
        (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(row), flush=True)
    if any(row.get('returncode') != 0 for row in report):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
