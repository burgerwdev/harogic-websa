#!/usr/bin/env python3
"""Single-browser SDR audio diagnostics against the existing hardware service."""
import argparse
import json
import time
import urllib.error
import urllib.request

from playwright.sync_api import sync_playwright


def state(url):
    with urllib.request.urlopen(url + '/api/state', timeout=5) as response:
        return json.load(response)


def command(url, data):
    req = urllib.request.Request(url + '/api/config', data=json.dumps(data).encode(),
                                 headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=15) as response:
            result = json.load(response)
    except urllib.error.HTTPError as exc:
        raise RuntimeError(f'{data.get("cmd")} HTTP {exc.code}: '
                           f'{exc.read().decode("utf-8", "replace")}') from exc
    if not result.get('changed', True) and result.get('last_error'):
        raise RuntimeError(result['last_error'])
    return result


def diag(page):
    raw = page.locator('#spectrum').get_attribute('data-sdr-iq') or ''
    return dict(field.split('=', 1) for field in raw.split() if '=' in field)


def starvation_summary(before, snapshots, sample_times, elapsed):
    pairs = list(zip(sample_times, snapshots, strict=True))
    midpoint = next((row for at, row in pairs if at >= elapsed / 2), snapshots[-1])
    events = []
    previous = int(before.get('dsp_starved', 0))
    for at, row in pairs:
        current = int(row.get('dsp_starved', previous))
        if current > previous:
            events.append((round(at, 2), current - previous))
        previous = current
    return {'last_half_starved_delta': int(snapshots[-1].get('dsp_starved', 0)) -
                int(midpoint.get('dsp_starved', 0)),
            'starve_events': events[:5] + events[-5:] if len(events) > 10 else events}


def restore(url, initial):
    if state(url)['mode'] != 'sdr':
        command(url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    old = initial['sdr']
    command(url, {'cmd': 'SET_SDR', 'center': old['center'], 'decimate': old['decimate']})
    command(url, {'cmd': 'SET_SDR_TUNE', 'listen': old['listen']})
    command(url, {'cmd': 'SET_SDR_DEMOD', 'mode': old['demod'], 'ifbw': old['if_bw'],
                  'squelch': old['squelch'], 'agc': old['agc'], 'volume': old['volume']})
    command(url, {'cmd': 'SET_MODE', 'mode': 'std'})
    swp = initial['req']['swp']
    for payload in (
        {'cmd': 'SET_FREQ', 'center': swp['center'], 'span': swp['span']},
        {'cmd': 'SET_POINTS', 'points': swp['points']},
        {'cmd': 'SET_RBW', 'mode': swp['rbw_mode'], 'rbw': swp['rbw']},
        {'cmd': 'SET_VBW', 'mode': swp['vbw_mode'], 'vbw': swp['vbw']},
        {'cmd': 'SET_REF', 'mode': swp['ref_mode'], 'ref': swp['ref']},
        {'cmd': 'SET_SWEEP', 'mode': swp['sweep_time_mode'], 'time': swp['sweep_time']},
        {'cmd': 'SET_SPUR', 'mode': swp['spur']},
        {'cmd': 'SET_WINDOW', 'window': swp['window']},
        {'cmd': 'SET_DETECTOR', 'mode': swp['detector']},
        {'cmd': 'SET_FRAMEDET', 'mode': swp['frame_detector']},
        {'cmd': 'SET_TRACEGRID', 'trace_align': swp['trace_align'],
         'points_strategy': swp['points_strategy']},
    ):
        command(url, payload)
    if initial['mode'] != 'std':
        command(url, {'cmd': 'SET_MODE', 'mode': initial['mode']})


def run(url, seconds, decimates, profiles, download_bytes, demod='am', ifbw=12000):
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
            command(url, {'cmd': 'SET_SDR_DEMOD', 'mode': demod, 'ifbw': ifbw,
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
                    sample_times = []
                    started = time.monotonic()
                    while time.monotonic() - started < seconds:
                        time.sleep(min(0.2, max(0.0, seconds - (time.monotonic() - started))))
                        snapshots.append(diag(page))
                        sample_times.append(time.monotonic() - started)
                    elapsed = time.monotonic() - started
                    end = snapshots[-1]
                    starvation = starvation_summary(start, snapshots, sample_times, elapsed)
                    rta_end = int(page.locator('#spectrum').get_attribute('data-rta-frames') or 0)
                    last = state(url)
                    health = last.get('sdr', {}).get('health', {})
                    recent_flushes = page.evaluate('''() => (window.__wfmFlush || [])
                        .filter(e => e.source === 'seq0').slice(-3)
                        .map(e => ({fastPrime: e.fastPrime, frameRate: e.frameRate,
                                    prevRate: e.prevRate}))''')
                    capture_sps = (round((health['capture_samples'] - first['capture_samples']) / elapsed)
                                   if 'capture_samples' in health and 'capture_samples' in first else None)
                    browser_sps = round((int(end.get('samples', 0)) - int(start.get('samples', 0))) / elapsed)
                    worklet_played_sps = round((int(end.get('dsp_played', 0)) -
                                                 int(start.get('dsp_played', 0))) / elapsed)
                    worklet_rendered_sps = round((int(end.get('dsp_rendered', 0)) -
                                                   int(start.get('dsp_rendered', 0))) / elapsed)
                    worklet_starved_delta = (int(end.get('dsp_starved', 0)) -
                                             int(start.get('dsp_starved', 0)))
                    print(json.dumps({'decimate': decimate, 'profile': profile,
                        'elapsed_s': round(elapsed, 3), 'capture_sps': capture_sps,
                        'browser_iq_sps': browser_sps,
                        'worklet_played_sps': worklet_played_sps,
                        'worklet_rendered_sps': worklet_rendered_sps,
                        'worklet_starved_delta': worklet_starved_delta,
                        **starvation, 'rta_fps': round((rta_end-rta_start)/elapsed, 2),
                        'underruns_delta': int(end.get('dsp_underruns', 0)) - int(start.get('dsp_underruns', 0)),
                        'limit_bytes_s': download_bytes if profile == 'limited' else None,
                        'rta_frames': rta_end - rta_start,
                        'span': last.get('sdr', {}).get('actual', {}).get('bandwidth'),
                        'gnss_stale': last.get('gnss', {}).get('stale', False),
                        'recent_flushes': recent_flushes,
                        'iq_rate': last.get('sdr', {}).get('actual', {}).get('iq_rate'),
                        'ddc_rate': last.get('sdr', {}).get('actual', {}).get('ddc_rate'),
                        'iqbf_block_deadline_ms': round(8000 / last['sdr']['actual']['ddc_rate'] * 1000, 2)
                            if last.get('sdr', {}).get('actual', {}).get('ddc_rate') else None,
                        'ddc_delivery_rate': last.get('sdr', {}).get('actual', {}).get('ddc_delivery_rate'),
                        'before': {k: start.get(k) for k in ('blocks','samples','dropped','flushes','pcm_samples','dsp_delivered','dsp_received','dsp_rendered','dsp_played','dsp_starved','dsp_ratio','dsp_avail','dsp_underruns','dsp_slip','dsp_ring_resets','dsp_builds','dsp_discontinuities')},
                        'after': {k: end.get(k) for k in ('blocks','samples','dropped','flushes','pcm_samples','dsp_delivered','dsp_received','dsp_rendered','dsp_played','dsp_starved','dsp_ratio','dsp_avail','dsp_underruns','dsp_slip','dsp_ring_resets','dsp_builds','dsp_discontinuities','dsp_fill_ms','dsp_proc_ms','dsp_proc_avg_ms','dsp_block_avg_ms','dsp_mode','dsp_playing','dsp_error','cfg_frame_rate')},
                        'playing_samples': sum(row.get('dsp_playing') == '1' for row in snapshots),
                        'health_before': first, 'health_after': health,
                        'page_errors': errors[-3:]}, ensure_ascii=True), flush=True)
            cdp.send('Network.emulateNetworkConditions', {
                'offline': False, 'latency': 0, 'downloadThroughput': -1, 'uploadThroughput': -1,
            })
        finally:
            context.close()
            browser.close()
            restore(url, initial)


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--url', default='http://127.0.0.1:8080')
    p.add_argument('--seconds', type=int, default=8)
    p.add_argument('--decimates', type=int, nargs='+', default=[16, 32, 8])
    p.add_argument('--profiles', nargs='+', choices=('local', 'limited'), default=['local', 'limited'])
    p.add_argument('--download-bytes', type=int, default=700_000)
    p.add_argument('--demod', choices=('am', 'nfm', 'wfm'), default='am')
    p.add_argument('--ifbw', type=float, default=12000)
    args = p.parse_args()
    if args.seconds <= 0:
        p.error('--seconds must be positive')
    run(args.url, args.seconds, args.decimates, args.profiles, args.download_bytes,
        args.demod, args.ifbw)
