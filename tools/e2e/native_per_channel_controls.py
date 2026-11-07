#!/usr/bin/env python3
"""Real browser mode controls and applied receipts on an owned fixture service."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import struct
import time
from pathlib import Path

from native_eight_channel_workflow import setup
from native_multichannel import api
from native_multichannel_recording import until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready
from websockets.sync.client import connect


def audio_format(origin, uid, rate, channels):
    trace = {'frames': 0, 'samples': 0, 'energy': 0.0, 'actual_rate': rate, 'channels': channels}
    previous = None
    with connect(origin.replace('http://', 'ws://') + '/ws/audio?receiver_uid=' + uid) as socket:
        deadline = time.monotonic() + 10
        while trace['samples'] < rate:
            frame = socket.recv(timeout=max(0.01, deadline - time.monotonic()))
            if not isinstance(frame, bytes) or not frame.startswith(b'AUD2'):
                continue
            count = struct.unpack_from('<I', frame, 12)[0]
            assert struct.unpack_from('<d', frame, 16)[0] == rate
            assert struct.unpack_from('<I', frame, 56)[0] == channels
            sequence = struct.unpack_from('<I', frame, 8)[0]
            index = struct.unpack_from('<Q', frame, 64)[0]
            if previous:
                assert (sequence, index) == (previous[0] + 1, previous[1] + previous[2])
            previous = sequence, index, count
            values = struct.unpack_from(f'<{count * channels}f', frame, 96)
            assert all(math.isfinite(value) for value in values)
            trace['energy'] += sum(value * value for value in values)
            trace['samples'] += count
            trace['frames'] += 1
    assert trace['energy'] > 1e-6, trace
    return trace


def gate(origin, artifacts):
    initial = setup(origin, modes=['am'] * 8)
    uids = [row['receiver_uid'] for row in initial['receivers']]
    healthy = [{key: row[key] for key in ['receiver_uid', 'revision', 'confirmed', 'stream']} for row in initial['receivers'][1:7]]
    report = {'source': 'fixture; browser clicks, not RF', 'cases': []}

    def row(index):
        return api(origin)['receivers'][index]

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1600, 'height': 1100})
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        try:
            page.goto(origin)
            wait_ui_ready(page, 'sdr')
            page.locator('[data-action=sdr-workbench]').click()
            view = page.locator('[data-view-id=overview-follow]')
            expect(view.locator('.mc-row')).to_have_count(8)

            def select(index):
                view.locator(f'.mc-row[data-uid="{uids[index]}"] [data-action=select]').click()
                expect(view).to_have_attribute('data-receiver-uid', uids[index])
                expect(view.locator('[data-config]')).to_have_attribute('data-revision', row(index)['revision'])

            def apply(index, label):
                previous = row(index)['revision']
                view.locator('[data-config] button[type=submit]').click()
                until(lambda: row(index)['revision'] != previous, timeout=15)
                receipt = until(lambda: next((r for r in row(index)['control_receipts']
                    if r['revision'] == row(index)['revision'] and r['status'] == 'applied'), None), timeout=15)
                assert receipt['receiver_uid'] == uids[index]
                assert receipt['boundary']['precision'] == 'block'
                expect(view.locator('[data-config]')).to_have_attribute('data-revision', row(index)['revision'])
                report['cases'].append({'case': label, 'receipt': receipt, 'confirmed': row(index)['confirmed']})

            for index in [0, 7]:
                select(index)
                view.locator('[name=mode-am_sync]').check()
                apply(index, f'C{index + 1} AM sync')
                assert row(index)['confirmed']['processors'][0]['am_sync'] is True
            for _ in range(7):
                page.locator('[data-action=sdr-channel-next]').click()
            expect(page.locator('#sdr-current-channel')).to_have_text('C8')
            expect(page.locator('#sdr-mode-settings')).to_have_count(0)
            expect(page.locator('#sdr-settings [data-mode-controls]')).to_have_count(0)
            main = page.locator('#sdr-audio-rate-row')
            expect(main.locator('[data-sdr-audio-rate]')).to_have_count(3)
            expect(main.locator('[data-sdr-audio-rate="48000"]')).to_have_attribute('aria-pressed', 'true')
            previous = row(7)['revision']
            main.locator('[data-sdr-audio-rate="24000"]').click()
            page.locator('[data-action=sdr-channel-prev]').click()
            until(lambda: row(7)['revision'] != previous)
            assert row(7)['confirmed']['processors'][0]['audio_rate'] == 24000
            assert row(7)['confirmed']['processors'][0]['am_sync'] is True
            assert row(6)['revision'] == initial['receivers'][6]['revision']
            report['cases'].append({'case': 'Main C8 audio rate followed immediately by selection change', 'receiver': row(7)})

            select(7)
            view.locator('[name=mode]').select_option('cw')
            view.locator('[name=mode-pitch]').fill('1350')
            apply(7, 'C8 CW pitch')
            view.locator('[name=mode]').select_option('wfm')
            view.locator('[name=mode-deemph_us]').fill('75')
            view.locator('[name=mode-stereo]').uncheck()
            view.locator('[name=mode-audio_rate]').fill('24000')
            apply(7, 'C8 WFM mono, deemphasis and audio rate')
            assert row(7)['confirmed']['processors'][0]['stereo'] is False
            report['actual_audio'] = audio_format(origin, uids[7], 24000, 1)
            view.locator('[name=mode]').select_option('sstv')
            # This committed SSTV replay is 8 kS/s. Select its explicit IQ plan;
            # this is a fixture constraint, not a reduced RF capture regression.
            view.locator('[data-action=advanced-settings]').click()
            iq = view.locator('[data-section=iq]')
            if not iq.evaluate('element=>element.open'):
                iq.locator('summary').click()
            view.locator('[name=bandwidth]').fill('6000')
            view.locator('[name=minimum-rate]').fill('8000')
            view.locator('[name=mode-sstv_mode]').select_option('1')
            apply(7, 'C8 fixed SSTV mode')
            view.locator('[name=mode]').select_option('radiofax')
            for field, value in {'acquisition': 'manual', 'ioc': '576', 'lpm': '120', 'modulation': 'am', 'polarity': 'inverted', 'clock_recovery': 'off'}.items():
                view.locator(f'[name=mode-radiofax_{field}]').select_option(value)
            view.locator('[name=mode-radiofax_phase_pixels]').fill('3.5')
            view.locator('[name=mode-radiofax_clock_ppm]').fill('12.5')
            apply(7, 'C8 full Radiofax controls')
            view.locator('[data-action=radiofax_new_page]').click()
            action = until(lambda: next((r for r in row(7)['control_receipts'] if r.get('action_id') and r['status'] == 'applied'), None))
            assert sum(r.get('action_id') == action['action_id'] for r in row(7)['control_receipts']) == 1
            view.locator('[data-action=radiofax_reset_correction]').click()
            until(lambda: row(7)['confirmed']['processors'][0]['radiofax_clock_ppm'] == 0)
            expect(view.locator('[data-config]')).to_have_attribute('data-revision', row(7)['revision'])
            report['cases'].append({'case': 'Radiofax actions applied once with source boundaries', 'receipts': row(7)['control_receipts']})
            # A legitimate requested model with an unsupported PCM rate must
            # fail explicitly; the UI must never substitute Wiener or claim ready.
            view.locator('[name=mode]').select_option('am')
            view.locator('[name=mode-audio_rate]').fill('24000')
            view.locator('[name=nr]').check()
            view.locator('[name=nr-algorithm]').select_option('dfn')
            previous = row(7)['revision']
            view.locator('[data-config] button[type=submit]').click()
            until(lambda: row(7)['revision'] != previous)
            def published_failure():
                current = row(7)
                return current if current.get('processing_state') == 'failed' and current.get('reception') is not None else None
            failed = until(published_failure)
            failure_reason = 'selected dfn model preparation failed: rate 24000'
            assert failed['processing_fault']['error'] == failure_reason
            assert failed['confirmed']['processors'][0]['nr_algorithm'] == 'dfn'
            assert failed['reception']['state'] == 'stopped'
            assert failed['reception']['manual_recovery_allowed'] is False
            expect(view.locator('[data-config]')).to_have_attribute('data-revision', failed['revision'])
            live = view.locator('[data-live-state=fault]')
            expect(live).to_be_visible()
            expect(live.locator('[role=status]')).to_have_text('Reception fault')
            diagnostics = live.locator('[data-section=live-diagnostics]')
            if not diagnostics.evaluate('element=>element.open'):
                diagnostics.locator('summary').click()
            expect(diagnostics.locator('pre')).to_be_visible()
            expect(diagnostics.locator('pre')).to_contain_text(failure_reason)
            expect(view.locator('[data-action=recover-processing]')).to_have_count(0)
            view.locator('[data-action=tab-output]').click()
            expect(view.locator('[data-processing-runtime]')).to_contain_text(failure_reason)
            expect(view.locator('[data-action=recover-processing]')).to_have_count(0)
            page.locator('[data-action=sdr-channel-next]').click()
            expect(page.locator('#sdr-current-channel')).to_have_text('C8')
            expect(page.locator('#sdr-mode-settings')).to_have_count(0)
            expect(main.locator('[data-sdr-audio-rate="24000"]')).to_have_attribute('aria-pressed', 'true')
            # An internal model/rate error is not a recoverable interruption.
            # A direct retry must be rejected without changing the failed owner;
            # repair the selected configuration before requesting new processing.
            rejected = api(origin, payload={'type': 'configure', 'request_id': 'invalid-rate-recovery',
                'service_id': initial['service_id'], 'receiver_uid': uids[7],
                'expected_revision': failed['revision'], 'config': failed['confirmed'], 'recover': True})
            assert rejected['request_id'] == 'invalid-rate-recovery'
            assert rejected['fault'] is not None
            assert 'requires source repair' in rejected['fault']['message'], rejected['fault']
            after_rejection = row(7)
            for key in ['receiver_uid', 'revision', 'confirmed', 'stream']:
                assert after_rejection[key] == failed[key], key
            assert after_rejection['processing_runtime']['processing_generation'] == failed['processing_runtime']['processing_generation']
            assert after_rejection['processing_state'] == 'failed'
            view.locator('[data-action=tab-receive]').click()
            expect(view.locator('[data-config]')).to_have_attribute('data-revision', failed['revision'])
            view.locator('[name=mode-audio_rate]').fill('48000')
            previous = row(7)['revision']
            view.locator('[data-config] button[type=submit]').click()
            until(lambda: row(7)['revision'] != previous)
            expect(view.locator('[data-config]')).to_have_attribute('data-revision', row(7)['revision'])
            def effective_model():
                current = row(7)
                runtime = current.get('processing_runtime') or {}
                observed = runtime.get('observed_receiver') or {}
                return current if current.get('processing_state') == 'active' and observed.get('nr_effective') == 'dfn' else None
            recovered = until(effective_model, timeout=20)
            assert recovered['processing_runtime']['processing_generation'] != failed['processing_runtime']['processing_generation']
            assert recovered['receiver_uid'] == failed['receiver_uid']
            assert recovered['confirmed']['processors'][0]['audio_rate'] == 48000
            assert recovered['confirmed']['processors'][0]['nr_algorithm'] == 'dfn'
            expect(view.locator('[data-live-state=healthy]')).to_be_visible()
            report['cases'].append({'case': 'Selected-model failure, denied invalid retry and configuration repair',
                'failed': failed, 'rejected_retry': rejected['fault'], 'after_rejection': after_rejection, 'recovered': recovered})
            report['recovered_pcm'] = audio_format(origin, uids[7], 48000, 1)
            final = api(origin)
            assert [{key: r[key] for key in ['receiver_uid', 'revision', 'confirmed', 'stream']} for r in final['receivers'][1:7]] == healthy
            view.locator('[data-close]').click()
            page.locator('[data-action=sdr-channel-next]').click()
            expect(page.locator('#sdr-current-channel')).to_have_text('C1')
            page.locator('[data-sdr-audio-rate="12000"]').click()
            until(lambda: row(0)['confirmed']['processors'][0]['audio_rate'] == 12000)
            expect(main.locator('[data-sdr-audio-rate="12000"]')).to_have_attribute('aria-pressed', 'true')
            report['12khz_pcm'] = audio_format(origin, uids[0], 12000, 1)
            page.locator('[data-action=preset]').click()
            def preset_audio():
                current = api(origin)
                return current if current['receivers'][0]['confirmed']['processors'][0]['audio_rate'] == 48000 else None
            preset = until(preset_audio)
            expect(main.locator('[data-sdr-audio-rate="48000"]')).to_have_attribute('aria-pressed', 'true')
            report['preset_audio_rate'] = preset['receivers'][0]['confirmed']['processors'][0]['audio_rate']
            page.reload(); wait_ui_ready(page, 'sdr')
            expect(main.locator('[data-sdr-audio-rate="48000"]')).to_have_attribute('aria-pressed', 'true')
            expect(page.locator('#sdr-mode-settings')).to_have_count(0)
            assert not errors, errors
            page.screenshot(path=str(artifacts / 'desktop.png'))
            page.set_viewport_size({'width': 420, 'height': 900})
            page.screenshot(path=str(artifacts / 'mobile.png'))
            report['browser_errors'] = errors
            return report
        except BaseException:
            page.screenshot(path=str(artifacts / 'failure.png'))
            (artifacts / 'failure-state.json').write_text(json.dumps(api(origin), indent=2))
            raise
        finally:
            browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=False)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        process, origin = start_service(free_port(), log=log)
        try:
            report = gate(origin, args.artifacts)
            report['backend_sha256'] = hashlib.sha256(Path(f'/proc/{process.pid}/exe').read_bytes()).hexdigest()
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: C1/C8 full controls, main/workbench and DSP applied receipts')
        finally:
            stop(process)


if __name__ == '__main__':
    main()
