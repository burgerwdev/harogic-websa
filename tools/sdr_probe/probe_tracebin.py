#!/usr/bin/env python3
"""SWP trace-grid probe: the effect of TracePointsStrategy on a sweep.

The WebSA profile did not program `SWP_Profile_TypeDef.TraceBinSize_Hz`. With
`points_strategy='bin_size'` (BinSizeAssined) the device used its own ProfileDeInit default for
the trace spacing. This probe records three things:

  - the ProfileDeInit defaults of the device,
  - the trace info of each strategy and bin-size combination (`TraceBinBW_Hz`, `TotalHops`,
    `EstimateMinSweepTime`),
  - the real cost of one full sweep for each span.

The device handle is exclusive. Stop the WebSA service before a run:

    ./stop.sh && python3 tools/sdr_probe/probe_tracebin.py [--json] && ./run.sh
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)

from ctypes import pointer  # noqa: E402

import htra_api  # noqa: E402
from web_sa.hardware import sdk_bindings as sb  # noqa: E402

T = htra_api

FULL_SPAN_HZ = 9.02e9
FULL_CENTER_HZ = 4.51e9
TRACE_INFO_FIELDS = (
    'FullsweepTracePoints', 'PartialsweepTracePoints', 'TotalHops', 'UserStartIndex',
    'UserStopIndex', 'TraceBinBW_Hz', 'StartFreq_Hz', 'AnalysisBW_Hz', 'TraceDetectRatio',
    'DecimateFactor', 'FrameTimeMultiple', 'FrameTime', 'EstimateMinSweepTime',
    'SamplePoints', 'GainParameter',
)


def open_device():
    dev = T.c_void_p()
    # The patched bindings rebind Device_Open to their own BootInfo class. The vendor wrapper
    # keeps the stale DeviceInfo. Pass the structs from `sb` to Device_Open.
    bp = sb.BootProfile_TypeDef()
    bi = sb.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    st = T.dll.Device_Open(pointer(dev), T.c_int(0), pointer(bp), pointer(bi))
    if st not in (0, -49):
        raise RuntimeError(f'Device_Open status={st}')
    return dev, bi


def as_num(v):
    """Convert a ctypes field value to a Python number. A c_int subclass comes back as an instance."""
    raw = getattr(v, 'value', v)
    if isinstance(raw, bytes):
        return int.from_bytes(raw, 'little', signed=True)
    return raw


def defaults(dev) -> dict:
    """Return the ProfileDeInit seed. Every field that the profile does not override comes from here."""
    p = sb.SWP_Profile_TypeDef()
    T.dll.SWP_ProfileDeInit(pointer(dev), pointer(p))
    return {
        'TraceBinSize_Hz': float(p.TraceBinSize_Hz),
        'TracePoints': int(p.TracePoints),
        'TracePointsStrategy': as_num(p.TracePointsStrategy),
        'TraceAlign': as_num(p.TraceAlign),
        'RBW_Hz': float(p.RBW_Hz),
        'RBWMode': as_num(p.RBWMode),
        'VBWMode': as_num(p.VBWMode),
        'SweepTimeMode': as_num(p.SweepTimeMode),
        'SweepTime': float(p.SweepTime),
        'FFTExecutionStrategy': as_num(p.FFTExecutionStrategy),
    }


def trace_info(ti) -> dict:
    return {f: as_num(getattr(ti, f)) for f in TRACE_INFO_FIELDS}


def configure(dev, span_hz, center_hz, strategy=None, bin_size=None, points=None,
              align=None, rbw_mode='auto') -> dict:
    """Call SWP_Configuration once. Return the echo, the trace info, and the wall time of the call."""
    pin = sb.SWP_Profile_TypeDef()
    T.dll.SWP_ProfileDeInit(pointer(dev), pointer(pin))
    pin.FreqAssignment = T.SWP_FreqAssignment_TypeDef.StartStop
    pin.StartFreq_Hz = center_hz - span_hz / 2
    pin.StopFreq_Hz = center_hz + span_hz / 2
    pin.RBWMode = (T.RBWMode_TypeDef.RBW_Auto if rbw_mode == 'auto'
                   else T.RBWMode_TypeDef.RBW_Manual)
    if strategy is not None:
        pin.TracePointsStrategy = strategy
    if bin_size is not None:
        pin.TraceBinSize_Hz = float(bin_size)      # None = leave the ProfileDeInit default
    if points is not None:
        pin.TracePoints = int(points)
    if align is not None:
        pin.TraceAlign = align
    pout = sb.SWP_Profile_TypeDef()
    ti = sb.SWP_TraceInfo_TypeDef()
    t0 = time.perf_counter()
    st = T.dll.SWP_Configuration(pointer(dev), pointer(pin), pointer(pout), pointer(ti))
    dt = time.perf_counter() - t0
    row = {'status': st, 'config_s': round(dt, 4),
           'bin_requested': None if bin_size is None else float(bin_size),
           'echo_rbw': round(float(pout.RBW_Hz), 3),
           'echo_start': float(pout.StartFreq_Hz), 'echo_stop': float(pout.StopFreq_Hz)}
    if st == 0:
        row['ti'] = trace_info(ti)
    return row


def fetch_cost(dev, start_hz, stop_hz, points, n=3, cap=1 << 20) -> dict:
    """Measure the real cost of `n` full sweeps (SWP_GetFullSweep + DSP_InterceptSpectrum).

    Keep ``cap`` well above the traces that this probe asks for. SWP_GetFullSweep writes the trace
    length of the device. A short buffer is what used to crash the DSP call.
    """
    fbuf = (T.c_double * cap)()
    sbuf = (T.c_float * cap)()
    ifbuf = (T.c_double * cap)()
    isbuf = (T.c_float * cap)()
    aux = sb.Full_MeasAuxInfo()
    cnt = T.c_uint32(0)
    times = []
    for _ in range(n):
        t0 = time.perf_counter()
        st = T.dll.SWP_GetFullSweep(pointer(dev), fbuf, sbuf, pointer(aux))
        if st != 0:
            return {'get_status': st, 'times_s': times}
        T.dll.DSP_InterceptSpectrum(T.c_double(start_hz), T.c_double(stop_hz), fbuf, sbuf,
                                    T.c_uint32(points), ifbuf, isbuf, pointer(cnt))
        times.append(round(time.perf_counter() - t0, 4))
    return {'get_status': 0, 'times_s': times, 'intercept_points': int(cnt.value)}


def timing_row(dev, span, center, strategy, points=1000, n=1) -> dict:
    row = configure(dev, span, center, strategy, None, points)
    cost = fetch_cost(dev, row['echo_start'], row['echo_stop'], points, n=n)
    ti = row.get('ti', {})
    return {
        'span_hz': span, 'strategy': int(strategy), 'config_s': row['config_s'],
        'full_points': ti.get('FullsweepTracePoints'), 'hops': ti.get('TotalHops'),
        'bin_bw_hz': ti.get('TraceBinBW_Hz'), 'analysis_bw_hz': ti.get('AnalysisBW_Hz'),
        'decimate': ti.get('DecimateFactor'), 'frame_time_s': ti.get('FrameTime'),
        'detect_ratio': ti.get('TraceDetectRatio'),
        'sweep_s': (cost['times_s'][0] if cost.get('times_s') else None),
        'get_status': cost.get('get_status'),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--json', action='store_true')
    ap.add_argument('--no-timing', action='store_true',
                    help='skip the sweep-time table (its last row sweeps the full span for ~30 s)')
    args = ap.parse_args()

    dev, bi = open_device()
    out = {'device': {'model': int(bi.DeviceInfo.Model), 'mfw': int(bi.DeviceInfo.MFWVersion)}}
    try:
        out['profile_defaults'] = defaults(dev)
        print(f"device {out['device']}")
        print(f"ProfileDeInit defaults: {out['profile_defaults']}")
        speed = T.TracePointsStrategy_TypeDef.SweepSpeedPreferred
        binsize = T.TracePointsStrategy_TypeDef.BinSizeAssined
        cases = {
            'speed_10M_pts1000': (10e6, 1e9, speed, None, 1000),
            'binsize_10M_unset': (10e6, 1e9, binsize, None, 1000),
            'binsize_10M_bin10k': (10e6, 1e9, binsize, 10e3, 1000),
            'binsize_10M_bin100k': (10e6, 1e9, binsize, 100e3, 1000),
            'binsize_10M_bin1M': (10e6, 1e9, binsize, 1e6, 1000),
            'binsize_100M_bin1M': (100e6, 1e9, binsize, 1e6, 1000),
            'binsize_full_unset': (FULL_SPAN_HZ, FULL_CENTER_HZ, binsize, None, 1000),
            'binsize_full_bin1M': (FULL_SPAN_HZ, FULL_CENTER_HZ, binsize, 1e6, 1000),
            'speed_full_pts1000': (FULL_SPAN_HZ, FULL_CENTER_HZ, speed, None, 1000),
        }
        out['cases'] = {}
        for name, (span, center, strat, b, pts) in cases.items():
            out['cases'][name] = configure(dev, span, center, strat, b, pts)

        out['timing'] = []
        if not args.no_timing:
            # Measure one full sweep for each span and strategy. This is the cost that the
            # acquisition loop pays, and it must stay below the watchdog of the worker.
            for span, center in ((10e6, 1e9), (100e6, 1e9), (1e9, 1e9),
                                 (FULL_SPAN_HZ, FULL_CENTER_HZ)):
                for strat in (speed, binsize):
                    row = timing_row(dev, span, center, strat)
                    out['timing'].append(row)
                    print('TIMING ' + json.dumps(row), flush=True)
    finally:
        T.dll.Device_Close(pointer(dev))

    if args.json:
        print(json.dumps(out, indent=2))
    else:
        for name, row in out['cases'].items():
            print(f"== {name}: {row}")


if __name__ == '__main__':
    main()
