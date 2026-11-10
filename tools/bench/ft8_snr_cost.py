#!/usr/bin/env python3
"""Compare precompiled FT8 test executables, serially; never invoke a compiler.

Each directory contains executables named ft8_snr_robust and ft8_reference.
Copy the baseline before changing the estimator, then compile/copy the candidate.
Keep other builds/tests stopped. The normal hardware service can remain running.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
import re
import resource
import statistics
import subprocess
import time
from pathlib import Path


def run(executable: Path, arguments: list[str]) -> tuple[str, dict]:
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.perf_counter()
    result = subprocess.run([str(executable.resolve()), *arguments], check=True,
                            capture_output=True, text=True, timeout=60)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    return result.stdout, dict(process_wall_seconds=time.perf_counter() - start,
                              user_seconds=after.ru_utime-before.ru_utime,
                              system_seconds=after.ru_stime-before.ru_stime)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before-dir', type=Path, required=True)
    parser.add_argument('--after-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    samples = {metric: {version: [] for version in ['before', 'after']}
               for metric in ['meter', 'decoder']}
    hashes = {}
    for version, directory in [('before', args.before_dir), ('after', args.after_dir)]:
        hashes[version] = {name: hashlib.sha256((directory/name).read_bytes()).hexdigest()
                           for name in ['ft8_snr_robust', 'ft8_reference']}
    for metric in samples:
        for trial in range(7):
            # Alternate order to reduce directional temperature/clock drift.
            for version in (['before', 'after'] if trial % 2 == 0 else ['after', 'before']):
                directory = args.before_dir if version == 'before' else args.after_dir
                if metric == 'meter':
                    output, stats = run(directory/'ft8_snr_robust',
                        ['--ignored', '--exact', 'meter_timing', '--nocapture'])
                    times = [float(value) for value in re.findall(r'SNR_TIMING,\d+,([\d.eE+-]+)', output)]
                    assert len(times) == 7, output
                    stats['wall_seconds_per_call'] = statistics.median(times)
                    # Five warmups + seven blocks of 32 meter calls. Process CPU
                    # includes the identical waveform generation and test harness.
                    stats['cpu_seconds_per_call'] = (stats['user_seconds']+stats['system_seconds'])/229
                else:
                    output, stats = run(directory/'ft8_reference',
                        ['--exact', 'a_slot_decode_stays_well_inside_a_slot', '--nocapture'])
                    match = re.search(r'FT8 slot decode: ([\d.]+) s', output)
                    assert match, output
                    stats['wall_seconds_per_call'] = float(match[1])
                    stats['cpu_seconds_per_call'] = stats['user_seconds']+stats['system_seconds']
                samples[metric][version].append(stats)
            print(f'{metric}: paired trial {trial+1}/7 complete', flush=True)
    summary = {}
    for metric, rows in samples.items():
        medians = {version: {field: statistics.median(row[field] for row in values)
                            for field in ['wall_seconds_per_call', 'cpu_seconds_per_call']}
                   for version, values in rows.items()}
        ratios = {field: medians['after'][field]/medians['before'][field]
                  for field in medians['before']}
        limit = 1.25 if metric == 'meter' else 1.10
        summary[metric] = dict(medians=medians, after_over_before=ratios,
                               acceptance_ratio=limit, passed=all(r<=limit for r in ratios.values()))
    document = dict(machine=platform.platform(), executable_sha256=hashes,
                    compilation_in_timed_region=False, trials=samples, summary=summary,
                    cpu_scope='child process user+system CPU, including identical test/input setup')
    args.output.write_text(json.dumps(document, indent=2)+'\n')
    assert all(row['passed'] for row in summary.values()), summary


if __name__ == '__main__':
    main()
