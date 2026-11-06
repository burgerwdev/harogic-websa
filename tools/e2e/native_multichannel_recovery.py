#!/usr/bin/env python3
"""Real Chromium UID playback recovery, retirement and compact workbench interactions.

Uses an isolated native synthetic/replay service. A TCP proxy forwards real
AUD2/SDE2 traffic and cuts only the browser audio connection to inject a gap.
"""
from __future__ import annotations

import argparse
import json
import re
import select
import socket
import socketserver
import threading
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from native_multichannel import api
from native_multichannel_processing import assert_receivers_continue, setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_ui_ready


class TransportProxy:
    """Forward real HTTP/WS bytes; cut only one UID's TCP audio transport.

    Playwright's WebSocket routes do not intercept sockets created by workers.
    This proxy changes transport availability without replacing application data.
    """

    def __init__(self, upstream_origin, uid):
        self.upstream_port = urlsplit(upstream_origin).port
        self.uid = uid
        self.blocked = False
        self.connections = 0
        self.lock = threading.Lock()
        self.active = set()
        self.audio = set()
        owner = self

        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                header = b''
                self.request.settimeout(5)
                while b'\r\n\r\n' not in header:
                    chunk = self.request.recv(4096)
                    if not chunk:
                        return
                    header += chunk
                    if len(header) > 65536:
                        return
                path = urlsplit(header.split(b'\r\n', 1)[0].decode().split()[1])
                audio = path.path == '/ws/audio' and parse_qs(path.query).get('receiver_uid') == [owner.uid]
                with owner.lock:
                    if audio:
                        owner.connections += 1
                        if owner.blocked:
                            return
                with socket.create_connection(('127.0.0.1', owner.upstream_port), timeout=5) as upstream:
                    pair = (self.request, upstream)
                    with owner.lock:
                        owner.active.add(pair)
                        if audio:
                            owner.audio.add(pair)
                    try:
                        upstream.sendall(header)
                        while True:
                            readable, _, _ = select.select(pair, [], [], 0.2)
                            for source in readable:
                                data = source.recv(65536)
                                if not data:
                                    return
                                (upstream if source is self.request else self.request).sendall(data)
                    except OSError:
                        return
                    finally:
                        with owner.lock:
                            owner.active.discard(pair)
                            owner.audio.discard(pair)

        self.server = socketserver.ThreadingTCPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.origin = f'http://127.0.0.1:{self.server.server_address[1]}'

    @staticmethod
    def disconnect(pairs):
        for pair in pairs:
            for endpoint in pair:
                try:
                    endpoint.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def cut_audio(self):
        with self.lock:
            self.blocked = True
            self.disconnect(list(self.audio))

    def close(self):
        self.server.shutdown()
        with self.lock:
            self.disconnect(list(self.active))
        self.server.server_close()
        self.thread.join(timeout=2)


def browser_gate(proxy, snapshot, artifacts):
    origin = proxy.origin
    first, second = [r['receiver_uid'] for r in snapshot['receivers'][:2]]
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1366, 'height': 900})
        page = context.new_page()
        errors, writes = [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: writes.append(json.loads(request.post_data))
                if request.method == 'POST' and '/api/' in request.url and request.post_data else None)
        page.goto(origin)
        wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        def row(uid):
            return overview.locator(f'.mc-row[data-uid="{uid}"]')
        expect(overview.locator('[data-action=scene],[data-action=preset-save]')).to_have_count(0)
        expect(overview.locator('.mc-list [data-action=rec-audio]')).to_have_count(2)
        expect(overview.locator('.mc-list [data-action=rec-results]')).to_have_count(2)
        expect(row(snapshot['receivers'][2]['receiver_uid']).locator('[data-action=rec-audio],[data-action=rec-results]')).to_have_count(0)
        expect(overview.locator('.mc-list [data-action=rec-iq]')).to_have_count(3)
        expect(overview.locator('[data-section=iq]')).not_to_have_attribute('open')
        expect(overview.locator('[data-config] button[type=submit]')).to_be_visible()
        if overview.locator('[data-action=master-mute]').get_attribute('aria-pressed') == 'true':
            overview.locator('[data-action=master-mute]').click()
        if row(first).locator('[data-action=listen]').get_attribute('aria-pressed') != 'true':
            row(first).locator('[data-action=listen]').click()
        page.wait_for_function('(uid) => window.websaAudioStatus().some(r => r.channel===uid && r.playing && r.playedFrames>2000 && !r.error)', arg=first)
        assert proxy.connections == 1, 'Audio WebSocket from the real worker must pass through the TCP proxy'
        page.evaluate('window.websaNativeMixLevels()')
        page.wait_for_timeout(200)
        initial = page.evaluate('window.websaNativeMixLevels()')
        assert min(initial.values()) > 0.001, initial
        before, count = api(origin), len(writes)

        # Block audio transport and cut the established socket. The real
        # worker must display recovery and reopen its backend subscription.
        proxy.cut_audio()
        expect(overview.locator('[data-playback-state]')).to_contain_text(re.compile('reconnecting|重连', re.IGNORECASE))
        page.screenshot(path=str(artifacts / 'network-reconnecting.png'))
        proxy.blocked = False
        page.wait_for_function('(uid) => window.websaAudioStatus().some(r => r.channel===uid && r.connectionState==="connected" && r.playing && !r.error)', arg=first)
        assert proxy.connections >= 2
        page.wait_for_function('() => { const p=window.websaNativeMixLevels(); return p.left>0.001 && p.right>0.001; }')
        recovered = page.evaluate('window.websaNativeMixLevels()')
        assert_receivers_continue(before['receivers'], api(origin)['receivers'])
        assert len(writes) == count, writes[count:]

        # A direct Listen action in follow mode changes only the local mix and
        # leaves the editor on C1. Both actual playback producers must run.
        overview.locator('[data-monitor-mode]').select_option('follow')
        row(second).locator('[data-action=listen]').click()
        expect(overview.locator('[data-monitor-mode]')).to_have_value('manual')
        expect(overview).to_have_attribute('data-receiver-uid', first)
        page.wait_for_function('() => window.websaAudioStatus().filter(r=>r.enabled&&r.playedFrames>1000&&!r.error).length===2')
        assert len(writes) == count, writes[count:]
        row(second).locator('[data-action=listen]').click()
        page.wait_for_function('(uid) => window.websaAudioStatus().filter(r=>r.enabled).every(r=>r.channel===uid)', arg=first)

        # Real focus/keyboard editing survives incoming receiver snapshots. The
        # advanced disclosure also stays open while polling refreshes the UI.
        overview.locator('[data-action=advanced-settings]').click()
        overview.locator('[data-section=iq] summary').click()
        page.wait_for_timeout(700)
        expect(overview.locator('[data-section=iq]')).to_have_attribute('open', '')
        overview.locator('[data-section=iq] summary').click()
        frequency = overview.locator('[name=frequency]')
        overview.locator('[data-frequency-unit=Hz]').click()
        frequency.fill('100001000')
        frequency.press('Tab')
        page.wait_for_timeout(700)
        expect(overview.locator('[name=frequency]')).to_have_value('100001000')
        overview.locator('[data-config] button[type=submit]').click()
        preview = overview.locator('[data-preview] button')
        if preview.count():
            preview.click()
        page.wait_for_function('(uid) => fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.receiver_uid===uid).confirmed.baseband.frequency_hz===100001000)', arg=first)
        page.wait_for_function('(uid) => window.websaAudioStatus().some(r=>r.channel===uid&&r.playing&&!r.error)', arg=first)
        assert_receivers_continue(before['receivers'][1:], api(origin)['receivers'][1:])
        page.screenshot(path=str(artifacts / 'desktop-clean-workbench.png'))

        # Pop out the shared workbench, operate it at phone-like width, and Dock
        # it back. Details must remain available instead of disappearing.
        with page.expect_popup() as popup:
            overview.locator('[data-pop]').click()
        child = popup.value
        child.set_viewport_size({'width': 420, 'height': 760})
        compact = child.locator('[data-view-id=overview-follow]')
        expect(compact.locator('[data-editor]')).to_be_visible()
        compact.locator('[data-action=tab-receive]').click()
        compact.locator('[data-section=processor] summary').click()
        expect(compact.locator('[name=processor-bandwidth]')).to_be_visible()
        compact.locator('[data-action=tab-receive]').click()
        compact.locator('[name=name]').fill('AM compact draft')
        compact.locator('[name=name]').press('Tab')
        compact.locator('[data-action=cancel]').click()
        overflow = compact.locator('.mc-content').evaluate('(el) => el.scrollWidth-el.clientWidth')
        assert overflow <= 1, overflow
        child.screenshot(path=str(artifacts / 'compact-popout.png'))
        child.locator('[data-dock]').click()
        expect(overview).to_be_visible()
        expect(overview.locator('[name=name]')).to_have_value('AM native')
        assert api(origin)['receivers'][0]['confirmed']['name'] == 'AM native'

        # Retire the live decoder through UI while its fixed result view exists,
        # then reuse the slot as a fresh IQ UID. Healthy C1 keeps playing.
        row(second).locator('[data-action=select]').click()
        row(second).locator('.mc-row-more > summary').click()
        row(second).locator('[data-action=results]').click()
        results = page.locator(f'[data-view-id="results-{second}"]')
        expect(results).to_be_visible()
        with page.expect_popup() as popup:
            results.locator('[data-pop]').click()
        result_child = popup.value
        page.locator('[data-action=sdr-workbench]').click()
        assert not result_child.is_closed(), 'Opening workbench must preserve independent popouts'
        expect(page.locator(f'[data-view-id="results-{second}"]')).to_have_count(0)
        results = result_child.locator(f'[data-view-id="results-{second}"]')
        healthy = api(origin)['receivers'][0]
        overview.locator('.mc-channel-actions [data-action=retire]').click()
        expect(row(second)).to_have_count(0)
        expect(results.locator('.mc-head strong')).to_contain_text(re.compile('retired|已删除'))
        expect(results.locator('[data-decode-view] .decoder-result, .mc-message-table tbody tr')).to_have_count(0)
        page.wait_for_function('(uid)=>!window.websaAudioStatus().some(r=>r.channel===uid)', arg=second)
        overview.locator('[data-action=create]').click()
        create = overview.locator('[data-create-channel]')
        expect(create).to_be_visible()
        create.locator('[name=mode]').select_option('')
        create.locator('button[type=submit]').click()
        expect(overview.locator('.mc-row')).to_have_count(3)
        after = api(origin)
        replacement = next(r for r in after['receivers'] if r['receiver_uid'] not in {old['receiver_uid'] for old in before['receivers']})
        assert replacement['receiver_uid'] != second and replacement['confirmed']['processors'] == []
        assert_receivers_continue([healthy], [after['receivers'][0]])
        result_child.close()
        page.wait_for_function('(uid)=>window.websaAudioStatus().some(r=>r.channel===uid&&r.playing&&!r.error)', arg=first)
        assert not errors, errors
        report = {'source': 'isolated native synthetic AM/baseband and FT8 replay; no RF hardware',
                  'initial_pcm_rms': initial, 'recovered_pcm_rms': recovered, 'audio_connections': proxy.connections,
                  'compact_horizontal_overflow': overflow, 'replacement_uid': replacement['receiver_uid'],
                  'errors': errors, 'checks': ['real worker WebSocket cut and reconnect', 'actual output PCM resumes',
                  'no configuration writes during local recovery/mixing', 'follow to manual mix preserves editor',
                  'keyboard draft survives polling', 'actual frequency form confirmation',
                  'compact editor and processor controls', 'Pop out/Dock retains playback', 'workbench focus preserves result popouts',
                  'retirement releases audio and result view', 'slot reuse gets new UID', 'healthy receiver continues']}
        browser.close()
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-p4-recovery'))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    with (args.artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        proxy = None
        try:
            snapshot = setup(origin)
            proxy = TransportProxy(origin, snapshot['receivers'][0]['receiver_uid'])
            report = browser_gate(proxy, snapshot, args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: real browser network recovery, compact forms, Pop out/Dock and UID retirement (synthetic/replay)')
        finally:
            if proxy:
                proxy.close()
            stop(proc)


if __name__ == '__main__':
    main()
