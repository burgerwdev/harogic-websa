#!/usr/bin/env python3
"""Single-browser SDR audio diagnostics against the existing hardware service."""
import argparse
import json
import time
import urllib.request

from playwright.sync_api import sync_playwright


def state(url):
    with urllib.request.urlopen(url + '/api/state', timeout=5) as response:
        return json.load(response)


def command(url, data):
    req = urllib.request.Request(url + '/api/config', data=json.dumps(data).encode(),
                                 headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=15) as response:
        result = json.load(response)
    if not result.get('changed', True) and result.get('last_error'):
        raise RuntimeError(result['last_error'])
    return result


def diag(page):
    raw = page.locator('#spectrum').get_attribute('data-sdr-iq') or ''
    return dict(field.split('=', 1) for field in raw.split() if '=' in field)


def run(url, seconds, decimates, profiles, download_bytes):
    initial = state(url)
    print('initial:', json.dumps({key: initial.get(key) for key in ('mode', 'connected', 'status_warning')}), flush=True)
    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=True, args=['--autoplay-policy=no-user-gesture-required'])
        context = browser.new_context(viewport={'width': 1440, 'height': 900})
        page = context.new_page()
        errors = []
        page.on('pageerror', lambda err: errors.append(str(err)))
        page.on('dialog', lambda dialog: dialog.dismiss())
        try:
            page.goto(url, wait_until='networkidle', timeout=25000)
            page.evaluate("document.getElementById('btn-mode-sdr').click()")
            page.wait_for_timeout(800)
            command(url, {'cmd': 'SET_SDR', 'center': 100e6, 'decimate': 16})
            command(url, {'cmd': 'SET_SDR_TUNE', 'listen': 100e6})
            command(url, {'cmd': 'SET_SDR_DEMOD', 'mode': 'am', 'ifbw': 12000,
                          'squelch': -120, 'agc': False, 'volume': 0.8})
            page.wait_for_timeout(1300)
            # Audio button text is on/off; click only if disabled.
            button = page.locator('#btn-sdr-audio')
            if 'on' not in (button.inner_text() or '').lower():
                button.evaluate('(el) => el.click()')
            page.wait_for_timeout(2500)
            cdp = context.new_cdp_session(page)
            cdp.send('Network.enable')
            for decimate in decimates:
                command(url, {'cmd': 'SET_SDR', 'center': 100e6, 'decimate': decimate})
                page.wait_for_timeout(2000)
                for profile in profiles:
                    # CDP throughput is bytes/s; verify whether IQBF gets throttled via blocks/s.
                    cdp.send('Network.emulateNetworkConditions', {
                        'offline': False, 'latency': 80 if profile == 'limited' else 0,
                        'downloadThroughput': download_bytes if profile == 'limited' else -1,
                        'uploadThroughput': download_bytes if profile == 'limited' else -1,
                    })
                    page.wait_for_timeout(1000)
                    start = diag(page)
                    rta_start = int(page.locator('#spectrum').get_attribute('data-rta-frames') or 0)
                    first = state(url).get('sdr', {}).get('health', {})
                    snapshots = []
                    for _ in range(seconds * 5):
                        time.sleep(0.2)
                        snapshots.append(diag(page))
                    end = snapshots[-1]
                    rta_end = int(page.locator('#spectrum').get_attribute('data-rta-frames') or 0)
                    last = state(url)
                    print(json.dumps({'decimate': decimate, 'profile': profile,
                        'limit_bytes_s': download_bytes if profile == 'limited' else None,
                        'rta_frames': rta_end - rta_start,
                        'span': last.get('sdr', {}).get('actual', {}).get('bandwidth'),
                        'iq_rate': last.get('sdr', {}).get('actual', {}).get('iq_rate'),
                        'ddc_rate': last.get('sdr', {}).get('actual', {}).get('ddc_rate'),
                        'ddc_delivery_rate': last.get('sdr', {}).get('actual', {}).get('ddc_delivery_rate'),
                        'before': {k: start.get(k) for k in ('blocks','samples','dropped','flushes','pcm_samples','dsp_delivered','dsp_received','dsp_ratio','dsp_avail','dsp_underruns','dsp_slip','dsp_ring_resets','dsp_builds','dsp_discontinuities')},
                        'after': {k: end.get(k) for k in ('blocks','samples','dropped','flushes','pcm_samples','dsp_delivered','dsp_received','dsp_ratio','dsp_avail','dsp_underruns','dsp_slip','dsp_ring_resets','dsp_builds','dsp_discontinuities','dsp_fill_ms','dsp_proc_ms','dsp_mode','dsp_playing','dsp_error','cfg_frame_rate')},
                        'playing_samples': sum(row.get('dsp_playing') == '1' for row in snapshots),
                        'health_before': first, 'health_after': last.get('sdr', {}).get('health', {}),
                        'page_errors': errors[-3:]}, ensure_ascii=True), flush=True)
            cdp.send('Network.emulateNetworkConditions', {
                'offline': False, 'latency': 0, 'downloadThroughput': -1, 'uploadThroughput': -1,
            })
        finally:
            context.close()
            browser.close()
            old = initial['sdr']
            command(url, {'cmd': 'SET_SDR', 'center': old['center'], 'decimate': old['decimate']})
            command(url, {'cmd': 'SET_SDR_TUNE', 'listen': old['listen']})
            command(url, {'cmd': 'SET_SDR_DEMOD', 'mode': old['demod'], 'ifbw': old['if_bw'],
                          'squelch': old['squelch'], 'agc': old['agc'], 'volume': old['volume']})
            if initial['mode'] != 'sdr':
                command(url, {'cmd': 'SET_MODE', 'mode': initial['mode']})


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--url', default='http://127.0.0.1:8080')
    p.add_argument('--seconds', type=int, default=8)
    p.add_argument('--decimates', type=int, nargs='+', default=[16, 32, 8])
    p.add_argument('--profiles', nargs='+', choices=('local', 'limited'), default=['local', 'limited'])
    p.add_argument('--download-bytes', type=int, default=700_000)
    args = p.parse_args()
    run(args.url, args.seconds, args.decimates, args.profiles, args.download_bytes)
