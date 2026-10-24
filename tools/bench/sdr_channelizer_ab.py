#!/usr/bin/env python3
"""Offline same-int16-IQ A/B. Stop WebSA before using the vendor DSP handle."""
from __future__ import annotations

import argparse
import ctypes as C
import json
import time
from pathlib import Path

import numpy as np


def native(path, fs, offset, decimate, rational=None):
    lib = C.CDLL(str(path))
    lib.channel_open.argtypes = [C.c_double, C.c_double, C.c_int]
    lib.channel_open.restype = C.c_void_p
    lib.channel_open_ratio.argtypes = [C.c_double, C.c_double, C.c_int,
                                       C.c_uint32, C.c_uint32, C.c_double]
    lib.channel_open_ratio.restype = C.c_void_p
    lib.channel_process.argtypes = [C.c_void_p, C.POINTER(C.c_int16), C.c_int,
                                    C.POINTER(C.c_float), C.c_int]
    lib.channel_process.restype = C.c_int
    lib.channel_close.argtypes = [C.c_void_p]
    lib.channel_delay.argtypes = [C.c_void_p]
    lib.channel_delay.restype = C.c_double
    handle = (lib.channel_open_ratio(fs, offset, *rational)
              if rational else lib.channel_open(fs, offset, decimate))
    if not handle:
        raise ValueError('Unsupported channelizer geometry')
    return lib, handle


def fixture(fs, n, packets, offset, tone, kind):
    t = np.arange(n * packets, dtype=np.float64) / fs
    if kind == 'am':
        z = (1 + 0.45 * np.sin(2 * np.pi * 1000 * t)) * np.exp(
            1j * 2 * np.pi * (tone - offset) * t)
    elif kind == 'fm':
        z = np.exp(1j * (2 * np.pi * (tone - offset) * t -
                         5000 / 1000 * np.cos(2 * np.pi * 1000 * t)))
    else:
        z = np.exp(1j * 2 * np.pi * (tone - offset) * t)
    iq = np.empty(t.size * 2, np.int16)
    iq[::2] = np.rint(z.real * 12000).astype(np.int16)
    iq[1::2] = np.rint(z.imag * 12000).astype(np.int16)
    return iq.reshape(packets, n * 2)


def run(libpath, blocks, fs, offset, decimate, rational=None):
    from tools.reference import sdk_bindings as sb
    from tools.reference.ddc import DdcChannel

    dsp = sb.c_void_p()
    status = sb.dll.DSP_Open(sb.pointer(dsp))
    if status != 0:
        raise RuntimeError(f'DSP_Open status={status}')
    lib = handle = None
    try:
        vendor = DdcChannel(dsp)
        vendor.configure(fs, offset, decimate, blocks.shape[1] // 2)
        lib, handle = native(libpath, fs, offset, decimate, rational)
        capacity_decimate = rational[0] if rational else decimate
        out = np.empty(2 * (blocks.shape[1] // capacity_decimate + 4), np.float32)
        results, times = [[], []], [[], []]
        for block in blocks:
            start = time.perf_counter_ns()
            vi, vq = vendor.process(block)
            times[0].append((time.perf_counter_ns() - start) / 1e6)
            results[0].append(vi + 1j * vq)
            start = time.perf_counter_ns()
            n = lib.channel_process(handle, block.ctypes.data_as(C.POINTER(C.c_int16)),
                                    block.size // 2, out.ctypes.data_as(C.POINTER(C.c_float)),
                                    out.size // 2)
            if n < 0:
                raise RuntimeError('channel_process rejected input')
            times[1].append((time.perf_counter_ns() - start) / 1e6)
            results[1].append(out[:2*n:2].copy() + 1j*out[1:2*n:2].copy())
        return [np.concatenate(row) for row in results], [
            {'p50_ms': round(float(np.median(t)), 4),
             'p99_ms': round(float(np.percentile(t, 99)), 4),
             'max_ms': round(max(t), 4), 'samples': sum(map(len, results[i])),
             'filter_delay_samples': vendor.delay if i == 0 else
                 round(lib.channel_delay(handle), 2)}
            for i, t in enumerate(times)]
    finally:
        if handle:
            lib.channel_close(handle)
        sb.dll.DSP_Close(sb.pointer(dsp))


def _tone_quality(audio, fs_out, tone_hz):
    t = np.arange(audio.size) / fs_out
    phase = 2 * np.pi * tone_hz * t
    basis = np.column_stack((np.cos(phase), np.sin(phase)))
    fitted = basis @ np.linalg.lstsq(basis, audio, rcond=None)[0]
    tone_rms = float(np.sqrt(np.mean(fitted**2)))
    residual_rms = float(np.sqrt(np.mean((audio - fitted)**2)))
    return {'tone_rms': round(tone_rms, 4),
            'residual_dbc': round(20 * np.log10(max(residual_rms, 1e-12) /
                                                max(tone_rms, 1e-12)), 2)}


def compare(v, n, fs_out, tone=None, kind='cw'):
    # Align unknown vendor filter delay and overall gain; report residual waveform error.
    v, n = v[200:], n[200:]
    best = (float('inf'), 0, 0j)
    aligned = None
    for lag in range(-140, 141):
        a, b = (v[max(0, lag):], n[max(0, -lag):])
        a, b = a[:min(len(a), len(b))], b[:min(len(a), len(b))]
        gain = np.vdot(b, a) / np.vdot(b, b)
        error = float(np.linalg.norm(a - gain * b) / np.linalg.norm(a))
        if error < best[0]:
            best = error, lag, gain
            aligned = (a, b * gain)
    freq = []
    for z in (v, n):
        freq.append(float(np.angle(np.vdot(z[:-1], z[1:])) * fs_out / (2*np.pi)))
    result = {'relative_error': round(best[0], 5), 'lag_samples': best[1],
              'gain_abs': round(abs(best[2]), 5), 'freq_hz': [round(x, 2) for x in freq],
              'expected_hz': tone}
    if kind in ('am', 'fm'):
        a, b = aligned
        if kind == 'am':
            a, b = abs(a), abs(b)
        else:
            a = np.angle(a[1:] * a[:-1].conj())
            b = np.angle(b[1:] * b[:-1].conj())
        a, b = a - a.mean(), b - b.mean()
        result['audio_relative_error'] = round(float(np.linalg.norm(a - b) / np.linalg.norm(a)), 5)
        fft_len = 1 << (len(a) * 16 - 1).bit_length()
        spectrum = abs(np.fft.rfft(a * np.hanning(len(a)), fft_len))
        freqs = np.fft.rfftfreq(fft_len, 1 / fs_out)
        band = np.flatnonzero((freqs >= 800) & (freqs <= 1200))
        mod_hz = float(freqs[band[np.argmax(spectrum[band])]])
        scale = fs_out / (2 * np.pi) if kind == 'fm' else 1.0
        result['audio_tone_hz'] = round(mod_hz, 2)
        result['audio_quality'] = [_tone_quality(x * scale, fs_out, mod_hz) for x in (a, b)]
    return result


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--lib', type=Path, default=Path('native-ddc/target/release/libwebsa_channelizer.so'))
    p.add_argument('--packets', type=int, default=120)
    p.add_argument('--raw', type=Path, help='Optional saved contiguous little-endian interleaved int16 IQ')
    p.add_argument('--offset-hz', type=float, default=0.0, help='DDC offset; positive input tone uses negative offset')
    p.add_argument('--ddc-decimate', type=int, default=640)
    p.add_argument('--rational-base', type=int, default=0,
                   help='Rust integer decimate (e.g. 96 with 8/9 -> vendor 108)')
    p.add_argument('--up', type=int, default=8)
    p.add_argument('--down', type=int, default=9)
    p.add_argument('--passband-hz', type=float, default=90000)
    p.add_argument('--kind', choices=('raw', 'cw', 'am', 'fm'), default='raw')
    args = p.parse_args()
    fs, decimate, packet = 31250000.0, args.ddc_decimate, 16240
    if decimate < 2:
        p.error('--ddc-decimate must be >= 2')
    rational = ((args.rational_base, args.up, args.down, args.passband_hz)
                if args.rational_base else None)
    if rational and args.rational_base * args.down != decimate * args.up:
        p.error('vendor --ddc-decimate must equal --rational-base * --down / --up')
    if args.packets < 2:
        p.error('--packets must be >= 2')
    blocks = (np.fromfile(args.raw, dtype='<i2').reshape(-1, packet*2) if args.raw else
              fixture(fs, packet, args.packets, -300000, 1000, 'am'))
    if blocks.size == 0:
        p.error('empty IQ file')
    offset = args.offset_hz if args.raw else -300000
    (v, n), timing = run(args.lib, blocks, fs, offset, decimate, rational)
    print(json.dumps({'case': args.kind if args.raw else 'am', 'vendor': timing[0],
                      'native': timing[1], 'packet_deadline_ms': packet/fs*1e3,
                      'comparison': compare(v, n, fs/decimate,
                                            None if args.raw else 1000,
                                            args.kind if args.raw else 'am')}))
    if args.raw:
        return
    for kind, tone in (('cw', 1000), ('fm', 1000), ('cw', fs/decimate + 1000),
                       ('cw', -fs/decimate + 1000)):
        blocks = fixture(fs, packet, args.packets, offset, tone, kind)
        (v, n), timing = run(args.lib, blocks, fs, offset, decimate, rational)
        rms = [float(np.sqrt(np.mean(abs(z[200:]) ** 2))) for z in (v, n)]
        print(json.dumps({'case': kind, 'tone_hz': tone, 'rms': rms,
                          'vendor': timing[0], 'native': timing[1],
                          'comparison': compare(v, n, fs/decimate, tone, kind)
                          if tone == 1000 else None}))


if __name__ == '__main__':
    main()
