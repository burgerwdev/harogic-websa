#!/usr/bin/env python3
"""Exercise confirmed AM sync control and real native AUD2/am-info consumers.

Requires a built running native websad and frontend. Uses its existing AM IQ
fixture without changing fixture bytes. The Rust consumer regression separately
covers offset carrier acquisition and RF fade. Example:
  python3 tools/e2e/am_sync.py --url http://127.0.0.1:8099
"""
from __future__ import annotations

import argparse
import copy
import json
import urllib.error
import urllib.request

from playwright.sync_api import sync_playwright
from viewport_baseline import wait_async_condition


def post(origin: str, payload: dict) -> dict:
    request = urllib.request.Request(origin + '/api/config', json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def state(origin: str) -> dict:
    with urllib.request.urlopen(origin + '/api/state', timeout=5) as response:
        return json.load(response)


def configuration_and_stream(channel: dict) -> dict:
    # Live observations and control envelopes have their own checks below.
    # Keep the complete confirmed configuration, compatibility settings and
    # stream identity so unrelated changes still fail the sync-only operation.
    observations = {'level_dbfs', 'channel_dbm', 'squelch_open',
                    'processing_runtime', 'control_receipts', 'control'}
    return {key: copy.deepcopy(value) for key, value in channel.items()
            if key not in observations}


PROBE = """() => {
  const lab = window.__amSyncProbe = {audio: 0, markers: 0, gaps: [], events: [], errors: []};
  const base = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}`;
  const audio = new WebSocket(`${base}/ws/audio?channel=1`);
  const decode = new WebSocket(`${base}/ws/decode?channel=1`);
  lab.sockets = [audio, decode];
  for (const socket of lab.sockets) {
    socket.binaryType = 'arraybuffer';
    socket.onerror = () => lab.errors.push('websocket error');
  }
  let expectedSeq = 0, expectedIndex = null, identity = null;
  audio.onmessage = ({data}) => {
    if (!(data instanceof ArrayBuffer)) return;
    const d = new DataView(data);
    if (new TextDecoder().decode(new Uint8Array(data, 0, 4)) !== 'AUD2') return;
    const seq = d.getUint32(8, true), count = d.getUint32(12, true);
    const id = `${d.getUint32(32, true)}/${d.getUint32(36, true)}`;
    const index = d.getBigUint64(64, true);
    if (seq === 0) lab.markers++;
    if (seq !== expectedSeq || (expectedIndex !== null && index !== expectedIndex) || (identity && id !== identity))
      lab.gaps.push({seq, expectedSeq, index: String(index), expectedIndex: String(expectedIndex), id, identity});
    expectedSeq = seq + 1; expectedIndex = index + BigInt(count); identity = id;
    if (count) lab.audio++;
  };
  decode.onmessage = ({data}) => {
    if (!(data instanceof ArrayBuffer)) return;
    if (new TextDecoder().decode(new Uint8Array(data, 0, 4)) !== 'SDE2') return;
    const message = JSON.parse(new TextDecoder().decode(new Uint8Array(data, 8)));
    if (message.event?.type === 'am-info') lab.events.push(message.event);
  };
}"""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    parser.add_argument('--screenshot', help='optional screenshot of the confirmed AM window')
    args = parser.parse_args()
    origin = args.url.rstrip('/')
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(origin, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                  'mode': 'am', 'am_sync': False, 'ifbw': 6000,
                  'agc': True, 'squelch': -140, 'volume': 0.8})
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page()
        page.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
        page.goto(origin, wait_until='domcontentloaded')
        page.locator('#btn-decode-window').wait_for(state='visible')
        page.wait_for_function("!document.querySelector('#btn-decode-window').disabled")
        if not page.locator('#am-sync-toggle').is_visible():
            page.locator('#btn-decode-window').click()
        toggle = page.locator('#am-sync-toggle')
        toggle.wait_for(state='visible')
        page.evaluate(PROBE)
        page.wait_for_function("window.__amSyncProbe.audio > 5 && window.__amSyncProbe.events.some(e => !e.syncEnabled)")
        # Await the resolved condition and inspect that same snapshot; a Promise
        # itself must not satisfy a synchronous browser polling condition.
        before = wait_async_condition(page, '''async () => {
          const snapshot = await fetch('/api/state').then(response => response.json());
          const channel = snapshot.sdr.channels[0];
          return channel.processing_runtime?.observed_receiver?.mode === 'am' ? channel : null;
        }''')
        page.locator('#am-title-badge').click()
        assert state(origin)['sdr']['channels'][0]['am_sync_generation'] == before['am_sync_generation']
        generation = before['am_sync_generation']
        revision = int(before['control']['revision'])
        receiver_uid = before['control']['receiver_uid']
        service_id = before['control']['service_id']
        processing_generation = before['processing_runtime']['processing_generation']
        applied_controls = []
        for enabled in (True, False, True):
            page.evaluate('window.__amSyncProbe.events = []')
            toggle.click()
            page.wait_for_function('(enabled) => document.querySelector("#am-sync-toggle").getAttribute("aria-pressed") === String(enabled)', arg=enabled)
            page.wait_for_function('(enabled) => window.__amSyncProbe.events.some(e => e.syncEnabled === enabled)', arg=enabled)
            generation += 1
            revision += 1
            page.wait_for_function('(generation) => window.__amSyncProbe.events.some(e => e.amSyncGeneration === generation)', arg=generation)
            current = wait_async_condition(page, '''async ({revision, uid}) => {
              const snapshot = await fetch('/api/state').then(response => response.json());
              const channel = snapshot.sdr.channels[0];
              return channel.processing_runtime?.observed_receiver?.mode === 'am'
                && channel.control_receipts.some(receipt =>
                  receipt.receiver_uid === uid && receipt.revision === revision && receipt.status === 'applied')
                ? channel : null;
            }''', arg={'revision': str(revision), 'uid': receiver_uid})
            expected = configuration_and_stream(before)
            expected.update(am_sync=enabled, am_sync_generation=generation)
            assert len(expected['confirmed']['processors']) == 1, expected['confirmed']
            expected['confirmed']['processors'][0]['am_sync'] = enabled
            assert configuration_and_stream(current) == expected, (expected, current)
            control = current['control']
            assert (control['receiver_uid'], control['service_id'], control['revision']) == (
                receiver_uid, service_id, str(revision)), control
            runtime = current['processing_runtime']
            assert runtime['processing_generation'] == processing_generation, runtime
            assert runtime['fault'] is None and runtime['state'] in ('active', 'backlogged'), runtime
            # Squelch gain/open are measurements; the observed mode, NR state
            # and model-session ownership must remain the same receiver setup.
            assert {key: value for key, value in runtime['observed_receiver'].items() if key != 'squelch'} == {
                key: value for key, value in before['processing_runtime']['observed_receiver'].items()
                if key != 'squelch'}, runtime
            receipt = next(receipt for receipt in current['control_receipts']
                           if receipt['revision'] == str(revision) and receipt['status'] == 'applied')
            assert receipt['receiver_uid'] == receiver_uid, receipt
            assert receipt['processing_generation'] == processing_generation, receipt
            applied_controls.append(receipt)
            assert state(origin)['sdr']['am_sync'] is enabled
        before_invalid = state(origin)['sdr']['channels'][0]
        try:
            response = post(origin, {'cmd': 'SET_SDR_CHANNEL', 'am_sync': 'true', 'volume': 0.1})
            assert response.get('cmd') == 'ERROR', response
        except urllib.error.HTTPError as error:
            assert error.code == 400, error.code
        after_invalid = state(origin)['sdr']['channels'][0]
        assert configuration_and_stream(after_invalid) == configuration_and_stream(before_invalid)
        assert after_invalid['control']['revision'] == before_invalid['control']['revision']
        assert after_invalid['control']['receiver_uid'] == receiver_uid
        page.wait_for_function('(generation) => window.__amSyncProbe.events.some(e => e.syncEnabled && e.syncLocked && e.amSyncGeneration === generation)', arg=generation, timeout=15000)
        page.wait_for_function("document.querySelector('#am-title-badge').classList.contains('on')")
        report = page.evaluate('''() => ({audio: __amSyncProbe.audio, markers: __amSyncProbe.markers,
            gaps: __amSyncProbe.gaps, errors: __amSyncProbe.errors,
            locked: __amSyncProbe.events.some(e => e.syncLocked),
            badge: document.querySelector('#am-title-badge').textContent,
            lock: document.querySelector('#am-title-badge').classList.contains('on'),
            carrier: document.querySelector('#am-carrier').textContent})''')
        report['applied_controls'] = applied_controls
        assert report['audio'] > 5 and report['markers'] == 1, report
        assert report['lock'] and report['locked'], report
        assert not report['gaps'] and not report['errors'], report
        if args.screenshot:
            page.screenshot(path=args.screenshot, full_page=True)
        page.evaluate('window.__amSyncProbe.sockets.forEach(socket => socket.close())')
        for mode in ('usb', 'am'):
            post(origin, {'cmd': 'SET_SDR_CHANNEL', 'mode': mode})
            assert state(origin)['sdr']['channels'][0]['am_sync'] is True
        page.reload(wait_until='domcontentloaded')
        page.wait_for_function('document.querySelector("#am-sync-toggle")?.getAttribute("aria-pressed") === "true"')
        post(origin, {'cmd': 'SET_PRESET'})
        assert state(origin)['sdr']['am_sync'] is False
        browser.close()
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
