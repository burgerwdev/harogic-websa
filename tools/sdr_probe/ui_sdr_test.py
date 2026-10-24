#!/usr/bin/env python3
"""Playwright UI test for the SDR mode: enters SDR, tunes to a tinySA AM signal,
counts WebSocket binary frames in the browser, and checks the canvas renders."""
from __future__ import annotations

import json
import math
import os
import struct
import sys
import urllib.request
from fractions import Fraction
from urllib.parse import parse_qs, urlparse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from playwright.sync_api import sync_playwright  # noqa: E402
from tinysa import TinySA  # noqa: E402

URL = 'http://127.0.0.1:8080'
CENTER_MHZ = 100.0
LISTEN_MHZ = 100.2


def canvas_sig(page):
    return page.evaluate(
        "() => { const c=document.getElementById('spectrum'); const g=c.getContext('2d');"
        " const d=g.getImageData(0,0,c.width,c.height).data; let h=2166136261>>>0;"
        " for (let i=0;i<d.length;i+=7){ h^=d[i]; h=Math.imul(h,16777619)>>>0; } return h; }")


def main():
    sa = TinySA('/dev/ttyACM0')
    errors = []
    counts = {'rtaf': 0, 'aud2': 0, 'sde2': 0, 'other': 0}
    audio_sources = {}
    decode_sources = {}
    audio_header = struct.Struct('<4sIIIddIIQdIIQdQQ')
    try:
        sa.enter_low_output()
        sa.am(100.2e6, -25, 1000, 50)
        print('tinySA', sa.cmd('sweep'), flush=True)
        with sync_playwright() as p:
            browser = p.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
            page = browser.new_page(viewport={'width': 1600, 'height': 950})
            page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None)
            page.on('pageerror', lambda e: errors.append(str(e)))

            def on_ws(ws):
                def on_frame(frame):
                    data = getattr(frame, 'payload', frame)
                    if isinstance(data, str):
                        return
                    if not data or len(data) < 4:
                        return
                    magic = bytes(data[:4])
                    if magic in (b'RTAF', b'RTA2'):
                        counts['rtaf'] += 1
                    elif magic == b'AUD2':
                        if '/ws/audio' not in ws.url or len(data) < audio_header.size:
                            errors.append('AUD2 outside audio endpoint or truncated')
                            return
                        (magic, version, seq, frames, rate, freq, channel_id, epoch,
                         input_index, delay, channels, sample_format, audio_index,
                         input_rate, fraction_num, fraction_den) = audio_header.unpack_from(data)
                        if (version != 1 or sample_format & 0xFFFF != 1
                                or sample_format & ~0x30001 or not channel_id or channels not in (1, 2)
                                or not math.isfinite(rate) or rate <= 0
                                or not math.isfinite(input_rate) or input_rate <= 0
                                or not math.isfinite(freq) or not math.isfinite(delay) or delay < 0
                                or not fraction_den or fraction_num >= fraction_den
                                or len(data) != audio_header.size + frames * channels * 4):
                            errors.append('invalid AUD2 metadata/payload')
                            return
                        source = (channel_id, epoch, freq, rate, channels, input_rate)
                        previous = audio_sources.get(ws.url)
                        if seq == 0:
                            if frames or audio_index:
                                errors.append('AUD2 marker must have empty PCM at audio index zero')
                                return
                            audio_sources[ws.url] = {'source': source, 'seq': 0,
                                                     'audio_index': 0, 'frames': 0}
                        elif (not frames or previous is None or previous['source'] != source
                              or seq != (previous['seq'] + 1) & 0xFFFFFFFF
                              or (previous['audio_index'] is not None
                                  and audio_index != previous['audio_index'] + previous['frames'])):
                            errors.append('AUD2 source/sequence changed without marker')
                        else:
                            previous.update(seq=seq, audio_index=audio_index, frames=frames)
                            counts['aud2'] += 1
                    elif magic == b'SDE2':
                        if ('/ws/decode' not in ws.url or len(data) < 8 or len(data) - 8 > 2 * 1024 * 1024
                                or struct.unpack_from('<I', data, 4)[0] != len(data) - 8):
                            errors.append('invalid SDE2 endpoint/header/size')
                            return
                        try:
                            envelope = json.loads(data[8:])
                            slot = int(parse_qs(urlparse(ws.url).query).get('channel', ['1'])[0])
                            source = (envelope['channel_id'], envelope['epoch'], envelope['mode'],
                                      envelope['input_rate'], envelope['delay_seconds'],
                                      envelope['delay_known'], envelope['causal_presentation'])
                            seq = envelope['seq']
                            numerator = int(envelope['input_fraction_num'])
                            denominator = int(envelope['input_fraction_den'])
                            input_index = int(envelope['input_index'])
                            if (envelope['version'] != 1 or envelope['channel'] != slot
                                    or not envelope['channel_id'] or not 0 <= seq <= 0xFFFFFFFF
                                    or input_index < 0 or denominator <= 0 or not 0 <= numerator < denominator
                                    or not math.isfinite(source[3]) or source[3] <= 0
                                    or not math.isfinite(source[4]) or source[4] < 0
                                    or not isinstance(source[5], bool) or not isinstance(source[6], bool)):
                                raise ValueError('invalid source metadata')
                            position = Fraction(input_index) + Fraction(numerator, denominator)
                            previous = decode_sources.get(ws.url)
                            if seq == 0:
                                if envelope['event']['type'] != 'start':
                                    raise ValueError('missing start marker')
                            elif (previous is None or previous['source'][:4] != source[:4]
                                  or seq != (previous['seq'] + 1) & 0xFFFFFFFF
                                  or position < previous['position']):
                                raise ValueError('source/sequence changed without start marker')
                            decode_sources[ws.url] = {'source': source, 'seq': seq, 'position': position}
                            counts['sde2'] += 1
                        except (KeyError, TypeError, ValueError, UnicodeError) as exc:
                            errors.append(f'invalid SDE2: {exc}')
                    else:
                        counts['other'] += 1
                ws.on('framereceived', on_frame)
            page.on('websocket', on_ws)

            page.goto(URL, wait_until='networkidle')
            page.wait_for_timeout(1500)
            page.click('#btn-mode-sdr')
            page.wait_for_selector('#sdr-settings', state='visible', timeout=10000)
            page.wait_for_function("() => document.body.classList.contains('rta-mode')", timeout=10000)
            page.fill('#input-sdr-center', str(CENTER_MHZ))
            page.click('button[data-action="apply-sdr"]')
            page.wait_for_timeout(1500)
            page.fill('#input-sdr-listen', str(LISTEN_MHZ))
            page.click('button[data-action="apply-sdr-tune"]')
            page.click('button[data-sdr-demod="am"]')
            page.click('button[data-sdr-ifbw="6000"]')
            page.click('#btn-sdr-audio')
            page.wait_for_timeout(3500)

            st = json.load(urllib.request.urlopen('http://127.0.0.1:8080/api/state', timeout=3))
            primary = next((ch for ch in st.get('sdr', {}).get('channels', [])
                            if ch.get('channel') == 1), {})
            print('backend mode:', st.get('mode'), 'actual listen:', primary.get('freq'), flush=True)
            for url, receipt in audio_sources.items():
                channel_slot = int(parse_qs(urlparse(url).query).get('channel', ['1'])[0])
                channel = next((ch for ch in st.get('sdr', {}).get('channels', [])
                                if ch.get('channel') == channel_slot), {})
                if receipt['source'][:2] != (channel.get('channel_id'), channel.get('epoch')):
                    errors.append(f'AUD2 source disagrees with channel {channel_slot} status')
            for url, receipt in decode_sources.items():
                channel_slot = int(parse_qs(urlparse(url).query).get('channel', ['1'])[0])
                channel = next((ch for ch in st.get('sdr', {}).get('channels', [])
                                if ch.get('channel') == channel_slot), {})
                if receipt['source'][:2] != (channel.get('channel_id'), channel.get('epoch')):
                    errors.append(f'SDE2 source disagrees with channel {channel_slot} status')
            if not counts['aud2']:
                errors.append('no native PCM received')
            sig1 = canvas_sig(page)
            page.wait_for_timeout(1200)
            sig2 = canvas_sig(page)
            active = page.eval_on_selector('#btn-mode-sdr', 'el => el.classList.contains("active")')
            level = page.eval_on_selector('#cur-sdr-level', 'el => el.textContent')
            print(f'frames rtaf={counts["rtaf"]} aud2={counts["aud2"]} '
                  f'sde2={counts["sde2"]} other={counts["other"]}')
            print(f'canvas sig1={sig1} sig2={sig2} changed={sig1 != sig2}')
            print(f'sdr button active={active} level="{level}" channel={primary}')
            page.screenshot(path='/tmp/sdr_ui.png')
            print('errors:', errors[:8])
            browser.close()
    finally:
        try:
            sa.off()
        except Exception:
            pass
        sa.ser.close()


if __name__ == '__main__':
    main()
