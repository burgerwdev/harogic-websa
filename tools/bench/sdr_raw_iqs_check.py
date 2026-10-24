#!/usr/bin/env python3
"""Measure raw IQS delivery without WebSA's DDC/publisher; service must be stopped."""
from __future__ import annotations

import argparse
import json
import time
from ctypes import byref, c_void_p, string_at
from statistics import median


def open_device():
    import htra_api as T
    from tools.reference import sdk_bindings as sb

    dev = c_void_p()
    profile = T.BootProfile_TypeDef()
    info = sb.BootInfo_TypeDef()
    profile.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    profile.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    status = T.dll.Device_Open(byref(dev), T.c_int(0), byref(profile), byref(info))
    if status not in (0, -49):
        raise RuntimeError(f'Device_Open status={status}')
    return dev


def timestamp_summary(records: list[tuple[int, int]], fs: float) -> dict:
    if len(records) < 3 or any(t <= 0 for t, _ in records):
        return {'timestamp_usable': False, 'timestamp_packets': len(records)}
    steps = [(b[0] - a[0], a[1]) for a, b in zip(records, records[1:], strict=False)]
    positive = [delta / samples for delta, samples in steps if delta > 0 and samples > 0]
    if len(positive) < 2:
        return {'timestamp_usable': False, 'timestamp_packets': len(records)}
    ticks_per_sample = median(positive)
    ratios = [delta / (samples * ticks_per_sample) for delta, samples in steps if samples > 0]
    device_duration = ((records[-1][0] - records[0][0]) / (ticks_per_sample * fs)
                       + records[-1][1] / fs)
    return {'timestamp_usable': True, 'timestamp_packets': len(records),
            'timestamp_first': records[0][0], 'timestamp_last': records[-1][0],
            'ticks_per_sample_median': round(ticks_per_sample, 6),
            'estimated_timer_hz': round(ticks_per_sample * fs),
            'device_duration_s': round(device_duration, 6),
            'device_window_sps': round(sum(points for _, points in records) / device_duration),
            'nonmonotonic_steps': sum(delta <= 0 for delta, _ in steps),
            'step_ratio_max': round(max(ratios), 3),
            # Candidates, not proven loss: SDK/device clock units must be verified first.
            'timestamp_gap_candidates': sum(ratio > 1.5 for ratio in ratios)}


def measure(dev, decimate: int, seconds: float = 5.0, raw_path=None, fmt='16',
            timestamps=False) -> dict:
    import htra_api as T
    from tools.reference import sdk_bindings as sb
    from tools.sdr_probe.probe_stream import config

    formats = {'8': T.DataFormat_TypeDef.Complex8bit,
               '16': T.DataFormat_TypeDef.Complex16bit}
    st, _out, info = config(dev, 100e6, decimate, T.TriggerMode_TypeDef.Adaptive,
                            data_format=formats[fmt])
    if st != 0:
        return {'decimate': decimate, 'config_status': st}
    stream = sb.IQStream_TypeDef()
    packet_samples = int(info.PacketSamples)
    count = samples = errors = 0
    first = last = None
    max_gap = 0.0
    saved = 0
    stamps = []
    raw = open(raw_path, 'wb') if raw_path else None  # noqa: SIM115 - closed in finally
    try:
        status = T.dll.IQS_BusTriggerStart(byref(dev))
        if status != 0:
            return {'decimate': decimate, 'start_status': status}
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            status = T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream))
            now = time.monotonic()
            if status:
                errors += 1
                continue
            if first is None:
                first = now
            if last is not None:
                max_gap = max(max_gap, now - last)
            last = now
            count += 1
            points = int(stream.IQS_StreamInfo.PacketSamples)
            samples += points
            if timestamps:
                stamps.append((int(stream.IQS_TriggerInfo.SysTimerCountOfFirstDataPoint), points))
            if raw and saved < 120 and points == packet_samples:
                raw.write(string_at(stream.AlternIQStream, packet_samples * 4))
                saved += 1
    finally:
        T.dll.IQS_BusTriggerStop(byref(dev))
        if raw:
            raw.close()
    duration = (last - first) if first is not None and count > 1 else 0
    # Include one packet's expected duration in denominator: measuring between completions
    # otherwise counts N packets across N-1 intervals and biases a short run high.
    duration += packet_samples / float(info.IQSampleRate)
    stamp_info = timestamp_summary(stamps, float(info.IQSampleRate)) if timestamps else None
    return {'decimate': decimate, 'format_bits': int(fmt),
            'configured_sps': float(info.IQSampleRate),
            'timestamps': stamp_info,
            'host_minus_device_ms': (round((duration - stamp_info['device_duration_s']) * 1000, 2)
                                     if stamp_info and stamp_info['timestamp_usable'] else None),
            'saved_packets': saved,
            'delivered_sps': round(samples / duration) if duration else 0,
            'packet_samples': packet_samples, 'packets': count,
            'errors': errors, 'max_gap_ms': round(max_gap * 1000, 1),
            'seconds': round(duration, 2)}


if __name__ == '__main__':
    import htra_api as T

    parser = argparse.ArgumentParser()
    parser.add_argument('--decimates', nargs='+', type=int, default=[16, 8])
    parser.add_argument('--seconds', type=float, default=5.0)
    parser.add_argument('--raw-path', help='Record first 120 full 16-bit packets; one decimate only')
    parser.add_argument('--format', choices=('8', '16'), default='16')
    parser.add_argument('--timestamps', action='store_true')
    args = parser.parse_args()
    if args.raw_path and (len(args.decimates) != 1 or args.format != '16'):
        parser.error('--raw-path requires exactly one decimate and --format 16')
    dev = open_device()
    try:
        for decimate in args.decimates:
            print(json.dumps(measure(dev, decimate, args.seconds, args.raw_path,
                                     args.format, args.timestamps)), flush=True)
    finally:
        T.dll.Device_Close(byref(dev))
