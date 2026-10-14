#!/usr/bin/env python3
"""The FM stereo/RDS bench rig: a real PlutoSDR transmission, the SAN-90 receiver and the
browser's own demodulator, sampled through the page's diagnostics.

What this rig measures is the page itself: the Playwright session opens the app against the
real backend, drives the same UI paths an operator uses (the demodulator buttons, the listen
field, the audio switch), and samples the bench diagnostics the page already publishes —
`window.__wfmTrail` (one entry per demodulated block, with the stereo/RDS state) and
`spectrum.dataset.sdrIq` / `.sdrAudio` (the stream and the worklet's playback gate). The
numbers that matter are therefore the ones the listener hears, not a re-implementation.

The transmitter runs in a subprocess of `tools/pluto_tx.py`, so the scenarios stay one
command each:

    python3 tools/e2e/wfm_rig.py --scenario retune --freq 411e6
    python3 tools/e2e/wfm_rig.py --scenario hold   --freq 411e6 --seconds 30 --tx stereo
    python3 tools/e2e/wfm_rig.py --scenario retune --freq 411e6 --tx rds --away-hz 400e3

Scenarios:
  * hold    — hold the frequency and record the trail (stereo badge stability, RDS decode).
  * retune  — step away and back through the listen field while recording the audio gate;
              the report holds the resume latency after each step (the <= 0.5 s contract).

The JSON dump (stdout and --out) carries the sampled timeline, so a failure can be read
after the fact without a re-run.
"""
from __future__ import annotations

import argparse
import json
import re
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
    # A stereo speech station: left-only speech, so the recovered L/R separation is the
    # whole programme and a matrix error cannot hide in a symmetric signal.
    'stereo': ['wfm', '--stereo', '__STEREO_WAV__', '--stereo-pilot', '--rds-rt',
               'RIG STEREO RT', '--rds-ps', 'RIG ST', '--rds-pi', '0x4147'],
    # A mono station with no pilot at all: the badge must read MONO and the RDS must lock
    # (RDS needs the pilot, so the tool adds it; --no-audio keeps the deviation for RDS).
    'rds': ['wfm', '--no-audio', '--rds-rt', 'RIG RDS OK 0123456789', '--rds-ps',
            'RIG RDS', '--rds-pi', '0x1234'],
    # A pure mono station: speech on both channels, no pilot, no RDS.
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
        rate, frames = src.getframerate(), src.readframes(src.getnframes())
    audio = wave.open(str(out), 'wb')
    audio.setnchannels(2)
    audio.setsampwidth(2)
    audio.setframerate(rate)
    left = frames
    right = b'\x00\x00' * (len(frames) // 2)
    stereo = bytearray()
    for k in range(len(frames) // 2):
        stereo += left[2 * k:2 * k + 2] + right[2 * k:2 * k + 2]
    audio.writeframes(bytes(stereo))
    audio.close()


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
    return subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)


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
    parser.add_argument('--scenario', choices=('hold', 'retune'), default='hold')
    parser.add_argument('--seconds', type=float, default=20.0)
    parser.add_argument('--out', default='')
    args = parser.parse_args()

    tx = start_tx(args.tx, args.freq, args.gain)
    report: dict = {'scenario': args.scenario, 'tx': args.tx, 'freq': args.freq}
    try:
        time.sleep(2.0)  # the transmitter's buffer fills before the RF starts
        post(args.url, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page()
            page.goto(args.url, wait_until='networkidle')
            page.evaluate("document.getElementById('btn-mode-sdr').click()")
            page.wait_for_timeout(1500)
            page.evaluate("document.querySelector('[data-sdr-demod=\"wfm\"]').click()")
            page.wait_for_timeout(300)
            # Off the broadcast band the panel does not auto-widen the IF filter (the band-aware
            # pick at 87.5-108 MHz is what normally sets 180 kHz with WFM), and a narrow filter
            # starves the side chain: the demodulator runs at 48 kSps and has no 57 kHz room.
            page.evaluate("document.querySelector('[data-sdr-ifbw=\"180000\"]').click()")
            page.wait_for_timeout(300)
            if (page.query_selector('#btn-sdr-audio')):
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
            else:  # retune
                away = args.freq + args.away_hz
                for step, target in enumerate((away, args.freq, away, args.freq)):
                    tune_ui(page, target)
                    timeline += sample(page, 6.0, event=f'retune->{target / 1e6:.4f}MHz')
                report['timeline'] = timeline
                report['gaps'] = audio_gaps(timeline)
                worst = max((g['ms'] for g in report['gaps']), default=0)
                report['summary'] = {'gap_count': len(report['gaps']), 'worst_gap_ms': worst}
                print(json.dumps(report['gaps'], indent=2))
                check('every retune resumes within 500 ms', worst <= 500,
                      f'worst gap {worst} ms')
            browser.close()
    finally:
        if tx:
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
