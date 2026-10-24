#!/usr/bin/env python3
"""Native fixture IQ -> AUD2 -> browser audio worker/AudioWorklet, up to three slots.

Uses the production websad service and committed AM fixture (1 kHz, 50% depth).
Requires an already built backend/target/release/websad and frontend/dist.
Checks concurrent playback, affected-slot retuning, and consumer shutdown.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
WEBSAD = ROOT / 'backend/target/release/websad'
CHANNELS = [100_000_000.0, 100_100_000.0, 100_200_000.0]


def asset(pattern: str) -> str:
    found = sorted((ROOT / 'frontend/dist/assets').glob(pattern))
    if len(found) != 1:
        raise SystemExit(f'expected one built {pattern}, found {found}; rebuild the frontend')
    return '/static/dist/assets/' + found[0].name


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def stop(proc: subprocess.Popen | None) -> None:
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.communicate(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate()


def post(origin, payload):
    headers = {'Content-Type': 'application/json'}
    request = urllib.request.Request(origin + '/api/config', json.dumps(payload).encode(),
                                     headers=headers)
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def state(origin):
    with urllib.request.urlopen(origin + '/api/state', timeout=3) as response:
        return json.load(response)


def start_service(port, *, hardware=False, log=None):
    env = {**os.environ, 'WEBSA_PORT': str(port), 'WEBSA_HOST': '127.0.0.1'}
    if hardware:
        shim = os.environ.get('WEBSA_RUST_IQS_SHIM')
        if not shim or not Path(shim).is_file():
            raise RuntimeError('hardware requires WEBSA_RUST_IQS_SHIM pointing to the matching SDK shim')
        env['WEBSA_RUST_IQS_SHIM'] = shim
    else:
        env.pop('WEBSA_RUST_IQS_SHIM', None)
    proc = subprocess.Popen([str(WEBSAD)], cwd=ROOT, env=env,
                            stdout=log or subprocess.DEVNULL,
                            stderr=log or subprocess.DEVNULL)
    origin = f'http://127.0.0.1:{port}'
    for _ in range(100):
        if proc.poll() is not None:
            break
        try:
            state(origin)
            return proc, origin
        except OSError:
            time.sleep(0.1)
    stop(proc)
    raise RuntimeError('native websad did not start; inspect the service log')


def configure(origin, frequencies):
    post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
    for slot in range(1, 4):
        payload = {'cmd': 'SET_SDR_CHANNEL', 'channel': slot,
                   'enabled': slot <= len(frequencies)}
        if slot <= len(frequencies):
            payload.update(freq=frequencies[slot - 1], mode='am', ifbw=12000,
                           volume=1, audio_rate=48000)
        post(origin, payload)


PREWARM = '''async ({worklet, worker, urls}) => {
  const lab = [];
  for (const url of urls) {
    const ctx = new AudioContext({sampleRate: 48000});
    await ctx.audioWorklet.addModule(worklet);
    const node = new AudioWorkletNode(ctx, 'sdr-audio-processor', {outputChannelCount: [2]});
    const splitter = ctx.createChannelSplitter(2);
    const merger = ctx.createChannelMerger(2);
    const analysers = [ctx.createAnalyser(), ctx.createAnalyser()];
    node.connect(splitter);
    analysers.forEach((analyser, channel) => {
      analyser.fftSize = 8192;
      splitter.connect(analyser, channel);
      analyser.connect(merger, 0, channel);
    });
    merger.connect(ctx.destination);
    await ctx.resume();
    const w = new Worker(worker, {type: 'module'});
    const s = {ctx, node, analysers, worker: w, stats: {}, identity: null, timing: null, errors: []};
    w.onerror = (e) => s.errors.push(String(e.message || e));
    w.onmessage = (e) => {
      const d = e.data;
      if (d.type === 'error') s.errors.push(String(d.message || d));
      if (d.type === 'stats') Object.assign(s.stats, d);
      if (d.type === 'identity') {
        s.identity = {channel_id: d.channelId, epoch: d.epoch};
        s.timing = {delaySeconds: d.delaySeconds, totalDelayUnknown: d.totalDelayUnknown,
          causalPresentation: d.causalPresentation};
      }
    };
    w.postMessage({type: 'init', url, targetRate: ctx.sampleRate, port: node.port}, [node.port]);
    w.postMessage({type: 'enabled', value: true});
    w.postMessage({type: 'mute', value: false});
    lab.push(s);
  }
  window.__lab = lab;
}'''

SNAPSHOT = '''() => window.__lab.map((s) => {
  const outputs = s.analysers.map((analyser) => {
    const wave = new Float32Array(analyser.fftSize);
    analyser.getFloatTimeDomainData(wave);
    const spectrum = new Float32Array(analyser.frequencyBinCount);
    analyser.getFloatFrequencyData(spectrum);
    let peak = 1;
    for (let i = 2; i < spectrum.length; i++) if (spectrum[i] > spectrum[peak]) peak = i;
    return {rms: Math.sqrt(wave.reduce((sum, v) => sum + v*v, 0) / wave.length),
      toneHz: peak * s.ctx.sampleRate / analyser.fftSize};
  });
  return {blocks: s.stats.frames ?? 0, played: s.stats.playedFrames ?? 0,
    underruns: s.stats.underruns ?? 0, slipped: s.stats.slipped ?? 0,
    buffered: s.stats.bufferedSamples ?? 0, pcmRms: s.stats.rms ?? 0,
    outputRms: Math.min(...outputs.map(o => o.rms)),
    toneHz: outputs[0].toneHz, outputs,
    identity: s.identity, timing: s.timing, playbackRate: s.ctx.sampleRate,
    errors: s.errors.slice(), ctx: s.ctx.state};
})'''

READY = 'window.__lab.every((s) => s.identity && (s.stats.frames ?? 0) > 10 && (s.stats.playedFrames ?? 0) > 0)'


def browser_urls(origin, slots):
    base = origin.replace('http://', 'ws://')
    return [f'{base}/ws/audio?channel={slot}'
            for slot in slots]


def playback_problems(before, after, channels, *, tone=False):
    problems = []
    for slot, a, b in zip(channels, before, after, strict=True):
        if (b['blocks'] <= a['blocks'] or b['played'] <= a['played']
                or b['underruns'] != a['underruns'] or b['slipped'] != a['slipped']
                or b['errors'] or b['ctx'] != 'running' or b['outputRms'] <= 1e-5
                or b['identity'] != {'channel_id': slot['channel_id'], 'epoch': slot['epoch']}):
            problems.append(f"channel {slot['channel']} native playback")
        if tone and any(abs(output['toneHz'] - 1000) > 30 for output in b['outputs']):
            problems.append(f"channel {slot['channel']} AM output tone")
    return problems


def shutdown(page):
    page.evaluate("() => window.__lab.forEach(s => s.worker.postMessage({type:'enabled',value:false}))")
    page.wait_for_timeout(250)
    before = page.evaluate(SNAPSHOT)
    page.wait_for_timeout(300)
    after = page.evaluate(SNAPSHOT)
    problems = ['disabled consumer still playing' for a, b in zip(before, after, strict=True)
                if b['played'] != a['played'] or b['errors']]
    page.evaluate('async () => { for (const s of window.__lab) { s.worker.terminate(); await s.ctx.close(); } }')
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--channels', type=int, default=3, choices=range(1, 4))
    parser.add_argument('--seconds', type=float, default=3.0)
    args = parser.parse_args()
    if args.seconds <= 0:
        parser.error('--seconds must be positive')
    worklet, worker = asset('sdrAudioWorklet-*.js'), asset('sdrAudioWorker-*.js')
    service, origin = start_service(free_port())
    try:
        configure(origin, CHANNELS[:args.channels])
        with sync_playwright() as driver:
            browser = driver.chromium.launch(
                headless=True, args=['--autoplay-policy=no-user-gesture-required'])
            try:
                page = browser.new_page()
                page.goto(origin + '/api/state', wait_until='networkidle')
                page.evaluate(PREWARM, {'worklet': worklet, 'worker': worker,
                                        'urls': browser_urls(origin, range(1, args.channels + 1))})
                page.wait_for_function(READY, timeout=15000)
                channels = state(origin)['sdr']['channels'][:args.channels]
                first = page.evaluate(SNAPSHOT)
                page.wait_for_timeout(args.seconds * 1000)
                last = page.evaluate(SNAPSHOT)
                problems = playback_problems(first, last, channels, tone=True)
                changed = post(origin, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1,
                                       'enabled': True, 'freq': CHANNELS[0] + 2000,
                                       'mode': 'am', 'ifbw': 12000, 'volume': 1,
                                       'audio_rate': 48000})['sdr']['channels'][:args.channels]
                if changed[0]['channel_id'] == channels[0]['channel_id']:
                    problems.append('retune did not rotate affected channel identity immediately')
                if any((a['channel_id'], a['epoch']) != (b['channel_id'], b['epoch'])
                       for a, b in zip(channels[1:], changed[1:], strict=True)):
                    problems.append('retune changed another channel identity')
                page.wait_for_function('(id) => window.__lab[0].identity?.channel_id === id',
                                       arg=changed[0]['channel_id'], timeout=10000)
                page.wait_for_timeout(800)
                retuned_first = page.evaluate(SNAPSHOT)
                page.wait_for_timeout(500)
                retuned_last = page.evaluate(SNAPSHOT)
                problems += playback_problems(retuned_first, retuned_last, changed, tone=True)
                problems += shutdown(page)
                print(json.dumps({'channels': channels, 'first': first, 'last': last,
                                  'retuned': retuned_last, 'problems': problems}, indent=2))
                print('FAIL' if problems else 'PASS')
                return 1 if problems else 0
            finally:
                browser.close()
    finally:
        stop(service)


if __name__ == '__main__':
    sys.exit(main())
