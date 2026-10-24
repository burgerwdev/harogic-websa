#!/usr/bin/env python3
"""Repeatable offline PFB packet benchmark; no device/SDK access."""
from __future__ import annotations

import argparse
import ctypes as C
import hashlib
import json
import time
from math import gcd
from pathlib import Path

import numpy as np


class StageTimes(C.Structure):
    _fields_ = [('fir_ns', C.c_uint64), ('fft_ns', C.c_uint64),
                ('output_ns', C.c_uint64), ('groups', C.c_uint64)]


def fixture(fs: float, packets: int, n: int, seed: int) -> np.ndarray:
    rng = np.random.default_rng(seed)
    iq = np.empty((packets, n * 2), dtype=np.int16)
    offsets = np.arange(n, dtype=np.float64)
    for block in range(packets):
        phase = 2 * np.pi * 1_250_000 * (block * n + offsets) / fs
        noise = rng.integers(-96, 97, size=n * 2, dtype=np.int16)
        iq[block, 0::2] = np.rint(12000 * np.cos(phase)).astype(np.int16) + noise[0::2]
        iq[block, 1::2] = np.rint(12000 * np.sin(phase)).astype(np.int16) + noise[1::2]
    return iq


def percentile_ms(samples_ns: list[int]) -> dict:
    ms = np.asarray(samples_ns, dtype=np.float64) / 1e6
    return {name: round(float(value), 4) for name, value in (
        ('p50_ms', np.percentile(ms, 50)), ('p95_ms', np.percentile(ms, 95)),
        ('p99_ms', np.percentile(ms, 99)), ('max_ms', np.max(ms)))}


def run(library: Path, fs: float, packets: int, repeats: int, raw: Path | None,
        oversample: int = 2, packet_samples: int = 16240, seed: int = 20261019,
        kernel: int = 0) -> list[dict]:
    lib = C.CDLL(str(library.resolve()))
    lib.pfb_open_os.argtypes = [C.c_double, C.c_uint32]
    lib.pfb_open_os.restype = C.c_void_p
    lib.pfb_open_kernel.argtypes = [C.c_double, C.c_uint32, C.c_uint32]
    lib.pfb_open_kernel.restype = C.c_void_p
    lib.pfb_close.argtypes = [C.c_void_p]
    lib.pfb_geometry.argtypes = [C.c_void_p, C.POINTER(C.c_uint32), C.POINTER(C.c_uint32),
                                 C.POINTER(C.c_double), C.POINTER(C.c_double)]
    lib.pfb_geometry.restype = C.c_int
    lib.pfb_diagnostics.argtypes = [C.c_void_p, C.POINTER(C.c_uint32),
                                    C.POINTER(C.c_uint32), C.POINTER(C.c_uint32)]
    lib.pfb_diagnostics.restype = C.c_int
    lib.pfb_process.argtypes = [C.c_void_p, C.POINTER(C.c_int16), C.c_int,
                                C.POINTER(C.c_uint32), C.c_int, C.POINTER(C.c_float), C.c_int]
    lib.pfb_process.restype = C.c_int
    profiled = hasattr(lib, 'pfb_take_profile')
    if profiled:
        lib.pfb_take_profile.argtypes = [C.c_void_p, C.POINTER(StageTimes)]
        lib.pfb_take_profile.restype = C.c_int
    n = packet_samples
    if raw is not None:
        if fs != 31_250_000.0 or n != 16240:
            raise ValueError('recorded fixture is 31.25 MSps Complex16 / 16,240 samples per packet')
        data = np.fromfile(raw, dtype='<i2')
        if data.size == 0 or data.size % (n * 2):
            raise ValueError('raw IQ must contain full Complex16 packets')
        iq = data.reshape(-1, n * 2)
        source = 'recorded-complex16'
    else:
        iq = fixture(fs, packets, n, seed)
        source = 'synthetic-complex16'
    digest = hashlib.sha256(iq.tobytes()).hexdigest()
    pointers = [block.ctypes.data_as(C.POINTER(C.c_int16)) for block in iq]
    copied = np.empty_like(iq[0])
    copy_timings = []
    for _ in range(repeats):
        for block in iq:
            t0 = time.perf_counter_ns()
            np.copyto(copied, block)
            copy_timings.append(time.perf_counter_ns() - t0)
    copy_stats = percentile_ms(copy_timings)
    results = []
    for wanted_bins in (0, 1, 8, 32):
        handle = lib.pfb_open_kernel(fs, oversample, kernel)
        if not handle:
            raise ValueError(f'unsupported PFB fs={fs}, oversample={oversample}, kernel={kernel}')
        try:
            m, hop = C.c_uint32(), C.c_uint32()
            rate, delay = C.c_double(), C.c_double()
            taps, per_phase, simd = C.c_uint32(), C.c_uint32(), C.c_uint32()
            if lib.pfb_geometry(handle, C.byref(m), C.byref(hop), C.byref(rate), C.byref(delay)):
                raise RuntimeError('pfb_geometry failed')
            if lib.pfb_diagnostics(handle, C.byref(taps), C.byref(per_phase), C.byref(simd)):
                raise RuntimeError('pfb_diagnostics failed')
            if wanted_bins > m.value:
                continue
            bins = (C.c_uint32 * wanted_bins)(*range(wanted_bins))
            output = (C.c_float * (2 * wanted_bins * (n // hop.value + 2)))()
            period = hop.value // gcd(n, hop.value)
            warmup = period * ((8 + period - 1) // period)
            for idx in range(warmup):
                got = lib.pfb_process(handle, pointers[idx % len(pointers)], n,
                                      bins, wanted_bins, output, len(output) // 2)
                if got < 0:
                    raise RuntimeError('PFB warmup rejected input')
            assert warmup * n % hop.value == 0
            record = StageTimes()
            if profiled:
                assert lib.pfb_take_profile(handle, C.byref(record)) == 0
            timings = []
            stages = {'fir': [], 'fft': [], 'post': []}
            total_groups = 0
            started = time.perf_counter()
            for _ in range(repeats):
                for ptr in pointers:
                    t0 = time.perf_counter_ns()
                    got = lib.pfb_process(handle, ptr, n, bins, wanted_bins,
                                          output, len(output) // 2)
                    timings.append(time.perf_counter_ns() - t0)
                    if got < 0:
                        raise RuntimeError('pfb_process rejected packet')
                    if profiled:
                        assert lib.pfb_take_profile(handle, C.byref(record)) == 0
                        assert record.groups == (got if wanted_bins else 0)
                        stages['fir'].append(record.fir_ns)
                        stages['fft'].append(record.fft_ns)
                        stages['post'].append(record.output_ns)
                    total_groups += got
            elapsed = time.perf_counter() - started
            input_samples = repeats * len(iq) * n
            assert total_groups == input_samples // hop.value
            overhead = []
            for _ in range(200):
                t0 = time.perf_counter_ns()
                lib.pfb_process(handle, pointers[0], 0, bins, wanted_bins,
                                output, len(output) // 2)
                overhead.append(time.perf_counter_ns() - t0)
            results.append({
                'source': source, 'seed': seed if raw is None else None,
                'iq_sha256': digest, 'packets': len(iq), 'repeats': repeats,
                'input_rate_msps': fs / 1e6, 'packet_samples': n,
                'packet_deadline_ms': round(n / fs * 1000, 4),
                'input_msps': round(input_samples / elapsed / 1e6, 3),
                'instrumented': profiled,
                **percentile_ms(timings),
                **{f'copy_{k}': v for k, v in copy_stats.items()},
                **{f'{stage}_{k}': value for stage, data in stages.items() if data
                   for k, value in percentile_ms(data).items()},
                'ctypes_empty_call_p50_us': round(float(np.median(overhead)) / 1000, 2),
                'coarse_bins': m.value, 'oversample': oversample,
                'fir_kernel': kernel,
                'requested_bins': wanted_bins, 'hop': hop.value,
                'taps_total': taps.value, 'taps_per_phase': per_phase.value,
                'fir_avx2_fma': bool(simd.value & 1), 'fft_avx': bool(simd.value & 2),
                'output_sps_per_bin': round(rate.value),
                'group_delay_outputs': round(delay.value, 2),
            })
        finally:
            lib.pfb_close(handle)
    return results


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--lib', type=Path,
                        default=Path('native-ddc/target/release/libwebsa_channelizer.so'))
    parser.add_argument('--raw', type=Path, help='explicit recorded 31.25 MSps Complex16 fixture')
    parser.add_argument('--fs', type=float, choices=(31_250_000.0, 62_500_000.0),
                        default=31_250_000.0)
    parser.add_argument('--packet-samples', type=int, choices=(16240, 32480), default=16240)
    parser.add_argument('--packets', type=int, default=120)
    parser.add_argument('--repeats', type=int, default=8)
    parser.add_argument('--seed', type=int, default=20261019)
    parser.add_argument('--oversample', type=int, choices=(2, 4), default=2)
    parser.add_argument('--kernel', type=int, choices=(0, 1), default=0,
                        help='0=unchanged baseline, 1=grouped AVX2 FIR (2x only)')
    args = parser.parse_args()
    if args.packets <= 0 or args.repeats <= 0:
        parser.error('--packets and --repeats must be positive')
    for row in run(args.lib, args.fs, args.packets, args.repeats, args.raw,
                   args.oversample, args.packet_samples, args.seed, args.kernel):
        print(json.dumps(row), flush=True)
