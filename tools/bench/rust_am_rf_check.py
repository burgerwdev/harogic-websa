#!/usr/bin/env python3
"""One exclusive SAN-90 AM round through the native three-channel product service.

H1 capture centre matches the request; H2 carrier selects TRUE rather than MIRROR;
H3 IQ/AUD2 sequences, exact audio source clocks, and AM tone survive concurrent load;
H4 native service and browser playback sustain real time; H5 AM depth agrees with
vendor raw IQ. Slots 1/2 are TRUE/MIRROR, slot 3 duplicates TRUE for real browser
AudioWorklet playback. Fixture mode exercises transport/playback without RF claims.
Hardware mode saves state, stops the service, drives tinySA, and always turns RF
off, restarts the service, restores saved configuration, and prints field differences.
Requires an already built websad and frontend; this tool does not build them.
Hardware mode requires WEBSA_RUST_IQS_SHIM pointing to the matching SDK shim.
"""
from __future__ import annotations

import argparse
import json
import math
import struct
import subprocess
import sys
import tempfile
import threading
import time
from fractions import Fraction
from pathlib import Path

import numpy as np
import websockets.sync.client as wsc
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / 'tools/sdr_probe'))
from tools.bench.sdr_network_diagnostics import restore, state  # noqa: E402
from tools.e2e.rust_iq_browser import (  # noqa: E402
    PREWARM,
    READY,
    SNAPSHOT,
    asset,
    browser_urls,
    configure,
    free_port,
    playback_problems,
    post,
    shutdown,
    start_service,
    stop,
)

SERVICE = 'http://127.0.0.1:8080'
FS = 62_500_000.0
COARSE_HZ = 100e6 + 5 * FS / 128
TARGET_HZ = COARSE_HZ + 20_000.0
MIRROR_HZ = COARSE_HZ - 20_000.0
AUDIO_HEADER = struct.Struct('<4sIIIddIIQdIIQdQQ')


def parse_iq(raw):
    if not isinstance(raw, bytes) or len(raw) < 56 or raw[:4] not in (b'IQBF', b'IQAX'):
        raise ValueError('not an IQBF/IQAX frame')
    version, seq, count, rate, center, channel, epoch, index, delay = struct.unpack_from(
        '<IIIddIIQd', raw, 4)
    if (version != 2 or len(raw) != 56 + 8 * count or not math.isfinite(rate) or rate <= 0
            or not math.isfinite(center) or channel == 0 or not math.isfinite(delay) or delay < 0):
        raise ValueError('invalid IQBF v2 geometry')
    samples = np.frombuffer(raw, '<f4', offset=56)
    if not np.isfinite(samples).all():
        raise ValueError('non-finite IQ')
    return dict(seq=seq, count=count, rate=rate, center=center, channel=channel, epoch=epoch,
                index=index, samples=samples)


def parse_audio(raw):
    if not isinstance(raw, bytes) or len(raw) < AUDIO_HEADER.size:
        raise ValueError('truncated AUD2 header')
    (magic, version, seq, count, rate, center, channel, epoch, index, delay, channels,
     flags, audio_index, input_rate, numerator, denominator) = AUDIO_HEADER.unpack_from(raw)
    if (magic != b'AUD2' or version != 1 or (flags & 0xffff) != 1
            or flags & ~0x30001 or channels not in (1, 2)
            or not math.isfinite(rate) or rate <= 0 or not math.isfinite(input_rate) or input_rate <= 0
            or not math.isfinite(center) or center < 0 or channel == 0
            or not math.isfinite(delay) or delay < 0
            or denominator == 0 or numerator >= denominator
            or len(raw) != AUDIO_HEADER.size + count * channels * 4):
        raise ValueError('invalid AUD2 geometry/source metadata')
    samples = np.frombuffer(raw, '<f4', offset=AUDIO_HEADER.size)
    if not np.isfinite(samples).all():
        raise ValueError('non-finite AUD2 PCM')
    return dict(seq=seq, count=count, rate=rate, center=center, channel=channel, epoch=epoch,
                index=index, source=Fraction(index * denominator + numerator, denominator),
                audio_index=audio_index, input_rate=input_rate, channels=channels, flags=flags,
                delay=delay, samples=samples)


def am_stats(z, rate):
    spec = np.abs(np.fft.fft(z)) ** 2
    freqs = np.fft.fftfreq(z.size, 1 / rate)
    env = np.abs(z)
    t = np.arange(env.size) / rate
    tones = np.arange(900, 1101, 2.0)
    ac = env - env.mean()
    level = [abs(np.mean(ac * np.exp(-2j * np.pi * f * t))) for f in tones]
    best = int(np.argmax(level))
    return {'concentration': round(float(spec[np.abs(freqs) <= 2000].sum() / spec.sum()), 3),
            'carrier_offset_hz': round(float(np.angle(np.mean(z[1:] * np.conj(z[:-1])))
                                             * rate / (2 * np.pi)), 1),
            'tone_hz': float(tones[best]), 'depth': round(float(2 * level[best] / env.mean()), 3)}


class Receiver(threading.Thread):
    def __init__(self, url, *, audio, identity=None):
        super().__init__(daemon=True)
        self.sock = wsc.connect(url, max_size=1 << 24)
        self.audio, self.identity = audio, identity
        self.frames, self.error, self.halt = [], None, threading.Event()
        self.opened = time.monotonic()
        self.closed = None

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    raw = self.sock.recv(timeout=0.1)
                except TimeoutError:
                    continue
                if self.audio:
                    row = parse_audio(raw)
                else:
                    if not isinstance(raw, bytes) or raw[:4] not in (b'IQBF', b'IQAX'):
                        continue
                    row = parse_iq(raw)
                    if (row['channel'], row['epoch']) != self.identity:
                        continue
                self.frames.append(row)
        except Exception as exc:
            if not self.halt.is_set():
                self.error = repr(exc)

    def finish(self):
        self.closed = time.monotonic()
        self.halt.set()
        self.sock.close()
        self.join(timeout=2)


def iq_report(rx, config, hardware):
    rows = rx.frames
    # Product display IQ subscriptions can join after sequence zero; subsequent
    # packets must be contiguous within the advertised channel identity.
    seq_ok = bool(rows) and all(b['seq'] == a['seq'] + 1 for a, b in zip(rows, rows[1:], strict=False))
    headers_ok = bool(rows) and all(
        r['rate'] == config['actual_iq_rate'] and r['center'] == config['freq']
        and (r['channel'], r['epoch']) == (config['channel_id'], config['epoch']) for r in rows)
    clock_ok = all(b['index'] >= a['index'] for a, b in zip(rows, rows[1:], strict=False))
    samples = sum(r['count'] for r in rows)
    result = dict(seq_ok=seq_ok, headers_ok=headers_ok, clock_ok=clock_ok,
                  frames=len(rows), samples=samples, rx_error=rx.error)
    if hardware and samples > 0.5 * config['actual_iq_rate']:
        iq = np.concatenate([r['samples'] for r in rows]).astype(np.float64)
        z = (iq[0::2] + 1j * iq[1::2])[int(0.3 * config['actual_iq_rate']):]
        result.update(am_stats(z, config['actual_iq_rate']))
    return result


def audio_report(rx, config):
    rows = rx.frames
    errors = []
    geometry = previous = None
    total = 0
    for row in rows:
        if ((row['channel'], row['epoch']) != (config['channel_id'], config['epoch'])
                or row['center'] != config['freq'] or row['rate'] != config['audio_rate']):
            errors.append('audio identity/rate/frequency differs from STATUS')
        current = tuple(row[k] for k in ('channel', 'epoch', 'center', 'rate', 'input_rate', 'channels'))
        if row['seq'] == 0:
            if row['count'] != 0 or row['audio_index'] != 0:
                errors.append('invalid audio configuration boundary')
            if previous is not None and (row['rate'], row['channels']) == (
                    previous['rate'], previous['channels']):
                errors.append('repeated configuration without audio format change')
            geometry = current
        else:
            if previous is None or geometry != current or row['count'] <= 0:
                errors.append('audio payload before matching configuration')
            elif row['seq'] != previous['seq'] + 1:
                errors.append('audio sequence gap')
            else:
                expected_audio = previous['audio_index'] + previous['count']
                expected_source = previous['source'] + previous['count'] * (
                    Fraction(previous['input_rate']) / Fraction(previous['rate']))
                causal = (row['flags'] | previous['flags']) & (1 << 17)
                source_ok = (row['source'] >= previous['source'] if causal
                             else row['source'] == expected_source)
                if row['audio_index'] != expected_audio or not source_ok:
                    errors.append('audio/source clock discontinuity')
        previous = row
        total += row['count']
    if not rows or total == 0:
        errors.append('no audio PCM')
    pcm = np.concatenate([r['samples'][::r['channels']] for r in rows]) if total else np.array([])
    output = {'frames': len(rows), 'samples': total, 'errors': sorted(set(errors)),
              'rx_error': rx.error,
              'clock_ratio': round(total / (config['audio_rate'] * (rx.closed - rx.opened)), 4),
              'delay_known': all(not (r['flags'] & (1 << 16)) for r in rows),
              'causal_presentation': any(r['flags'] & (1 << 17) for r in rows),
              'delay_seconds': sorted({r['delay'] for r in rows})}
    if pcm.size:
        output['rms'] = float(np.sqrt(np.mean(pcm.astype(np.float64) ** 2)))
        if config['channel'] == 1 and pcm.size > config['audio_rate'] * 0.5:
            settled = pcm[int(0.3 * config['audio_rate']):].astype(np.float64)
            spectrum = abs(np.fft.rfft(settled - settled.mean()))
            output['tone_hz'] = float(np.argmax(spectrum) * config['audio_rate'] / settled.size)
    return output


def vendor_depth(center):
    from ctypes import byref, c_void_p, cast

    import probe_adm as P
    P.bind_adm()
    dev, dsp, adm = P.open_device()
    try:
        buf, info, scale = P.capture(dev, center, 32, 131072)
        n, fs = int(info.StreamSamples), float(info.IQSampleRate)
        param = P.AMDemodParam()
        status = P.T.dll.ADM_AMDemod_PM1(byref(adm), cast(buf, c_void_p),
                                          int(P.T.DataFormat_TypeDef.Complex16bit),
                                          n, fs, scale, byref(param))
        raw = np.frombuffer(buf, dtype=np.int16).astype(np.float64)
        depth = float(param.ModDepth)
        return {'status': status, 'adm_depth': depth / 100 if depth > 1.5 else depth,
                'adm_mod_rate': round(float(param.ModRate), 1),
                'adm_carrier_dbm': round(float(param.CarrierPower), 2),
                'raw_estimator': am_stats(raw[0::2] + 1j * raw[1::2], fs)}
    finally:
        P.T.dll.ADM_Close(byref(adm))
        P.T.dll.Device_Close(byref(dev))


def run_round(hardware, seconds, capture_s):
    worklet, worker = asset('sdrAudioWorklet-*.js'), asset('sdrAudioWorker-*.js')

    result = {'iq': {}, 'audio': {}, 'browser': None, 'client_error': None, 'alive': False}
    receivers = []
    service = None
    with tempfile.TemporaryFile(mode='w+') as log:
        try:
            service, origin = start_service(free_port(), hardware=hardware, log=log)
            post(origin, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            post(origin, {'cmd': 'SET_SDR', 'center': 100e6, 'decimate': 1})
            configure(origin, [TARGET_HZ, MIRROR_HZ, TARGET_HZ])
            if hardware:
                # Keep the carrier 40 kHz from MIRROR inside its native IQ window
                # so the sign check measures its offset rather than filter rejection.
                for slot, frequency in enumerate((TARGET_HZ, MIRROR_HZ, TARGET_HZ), 1):
                    post(origin, {'cmd': 'SET_SDR_CHANNEL', 'channel': slot, 'enabled': True,
                                  'freq': frequency, 'mode': 'am', 'ifbw': 90000,
                                  'volume': 1, 'audio_rate': 48000})
            with sync_playwright() as driver:
                browser = driver.chromium.launch(
                    headless=True, args=['--autoplay-policy=no-user-gesture-required'])
                try:
                    page = browser.new_page()
                    page.goto(origin + '/api/state', wait_until='networkidle')
                    page.evaluate(PREWARM, {'worklet': worklet, 'worker': worker,
                                            'urls': browser_urls(origin, [3])})
                    page.wait_for_function('window.__lab[0].identity !== null', timeout=15000)
                    # Each subscription rotates only its slot. Wait for the browser
                    # identity before obtaining STATUS for the other two subscribers.
                    audio = {}
                    for slot in (1, 2):
                        audio[slot] = Receiver(browser_urls(origin, [slot])[0], audio=True)
                        receivers.append(audio[slot])
                        audio[slot].start()
                    current = state(origin)
                    configs = current['sdr']['channels']
                    result['channels'] = configs
                    base = origin.replace('http://', 'ws://')
                    iq = {}
                    for slot, query in ((1, 'iq=1'), (2, 'iqaux=1')):
                        config = configs[slot - 1]
                        url = f'{base}/ws?{query}'
                        iq[slot] = Receiver(url, audio=False,
                                            identity=(config['channel_id'], config['epoch']))
                        receivers.append(iq[slot])
                        iq[slot].start()
                    started = time.monotonic()
                    page.wait_for_function(READY, timeout=min(15000, seconds * 1000))
                    first = page.evaluate(SNAPSHOT)
                    page.wait_for_timeout(max(0.5, capture_s - (time.monotonic() - started)) * 1000)
                    last = page.evaluate(SNAPSHOT)
                    current = state(origin)
                    result['capture_center_hz'] = current['sdr']['actual']['capture_center']
                    for slot in (1, 2):
                        audio[slot].finish()
                        iq[slot].finish()
                        result['audio'][slot] = audio_report(audio[slot], configs[slot - 1])
                        result['iq'][slot] = iq_report(iq[slot], configs[slot - 1], hardware)
                    result['browser'] = {'first': first[0], 'last': last[0],
                                         'problems': playback_problems(first, last, [configs[2]], tone=True)
                                                     + shutdown(page)}
                    result['alive'] = service.poll() is None
                finally:
                    browser.close()
        except Exception as exc:
            result['client_error'] = repr(exc)
        finally:
            for rx in receivers:
                if rx.is_alive():
                    rx.finish()
            stop(service)
            log.seek(0)
            result['service_log'] = log.read()[-2000:]
    return result


def verdict(result, hardware, vendor):
    problems = []
    if not result['alive']:
        problems.append('H4 native service failed')
    if result['client_error']:
        problems.append(f"client {result['client_error']}")
    for kind in ('iq', 'audio'):
        if set(result[kind]) != {1, 2}:
            problems.append(f'H3 missing {kind} consumers')
    for slot, row in result['iq'].items():
        if not row['seq_ok'] or not row['headers_ok'] or not row['clock_ok'] or row['rx_error']:
            problems.append(f'H3 IQ channel {slot}')
    for slot, row in result['audio'].items():
        if row['errors'] or row['rx_error']:
            problems.append(f'H3 AUD2 channel {slot}')
        if not 0.95 <= row['clock_ratio'] <= 1.05:
            problems.append(f"H4 audio channel {slot} clock ratio {row['clock_ratio']}")
    if abs(result['audio'].get(1, {}).get('tone_hz', 0) - 1000) > 30:
        problems.append('H3 native AM output tone')
    b = result['browser']
    if not b or b['problems']:
        problems.append('browser chain' + (f": {b['problems']}" if b else ''))
    if hardware:
        true, mirror = result['iq'].get(1, {}), result['iq'].get(2, {})
        if abs(result.get('capture_center_hz', 0) - 100e6) > 1:
            problems.append('H1 capture centre')
        if true.get('concentration', 0) < 0.6 or mirror.get('concentration', 1) > 0.3:
            problems.append('H2 carrier not in TRUE only')
        if abs(true.get('tone_hz', 0) - 1000) > 30:
            problems.append('H3 AM IQ tone')
        if not vendor or 'raw_estimator' not in vendor or vendor.get('status') != 0:
            problems.append('H5 vendor reference unavailable')
        elif abs(true.get('depth', 0) - vendor['raw_estimator']['depth']) > 0.05:
            problems.append('H5 native IQ depth differs from vendor raw IQ')
    return problems


def report(result, vendor, problems):
    print(json.dumps({'vendor': vendor, **result, 'problems': problems}, indent=2))
    print('PROBLEMS' if problems else 'OK', problems)


def same(before, after):
    keys = ('center', 'decimate', 'listen', 'demod', 'if_bw', 'squelch', 'agc', 'volume')
    diff = ['mode'] if before['mode'] != after['mode'] else []
    diff += [f'swp.{k}' for k, v in before['req']['swp'].items() if after['req']['swp'].get(k) != v]
    diff += [f'sdr.{k}' for k in keys if before['req']['sdr'][k] != after['req']['sdr'][k]]
    if before.get('sdr', {}).get('channels'):
        channel_keys = ('channel', 'enabled', 'freq', 'mode', 'if_bw', 'volume', 'audio_rate',
                        'squelch', 'agc', 'pitch', 'deemph_us', 'nr', 'nr_strength',
                        'nr_algorithm', 'nr_atten_db', 'stereo')
        prior, current = before['sdr']['channels'], after['sdr']['channels']
        if len(prior) != len(current):
            diff.append('sdr.channels')
        for a, b in zip(prior, current, strict=False):
            diff += [f"sdr.channel{a['channel']}.{key}" for key in channel_keys if a.get(key) != b.get(key)]
    return diff


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hardware', action='store_true')
    parser.add_argument('--seconds', type=float, default=20.0, help='browser startup deadline ceiling')
    parser.add_argument('--capture', type=float, default=16.0)
    parser.add_argument('--level', type=float, default=-30.0)
    args = parser.parse_args()
    if args.seconds <= 0 or args.capture < 3:
        parser.error('--seconds must be positive and --capture must be at least 3 seconds')
    if not args.hardware:
        result = run_round(False, args.seconds, args.capture)
        problems = verdict(result, False, None)
        report(result, None, problems)
        return 1 if problems else 0
    from tinysa import TinySA
    saved = state(SERVICE)
    subprocess.run(['./stop.sh'], cwd=ROOT, check=True)
    if subprocess.run(['pgrep', '-x', 'websad']).returncode == 0:
        raise SystemExit('WebSA still running after ./stop.sh')
    source, vendor = None, None
    result = {'iq': {}, 'audio': {}, 'browser': None, 'client_error': 'not run', 'alive': False}
    diff = ['not restored']
    try:
        source = TinySA('/dev/ttyACM0')
        source.am(round(TARGET_HZ), args.level, 1000, 50)
        time.sleep(0.4)
        try:
            vendor = vendor_depth(round(TARGET_HZ))
        except Exception as exc:
            vendor = {'error': repr(exc)}
        result = run_round(True, args.seconds, args.capture)
    finally:
        try:
            if source is not None:
                source.close()
        finally:
            subprocess.run(['./run.sh'], cwd=ROOT, check=False)
            for _ in range(30):
                try:
                    if state(SERVICE).get('connected'):
                        break
                except OSError:
                    pass
                time.sleep(1)
            restore(SERVICE, saved)
            if saved.get('sdr', {}).get('channels'):
                post(SERVICE, {'cmd': 'SET_MODE', 'mode': 'sdr'})
            for config in saved.get('sdr', {}).get('channels', []):
                keys = ('channel', 'enabled', 'freq', 'mode', 'if_bw', 'volume', 'audio_rate',
                        'squelch', 'agc', 'pitch', 'deemph_us', 'nr', 'nr_strength', 'nr_algorithm',
                        'nr_atten_db', 'stereo')
                post(SERVICE, {'cmd': 'SET_SDR_CHANNEL', **{k: config[k] for k in keys if k in config}})
            if saved.get('sdr', {}).get('channels'):
                post(SERVICE, {'cmd': 'SET_MODE', 'mode': saved['mode']})
            diff = same(saved, state(SERVICE))
            print('restore diff', diff or 'none')
    problems = verdict(result, True, vendor) + (['restore'] if diff else [])
    report(result, vendor, problems)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
