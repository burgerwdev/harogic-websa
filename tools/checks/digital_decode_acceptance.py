#!/usr/bin/env python3
"""Known signal -> production Receiver -> asserted protocol payload; no RF hardware.

Requires built sdr_decode and independently installed decoder/encoder providers.
FT4/WSPR reschedule generated complete slots against host UTC. GPS streams the
pinned simulator through stdin, so no multi-gigabyte fixture is retained.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import numpy as np
from scipy.signal import resample_poly

ROOT = Path(__file__).resolve().parents[2]
MODES = ('rtty', 'rtty-reverse', 'aprs', 'ft4', 'wspr', 'gps')
MESSAGES = {'ft4': 'CQ K1ABC FN42', 'wspr': 'K1ABC FN42 30',
            'aprs': '!4903.50N/07201.75W>WEBSA APRS TEST'}


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def payload_evidence(mode, report):
    data = [event['data'] for event in report['events']
            if event.get('type') == 'decoder-result' and event.get('mode') == mode]
    if mode.startswith('rtty'):
        text = ''.join(row.get('text', '') for row in data)
        assert 'CQ CQ DE WEBSA TEST 0123456789 1/2 -.:? THE QUICK BROWN FOX\n' in text, text
        return {'decoded_text': text}
    if mode in ('ft4', 'wspr'):
        rows = [row for batch in data for row in batch.get('decodes', [])]
        expected = MESSAGES[mode]
        matched = [row for row in rows if expected in str(row.get('msg', row.get('message', '')))]
        assert matched, f'No {expected!r} in decoded rows: {rows}'
        assert report['generated_slot']['maximum_pacing_lateness_seconds'] <= 0.1
        return {'matching_decodes': matched, 'generated_slot': report['generated_slot']}
    if mode == 'aprs':
        rows = [row for batch in data for row in batch.get('frames', [])]
        matched = [row for row in rows if row.get('from') == 'N0CALL-1' and row.get('to') == 'APRS'
                   and 'WEBSA APRS TEST' in row.get('summary', '')
                   and abs(row.get('lat', 0) - 49.058333) < 0.00001
                   and abs(row.get('lon', 0) + 72.029167) < 0.00001]
        assert matched, f'No known AX.25 payload in frames: {rows}'
        return {'matching_frames': matched}
    fixes = [row for batch in data if batch.get('table') == 'pvt'
             for row in batch.get('rows', []) if row.get('valid') and not row.get('stale')]
    matched = [row for row in fixes if abs(row['lat_deg'] - 25.2) < 0.002
               and abs(row['lon_deg'] - 55.36) < 0.002 and row['valid_sats'] >= 4]
    nav = [row for batch in data if batch.get('table') == 'navigation' for row in batch.get('rows', [])]
    assert matched, f'No known-location PVT solution; {len(fixes)} valid fixes'
    assert nav, 'No decoded navigation subframes'
    assert all(batch.get('malformed_monitor_packets', 0) == 0 for batch in data)
    return {'valid_fixes': len(matched), 'last_fix': matched[-1], 'navigation_rows': len(nav)}


def run_mode(args, mode):
    directory = args.artifacts / mode
    directory.mkdir(parents=True, exist_ok=True)
    output = directory / 'receiver.json'
    iq = directory / 'source.cf32'
    commands = []
    provenance = {}
    if mode.startswith('rtty'):
        command = [sys.executable, str(ROOT / 'tools/fixtures/rtty_iq.py'), str(iq), '--repeat', '1']
        if mode.endswith('reverse'):
            command.append('--reverse')
        subprocess.run(command, check=True, timeout=60)
        commands.append(command)
        provenance = json.loads(iq.with_suffix(iq.suffix + '.json').read_text())
        rate, bandwidth, center = 48000, 3400, 14_080_000
    elif mode != 'gps':
        period = {'aprs': 3, 'ft4': 7.5, 'wspr': 120}[mode]
        source = directory / 'encoded.cf32'
        command = [str(args.encoder), str(source), mode, MESSAGES[mode],
                   '0' if mode == 'aprs' else '1500', str(period)]
        encoded = subprocess.run(command, check=True, capture_output=True, text=True, timeout=60)
        commands.append(command)
        provenance = json.loads(encoded.stdout)
        rate, bandwidth = 250000, 20000 if mode == 'aprs' else 6000
        center = 144800000 if mode == 'aprs' else 14080000 if mode == 'ft4' else 14095600
        samples = np.fromfile(source, dtype='<c8')
        assert samples.size == round(provenance['rate'] * period)
        assert np.isfinite(samples).all() and np.any(samples)
        samples /= np.max(np.abs(samples))
        divisor = int(np.gcd(rate, int(provenance['rate'])))
        converted = resample_poly(samples, rate // divisor, int(provenance['rate']) // divisor)
        repeats = 3 if mode in ('ft4', 'aprs') else 1
        with iq.open('wb') as stream:
            for _ in range(repeats):
                converted.astype('<c8', copy=False).tofile(stream)
        provenance['encoder_sha256'] = sha256(args.encoder)
    else:
        rate, bandwidth, center = 2600000, 2046000, 1575420000
        provenance = {'generator': 'pinned gps-sdr-sim via pluto_tx.py --stdout',
                      'duration_seconds': args.gps_seconds, 'expected_position': [25.2, 55.36, 10],
                      'source_manifest': json.loads((args.gps_source / 'provenance.json').read_text()),
                      'source_hashes': (args.gps_source / 'SHA256SUMS').read_text()}
    decode = [str(args.decoder), '--input', '/dev/stdin' if mode == 'gps' else str(iq),
              '--mode', mode, '--input-rate', str(rate), '--bandwidth', str(bandwidth),
              '--center-hz', str(center), '--realtime', '--report-json', str(output)]
    if mode in ('ft4', 'wspr'):
        decode.append('--generated-utc-slot')
    environment = dict(os.environ)
    if mode == 'gps':
        environment['WEBSA_GNSS_PRE_2009_FILE'] = 'false'
    if iq.exists():
        provenance.update(iq_sha256=sha256(iq), iq_bytes=iq.stat().st_size, rate_hz=rate)
    with (directory / 'decoder.log').open('w') as log:
        if mode == 'gps':
            generate = [sys.executable, str(ROOT / 'tools/pluto_tx.py'), 'gps', '--stdout', '--no-setup',
                        '--source-dir', str(args.gps_source), '--format', 'cf32',
                        '--duration', str(args.gps_seconds), '--rate', str(rate)]
            commands.append(generate)
            with subprocess.Popen(generate, stdout=subprocess.PIPE, stderr=log) as producer:
                try:
                    subprocess.run(decode, stdin=producer.stdout, stdout=log, stderr=log,
                                   env=environment, check=True, timeout=args.gps_seconds * 3 + 90)
                    producer.stdout.close()
                    assert producer.wait(timeout=15) == 0, 'GPS signal generator failed'
                finally:
                    if producer.poll() is None:
                        producer.terminate()
        else:
            subprocess.run(decode, stdout=log, stderr=log, env=environment, check=True,
                           timeout=360 if mode == 'wspr' else 120)
    commands.append(decode)
    report = json.loads(output.read_text())
    evidence = payload_evidence(mode, report)
    expected_samples = args.gps_seconds * rate if mode == 'gps' else iq.stat().st_size // 8
    assert report['input']['complex_samples'] == expected_samples
    result = {'passed': True, 'commands': commands, 'fixture': provenance,
              'input_complex_samples': expected_samples, 'evidence': evidence}
    (directory / 'acceptance.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--modes', nargs='+', choices=MODES, default=MODES)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-digital-acceptance'))
    parser.add_argument('--decoder', type=Path, default=ROOT / 'backend/target/release/sdr_decode')
    cache = Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'harogic-websa'
    parser.add_argument('--encoder', type=Path, default=cache / 'pluto-digital/bin/pluto_digital_source')
    parser.add_argument('--gps-source', type=Path, default=cache / 'gps-sdr-sim/28ca29a6719475195e3aabd5930c4ed02d67190f')
    parser.add_argument('--gps-seconds', type=int, default=90)
    args = parser.parse_args()
    args.artifacts = args.artifacts.resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    results = {}
    for mode in args.modes:
        started = time.monotonic()
        print(f'{mode}: generating known IQ and decoding through Receiver…', flush=True)
        try:
            results[mode] = run_mode(args, mode)
        except (AssertionError, OSError, ValueError, subprocess.SubprocessError) as error:
            results[mode] = {'passed': False, 'error': str(error)}
        results[mode]['elapsed_seconds'] = round(time.monotonic() - started, 3)
        print(f'{mode}: {"PASS" if results[mode]["passed"] else "FAIL"}', flush=True)
        (args.artifacts / 'report.json').write_text(json.dumps(results, indent=2) + '\n')
    return 0 if all(row['passed'] for row in results.values()) else 1


if __name__ == '__main__':
    raise SystemExit(main())
