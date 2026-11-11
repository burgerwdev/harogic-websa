#!/usr/bin/env python3
"""Per-channel audio controls and main-panel recovery on an isolated synthetic service."""
from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path

from native_multichannel import api, config, saved_receivers
from native_multichannel_processing import assert_receivers_continue, setup
from native_multichannel_recording import until
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


def bandwidth_gate(page, origin):
    """Independent IQ widths and confirmed mode memory must not cap main-panel edits."""
    modes = [('am', 3000, 12000), ('dsb', 3000, 12000), ('usb', 1800, 6000),
             ('lsb', 1800, 6000), ('cw', 100, 2000), ('nfm', 9000, 25000),
             ('wfm', 100000, 250000), ('pm', 6000, 25000)]
    results = []
    api(origin, '/api/config', {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True})
    uid = api(origin)['receivers'][0]['receiver_uid']
    for mode, narrow, wide in modes:
        api(origin, '/api/config', {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'mode': mode, 'ifbw': narrow})
        row = next(row for row in api(origin)['receivers'] if row['receiver_uid'] == uid)
        candidate = row['confirmed']
        candidate['name'] = f'{mode.upper()} narrow IQ'
        reply = api(origin, payload={'type': 'configure', 'request_id': f'narrow-{mode}',
                    'receiver_uid': uid, 'expected_revision': row['revision'], 'config': candidate})
        assert reply['fault'] is None, reply
        confirmed = next(row for row in reply['snapshot']['receivers'] if row['receiver_uid'] == uid)
        # API setup advances identity even when the visible width already matches.
        # Wait for the browser to observe that revision before creating an edit.
        page.wait_for_function('''({uid, revision}) =>
          window.__audioSettingsStatus?.sdr?.channels?.some(row =>
            row.control?.receiver_uid === uid && typeof row.control.revision === 'string'
            && BigInt(row.control.revision) >= BigInt(revision))''',
            arg={'uid': uid, 'revision': confirmed['revision']})
        group = page.locator(f'#sdr-ifbw-group[data-mode="{mode}"]')
        expect(group.locator(f'[data-sdr-ifbw="{narrow}"]')).to_have_attribute('aria-pressed', 'true')
        wider = group.locator(f'[data-sdr-ifbw="{wide}"]')
        expect(wider).to_be_enabled()
        wider.click()
        until(lambda mode=mode, wide=wide: any(r['receiver_uid'] == uid and r['confirmed']['processors'][0]['mode'] == mode
            and r['confirmed']['processors'][0]['bandwidth_hz'] == wide for r in api(origin)['receivers']))
        results.append({'mode': mode, 'narrow': narrow, 'wide': wide})
    # A real mode switch restores the saved independent baseband as well.
    page.locator('.sdr-mode-common [data-sdr-demod=am]').click()
    group = page.locator('#sdr-ifbw-group[data-mode=am]')
    expect(group.locator('[data-sdr-ifbw="12000"]')).to_have_attribute('aria-pressed', 'true')
    group.locator('[data-sdr-ifbw="3000"]').click()
    expect(group.locator('[data-sdr-ifbw="3000"]')).to_have_attribute('aria-pressed', 'true')
    until(lambda: api(origin, '/api/state')['sdr']['if_bw'] == 3000)
    page.locator('.sdr-mode-common [data-sdr-demod=usb]').click()
    expect(page.locator('#sdr-ifbw-group')).to_have_attribute('data-mode', 'usb')
    page.locator('.sdr-mode-common [data-sdr-demod=am]').click()
    expect(group.locator('[data-sdr-ifbw="3000"]')).to_have_attribute('aria-pressed', 'true')
    expect(group.locator('[data-sdr-ifbw="12000"]')).to_be_enabled()
    group.locator('[data-sdr-ifbw="12000"]').click()
    until(lambda: api(origin, '/api/state')['sdr']['if_bw'] == 12000)
    return results


def folded_strip_gate(page, origin, overview, uid, artifacts):
    """Exercise browser input dispatch and playback-clock meters on a collapsed window."""
    if not overview.is_visible():
        page.locator('[data-action=sdr-workbench]').click()
    row = overview.locator(f'.mc-row[data-uid="{uid}"]')
    row.locator('.mc-row-more > summary').click()
    row.locator('.mc-selection-mark[data-action=detail]').click()
    detail = page.locator(f'[data-view-id="detail-{uid}"]')
    detail.locator('[data-collapse]').click()
    expect(detail.locator('.mc-body')).to_be_hidden()
    name = detail.locator('[data-strip-name]'); edit = detail.locator('[data-strip-name-edit]')
    name.dblclick(); edit.fill('Collapsed renamed'); edit.press('Enter')
    expect(name).to_have_text('Collapsed renamed')
    until(lambda: any(r['receiver_uid'] == uid and r['confirmed']['name'] == 'Collapsed renamed' for r in api(origin)['receivers']))
    confirmed = next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)
    name.dblclick(); edit.fill('Escape must not save'); edit.press('Escape')
    expect(edit).to_have_count(0); expect(name).to_have_text('Collapsed renamed')
    name.dblclick(); edit.fill('Blur must not save')
    detail.locator('[data-action=strip-volume]').click()
    expect(edit).to_have_count(0); expect(name).to_have_text('Collapsed renamed')
    detail.locator('[data-strip-gain]').press('Escape')
    before = detail.bounding_box(); handle = detail.locator('.mc-strip-identity small').bounding_box()
    dx, dy = (-50 if before['x'] > 80 else 50), (-35 if before['y'] > 80 else 35)
    page.mouse.move(handle['x'] + handle['width'] / 2, handle['y'] + handle['height'] / 2)
    page.mouse.down(); page.mouse.move(handle['x'] + handle['width'] / 2 + dx,
        handle['y'] + handle['height'] / 2 + dy, steps=8); page.mouse.up()
    after = detail.bounding_box()
    assert abs(after['x'] - before['x']) > 20 and abs(after['y'] - before['y']) > 20, (before, after)
    expect(edit).to_have_count(0)
    assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['confirmed'] == confirmed['confirmed']

    # Deleting a channel during an edit cancels the old UID even when its C slot is reused.
    writes = []
    def collect(request):
        if request.method == 'POST' and request.url.endswith('/api/sdr/receivers'):
            writes.append(request.post_data_json)
    page.on('request', collect)
    name.dblclick(); edit.fill('Old UID must not rename replacement')
    retired = api(origin, payload={'type': 'retire', 'request_id': 'strip-retire', 'receiver_uid': uid})
    assert retired['fault'] is None, retired
    candidate = json.loads(json.dumps(confirmed['confirmed'])); candidate['name'] = 'Replacement intact'
    created = api(origin, payload={'type': 'create', 'request_id': 'strip-replacement', 'config': candidate})
    assert created['fault'] is None, created
    replacement = next(r for r in created['snapshot']['receivers'] if r['display_number'] == confirmed['display_number'])
    assert replacement['receiver_uid'] != uid
    expect(edit).to_have_count(0)
    page.keyboard.press('Enter')
    expect(overview.locator(f'.mc-row[data-uid="{replacement["receiver_uid"]}"]')).to_be_visible()
    assert not [request for request in writes if request.get('type') == 'configure'], writes
    assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == replacement['receiver_uid'])['confirmed'] == candidate
    page.remove_listener('request', collect)
    expect(detail).to_have_count(0)

    uid = replacement['receiver_uid']; row = overview.locator(f'.mc-row[data-uid="{uid}"]')
    row.locator('.mc-row-more > summary').click(); row.locator('.mc-selection-mark[data-action=detail]').click()
    detail = page.locator(f'[data-view-id="detail-{uid}"]'); detail.locator('[data-collapse]').click()
    detail.locator('.mc-channel-strip [data-action=listen]').click()
    page.wait_for_function('''uid => window.websaAudioStatus().some(r=>r.channel===uid
      && r.enabled && r.playedFrames>2000 && r.rms>0 && r.peak>=r.rms && !r.error)''', arg=uid)
    prior = page.evaluate('uid=>window.websaAudioStatus().find(r=>r.channel===uid)', uid)
    detail.locator('[data-action=strip-volume]').click()
    gain = detail.locator('[data-strip-gain]'); gain.focus(); page.keyboard.press('Home')
    expect(gain).to_have_value('0'); gain.press('Escape')
    try:
        page.wait_for_function('''({uid, played}) => window.websaAudioStatus().some(r=>r.channel===uid
          && r.localGain===0 && r.playedFrames>played && r.rms>0 && r.peak>=r.rms && !r.error)''',
            arg={'uid': uid, 'played': prior['playedFrames']})
    finally:
        (artifacts / 'folded-zero-gain.json').write_text(json.dumps({
            'before': prior, 'after': page.evaluate('uid=>window.websaAudioStatus().find(r=>r.channel===uid)', uid),
            'meter': detail.locator('[data-strip-meter]').get_attribute('aria-label')}, indent=2) + '\n')
    meter = detail.locator('[data-strip-meter]')
    expect(meter).to_have_attribute('data-available', 'true')
    expect(meter).to_have_attribute('aria-label', re.compile(r'播放音频（监听音量之前） · RMS .+峰值'))
    zero_gain = page.evaluate('uid=>window.websaAudioStatus().find(r=>r.channel===uid)', uid)
    # An independent source probe keeps receiving while the playback consumer is
    # suspended. Source availability must not substitute for a render-clock meter.
    page.evaluate('''uid => {
      const url=new URL('/ws/audio',location.href);url.protocol=location.protocol==='https:'?'wss:':'ws:';
      url.searchParams.set('receiver_uid',uid);window.__reviewSourceFrames=0;
      const socket=window.__reviewSourceSocket=new WebSocket(url);socket.binaryType='arraybuffer';
      socket.onmessage=event=>{
        if(event.data instanceof ArrayBuffer && event.data.byteLength>96 && new DataView(event.data).getUint32(12,true)>0)
          window.__reviewSourceFrames++;
      };
    }''', uid)
    page.wait_for_function('() => window.__reviewSourceFrames>0')
    source_before = page.evaluate('window.__reviewSourceFrames')
    page.evaluate('async () => { await window.__reviewAudioContext.suspend(); }')
    page.wait_for_function('''({uid, frames}) => window.websaAudioStatus().some(r=>r.channel===uid
      && window.__reviewSourceFrames>frames+3 && r.contextState==='suspended' && Date.now()-r.meterAt>600)''',
        arg={'uid': uid, 'frames': source_before})
    expect(meter).to_have_attribute('data-available', 'false')
    suspended = page.evaluate('uid=>window.websaAudioStatus().find(r=>r.channel===uid)', uid)
    page.evaluate('async () => { await window.__reviewAudioContext.resume(); }')
    expect(meter).to_have_attribute('data-available', 'true')
    source_frames = page.evaluate('() => { window.__reviewSourceSocket.close(); return window.__reviewSourceFrames; }')
    detail.locator('[data-action=strip-volume]').click()
    gain = detail.locator('[data-strip-gain]'); gain.focus(); page.keyboard.press('End'); gain.press('Escape')
    page.wait_for_function('''({uid, index})=>window.websaAudioStatus().some(r=>r.channel===uid
      && r.localGain===1 && r.playing && BigInt(r.audioIndex)>BigInt(index) && !r.error)''',
        arg={'uid': uid, 'index': zero_gain['audioIndex']})
    assert next(r for r in api(origin)['receivers'] if r['receiver_uid'] == uid)['confirmed'] == candidate
    detail.locator('[data-close]').click()
    return {'renamed': confirmed, 'replacement': replacement, 'drag_before': before, 'drag_after': after,
        'zero_gain_playback': zero_gain, 'suspended_playback': suspended, 'independent_source_frames': source_frames,
        'checks': ['real dblclick/Enter', 'Escape and blur cancel', 'header drag does not edit',
            'retired UID does not rename reused slot', 'pre-gain PCM RMS/peak at zero monitor gain',
            'available network source during suspended render clock does not display playback level',
            'restored local gain consumes current audio index']}


def browser_gate(origin, snapshot, artifacts):
    receivers = saved_receivers(snapshot)
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1440, 'height': 1000})
        page.add_init_script("""localStorage.setItem('web-sa-sdr-audio','1');localStorage.setItem('web-sa-lang','zh');
          const NativeWebSocket=window.WebSocket;
          window.WebSocket=class extends NativeWebSocket {
            constructor(...args) {
              super(...args);
              this.addEventListener('message', event => {
                if (typeof event.data !== 'string') return;
                try {
                  const value=JSON.parse(event.data);
                  if (value.cmd === 'STATUS') window.__audioSettingsStatus=value;
                } catch { /* Binary streams and unrelated messages are not STATUS. */ }
              });
            }
          };
          const NativeAudioContext=window.AudioContext;
          window.AudioContext=class extends NativeAudioContext {
            constructor(...args) { super(...args); window.__reviewAudioContext=this; }
          };""")
        errors, retirements, commands, workers = [], [], [], []
        page.on('worker', lambda worker: workers.append(worker.url))
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('console', lambda message: errors.append(message.text) if message.type == 'error' else None)
        def command(request):
            if request.method != 'POST' or not request.url.endswith(('/api/sdr/receivers', '/api/config')):
                return
            body = request.post_data_json; commands.append(body)
            (artifacts / 'browser-commands.json').write_text(json.dumps(commands, indent=2) + '\n')
            if body.get('type') == 'retire':
                retirements.append(body['receiver_uid'])
        page.on('request', command)
        websocket_commands = []
        def wire(socket):
            def sent(payload):
                if isinstance(payload, str) and payload.startswith('{'):
                    websocket_commands.append(json.loads(payload))
                    (artifacts / 'websocket-commands.json').write_text(json.dumps(websocket_commands, indent=2) + '\n')
            socket.on('framesent', sent)
        page.on('websocket', wire)
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        expect(page.locator('.sdr-mode-common [data-sdr-demod=drm]')).to_be_visible()
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        expect(overview).to_be_visible()
        uid = receivers[0]['receiver_uid']
        overview.locator(f'.mc-row[data-uid="{uid}"] [data-action=listen]').click()
        page.wait_for_function('''uid => window.websaAudioStatus().some(r => r.channel === uid
            && r.enabled && r.playedFrames > 2000 && r.rms > 0 && !r.error)''', arg=uid)
        strip_config = json.loads(json.dumps(receivers[0]['confirmed']))
        strip_config['name'] = 'Folded strip probe'; strip_config['baseband']['frequency_hz'] += 60000
        added = api(origin, payload={'type': 'create', 'request_id': 'strip-probe', 'config': strip_config})
        assert added['fault'] is None, added
        strip_uid = next(r['receiver_uid'] for r in added['snapshot']['receivers']
            if r['receiver_uid'] not in {old['receiver_uid'] for old in snapshot['receivers']})
        folded = folded_strip_gate(page, origin, overview, strip_uid, artifacts)
        (artifacts / 'folded-strip-report.json').write_text(json.dumps(folded, indent=2) + '\n')
        probe_uid = folded['replacement']['receiver_uid']
        uid = receivers[0]['receiver_uid']
        healthy_before = page.evaluate('uid => window.websaAudioStatus().find(r => r.channel === uid)', uid)
        assert healthy_before['enabled'] and healthy_before['playing'] and healthy_before['rms'] > 0, healthy_before
        stale_directory = api(origin)
        worker_count = len(workers)
        directory_url = re.compile(r'/api/sdr/receivers(?:\?.*)?$')
        held_directory_requests = []
        def hold_directory(route):
            if route.request.method == 'GET':
                held_directory_requests.append(route.request.url)
                route.fulfill(json=stale_directory)
            else:
                route.continue_()
        # End the active UID externally while its control view is deliberately
        # stale. A normal socket end must release playback without retry/error.
        # Include the live polling query so the directory stays stale throughout
        # the socket-end assertion, independently of the 500 ms refresh cadence.
        page.route(directory_url, hold_directory)
        try:
            cleared = api(origin, payload={'type': 'retire', 'request_id': 'strip-probe-cleanup',
                'receiver_uid': probe_uid})
            assert cleared['fault'] is None, cleared
            page.wait_for_function('''uid => window.websaAudioStatus().some(r => r.channel === uid
                && r.connectionState === 'stopped' && !r.error && !r.playing
                && r.rms === 0 && r.peak === 0 && r.meterAt === 0 && r.bufferedFrames === 0)''', arg=probe_uid)
            page.wait_for_timeout(650)
            assert held_directory_requests, 'receiver directory polling was not held'
            assert len(workers) == worker_count, (worker_count, workers)
            ended = page.evaluate('uid => window.websaAudioStatus().find(r => r.channel === uid)', probe_uid)
            assert ended['connectionState'] == 'stopped' and not ended['error'], ended
            page.wait_for_function('''({uid, frames}) => window.websaAudioStatus().some(r => r.channel === uid
                && r.playing && !r.error && r.playedFrames > frames + 4000)''',
                arg={'uid': uid, 'frames': healthy_before['playedFrames']})
            healthy_after = page.evaluate('uid => window.websaAudioStatus().find(r => r.channel === uid)', uid)
            assert healthy_after['streamKey'] == healthy_before['streamKey']
            assert healthy_after['resets'] == healthy_before['resets']
            (artifacts / 'active-retirement.json').write_text(json.dumps({
                'ended': ended, 'healthy_before': healthy_before, 'healthy_after': healthy_after,
                'worker_count_before': worker_count, 'worker_count_after': len(workers)}, indent=2) + '\n')
        finally:
            page.unroute(directory_url, hold_directory)
        expect(overview.locator(f'.mc-row[data-uid="{probe_uid}"]')).to_have_count(0)
        overview.locator(f'[data-uid="{uid}"] [data-action=select]').click()
        name = overview.locator('[data-channel-name]')
        name.fill('AM receiver'); name.press('Enter')
        until(lambda: any(r['receiver_uid'] == uid and r['confirmed']['name'] == 'AM receiver' for r in api(origin)['receivers']))
        expect(overview.locator('[name=nr]')).to_be_visible()
        expect(overview.locator('[name=squelch]')).to_be_visible()
        overview.locator('[name=nr]').check()
        overview.locator('[name=nr-algorithm]').select_option('wiener')
        overview.locator('[name=agc]').uncheck()
        for name, value in [('squelch', '-105.3'), ('nr-strength', '0.25')]:
            overview.locator(f'[name={name}]').evaluate(
                '(input,value)=>{input.value=value;input.dispatchEvent(new Event("input",{bubbles:true}));}', value)
        overview.locator('[data-config] [type=submit]').click()
        until(lambda: any(r['receiver_uid'] == uid and p['nr'] and p['nr_algorithm'] == 'wiener'
            and p['nr_strength'] == 0.25 and p['squelch_dbm'] == -105.3 and not p['agc']
            for r in api(origin)['receivers'] for p in r['confirmed']['processors']))
        confirmed = api(origin)
        assert_receivers_continue([r for r in snapshot['receivers'] if r['receiver_uid'] != uid],
            [r for r in confirmed['receivers'] if r['receiver_uid'] != uid])
        compact_layout = overview.evaluate('''root=>{
          const box = selector => {const r=root.querySelector(selector).getBoundingClientRect();
            return {x:r.x,y:r.y,width:r.width,height:r.height};};
          return {gain:box('[name=pcm-gain]'),squelch:box('[name=squelch]'),audio:box('.mc-channel-audio')};
        }''')
        overview.screenshot(path=str(artifacts / 'channel-audio-settings.png'))
        assert abs(compact_layout['gain']['y'] - compact_layout['squelch']['y']) < 2, compact_layout
        assert compact_layout['gain']['width'] < compact_layout['audio']['width'] * 0.35, compact_layout
        overview.locator('[data-action=advanced-settings]').click()
        overview.locator('[data-section=processor] summary').click()
        overview.locator('[data-bandwidth="3000"]').click()
        expect(overview.locator('[name=processor-bandwidth]')).to_have_value('3000')
        overview.locator('[data-section=iq] summary').click()
        overview.screenshot(path=str(artifacts / 'channel-advanced-settings.png'))
        overview.locator('[data-action=cancel]').click()
        overview.locator(f'[data-uid="{uid}"] .mc-row-more > summary').click()
        overview.locator(f'[data-uid="{uid}"] .mc-selection-mark[data-action=detail]').click()
        detail = page.locator(f'[data-view-id="detail-{uid}"]')
        detail.evaluate('root=>{root.style.width="420px";root.style.height="760px";}')
        expect(detail.locator('[name=pcm-gain]')).to_be_visible()
        overflow = detail.evaluate('''root=>[...root.querySelectorAll('.mc-body,[data-config],.mc-channel-audio,.mc-name-editor')]
          .filter(e=>e.scrollWidth>e.clientWidth+2).map(e=>({className:e.className,width:e.clientWidth,scroll:e.scrollWidth}))''')
        assert not overflow, overflow
        detail.screenshot(path=str(artifacts / 'channel-compact-420.png'))
        detail.locator('[data-close]').click()
        # Algorithm-specific controls keep this receiver's own draft across selection.
        overview.locator('[name=nr-algorithm]').select_option('dfn')
        expect(overview.locator('[name=nr-atten]')).to_be_visible()
        expect(overview.locator('[name=nr-strength]')).not_to_be_visible()
        overview.locator('[name=nr-atten]').evaluate(
            '(input)=>{input.value="14";input.dispatchEvent(new Event("input",{bubbles:true}));}')
        second = receivers[1]['receiver_uid']
        overview.locator(f'[data-uid="{second}"] [data-action=select]').click()
        expect(overview.locator('[name=nr]')).not_to_be_checked()
        overview.locator(f'[data-uid="{uid}"] [data-action=select]').click()
        expect(overview.locator('[name=nr-algorithm]')).to_have_value('dfn')
        expect(overview.locator('[name=nr-atten]')).to_have_value('14')

        overview.locator('[data-close]').click()
        bandwidths = bandwidth_gate(page, origin)

        def delete_all():
            if not overview.is_visible():
                page.locator('[data-action=sdr-workbench]').click()
            current_uid = api(origin)['receivers'][0]['receiver_uid']
            remaining = len(saved_receivers(api(origin)))
            expect(overview.locator(f'.mc-row:not([data-uid="{current_uid}"])')).to_have_count(remaining)
            # Burst clicks across every UID also exercise the shared admission queue.
            overview.locator('.mc-row [data-action=retire]').evaluate_all('''buttons => {
              for (const button of buttons) for (let i=0;i<20;i++) button.click();
            }''')
            expect(overview.locator('.mc-row')).to_have_count(1)
            expect(overview.locator(f'.mc-row[data-uid="{current_uid}"]')).to_be_visible()
            expect(overview.locator('[data-key=session-fault]')).to_have_count(0)
            overview.locator('[data-close]').click()
            api(origin, '/api/config', {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': False})
            page.wait_for_function('() => window.websaAudioStatus().every(r=>!r.enabled)')

        recoveries = []
        for index, action in enumerate(['spectrum', 'listen', 'center']):
            delete_all()
            if action != 'spectrum':
                reply = api(origin, payload={'type': 'create', 'request_id': f'iq-{action}', 'config': config(100e6)})
                assert reply['fault'] is None, reply
                delete_all()
            current_uid = api(origin)['receivers'][0]['receiver_uid']
            prior_audio = page.evaluate('uid=>window.websaAudioStatus().find(r=>r.channel===uid)', current_uid) or {}
            if action == 'spectrum':
                canvas = page.locator('#spectrum')
                box = canvas.bounding_box()
                canvas.click(position={'x': box['width'] * 0.52, 'y': box['height'] * 0.45})
            else:
                frequency = page.locator('#input-sdr-' + action)
                frequency.fill(str(100 + index * 0.01))
                frequency.press('Enter')
            try:
                observed = until(lambda: s['receivers'][0] if len((s := api(origin))['receivers']) == 1
                    and s['receivers'][0]['state'] == 'running' and s['receivers'][0]['confirmed']['purpose'] == 'listen' else None)
            except AssertionError:
                (artifacts / f'recovery-{action}-timeout.json').write_text(json.dumps({
                    'snapshot': api(origin), 'state': api(origin, '/api/state'),
                    'field': page.locator('#input-sdr-' + ('listen' if action == 'spectrum' else action)).input_value()}, indent=2) + '\n')
                page.screenshot(path=str(artifacts / f'recovery-{action}-timeout.png'))
                raise
            after = api(origin)
            (artifacts / f'recovery-{action}.json').write_text(json.dumps({'observed': observed, 'after': after}, indent=2) + '\n')
            assert len(after['receivers']) == 1 and after['receivers'][0]['receiver_uid'] == observed['receiver_uid'], after
            receiver = after['receivers'][0]
            assert receiver['display_number'] == 1 and receiver['receiver_uid'] == current_uid
            page.wait_for_function('''({uid, frames, stream}) => window.websaAudioStatus().some(r=>r.channel===uid
              && r.owner==='workbench' && r.enabled && r.playing && r.rms>0 && r.meterAt>0
              && r.playedFrames>frames+2000 && r.streamKey!==stream && !r.error)''',
                arg={'uid': receiver['receiver_uid'], 'frames': prior_audio.get('playedFrames', 0),
                     'stream': prior_audio.get('streamKey')})
            audio = page.evaluate('(uid)=>window.websaAudioStatus().find(r=>r.channel===uid)', receiver['receiver_uid'])
            assert not audio['masterMuted'] and audio['rms'] > 0, audio
            recoveries.append({'action': action, 'receiver': receiver, 'audio': audio})
        page.screenshot(path=str(artifacts / 'main-listening-restored.png'))
        assert len(retirements) == len(set(retirements)), retirements
        assert not errors, errors
        browser.close()
        return {'recoveries': recoveries, 'retirements': retirements, 'compact_layout': compact_layout,
            'bandwidths': bandwidths, 'folded_strip': folded, 'errors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-multichannel-audio-settings'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        try:
            report = browser_gate(origin, setup(origin, saved_only=True), args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: audio controls, eight editable bandwidth modes, mode memory, saved-channel retirement and Current restart')
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
