#!/usr/bin/env python3
"""Playwright hardware rig for WFM mono/stereo/RDS on PlutoSDR -> SAN-90.

Drive the real app UI. Read the per-block `__wfmTrail` and AudioWorklet gate events.
The page enables the trail only for this bench run. `--out` saves raw evidence.

    python3 tools/e2e/wfm_rig.py --scenario hold --tx stereo --seconds 30 --check-ui
    python3 tools/e2e/wfm_rig.py --scenario retune --tx stereo
    python3 tools/e2e/wfm_rig.py --scenario signal-loss --tx rds --seconds 12

`hold` measures badge, audio and RDS. `retune` measures click to new RF/PCM playback
against the 500 ms contract. `signal-loss` checks stale metadata expiry. `--dump-iq`
saves the exact browser input for offline WASM replay.
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
    # Left-only speech exposes channel leakage; a shared tone cannot prove separation.
    'stereo': ['wfm', '--stereo', '__STEREO_WAV__', '--rds-rt', 'RIG STEREO RT',
               '--rds-ps', 'RIG ST', '--rds-pi', '0x4147'],
    'stereo-no-rds': ['wfm', '--stereo', '__STEREO_WAV__',
                      '--rds-rt', 'DUMMY', '--rds-deviation-hz', '0'],
    # RDS-only programme with the pilot needed for bit timing.
    'rds': ['wfm', '--no-audio', '--rds-rt', 'RIG RDS OK 0123456789', '--rds-ps',
            'RIG RDS', '--rds-pi', '0x1234'],
    # Mono WAV: no pilot and no RDS.
    'mono': ['wfm', '--wav', str(SPEECH)],
    'none': None,
}


def check(label: str, ok: bool, detail: str = '') -> None:
    print(f'  {"PASS" if ok else "FAIL"}  {label}' + (f'  <- {detail}' if detail else ''),
          flush=True)
    if not ok:
        failures.append(label)


def make_stereo_wav(out: Path) -> None:
    """Left-only speech as a stereo WAV (the separation test's source)."""
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


def iq_diag(page) -> dict:
    raw = page.evaluate("document.getElementById('spectrum').dataset.sdrIq || ''")
    out: dict[str, str] = {}
    for part in raw.split():
        if '=' in part:
            key, value = part.split('=', 1)
            out[key] = value
    return out


def tune_ui(page, freq_hz: float) -> None:
    """Retune through the operator's path: the listen field's change handler."""
    mhz = freq_hz / 1e6
    page.evaluate(
        """(mhz) => {
          const w = window;
          w.__retuneMarks = (w.__retuneMarks || []).concat([Date.now()]);
          const el = document.getElementById('input-sdr-listen');
          el.value = mhz.toFixed(4);
          el.dispatchEvent(new Event('change', {bubbles: true}));
        }""", mhz)


def wait_gate(page, seconds: float) -> bool:
    """Wait for the worklet to actually play (the port hand-off and the prime are not instant)."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        diag = iq_diag(page)
        if diag.get('dsp_playing') == '1' and int(diag.get('blocks', '0')) > 20:
            return True
        time.sleep(0.2)
    return False


def wait_rf(page, scenario: str, freq_hz: float, seconds: float = 12.0) -> bool:
    """Confirm RF source on the tuned channel; a noise stream also opens the audio gate."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        info = page.evaluate('window.__wfmState || null')
        if info and abs(float(info.get('carrierHz', 0)) - freq_hz) < 1:
            if scenario == 'mono':
                if float(info.get('peakDevHz', 0)) > 3_000 and float(info.get('pcmRms', 0)) > 0.01:
                    return True
            elif float(info.get('pilotDevHz', 0)) > 3_000 and float(info.get('pilotSnrDb', -120)) > 9:
                return True
        time.sleep(0.2)
    return False


def wait_wfm(page, seconds: float) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if iq_diag(page).get('dsp_mode') == 'wfm':
            return
        time.sleep(0.5)


def sample(page, seconds: float, event: str = '') -> list[dict]:
    """Sample the diagnostics at ~20 ms into a timeline (the rig's own evidence)."""
    timeline: list[dict] = []
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        diag = iq_diag(page)
        trail = page.evaluate('window.__wfmTrail ? window.__wfmTrail.length : 0')
        timeline.append({
            't': round(time.monotonic() * 1000, 1),
            'event': event,
            'playing': diag.get('dsp_playing'),
            'underruns': diag.get('dsp_underruns'),
            'resets': diag.get('dsp_ring_resets'),
            'delivered': diag.get('dsp_delivered'),
            'flushes': diag.get('flushes'),
            'fill': diag.get('dsp_fill_ms'),
            'mode': diag.get('dsp_mode'),
            'trail': trail,
        })
        time.sleep(0.02)
    return timeline


def audio_gaps(timeline: list[dict]) -> list[dict]:
    """Playing->silent->playing spans, with the surrounding timestamps."""
    gaps: list[dict] = []
    down_at = None
    for row in timeline:
        if row['playing'] == '0' and down_at is None:
            down_at = row['t']
        elif row['playing'] == '1' and down_at is not None:
            gaps.append({'from': down_at, 'to': row['t'], 'ms': round(row['t'] - down_at, 0)})
            down_at = None
    return gaps


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--freq', type=float, default=411e6, help='the TX LO and the listen frequency')
    parser.add_argument('--away-hz', type=float, default=500e3, help='the retune scenario step')
    parser.add_argument('--gain', type=float, default=-10.0)
    parser.add_argument('--tx', choices=TX_ARGS, default='stereo')
    parser.add_argument('--scenario', choices=('hold', 'retune', 'signal-loss'), default='hold')
    parser.add_argument('--seconds', type=float, default=20.0)
    parser.add_argument('--out', default='')
    parser.add_argument('--dump-iq', default='', help='stash the decoder input frames to this file')
    parser.add_argument('--dump-after-retune', default='', help='stash frames after last return to TX frequency')
    parser.add_argument('--check-ui', action='store_true', help='open FM pane and assert rendered fields')
    parser.add_argument('--viewport', default='1280x720', help='browser viewport WIDTHxHEIGHT')
    args = parser.parse_args()

    tx = start_tx(args.tx, args.freq, args.gain)
    report: dict = {'scenario': args.scenario, 'tx': args.tx, 'freq': args.freq}
    try:
        time.sleep(2.0)  # the transmitter's buffer fills before the RF starts
        if tx and tx.poll() is not None:
            raise RuntimeError(f'Pluto TX exited {tx.returncode}: '
                               + Path('/tmp/wfm_rig_tx.log').read_text()[-1200:])
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            width, height = (int(part) for part in args.viewport.lower().split('x'))
            page = browser.new_page(viewport={'width': width, 'height': height})
            page.add_init_script('window.__wfmTrailEnabled = true')
            page_errors: list[str] = []
            dialogs: list[str] = []
            page.on('pageerror', lambda error: page_errors.append(str(error)))
            page.on('dialog', lambda dialog: (dialogs.append(dialog.message), dialog.dismiss()))
            page.goto(args.url, wait_until='networkidle')
            page.evaluate("document.getElementById('btn-mode-sdr').click()")
            page.wait_for_timeout(1500)
            page.evaluate("document.querySelector('[data-sdr-demod=\"wfm\"]').click()")
            page.wait_for_timeout(300)
            check('WFM gets a wide IF', page.evaluate(
                "document.querySelector('[data-sdr-ifbw=\"180000\"]').classList.contains('active')"))
            if (page.query_selector('#btn-sdr-audio')):
                # Idempotent: the button is also the OFF switch, and a persisted On preference
                # made the blind click mute the session (the ring then filled to its ceiling).
                state = page.evaluate("document.querySelector('#btn-sdr-audio').textContent.trim()")
                if state.lower() != 'on':
                    page.evaluate("document.querySelector('#btn-sdr-audio').click()")
            # The capture centre and the listen frequency both go through the operator's fields:
            # the centre change re-plans the channelizer, the listen change tunes inside it.
            page.evaluate(
                """(mhz) => {
                  const c = document.getElementById('input-sdr-center');
                  c.value = mhz.toFixed(4);
                  c.dispatchEvent(new Event('change', {bubbles: true}));
                }""", args.freq / 1e6)
            page.wait_for_timeout(800)
            tune_ui(page, args.freq)
            wait_wfm(page, 10)
            check('the browser DSP runs WFM', iq_diag(page).get('dsp_mode') == 'wfm',
                  json.dumps(iq_diag(page)))
            check('the worklet plays', wait_gate(page, 15),
                  'audio gate: ' + json.dumps(iq_diag(page)))
            if tx:
                on_air = wait_rf(page, args.tx, args.freq)
                check('RF signal reaches SAN-90', on_air,
                      json.dumps(page.evaluate('window.__wfmState || null')))
                if not on_air:
                    raise RuntimeError('no known RF signal; retune timing would test noise only')
            if args.check_ui:
                page.evaluate("document.getElementById('btn-decode-window').click()")
                page.wait_for_timeout(300)
                check('FM pane opens', page.evaluate(
                    "getComputedStyle(document.getElementById('decode-window')).display !== 'none'"))
                check('FM pane fits viewport', page.evaluate("""() => {
                  const r = document.getElementById('decode-window').getBoundingClientRect();
                  return r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight;
                }"""))
            # DIAG: capture the decoder's exact input frames for offline replay.
            if args.dump_iq:
                page.evaluate("window.dispatchEvent(new CustomEvent('wfm-dump-iq', {detail: true}))")
                deadline = time.monotonic() + 45
                dump = None
                while time.monotonic() < deadline:
                    dump = page.evaluate('window.__wfmIqDump || null')
                    if dump:
                        break
                    time.sleep(0.5)
                if dump:
                    Path(args.dump_iq).write_text(json.dumps(dump))
                    frames = dump.get('frames', [])
                    total = sum(len(f) for f in frames)
                    print(f'IQ dump: {len(frames)} frames, {total} f32, rate {dump.get("rate")}',
                          flush=True)
                else:
                    print('IQ dump: FAILED', flush=True)

            timeline: list[dict] = []
            if args.scenario == 'hold':
                timeline = sample(page, args.seconds)
                trail = page.evaluate('window.__wfmTrail || []')
                report['trail'] = trail
                stereo_runs = run_lengths([e['stereo'] for e in trail])
                rds_locked = sum(1 for e in trail if e.get('rdsLocked'))
                report['summary'] = {
                    'blocks': len(trail),
                    'stereo_blocks': sum(1 for e in trail if e.get('stereo')),
                    'stereo_runs': stereo_runs,
                    'rds_locked_blocks': rds_locked,
                    'ps': next((e['ps'] for e in reversed(trail) if e.get('ps')), ''),
                    'rt': next((e['rt'] for e in reversed(trail) if e.get('rt')), ''),
                    'pi': next((e.get('rdsPi', 0) for e in reversed(trail)), 0),
                    'frame_rate': next((e.get('auxRateHz', 0) for e in reversed(trail)), 0),
                    'gaps': audio_gaps(timeline),
                }
                print(json.dumps(report['summary'], indent=2))
                if args.check_ui:
                    ui = page.evaluate("""() => {
                      const text = (id) => document.getElementById(id)?.textContent?.trim() || '';
                      return { badge: text('wfm-title-badge'), pilot: text('wfm-pilot'),
                        station: text('wfm-station'), rt: text('wfm-rt'),
                        pi: text('wfm-rds-pi'), carrier: text('wfm-carrier') };
                    }""")
                    report['ui'] = ui
                    expected = 'MONO' if args.tx == 'mono' else 'STEREO'
                    check('FM badge matches RF pilot', ui['badge'] == expected, str(ui))
                    if args.tx in ('stereo', 'rds'):
                        check('RDS PI/PS/RT rendered', bool(ui['station'].startswith('RIG')
                             and ui['rt'].startswith('RIG') and ui['pi'].startswith('PI ')), str(ui))
                    if args.tx == 'mono':
                        check('mono pane has no RDS data', ui['pi'] == '—' and ui['rt'] == '—', str(ui))
                    page.screenshot(path=f'/tmp/wfm_ui_{args.tx}_{args.viewport}.png')
                check('browser has no page errors or dialogs', not page_errors and not dialogs,
                      f'errors={page_errors} dialogs={dialogs}')
            else:  # retune or signal-loss
                if args.scenario == 'signal-loss':
                    timeline = sample(page, 8.0)
                    live = page.evaluate('window.__wfmState || null')
                    check('RDS locked before RF loss', bool(live and live.get('rdsLocked')),
                          json.dumps(live))
                    stopped_at = int(time.time() * 1000)
                    if tx:
                        tx.terminate()
                        tx.wait(timeout=10)
                        tx = None
                    timeline += sample(page, args.seconds, event='transmitter-off')
                    trail = page.evaluate('window.__wfmTrail || []')
                    report['trail'] = trail
                    report['signalStoppedAt'] = stopped_at
                    report['timeline'] = timeline
                    report['summary'] = {
                        'stereo_at_end': trail[-1]['stereo'] if trail else None,
                        'rds_locked_at_end': trail[-1]['rdsLocked'] if trail else None,
                        'pilot_dev_at_end': trail[-1]['pilotDevHz'] if trail else None,
                        'groups_at_end': trail[-1]['rdsGroups'] if trail else None,
                    }
                    print(json.dumps(report['summary'], indent=2))
                    check('pilot lock drops after RF loss',
                          bool(trail) and not trail[-1]['stereo']
                          and float(trail[-1]['pilotSnrDb']) < 6)
                    check('RDS station metadata clears after RF loss',
                          bool(trail) and not trail[-1]['rdsLocked'])
                    report['flush'] = page.evaluate('window.__wfmFlush || []')
                    browser.close()
                    if args.out:
                        Path(args.out).write_text(json.dumps(report))
                        print(f'wrote {args.out}')
                    return 1 if failures else 0
                away = args.freq + args.away_hz
                for step, target in enumerate((away, args.freq, away, args.freq)):
                    tune_ui(page, target)
                    if step == 3 and args.dump_after_retune:
                        page.evaluate("window.__wfmIqDump = null; window.dispatchEvent(new CustomEvent('wfm-dump-iq', {detail: true}))")
                    timeline += sample(page, 6.0, event=f'retune->{target / 1e6:.4f}MHz')
                    if tx and target == args.freq:
                        check('RF pilot returns after retune', wait_rf(page, args.tx, args.freq, 2),
                              json.dumps(page.evaluate('window.__wfmState || null')))
                if args.dump_after_retune:
                    deadline = time.monotonic() + 30
                    dump = page.evaluate('window.__wfmIqDump || null')
                    while dump is None and time.monotonic() < deadline:
                        time.sleep(0.5)
                        dump = page.evaluate('window.__wfmIqDump || null')
                    if dump:
                        Path(args.dump_after_retune).write_text(json.dumps(dump))
                        print(f'IQ after retune: {len(dump["frames"])} frames, rate {dump["rate"]}', flush=True)
                    else:
                        check('IQ after retune captured', False)
                report['timeline'] = timeline
                report['trail'] = page.evaluate('window.__wfmTrail || []')
                report['gaps'] = audio_gaps(timeline)
                report['gate'] = page.evaluate('window.__wfmGate || []')
                report['marks'] = page.evaluate('window.__retuneMarks || []')
                worst = max((g['ms'] for g in report['gaps']), default=0)
                resume_ms = []
                first_frame_ms = []
                first_pilot_ms = []
                for step, mark in enumerate(report['marks'][1:]):
                    target = away if step % 2 == 0 else args.freq
                    down = next((g['at'] for g in report['gate']
                                 if g['at'] >= mark and g['playing'] == 0), None)
                    up = next((g['at'] for g in report['gate']
                               if down is not None and g['at'] >= down and g['playing'] == 1), None)
                    resume_ms.append(up - mark if up is not None else None)
                    new = [e for e in report['trail'] if e['at'] >= mark
                           and abs(float(e['carrierHz']) - target) < 1]
                    first_frame_ms.append(new[0]['at'] - mark if new else None)
                    pilot = next((e for e in new if float(e['pilotDevHz']) > 3000
                                  and float(e['pilotSnrDb']) > 9), None)
                    first_pilot_ms.append(pilot['at'] - mark if pilot else None)
                report['resumeMs'] = resume_ms
                report['firstFrameMs'] = first_frame_ms
                report['firstPilotMs'] = first_pilot_ms
                worst_resume = max((ms for ms in resume_ms if ms is not None), default=0)
                report['summary'] = {
                    'gap_count': len(report['gaps']), 'worst_gap_ms': worst,
                    'resume_ms': resume_ms, 'first_frame_ms': first_frame_ms,
                    'first_pilot_ms': first_pilot_ms, 'worst_resume_ms': worst_resume,
                }
                print(json.dumps(report['summary'], indent=2))
                check('the worklet still plays after retunes', iq_diag(page).get('dsp_playing') == '1')
                check('retune command to new-channel playback within 500 ms',
                      len(resume_ms) == 4 and all(
                          resume is not None and frame is not None
                          and frame <= resume <= 500
                          for resume, frame in zip(resume_ms, first_frame_ms, strict=True))
                      and all(first_pilot_ms[i] is not None and first_pilot_ms[i] <= 500
                              for i in (1, 3)),
                      f'frames {first_frame_ms}, pilot {first_pilot_ms}, playback {resume_ms} ms')
                if args.check_ui:
                    ui = page.evaluate("""() => {
                      const text = (id) => document.getElementById(id)?.textContent?.trim() || '';
                      return { badge: text('wfm-title-badge'), station: text('wfm-station'),
                        rt: text('wfm-rt'), pi: text('wfm-rds-pi') };
                    }""")
                    report['ui'] = ui
                    check('FM pane recovers after retunes', ui['badge'] == 'STEREO'
                          and ui['station'].startswith('RIG') and ui['rt'].startswith('RIG'), str(ui))
                    page.screenshot(path='/tmp/wfm_ui_retune.png')
                check('browser has no page errors or dialogs', not page_errors and not dialogs,
                      f'errors={page_errors} dialogs={dialogs}')
            report['flush'] = page.evaluate('window.__wfmFlush || []')
            browser.close()
    finally:
        if tx:
            exited = tx.poll()
            if exited is not None:
                check('Pluto TX stayed on air', False,
                      f'exit={exited} ' + Path('/tmp/wfm_rig_tx.log').read_text()[-600:])
            else:
                tx.terminate()
                tx.wait(timeout=10)
    if args.out:
        Path(args.out).write_text(json.dumps(report))
        print(f'wrote {args.out}')
    return 1 if failures else 0


def run_lengths(flags: list[bool]) -> list[list]:
    """The stereo badge's hold/flip structure, one [state, blocks] pair per run."""
    runs: list[list] = []
    for flag in flags:
        if runs and runs[-1][0] == flag:
            runs[-1][1] += 1
        else:
            runs.append([flag, 1])
    return runs


if __name__ == '__main__':
    sys.exit(main())
