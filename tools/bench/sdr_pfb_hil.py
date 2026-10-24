#!/usr/bin/env python3
"""Exclusive SAN-90 IQS -> bounded worker-pool Rust PFB -> 20 fps FFT probe.

WebSA must be stopped. Does not modify WebSA's live SDR path or tune the RF source.
The --fixture mode exercises the same scheduling and order logic without a device.
"""
from __future__ import annotations

import argparse
import ctypes as C
import os
import queue
import threading
import time
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from tools.bench.sdr_pfb_bench import fixture, percentile_ms
from tools.bench.sdr_raw_iqs_check import open_device, timestamp_summary
from tools.reference.sdr_capture import CapturedIq
from tools.reference.spectrum import Panadapter


class PfbApi:
    def __init__(self, path: Path, fs: float, bins: list[int]):
        self.lib = C.CDLL(str(path.resolve()))
        t = self.lib
        t.pfb_open_kernel.argtypes = [C.c_double, C.c_uint32, C.c_uint32]
        t.pfb_open_kernel.restype = C.c_void_p
        t.pfb_clone.argtypes = [C.c_void_p]
        t.pfb_clone.restype = C.c_void_p
        t.pfb_close.argtypes = [C.c_void_p]
        t.pfb_geometry.argtypes = [C.c_void_p, C.POINTER(C.c_uint32), C.POINTER(C.c_uint32),
                                   C.POINTER(C.c_double), C.POINTER(C.c_double)]
        t.pfb_geometry.restype = C.c_int
        t.pfb_process.argtypes = [C.c_void_p, C.POINTER(C.c_int16), C.c_int,
                                  C.POINTER(C.c_uint32), C.c_int, C.POINTER(C.c_float), C.c_int]
        t.pfb_process.restype = C.c_int
        self.controller = t.pfb_open_kernel(fs, 2, 1)
        if not self.controller:
            raise RuntimeError('PFB grouped AVX2/FMA kernel unavailable')
        m, hop, rate, delay = C.c_uint32(), C.c_uint32(), C.c_double(), C.c_double()
        if t.pfb_geometry(self.controller, C.byref(m), C.byref(hop),
                          C.byref(rate), C.byref(delay)):
            raise RuntimeError('pfb_geometry failed')
        if any(bin_index >= m.value or bin_index < 0 for bin_index in bins):
            raise ValueError('requested PFB bin outside capture')
        self.hop = hop.value
        self.bins = (C.c_uint32 * len(bins))(*bins)
        self.bin_count = len(bins)

    def process(self, handle, iq, output):
        got = self.lib.pfb_process(handle, iq.ctypes.data_as(C.POINTER(C.c_int16)),
                                   iq.size // 2, self.bins, self.bin_count,
                                   output, len(output) // 2)
        if got < 0:
            raise RuntimeError('pfb_process rejected IQ packet')
        return got

    def advance(self, iq):
        got = self.lib.pfb_process(self.controller,
                                   iq.ctypes.data_as(C.POINTER(C.c_int16)), iq.size // 2,
                                   None, 0, None, 0)
        if got < 0:
            raise RuntimeError('PFB history advance failed')
        return got

    def close(self):
        if self.controller:
            self.lib.pfb_close(self.controller)
            self.controller = None


def run_pipeline(next_packet, fs: float, n: int, center: float, api: PfbApi,
                 target_packets: int) -> dict:
    tasks, results = queue.Queue(maxsize=2), queue.Queue(maxsize=2)
    fft = Panadapter(2048)
    output_len = 2 * api.bin_count * (n // api.hop + 2)
    cancelled = threading.Event()
    worker_times = []
    threads = []
    first_packets = []
    first_outputs = {}
    pending = {}
    stamps = []
    next_result = produced = delivered_groups = controller_groups = pan_frames = 0
    input_samples = output_checksum = 0
    max_pending = max_task = max_result = task_full = 0
    send_wait_s = result_wait_s = clone_s = advance_s = 0.0
    first_host = last_host = 0.0
    last_pan = 0.0

    def worker():
        output = (C.c_float * output_len)()
        while not cancelled.is_set():
            try:
                seq, handle, packet = tasks.get(timeout=0.05)
            except queue.Empty:
                continue
            try:
                started = time.perf_counter()
                got = api.process(handle, packet.iq, output)
                ms = (time.perf_counter() - started) * 1000
                saved = (np.ctypeslib.as_array(output)[:2 * got * api.bin_count].copy()
                         if seq < 10 else None)
                value = float(output[0]) if got else 0.0
                row = (seq, got, value, ms, saved, None)
            except Exception as exc:
                row = (seq, 0, 0.0, 0.0, None, exc)
            finally:
                api.lib.pfb_close(handle)
            while not cancelled.is_set():
                try:
                    results.put(row, timeout=0.05)
                    break
                except queue.Full:
                    continue

    for _ in range(2):
        thread = threading.Thread(target=worker, name='pfb-only-worker', daemon=True)
        thread.start()
        threads.append(thread)

    def collect():
        nonlocal next_result, delivered_groups, output_checksum, max_pending, result_wait_s
        started = time.perf_counter()
        row = results.get(timeout=2)
        result_wait_s += time.perf_counter() - started
        seq, got, value, ms, saved, error = row
        if error:
            raise RuntimeError(f'PFB worker packet {seq} failed') from error
        worker_times.append(ms)
        pending[seq] = (got, value, saved)
        max_pending = max(max_pending, len(pending))
        while next_result in pending:
            count, checksum, output = pending.pop(next_result)
            delivered_groups += count
            output_checksum += checksum
            if output is not None:
                first_outputs[next_result] = output
            next_result += 1

    started = 0.0
    try:
        for seq in range(target_packets):
            packet = next_packet()
            if packet is None or packet.status:
                raise RuntimeError(f'IQS fetch failed at packet {seq}: {packet}')
            if not started:
                started = time.monotonic()
            points = packet.iq.size // 2
            if points < 2 or points > n:
                raise RuntimeError(f'invalid packet length={points}')
            if seq < 10:
                first_packets.append(packet.iq)
            if not first_host:
                first_host = packet.received_at
            last_host = packet.received_at
            stamps.append((int(packet.stream.IQS_TriggerInfo.SysTimerCountOfFirstDataPoint), points))
            input_samples += points
            if time.monotonic() - last_pan >= 0.05:
                if fft.process(packet.iq[::2], packet.iq[1::2], fs, center,
                               float(packet.stream.IQS_ScaleToV), bandwidth=0.8 * fs) is not None:
                    pan_frames += 1
                last_pan = time.monotonic()
            t0 = time.perf_counter()
            snapshot = api.lib.pfb_clone(api.controller)
            clone_s += time.perf_counter() - t0
            if not snapshot:
                raise RuntimeError('pfb_clone failed')
            try:
                t0 = time.perf_counter()
                controller_groups += api.advance(packet.iq)
                advance_s += time.perf_counter() - t0
                t0 = time.perf_counter()
                try:
                    tasks.put_nowait((seq, snapshot, packet))
                except queue.Full:
                    task_full += 1
                    tasks.put((seq, snapshot, packet), timeout=2)
                send_wait_s += time.perf_counter() - t0
            except Exception:
                api.lib.pfb_close(snapshot)
                raise
            produced += 1
            max_task = max(max_task, tasks.qsize())
            max_result = max(max_result, results.qsize())
            if produced - next_result >= 3:
                collect()
        while next_result < produced:
            collect()
        assert delivered_groups == controller_groups, (delivered_groups, controller_groups)
        elapsed = time.monotonic() - started
        serial = api.lib.pfb_open_kernel(fs, 2, 1)
        assert serial
        try:
            out = (C.c_float * output_len)()
            for seq, block in enumerate(first_packets):
                expected = api.process(serial, block, out)
                got = first_outputs[seq]
                np.testing.assert_array_equal(got, np.ctypeslib.as_array(out)[:2 * expected * api.bin_count])
        finally:
            api.lib.pfb_close(serial)
    finally:
        cancelled.set()
        while True:
            try:
                _, handle, _ = tasks.get_nowait()
                api.lib.pfb_close(handle)
            except queue.Empty:
                break
        for thread in threads:
            thread.join(timeout=2)
            if thread.is_alive():
                raise RuntimeError('PFB worker did not exit')
    elapsed = time.monotonic() - started
    host_window = last_host - first_host + n / fs if first_host else 0
    clock = timestamp_summary(stamps, fs)
    return {'input_msps': round(input_samples / elapsed / 1e6, 3),
            'capture_host_msps': round(input_samples / host_window / 1e6, 3),
            'host_minus_device_ms': (round((host_window - clock['device_duration_s']) * 1000, 2)
                                     if clock['timestamp_usable'] else None),
            'elapsed_s': round(elapsed, 3), 'packets': produced,
            'spectrum_fps': round(pan_frames / elapsed, 2),
            'pfb_groups': delivered_groups, 'output_checksum': round(output_checksum, 3),
            'timestamps': clock,
            'queue_task_max': max_task, 'queue_result_max': max_result,
            'queue_pending_max': max_pending, 'queue_task_full': task_full,
            'queue_send_wait_s': round(send_wait_s, 3),
            'result_wait_s': round(result_wait_s, 3),
            'clone_ms_per_packet': round(clone_s * 1000 / produced, 3),
            'history_advance_ms_per_packet': round(advance_s * 1000 / produced, 3),
            'worker': percentile_ms([int(ms * 1e6) for ms in worker_times])}


class PoolResult(C.Structure):
    _fields_ = [('seq', C.c_uint64), ('stamp', C.c_uint64),
                ('groups', C.c_uint32), ('first_re', C.c_float),
                ('process_ns', C.c_uint64)]


class PoolStats(C.Structure):
    _fields_ = [(name, C.c_uint64) for name in (
        'submitted', 'completed', 'groups', 'clone_ns', 'advance_ns',
        'task_wait_ns', 'result_wait_ns')] + [
        (name, C.c_uint32) for name in (
            'max_pending', 'max_ready', 'task_capacity', 'result_capacity')]


class NativePool:
    def __init__(self, library: Path, fs: float, n: int, bins: list[int], strategy: int = 0):
        self.lib = C.CDLL(str(library.resolve()))
        t = self.lib
        t.pfb_pool_open_strategy.argtypes = [C.c_double, C.c_uint32, C.POINTER(C.c_uint32),
                                             C.c_uint32, C.c_uint32]
        t.pfb_pool_open_strategy.restype = C.c_void_p
        t.pfb_pool_submit.argtypes = [C.c_void_p, C.c_uint64, C.c_uint64,
                                      C.POINTER(C.c_int16), C.c_uint32]
        t.pfb_pool_submit.restype = C.c_int
        t.pfb_pool_take.argtypes = [C.c_void_p, C.POINTER(PoolResult)]
        t.pfb_pool_take.restype = C.c_int
        t.pfb_pool_finish.argtypes = [C.c_void_p]
        t.pfb_pool_finish.restype = C.c_int
        t.pfb_pool_stats.argtypes = [C.c_void_p, C.POINTER(PoolStats)]
        t.pfb_pool_stats.restype = C.c_int
        t.pfb_pool_close.argtypes = [C.c_void_p]
        selected = (C.c_uint32 * len(bins))(*bins)
        self.handle = t.pfb_pool_open_strategy(fs, n, selected, len(bins), strategy)
        if not self.handle:
            raise RuntimeError('native PFB pool unavailable')

    def take(self, report):
        row = PoolResult()
        status = self.lib.pfb_pool_take(self.handle, C.byref(row))
        if status < 0:
            raise RuntimeError('pfb_pool_take failed')
        if status:
            report(row)
        return bool(status)

    def submit(self, seq, stamp, iq, report):
        while True:
            status = self.lib.pfb_pool_submit(self.handle, seq, stamp,
                        iq.ctypes.data_as(C.POINTER(C.c_int16)), iq.size // 2)
            if status == 0:
                break
            if status != -2 or not self.take(report):
                raise RuntimeError(f'pfb_pool_submit status={status}')
        while self.take(report):
            pass

    def finish(self, report):
        if self.lib.pfb_pool_finish(self.handle):
            raise RuntimeError('pfb_pool_finish failed')
        while self.take(report):
            pass

    def stats(self):
        data = PoolStats()
        if self.lib.pfb_pool_stats(self.handle, C.byref(data)):
            raise RuntimeError('pfb_pool_stats failed')
        return {name: getattr(data, name) for name, _ in data._fields_}

    def close(self):
        if self.handle:
            self.lib.pfb_pool_close(self.handle)
            self.handle = None


def run_native_pool(next_packet, fs: float, n: int, center: float,
                    library: Path, bins: list[int], target_packets: int,
                    strategy: int = 0) -> dict:
    pool = NativePool(library, fs, n, bins, strategy)
    fft = Panadapter(2048)
    stamps = []
    first_packets = []
    first_values = {}
    first_host = last_host = 0.0
    last_pan = 0.0
    pan_frames = input_samples = delivered = groups = 0
    worker_ns = []
    checksum = 0.0

    def on_result(row):
        nonlocal delivered, groups, checksum
        if row.seq != delivered or row.stamp != stamps[delivered][0]:
            raise RuntimeError(f'PFB result order/timestamp mismatch: {row.seq}')
        if row.seq < 10:
            first_values[row.seq] = (row.groups, row.first_re)
        delivered += 1
        groups += row.groups
        checksum += row.first_re
        worker_ns.append(row.process_ns)

    started = 0.0
    try:
        for seq in range(target_packets):
            packet = next_packet()
            if packet is None or packet.status:
                raise RuntimeError(f'IQS fetch failed at packet {seq}: {packet}')
            if not started:
                started = time.monotonic()
            points = packet.iq.size // 2
            if points < 2 or points > n:
                raise RuntimeError(f'invalid IQ packet length={points}')
            if seq < 10:
                first_packets.append(packet.iq)
            if not first_host:
                first_host = packet.received_at
            last_host = packet.received_at
            stamps.append((int(packet.stream.IQS_TriggerInfo.SysTimerCountOfFirstDataPoint), points))
            input_samples += points
            if time.monotonic() - last_pan >= 0.05:
                if fft.process(packet.iq[::2], packet.iq[1::2], fs, center,
                               float(packet.stream.IQS_ScaleToV), bandwidth=0.8 * fs) is not None:
                    pan_frames += 1
                last_pan = time.monotonic()
            pool.submit(seq, stamps[-1][0], packet.iq, on_result)
        pool.finish(on_result)
        elapsed = time.monotonic() - started
        assert delivered == target_packets
        stats = pool.stats()
        assert stats['submitted'] == stats['completed'] == delivered
        assert stats['groups'] == groups
        capacity = stats['task_capacity']
        assert capacity == stats['result_capacity'] == 6
        # Ready results are 32-byte metadata, but they are still bounded: the pool
        # never holds more than one in-flight result per worker plus its two slots.
        if stats['max_pending'] > capacity or stats['max_ready'] > capacity:
            raise RuntimeError(f'unbounded PFB result metadata: {stats}')
        serial = PfbApi(library, fs, bins)
        try:
            out = (C.c_float * (2 * len(bins) * (n // serial.hop + 2)))()
            for seq, block in enumerate(first_packets):
                count = serial.process(serial.controller, block, out)
                if (count, out[0]) != first_values[seq]:
                    raise RuntimeError(f'native pool differs from serial PFB at {seq}')
        finally:
            serial.close()
        host_window = last_host - first_host + n / fs if first_host else 0
        clock = timestamp_summary(stamps, fs)
        return {'input_msps': round(input_samples / elapsed / 1e6, 3),
                'capture_host_msps': round(input_samples / host_window / 1e6, 3),
                'host_minus_device_ms': (round((host_window - clock['device_duration_s']) * 1000, 2)
                                         if clock['timestamp_usable'] else None),
                'elapsed_s': round(elapsed, 3), 'packets': delivered,
                'spectrum_fps': round(pan_frames / elapsed, 2),
                'pfb_groups': groups, 'checksum': round(checksum, 3),
                'timestamps': clock, 'worker': percentile_ms(worker_ns),
                'pool': {key: (round(value / delivered / 1e6, 4) if key.endswith('_ns') else value)
                         for key, value in stats.items()}}
    finally:
        pool.close()


class FixtureSource:
    def __init__(self, fs: float, n: int, packets: int):
        self.iq = fixture(fs, min(packets, 120), n, 20261019)
        self.fs, self.n, self.at, self.target = fs, n, 0, packets
        self.iq.flags.writeable = False

    def get(self):
        if self.at == self.target:
            return None
        seq = self.at
        self.at += 1
        stream = SimpleNamespace(
            IQS_TriggerInfo=SimpleNamespace(SysTimerCountOfFirstDataPoint=1000 + seq * self.n * 2),
            IQS_ScaleToV=1e-5)
        return CapturedIq(0, seq * self.n, 0, stream, self.iq[seq % len(self.iq)], time.monotonic())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--lib', type=Path,
                        default=Path(__file__).resolve().parents[2] / 'backend/target/release/libwebsa_channelizer.so')
    parser.add_argument('--seconds', type=float, default=3.0)
    parser.add_argument('--bins', type=int, choices=(4, 32), default=32)
    parser.add_argument('--fixture', action='store_true')
    parser.add_argument('--native-pool', action='store_true',
                        help='run bounded scheduling in Rust instead of Python')
    parser.add_argument('--overlap-history', action='store_true',
                        help='use fixed FIR tail instead of full-packet history advance in native pool')
    args = parser.parse_args()
    if args.overlap_history and not args.native_pool:
        parser.error('--overlap-history requires --native-pool')
    if args.seconds <= 0:
        parser.error('--seconds must be positive')
    fs, n = 62_500_000.0, 16240
    target = round(fs * args.seconds / n)
    selected = [0, 5, 31, 127] if args.bins == 4 else list(range(32))
    api = None if args.native_pool else PfbApi(args.lib, fs, selected)

    def execute(next_packet, center):
        if args.native_pool:
            return run_native_pool(next_packet, fs, n, center, args.lib, selected, target,
                                   strategy=int(args.overlap_history))
        return run_pipeline(next_packet, fs, n, center, api, target)

    if args.fixture:
        src = FixtureSource(fs, n, target)
        try:
            print(execute(src.get, 100e6), flush=True)
        finally:
            if api is not None:
                api.close()
        return
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            executable = (entry / 'exe').readlink().name.removesuffix(' (deleted)')
            if executable == 'websad':
                try:
                    settings = (entry / 'environ').read_bytes().split(b'\0')
                except PermissionError:
                    parser.error('cannot inspect native WebSA environment to establish exclusive hardware access')
                if any(value.startswith(b'WEBSA_RUST_IQS_SHIM=')
                       and value != b'WEBSA_RUST_IQS_SHIM=' for value in settings):
                    parser.error('stop the native WebSA service before the exclusive hardware probe')
        except OSError:
            pass  # Processes may exit while /proc is being scanned.
    import htra_api as T
    from tools.reference.sdr_capture import SdrCapture
    from tools.sdr_probe.probe_stream import config

    dev = open_device()
    capture = None
    triggered = False
    try:
        status, out, info = config(dev, 100e6, 1, T.TriggerMode_TypeDef.Adaptive,
                                   bus_timeout_ms=250)
        if status:
            raise RuntimeError(f'IQS_Configuration status={status}')
        if int(info.PacketSamples) != n or float(info.IQSampleRate) != fs:
            raise RuntimeError(f'unexpected IQS geometry: {info.PacketSamples}, {info.IQSampleRate}')
        if T.dll.IQS_BusTriggerStart(C.byref(dev)):
            raise RuntimeError('IQS_BusTriggerStart failed')
        triggered = True
        capture = SdrCapture(SimpleNamespace(dev=dev), threading.RLock(), n)
        capture.start()
        result = execute(capture.get, float(out.CenterFreq_Hz))
        capture.stop()
        T.dll.IQS_BusTriggerStop(C.byref(dev))
        triggered = False
        result['capture_queue_highwater'] = capture.queue_highwater
        result['capture_queue_full'] = capture.queue_full
        result['capture_queue_wait_s'] = round(capture.queue_wait_s, 3)
        result['capture_queue_blocked'] = capture.queue_blocked
        result['sdk_fetch_mean_ms'] = round(capture.fetch_s * 1000 / max(1, capture.fetch_calls), 3)
        result['sdk_fetch_max_ms'] = round(capture.fetch_max_s * 1000, 3)
        result['stop_extra_samples'] = capture.captured_samples - result['packets'] * n
        print(result, flush=True)
        if (result['input_msps'] < fs / 1e6 * 0.995
                or result['timestamps'].get('timestamp_gap_candidates')
                or result['capture_queue_blocked'] or result['spectrum_fps'] < 19.5
                or result['stop_extra_samples'] > 3*n):
            raise RuntimeError('50 MHz capture/PFB did not sustain realtime')
    finally:
        try:
            if capture is not None:
                try:
                    capture.stop()
                except RuntimeError:
                    os._exit(2)  # A stuck SDK thread must not race Device_Close.
            if triggered:
                T.dll.IQS_BusTriggerStop(C.byref(dev))
        finally:
            T.dll.Device_Close(C.byref(dev))
            if api is not None:
                api.close()


if __name__ == '__main__':
    main()
