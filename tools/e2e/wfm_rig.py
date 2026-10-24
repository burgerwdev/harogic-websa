#!/usr/bin/env python3
"""Hardware WFM mono/stereo/RDS probe for PlutoSDR -> SAN-90.

Drive the operator UI, record native SDE2 WFM events and AUD2 playback diagnostics.
Use --scenario hold, retune or signal-loss; --out saves the evidence. Native
receiver input capture belongs to the server; browser IQ dumping is unavailable.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import wave
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent.parent
SPEECH = ROOT / 'tools' / 'bench' / 'audio' / 'speech.wav'
failures: list[str] = []
TX_ARGS = {
    'stereo': ['wfm', '--stereo', '__STEREO_WAV__', '--rds-rt', 'RIG STEREO RT',
               '--rds-ps', 'RIG ST', '--rds-pi', '0x4147'],
    'stereo-no-rds': ['wfm', '--stereo', '__STEREO_WAV__',
                      '--rds-rt', 'DUMMY', '--rds-deviation-hz', '0'],
    'rds': ['wfm', '--no-audio', '--rds-rt', 'RIG RDS OK 0123456789', '--rds-ps',
            'RIG RDS', '--rds-pi', '0x1234'],
    'mono': ['wfm', '--wav', str(SPEECH)],
    'none': None,
}


def check(label: str, ok: bool, detail: str = '') -> None:
    print(f'  {"PASS" if ok else "FAIL"}  {label}' + (f'  <- {detail}' if detail else ''), flush=True)
    if not ok:
        failures.append(label)


def make_stereo_wav(out: Path) -> None:
    """Left-only speech as a stereo WAV."""
    with wave.open(str(SPEECH)) as src:
        if src.getnchannels() != 1 or src.getsampwidth() != 2:
            raise ValueError(f'{SPEECH}: need mono 16-bit PCM')
        rate, frames = src.getframerate(), src.readframes(src.getnframes())
    stereo = bytearray()
    for k in range(0, len(frames), 2):
        stereo.extend(frames[k:k + 2])
        stereo.extend(b'\x00\x00')
    with wave.open(str(out), 'wb') as audio:
        audio.setnchannels(2)
        audio.setsampwidth(2)
        audio.setframerate(rate)
        audio.writeframes(bytes(stereo))


def start_tx(scenario: str, freq_hz: float, gain_db: float) -> subprocess.Popen | None:
    spec = TX_ARGS[scenario]
    if spec is None:
        return None
    tmp = Path('/tmp/rig_stereo.wav')
    if '__STEREO_WAV__' in spec:
        make_stereo_wav(tmp)
        spec = [str(tmp) if a == '__STEREO_WAV__' else a for a in spec]
    cmd = [sys.executable, str(ROOT / 'tools' / 'pluto_tx.py'), *spec,
           '--lo', str(freq_hz), '--gain', str(gain_db)]
    print(f'TX: {" ".join(cmd)}', flush=True)
    with open('/tmp/wfm_rig_tx.log', 'w') as log:
        return subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)


def post(url: str, payload: dict) -> dict:
    import urllib.request
    request = urllib.request.Request(f'{url}/api/config', data=json.dumps(payload).encode(),
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def diagnostics(page, name: str) -> dict:
    raw = page.evaluate('(name) => document.getElementById("spectrum").dataset[name] || ""', name)
    return dict(part.split('=', 1) for part in raw.split() if '=' in part)


def tune_ui(page, freq_hz: float) -> int:
    return page.evaluate('''(mhz) => {
      const el = document.getElementById('input-sdr-listen');
      const at = Date.now();
      el.value = mhz.toFixed(4);
      el.dispatchEvent(new Event('change', {bubbles: true}));
      return at;
    }''', freq_hz / 1e6)


def native_events(page) -> list[dict]:
    return page.evaluate('window.__rigNative.events')


def latest_wfm(page) -> dict:
    return page.evaluate('''() => {
      const rig = window.__rigNative;
      const last = rig.events.at(-1);
      return last?.event.type === 'wfm-info' ? last.event : {};
    }''')


def rf_present(info: dict, tx: str) -> bool:
    if tx == 'mono':
        return float(info.get('peakDevHz', 0)) > 3000 and float(info.get('peakDbFs', -160)) > -40
    return float(info.get('pilotDevHz', 0)) > 3000 and float(info.get('pilotSnrDb', -120)) > 9


def wait_rf(page, tx: str, seconds: float = 12) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if rf_present(latest_wfm(page), tx):
            return True
        page.wait_for_timeout(200)
    return False


def sample(page, seconds: float, event: str = '') -> list[dict]:
    timeline = []
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        timeline.append({'at': page.evaluate('Date.now()'), 'event': event,
                         'audio': diagnostics(page, 'sdrAudio'),
                         'decode': diagnostics(page, 'sdrIq')})
        page.wait_for_timeout(20)
    return timeline


def read_ui(page) -> dict:
    return page.evaluate('''() => {
      const text = (id) => document.getElementById(id)?.textContent?.trim() || '';
      return {badge: text('wfm-title-badge'), pilot: text('wfm-pilot'),
        station: text('wfm-station'), rt: text('wfm-rt'), pi: text('wfm-rds-pi'),
        carrier: text('wfm-carrier')};
    }''')


def check_ui(page, tx: str) -> dict:
    ui = read_ui(page)
    info = latest_wfm(page)
    mono = tx == 'mono' or info.get('stereo') is False
    check('FM badge matches native stereo state', ui['badge'] == ('MONO' if mono else 'STEREO'), str(ui))
    if tx in ('stereo', 'rds'):
        check('RDS PI/PS/RT rendered', ui['station'].startswith('RIG')
              and ui['rt'].startswith('RIG') and ui['pi'].startswith('PI '), str(ui))
    if tx in ('mono', 'stereo-no-rds'):
        check('pane has no RDS data', ui['pi'] == '—' and ui['rt'] == '—', str(ui))
    return ui


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--freq', type=float, default=411e6)
    parser.add_argument('--away-hz', type=float, default=500e3)
    parser.add_argument('--gain', type=float, default=-10.0)
    parser.add_argument('--tx', choices=TX_ARGS, default='stereo')
    parser.add_argument('--scenario', choices=('hold', 'retune', 'signal-loss'), default='hold')
    parser.add_argument('--seconds', type=float, default=20.0)
    parser.add_argument('--out', default='')
    parser.add_argument('--check-ui', action='store_true')
    parser.add_argument('--viewport', default='1280x720')
    args = parser.parse_args()
    if args.scenario == 'signal-loss' and args.tx not in ('stereo', 'rds'):
        parser.error('signal-loss requires an RDS transmitter (--tx stereo or rds)')
    tx = start_tx(args.tx, args.freq, args.gain)
    report = {'scenario': args.scenario, 'tx': args.tx, 'freq': args.freq}
    try:
        time.sleep(2)
        if tx and tx.poll() is not None:
            raise RuntimeError(f'Pluto TX exited {tx.returncode}: ' + Path('/tmp/wfm_rig_tx.log').read_text()[-1200:])
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        post(args.url, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True, 'mode': 'wfm', 'ifbw': 180000, 'stereo': True})
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            width, height = (int(part) for part in args.viewport.lower().split('x'))
            page = browser.new_page(viewport={'width': width, 'height': height})
            errors, dialogs = [], []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.on('dialog', lambda dialog: (dialogs.append(dialog.message), dialog.dismiss()))
            page.goto(args.url, wait_until='networkidle')
            page.evaluate("document.getElementById('btn-mode-sdr').click()")
            page.wait_for_selector('[data-sdr-demod="wfm"]', state='attached')
            page.evaluate("document.querySelector('[data-sdr-demod=\"wfm\"]').click()")
            page.wait_for_timeout(300)
            check('WFM gets a wide IF', page.evaluate(
                "document.querySelector('[data-sdr-ifbw=\"180000\"]').classList.contains('active')"))
            page.evaluate('''() => {
              const socket = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws/decode`);
              socket.binaryType = 'arraybuffer';
              const rig = window.__rigNative = {events: [], errors: [], socket};
              socket.onmessage = ({data}) => {
                try {
                  const bytes = new Uint8Array(data), view = new DataView(data);
                  if (bytes.length < 8 || String.fromCharCode(...bytes.subarray(0, 4)) !== 'SDE2'
                      || view.getUint32(4, true) !== bytes.length - 8) throw Error('Invalid SDE2 frame');
                  const envelope = JSON.parse(new TextDecoder().decode(bytes.subarray(8)));
                  if (envelope.version !== 1) throw Error('Unsupported SDE2 version');
                  if (envelope.channel === 1 && ['start', 'wfm-info'].includes(envelope.event.type))
                    rig.events.push({...envelope, at: Date.now()});
                } catch (error) { rig.errors.push(String(error)); }
              };
              socket.onerror = () => rig.errors.push('Native decode socket error');
            }''')
            page.evaluate('''() => {
              const rig = window.__rigNative;
              rig.audio = {channels: [], stereoFrames: 0, differentFrames: 0};
              const socket = rig.audioSocket = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws/audio?channel=1`);
              socket.binaryType = 'arraybuffer';
              socket.onmessage = ({data}) => {
                try {
                  const view = new DataView(data), bytes = new Uint8Array(data);
                  if (bytes.length < 96 || String.fromCharCode(...bytes.subarray(0, 4)) !== 'AUD2'
                      || view.getUint32(4, true) !== 1) throw Error('Invalid AUD2 frame');
                  const frames = view.getUint32(12, true), channels = view.getUint32(56, true);
                  if (![1, 2].includes(channels) || bytes.length !== 96 + frames * channels * 4)
                    throw Error('Invalid AUD2 channels or payload');
                  if (!rig.audio.channels.includes(channels)) rig.audio.channels.push(channels);
                  if (channels === 2) {
                    rig.audio.stereoFrames += frames;
                    for (let i = 0; i < frames; i++) {
                      if (Math.abs(view.getFloat32(96 + i * 8, true)
                          - view.getFloat32(100 + i * 8, true)) > 1e-5) rig.audio.differentFrames++;
                    }
                  }
                } catch (error) { rig.errors.push(String(error)); }
              };
              socket.onerror = () => rig.errors.push('Native audio socket error');
            }''')
            page.evaluate('''() => {
              const button = document.getElementById('btn-sdr-audio');
              if (button && !button.classList.contains('active')) button.click();
            }''')
            page.evaluate('''(mhz) => {
              const el = document.getElementById('input-sdr-center');
              el.value = mhz.toFixed(4); el.dispatchEvent(new Event('change', {bubbles: true}));
            }''', args.freq / 1e6)
            page.wait_for_timeout(800)
            tune_ui(page, args.freq)
            page.wait_for_function('''() => {
              const raw = document.getElementById('spectrum').dataset.sdrAudio || '';
              return raw.includes('playing=1') && /frames=[1-9]/.test(raw);
            }''', timeout=15000)
            check('native receiver stream connected', diagnostics(page, 'sdrIq').get('backend') == '1'
                  and diagnostics(page, 'sdrIq').get('pipeline') == 'true', str(diagnostics(page, 'sdrIq')))
            if tx:
                on_air = wait_rf(page, args.tx)
                check('RF signal reaches native WFM receiver', on_air, str(latest_wfm(page)))
                if not on_air:
                    raise RuntimeError('no known RF signal; timing would test noise only')
            if args.check_ui:
                page.evaluate("document.getElementById('btn-decode-window').click()")
                page.wait_for_timeout(300)
                check('FM pane opens and fits viewport', page.evaluate('''() => {
                  const el = document.getElementById('decode-window'), r = el.getBoundingClientRect();
                  return getComputedStyle(el).display !== 'none' && r.left >= 0 && r.top >= 0
                    && r.right <= innerWidth && r.bottom <= innerHeight;
                }'''))
            timeline = []
            if args.scenario == 'hold':
                timeline = sample(page, args.seconds)
                report['summary'] = latest_wfm(page)
                report['audio'] = page.evaluate('window.__rigNative.audio')
                audio = report['audio']
                info = report['summary']
                check('WFM explicitly reports mono or stereo', isinstance(info.get('stereo'), bool), str(info))
                check('stereo-enabled WFM emits two AUD2 channels', audio['channels'] == [2], str(audio))
                if args.tx in ('stereo', 'stereo-no-rds') and info.get('stereo') is True:
                    check('locked stereo carries distinct left and right AUD2 samples',
                          2 in audio['channels'] and audio['differentFrames'] > 0, str(audio))
                post(args.url, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'stereo': False})
                try:
                    # Allow the configuration boundary and old frames to drain before sampling.
                    page.wait_for_timeout(1000)
                    page.evaluate('window.__rigNative.audio = {channels: [], stereoFrames: 0, differentFrames: 0}')
                    sample(page, 2)
                    mono_audio = page.evaluate('window.__rigNative.audio')
                    report['monoAudio'] = mono_audio
                    check('explicit stereo=false emits mono AUD2', mono_audio['channels'] == [1], str(mono_audio))
                finally:
                    post(args.url, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'stereo': True})
                if args.check_ui:
                    report['ui'] = check_ui(page, args.tx)
            elif args.scenario == 'signal-loss':
                timeline = sample(page, 8)
                check('RDS locked before RF loss', latest_wfm(page).get('rdsLocked') is True)
                report['signalStoppedAt'] = page.evaluate('Date.now()')
                tx.terminate()
                tx.wait(timeout=10)
                tx = None
                timeline += sample(page, args.seconds, 'transmitter-off')
                state = latest_wfm(page)
                report['summary'] = state
                check('pilot lock drops after RF loss', bool(state) and not state['stereo'] and state['pilotSnrDb'] < 6)
                check('RDS station metadata clears after RF loss', bool(state) and not state['rdsLocked'])
            else:
                timings = []
                for target in (args.freq + args.away_hz, args.freq) * 2:
                    old_key = diagnostics(page, 'sdrAudio').get('stream_key')
                    mark = tune_ui(page, target)
                    rows = sample(page, 6, f'retune->{target / 1e6:.4f}MHz')
                    timeline += rows
                    events = [e for e in native_events(page) if e['at'] >= mark]
                    start = next((e for e in events if e['event']['type'] == 'start'
                                  and f"{e['epoch']}:{e['channel_id']}" != old_key), None)
                    key = f"{start['epoch']}:{start['channel_id']}" if start else None
                    played = next((r for r in rows if key and r['audio'].get('stream_key') == key
                                   and r['audio'].get('playing') == '1'), None)
                    pilot = next((e for e in events if key == f"{e['epoch']}:{e['channel_id']}"
                                  and e['event']['type'] == 'wfm-info' and rf_present(e['event'], args.tx)), None)
                    timing = {'target': target, 'start_ms': start['at'] - mark if start else None,
                              'playback_ms': played['at'] - mark if played else None,
                              'rf_ms': pilot['at'] - mark if pilot else None}
                    timings.append(timing)
                    check('retune command to new-channel playback within 500 ms',
                          timing['playback_ms'] is not None and timing['playback_ms'] <= 500, str(timing))
                    if tx and target == args.freq:
                        check('RF returns within 500 ms', timing['rf_ms'] is not None and timing['rf_ms'] <= 500, str(timing))
                report['summary'] = timings
                if args.check_ui:
                    report['ui'] = check_ui(page, args.tx)
            report['timeline'] = timeline
            report['events'] = native_events(page)
            protocol_errors = page.evaluate('window.__rigNative.errors')
            check('browser has no page errors, dialogs or protocol errors', not errors and not dialogs and not protocol_errors,
                  str({'errors': errors, 'dialogs': dialogs, 'protocol': protocol_errors}))
            if args.check_ui:
                page.screenshot(path=f'/tmp/wfm_ui_{args.tx}_{args.viewport}.png')
            page.evaluate('window.__rigNative.socket.close()')
            browser.close()
    finally:
        if tx:
            if tx.poll() is not None:
                check('Pluto TX stayed on air', False, f'exit={tx.returncode} ' + Path('/tmp/wfm_rig_tx.log').read_text()[-600:])
            else:
                tx.terminate()
                tx.wait(timeout=10)
    print(json.dumps(report.get('summary'), indent=2))
    if args.out:
        Path(args.out).write_text(json.dumps(report))
        print(f'wrote {args.out}')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
