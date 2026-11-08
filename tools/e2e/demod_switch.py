#!/usr/bin/env python3
"""Demodulator switching: every mode change must work on its own.

Why this exists: switching demodulators used to need a page refresh or a Preset to take effect.
This probe covers panel selection, native receiver events, the decode pane, and return to
the mode that decodes without reloading the page.

The checks deliberately *clear the log* before each round trip and then wait for a **new** decode:
text left over from an earlier step would otherwise make a dead decoder look alive.

Usage:  python3 tools/e2e/demod_switch.py [--url http://127.0.0.1:8099]
Exit status is non-zero when any check fails, so it can be a CI gate.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent.parent.parent))

from playwright.sync_api import sync_playwright  # noqa: E402
from websockets.sync.client import connect  # noqa: E402

# Native UID receivers use FixtureSource::for_mode in service_fixture.rs.
# The separate raw-IQ fixture's TEST DE N0CALL is not this subscription's source.
CW_TEXT = 'CQ DE W1ABC'
failures: list[str] = []


def post(url: str, payload: dict) -> dict:
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def js_click(page, selector: str) -> None:
    page.evaluate(f"document.querySelector('{selector}').click()")


def check(label: str, ok: bool, detail: str = '') -> None:
    print(f'  {"PASS" if ok else "FAIL"}  {label}' + (f'  <- {detail}' if detail else ''), flush=True)
    if not ok:
        failures.append(label)


def demod(page, mode: str) -> None:
    js_click(page, f'[data-sdr-demod="{mode}"]')


def iq_diag(page) -> str:
    return page.evaluate("document.getElementById('spectrum').dataset.sdrIq || ''") or ''


def counter(page, name: str) -> int:
    found = re.search(rf'{name}=(\d+)', iq_diag(page))
    return int(found.group(1)) if found else 0


def get_json(url: str, path: str) -> dict:
    with urllib.request.urlopen(url + path, timeout=10) as response:
        return json.load(response)


def receiver_identity(receiver: dict) -> dict:
    return {key: receiver[key] for key in ['receiver_uid', 'revision', 'confirmed', 'stream']}


def source_key(page) -> str | None:
    found = re.search(r'stream_key=(\d+:\d+)', iq_diag(page))
    return found.group(1) if found else None


def receiver_delivery(url: str, uid: str) -> dict:
    # The legacy aggregate intentionally omits UID/baseband fields. Observe the
    # real UID subscription to bind its processing identity to the current API.
    deadline = time.monotonic() + 4
    with connect(url.replace('http', 'ws') + '/ws/decode?receiver_uid=' + uid) as socket:
        while True:
            frame = socket.recv(timeout=max(0, deadline - time.monotonic()))
            assert isinstance(frame, bytes) and frame[:4] == b'SDE2'
            envelope = json.loads(frame[8:])
            if envelope['event']['type'] not in ('start', 'processing-end'):
                return {**{key: value for key, value in envelope.items() if key != 'event'},
                        'event': envelope['event']['type']}


def cw_text(page) -> str:
    return page.evaluate("document.getElementById('decode-window-cw').textContent") or ''


def wait_for_cw(page, seconds: float = 60.0) -> str:
    """The fake backend keys the same message forever; wait for it to appear again."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        text = cw_text(page)
        if CW_TEXT in text:
            return text
        time.sleep(2)
    return cw_text(page)


def cw_decodes_again(page, label: str, seconds: float = 60.0) -> None:
    js_click(page, '#btn-decode-clear')
    page.wait_for_timeout(500)
    text = wait_for_cw(page, seconds)
    check(label, CW_TEXT in text, f'text={text[:70]!r}')


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8099')
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-demod-switch'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    fixture = get_json(args.url, '/api/state').get('device_detail', {}).get('uid', '').startswith('FAKE')
    descriptors = {mode['id']: mode for mode in get_json(args.url, '/api/sdr/receivers')['capabilities']['mode_descriptors']}
    mode_checks = []

    post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    post(args.url, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                    'mode': 'am', 'ifbw': 6000, 'pitch': 700,
                    'volume': 0.8, 'squelch': -140.0, 'agc': True})

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        context = browser.new_context()
        context.add_init_script("localStorage.setItem('web-sa-mode', 'sdr')")
        # Observe real validated worker deliveries without replacing any data.
        # Processing identity differs from the receiver's baseband/IQ identity.
        context.add_init_script('''(() => {
          const Native=Worker; window.__nativeDeliveries=[]; window.__deliveryOrder=0;
          window.Worker=class extends Native { constructor(...args) {
            super(...args); this.addEventListener('message', ({data}) => {
              if (data.type!=='frames' || !Array.isArray(data.envelopes)) return;
              for (const e of data.envelopes) window.__nativeDeliveries.push({order:++window.__deliveryOrder,
                key:`${e.epoch}:${e.channel_id}`,channel:e.channel,receiver_uid:e.receiver_uid,service_id:e.service_id,
                mode:e.mode,baseband_stream:e.baseband_stream,event:e.event.type,seq:e.seq,input_index:e.input_index});
              window.__nativeDeliveries=window.__nativeDeliveries.slice(-256);
            });
          }};
        })()''')
        page = context.new_page()
        sent, replies = [], []
        def wire(socket):
            def remember(target, payload):
                if isinstance(payload, str) and payload.startswith('{'):
                    data = json.loads(payload)
                    if not data.get('request_id'):
                        return
                    target.append({key: data[key] for key in ['cmd', 'mode', 'request_id', 'response_to', 'msg', 'code', 'config_version'] if key in data})
                    del target[:-128]
            socket.on('framesent', lambda payload: remember(sent, payload))
            socket.on('framereceived', lambda payload: remember(replies, payload))
        page.on('websocket', wire)
        page.goto(args.url, wait_until='domcontentloaded')
        # Wait for the controls to mount before checking the selected mode's pane.
        page.wait_for_selector('[data-sdr-demod="cw"]', state='attached', timeout=30000)
        page.wait_for_timeout(3000)

        print('A) the mode list comes from the DSP registry, not from this test', flush=True)
        modes = page.evaluate(
            "() => [...document.querySelectorAll('[data-sdr-demod]')]"
            ".filter((b) => !b.disabled).map((b) => b.dataset.sdrDemod)")
        check('the panel offers analog and digital demodulators',
              'cw' in modes and 'ft8' in modes and len(modes) >= 4, f'modes={modes}')
        # AM now shares this window for actual signal information and sync controls.
        page.wait_for_function("!document.getElementById('btn-decode-window').disabled")
        if not page.locator('#decode-window').is_visible():
            js_click(page, '#btn-decode-window')
        page.locator('#am-sync-toggle').wait_for(state='visible')
        check('AM offers its signal information pane',
              page.locator('#decode-window-title').inner_text() == 'AM'
              and page.locator('#decode-am').is_visible())
        js_click(page, '#btn-decode-window')

        print('B) CW decodes when it is selected from the panel', flush=True)
        demod(page, 'cw')
        page.wait_for_function(
            "() => { const b = document.getElementById('btn-decode-window'); return b && !b.disabled; }",
            timeout=20000)
        check('CW offers the decode window again', True)
        # Enable native PCM playback through the same control an operator uses.
        if page.query_selector('#btn-sdr-audio:not(.active)'):
            js_click(page, '#btn-sdr-audio')
        js_click(page, '#btn-decode-window')
        page.wait_for_timeout(2000)
        check('native receiver events arrive before the first decode',
              'backend=1' in iq_diag(page) and 'pipeline=true' in iq_diag(page)
              and counter(page, 'blocks') > 0, iq_diag(page))
        cw_decodes_again(page, 'CW decodes straight after being selected')

        print('C) a digital mode with a companion demodulator, and back', flush=True)
        demod(page, 'ft8')
        page.wait_for_timeout(3000)
        title = page.evaluate("document.getElementById('decode-window-title').textContent")
        check('the window follows the mode (FT8 pane)', title == 'FT8', f'title={title!r}')
        demod(page, 'cw')
        page.wait_for_timeout(3000)
        title = page.evaluate("document.getElementById('decode-window-title').textContent")
        check('the window follows the mode back (CW pane)', title == 'CW', f'title={title!r}')
        cw_decodes_again(page, 'CW decodes after FT8')

        print('D) every other demodulator, each followed by a return to CW', flush=True)
        for mode in [m for m in modes if m not in ('cw', 'ft8')]:
            before = counter(page, 'blocks')
            previous = get_json(args.url, '/api/sdr/receivers')['receivers'][0]
            old_source = source_key(page)
            command_start = len(sent)
            demod(page, mode)
            page.wait_for_timeout(4000)
            command = next((entry for entry in reversed(sent[command_start:]) if entry.get('mode') == mode), None)
            request_id = command.get('request_id') if command else None
            reply = next((entry for entry in reversed(replies) if isinstance(request_id, str) and request_id
                and entry.get('request_id') == request_id and entry.get('response_to') == command['cmd']), None)
            snapshot = get_json(args.url, '/api/sdr/receivers')
            current = snapshot['receivers'][0]
            message = str((reply or {}).get('msg', ''))
            missing = fixture and reply is not None and reply.get('cmd') == 'ERROR' and (
                mode in ('ft4', 'wspr') and 'require UTC-aligned live IQ; looping fixture replay is unsupported' in message
                or descriptors[mode]['provider'] != 'native' and f'external/{mode}.f32' in message
                    and 'reading fixture ' in message and 'os error 2' in message)
            new_source = source_key(page)
            observed = (current.get('processing_runtime') or {}).get('observed_receiver') or {}
            check(f'{mode}: control received a matching reply', command is not None and reply is not None, str(reply))
            source = receiver_delivery(args.url, current['receiver_uid'])
            baseline = page.evaluate('window.__deliveryOrder')
            source_baseline = counter(page, 'blocks')
            page.wait_for_timeout(500)
            deliveries = page.evaluate('order => window.__nativeDeliveries.filter(e => e.order>order)', baseline)
            expected_mode = 'cw' if missing else mode
            accepted = [e for e in deliveries if e['key'] == new_source and e['channel'] == current['display_number']
                and e['mode'] == expected_mode and e['event'] not in ('start', 'processing-end')]
            bound = source['receiver_uid'] == current['receiver_uid'] and source['service_id'] == snapshot['service_id']
            bound = bound and source['baseband_stream'] == current['stream'] and source['mode'] == expected_mode
            bound = bound and f"{source['epoch']}:{source['channel_id']}" == new_source
            moving = bound and counter(page, 'blocks') > source_baseline > before and source_key(page) == new_source and bool(accepted)
            if missing:
                check(f'{mode}: unavailable fixture is rejected without changing the active receiver',
                      receiver_identity(previous) == receiver_identity(current) and new_source == old_source
                      and observed.get('mode') == 'cw' and moving, message)
                print(f'  NOT COVERED  {mode} decoding: {message}', flush=True)
            else:
                check(f'{mode}: new native receiver events keep arriving', moving
                      and reply is not None and reply.get('cmd') == 'STATUS'
                      and current['confirmed']['processors'][0]['mode'] == mode and observed.get('mode') == mode
                      and old_source is not None and new_source is not None and old_source != new_source
                      and 'backend=1' in iq_diag(page) and 'pipeline=true' in iq_diag(page),
                      f'events {before} -> {counter(page, "blocks")} | mode={observed.get("mode")} | {iq_diag(page)} | {reply}')
            mode_checks.append({'mode': mode, 'provider': descriptors[mode]['provider'], 'reply': reply,
                'source_available': not missing, 'before': receiver_identity(previous), 'after': receiver_identity(current),
                'observed': observed, 'runtime': iq_diag(page), 'uid_source': source, 'accepted_after_source_baseline': accepted})
            (args.artifacts / 'mode-sources.json').write_text(json.dumps(mode_checks, indent=2) + '\n')
            if mode in ('drm', 'drmplus'):
                controls = page.evaluate('''() => ({
                  bypass: [...document.querySelectorAll('#btn-sdr-agc, [data-action="toggle-sdr-nr"], #input-sdr-squelch, [data-sdr-deemph]')]
                    .map(el => ({id: el.id || el.dataset.sdrDeemph, disabled: el.disabled})),
                  volume: document.getElementById('input-sdr-volume')?.disabled,
                  audio: document.getElementById('btn-sdr-audio')?.disabled
                })''')
                check(f'{mode}: AGC, NR, squelch and de-emphasis are unavailable',
                      len(controls['bypass']) >= 4 and all(c['disabled'] for c in controls['bypass']), str(controls))
                check(f'{mode}: volume and audio remain available',
                      controls['volume'] is False and controls['audio'] is False, str(controls))
            demod(page, 'cw')
            page.wait_for_timeout(2500)
            cw_decodes_again(page, f'CW decodes after {mode}', seconds=45)

        print('E) fast switching: clicks with no wait between them', flush=True)
        # A burst of receiver reconfigurations must still produce fresh CW text.
        for mode in ('cw', 'am', 'cw', 'ft8', 'cw'):
            demod(page, mode)
        page.wait_for_timeout(3000)
        cw_decodes_again(page, 'CW decodes after a burst of switches', seconds=60)

        print('F) confirmed filters are restored separately for each demodulator', flush=True)
        js_click(page, '[data-sdr-ifbw="500"]')
        page.wait_for_timeout(800)
        demod(page, 'am')
        page.wait_for_timeout(800)
        js_click(page, '[data-sdr-ifbw="9000"]')
        page.wait_for_timeout(800)
        for mode in ('cw', 'ft8', 'cw'):
            demod(page, mode)
            page.wait_for_timeout(600)
        picked = page.evaluate(
            "() => document.querySelector('[data-sdr-ifbw].active')?.dataset.sdrIfbw")
        check('CW restores its own filter after AM and FT8 switches', picked == '500',
              f'ifbw={picked}')
        demod(page, 'am')
        page.wait_for_timeout(800)
        picked = page.evaluate(
            "() => document.querySelector('[data-sdr-ifbw].active')?.dataset.sdrIfbw")
        check('AM restores its own independently confirmed filter', picked == '9000',
              f'ifbw={picked}')
        demod(page, 'cw')
        page.wait_for_timeout(800)

        print('G) the window can be closed and reopened across switches', flush=True)
        js_click(page, '#btn-decode-window')          # close
        demod(page, 'am')
        page.wait_for_timeout(1500)
        demod(page, 'cw')
        page.wait_for_timeout(1500)
        js_click(page, '#btn-decode-window')          # reopen
        page.wait_for_timeout(1000)
        shown = page.evaluate("getComputedStyle(document.getElementById('decode-window')).display")
        cw_decodes_again(page, 'CW decodes after the window was closed and reopened')
        check('the window is visible again', shown != 'none', f'display={shown}')

        browser.close()

    (args.artifacts / 'report.json').write_text(json.dumps({'mode_checks': mode_checks,
        'unverified_decode_modes': [entry['mode'] for entry in mode_checks if not entry['source_available']],
        'failures': failures}, indent=2) + '\n')
    print(f'\n{"FAILED (%d): %s" % (len(failures), ", ".join(failures)) if failures else "ALL PASS"}')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
