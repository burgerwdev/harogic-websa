#!/usr/bin/env python3
"""Observe an existing SDR service using HTTP GET only; never arms capture.

No WebSocket subscriptions, configuration changes, service starts or device opens.
Keep a browser/authorized recording consumer running separately during RF tests.
The report is observation evidence, not an RF or physical-speaker verdict.
"""
from __future__ import annotations

import argparse
import datetime
import json
import math
import os
import time
import urllib.parse
import urllib.request
from pathlib import Path


def get(origin, path):
    # Refuse redirects: observation must stay on the explicitly selected service.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None
    with urllib.request.build_opener(NoRedirect).open(origin + path, timeout=8) as response:
        return json.load(response)


def sample(origin, expected):
    snapshot = get(origin, '/api/sdr/receivers')
    if snapshot.get('service_id') != expected:
        raise ValueError(f"service identity mismatch: expected {expected}, got {snapshot.get('service_id')}")
    state = get(origin, '/api/state')
    actual = state.get('sdr', {}).get('actual', {})
    capture = actual.get('iqs_capture', {})
    observation = {
        'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'service_id': snapshot['service_id'], 'capture': snapshot['capture'],
        'connected': state.get('connected'), 'mode': state.get('mode'),
        'device': state.get('device'), 'config_version': state.get('config_version'),
        'last_error': state.get('last_error'), 'status_warning': state.get('status_warning'),
        'actual': {key: actual.get(key) for key in [
            'capture_center', 'capture_bandwidth', 'iq_rate', 'ddc_rate', 'input_samples',
            'ddc_error_count', 'ddc_error_last', 'channel_clocks', 'channel_registry']},
        'iqs': {key: capture.get(key) for key in [
            'packets', 'skips', 'fail_calls', 'last_skip_status', 'queue_full',
            'recovery_stall_ms', 'queue_wait_max_ms', 'device_seconds', 'host_seconds']},
        'receivers': [{key: receiver.get(key) for key in [
            'receiver_uid', 'display_number', 'revision', 'confirmed', 'state', 'stream', 'clock', 'fault']}
            for receiver in snapshot.get('receivers', [])],
        'recordings': snapshot.get('recordings', []),
        'loadavg': list(os.getloadavg()),
    }
    advertised = snapshot['capture']
    observation['capture_matches_last_actual'] = all(
        advertised.get(key) is not None and actual.get(actual_key) is not None
        and math.isclose(advertised[key], actual[actual_key], rel_tol=1e-10, abs_tol=1e-6)
        for key, actual_key in [('center_hz', 'capture_center'),
                                ('usable_bandwidth_hz', 'capture_bandwidth'), ('input_rate_hz', 'iq_rate')])
    return observation, {'snapshot': snapshot, 'state': state}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--origin', required=True)
    parser.add_argument('--expect-service-id', required=True, help='Read from GET /api/sdr/receivers first')
    parser.add_argument('--seconds', type=float, default=0, help='0 takes one observation; use 300 for a five minute window')
    parser.add_argument('--interval', type=float, default=5)
    parser.add_argument('--artifacts', type=Path, required=True, help='New directory; existing paths are refused')
    args = parser.parse_args()
    url = urllib.parse.urlsplit(args.origin)
    if url.scheme not in ('http', 'https') or not url.hostname or url.path not in ('', '/') or url.query or url.fragment or url.username or url.password:
        parser.error('origin must be an HTTP(S) origin without a path, credentials, query or fragment')
    if not math.isfinite(args.seconds) or args.seconds < 0 or not math.isfinite(args.interval) or not 0.2 <= args.interval <= 60:
        parser.error('seconds must be finite and nonnegative; interval must be between 0.2 and 60 seconds')
    args.artifacts.mkdir(parents=True, exist_ok=False)
    started = time.monotonic()
    count = progressing = changes = 0
    previous = None
    error = None
    all_running = True
    try:
        with (args.artifacts / 'observations.jsonl').open('w') as output:
            while True:
                row, raw = sample(args.origin.rstrip('/'), args.expect_service_id)
                row['elapsed_seconds'] = time.monotonic() - started
                if previous:
                    boundary = (row['capture'].get('epoch'), [r.get('stream') for r in row['receivers']]) != (
                        previous['capture'].get('epoch'), [r.get('stream') for r in previous['receivers']])
                    changes += int(boundary)
                    row['identity_changed'] = boundary
                    current_samples, old_samples = row['actual']['input_samples'], previous['actual']['input_samples']
                    row['input_samples_advanced'] = None if current_samples is None or old_samples is None or boundary else current_samples > old_samples
                    progressing += int(row['input_samples_advanced'] is True)
                all_running &= row['capture'].get('state') == 'running'
                if not count:
                    (args.artifacts / 'initial.json').write_text(json.dumps(raw, indent=2) + '\n')
                output.write(json.dumps(row, separators=(',', ':')) + '\n'); output.flush()
                (args.artifacts / 'final.json').write_text(json.dumps(raw, indent=2) + '\n')
                count += 1
                previous = row
                remaining = args.seconds - (time.monotonic() - started)
                if remaining <= 0:
                    break
                time.sleep(min(args.interval, remaining))
    except (OSError, ValueError, KeyboardInterrupt) as failure:
        error = str(failure) or type(failure).__name__
    report = {'origin': args.origin, 'expected_service_id': args.expect_service_id,
              'actions': 'HTTP GET only; no capture consumer or device access',
              'requested_seconds': args.seconds, 'elapsed_seconds': time.monotonic() - started,
              'observations': count, 'all_observed_capture_states_running': all_running if count else None,
              'advancing_input_sample_intervals': progressing, 'identity_change_intervals': changes,
              'error': error, 'rf_signal_and_physical_audio': 'not evaluated'}
    (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return 2 if error else 0


if __name__ == '__main__':
    raise SystemExit(main())
