#!/usr/bin/env python3
"""Measure raw IQS delivery without WebSA's DDC/publisher; service must be stopped."""
from __future__ import annotations

import argparse
import json
import time
from ctypes import byref, c_void_p

import htra_api as T
from tools.sdr_probe.probe_stream import config
from web_sa.hardware import sdk_bindings as sb


def open_device():
    dev = c_void_p()
    profile = T.BootProfile_TypeDef()
    info = sb.BootInfo_TypeDef()
    profile.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    profile.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    status = T.dll.Device_Open(byref(dev), T.c_int(0), byref(profile), byref(info))
    if status not in (0, -49):
        raise RuntimeError(f'Device_Open status={status}')
    return dev


def measure(dev, decimate: int, seconds: float = 5.0) -> dict:
    st, _out, info = config(dev, 100e6, decimate, T.TriggerMode_TypeDef.Adaptive)
    if st != 0:
        return {'decimate': decimate, 'config_status': st}
    stream = sb.IQStream_TypeDef()
    packet_samples = int(info.PacketSamples)
    count = samples = errors = 0
    first = last = None
    max_gap = 0.0
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
            samples += int(stream.IQS_StreamInfo.PacketSamples)
    finally:
        T.dll.IQS_BusTriggerStop(byref(dev))
    duration = (last - first) if first is not None and count > 1 else 0
    # Include one packet's expected duration in denominator: measuring between completions
    # otherwise counts N packets across N-1 intervals and biases a short run high.
    duration += packet_samples / float(info.IQSampleRate)
    return {'decimate': decimate, 'configured_sps': float(info.IQSampleRate),
            'delivered_sps': round(samples / duration) if duration else 0,
            'packet_samples': packet_samples, 'packets': count,
            'errors': errors, 'max_gap_ms': round(max_gap * 1000, 1),
            'seconds': round(duration, 2)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--decimates', nargs='+', type=int, default=[16, 8])
    parser.add_argument('--seconds', type=float, default=5.0)
    args = parser.parse_args()
    dev = open_device()
    try:
        for decimate in args.decimates:
            print(json.dumps(measure(dev, decimate, args.seconds)), flush=True)
    finally:
        T.dll.Device_Close(byref(dev))
