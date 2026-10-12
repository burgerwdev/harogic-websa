#!/usr/bin/env python3
"""Read-only SWP capability probe: decimate list, bus bandwidth, RBW/points limits,
frame-detector and trace-type acceptance. Run with the WebSA service stopped.

Usage:  python3 tools/sdr_probe/probe_caps_rbw.py [--json]
"""
from __future__ import annotations

import argparse
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)

from ctypes import pointer  # noqa: E402

import htra_api  # noqa: E402  (BootProfile enums + Device_Open live in the vendor wrapper)
from web_sa.hardware import sdk_bindings as sb  # noqa: E402  (patched SWP profile + caps bindings)

T = htra_api


def open_device():
    dev = T.c_void_p()
    bp = T.BootProfile_TypeDef()
    bi = T.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    st = T.dll.Device_Open(pointer(dev), T.c_int(0), pointer(bp), pointer(bi))
    if st not in (0, -49):
        raise RuntimeError(f'Device_Open status={st}')
    return dev, bi


def capability_rows(dev):
    out = {}
    lst = (T.c_uint32 * sb.DECIMATE_FACTOR_LIST_MAX)()
    cnt = T.c_uint32(0)
    st = T.dll.Device_GetDecimateFactorList(pointer(dev), lst, sb.DECIMATE_FACTOR_LIST_MAX,
                                            pointer(cnt))
    out['decimate_list_status'] = st
    out['decimate_factors'] = sorted(int(lst[i]) for i in range(cnt.value)) if st == 0 else []
    bw = T.c_uint64(0)
    st = T.dll.Device_GetBusBandwidth(pointer(dev), pointer(bw))
    out['bus_bandwidth_status'] = st
    out['bus_bandwidth'] = int(bw.value) if st == 0 else 0
    return out


def swp_try(dev, **overrides):
    """Configure SWP with overrides, return the echoed profile + trace info.

    structs come from sb: sdk_bindings rebinds SWP_ProfileDeInit/SWP_Configuration
    argtypes to its patched profile class, so htra_api's own class fails the check.
    """
    pin = sb.SWP_Profile_TypeDef()
    T.dll.SWP_ProfileDeInit(pointer(dev), pointer(pin))
    pin.StartFreq_Hz = 999.9e6
    pin.StopFreq_Hz = 1000.1e6
    for k, v in overrides.items():
        if k in ('RBW_Hz', 'VBW_Hz', 'SweepTime', 'TracePoints'):
            setattr(pin, k, v)
        else:
            setattr(pin, k, v.value if hasattr(v, 'value') else v)
    pout = sb.SWP_Profile_TypeDef()
    ti = sb.SWP_TraceInfo_TypeDef()
    st = T.dll.SWP_Configuration(pointer(dev), pointer(pin), pointer(pout), pointer(ti))
    row = {'status': st}
    if st == 0:
        row.update(rbw_actual=float(pout.RBW_Hz), vbw_actual=float(pout.VBW_Hz),
                   points_actual=int(ti.FullsweepTracePoints),
                   est_sweep_s=float(ti.EstimateMinSweepTime))
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--json', action='store_true')
    args = ap.parse_args()

    dev, bi = open_device()
    out = {'device': {'model': int(bi.DeviceInfo.Model), 'mfw': int(bi.DeviceInfo.MFWVersion)}}
    try:
        out['capability'] = capability_rows(dev)

        rbw = out['rbw_limits'] = {}
        for rbw_hz in (1.0, 3.0, 10.0, 100.0, 1e3):
            rbw[str(rbw_hz)] = swp_try(dev, RBWMode=T.RBWMode_TypeDef.RBW_Manual, RBW_Hz=rbw_hz)

        pts = out['points_limits'] = {}
        for n in (4001, 10001, 100001):
            pts[str(n)] = swp_try(dev, TracePoints=int(n))

        det = out['frame_detector'] = {}
        for name, d in (('sample', T.Detector_TypeDef.Detector_Sample),
                        ('average', T.Detector_TypeDef.Detector_Average),
                        ('max_power', T.Detector_TypeDef.Detector_MaxPower),
                        ('raw_frames', T.Detector_TypeDef.Detector_RawFrames)):
            det[name] = swp_try(dev, Detector=d)

        tt = out['trace_type'] = {}
        for name, t in (('clear', T.SWP_TraceType_TypeDef.ClearWrite),
                        ('max_hold', T.SWP_TraceType_TypeDef.MaxHold),
                        ('min_hold', T.SWP_TraceType_TypeDef.MinHold)):
            tt[name] = swp_try(dev, TraceType=t)

        fft = out['fft_strategy'] = {}
        for name, s in (('auto', T.FFTExecutionStrategy_TypeDef.Auto),
                        ('fpga_only', T.FFTExecutionStrategy_TypeDef.FPGAOnly),
                        ('cpu_high_4m', T.FFTExecutionStrategy_TypeDef.CPUOnly_HighResOcc)):
            fft[name] = swp_try(dev, FFTExecutionStrategy=s,
                                RBWMode=T.RBWMode_TypeDef.RBW_OneThousandthSpan)

        span_modes = out['rbw_span_modes'] = {
            'one_percent': swp_try(dev, RBWMode=T.RBWMode_TypeDef.RBW_OnePercentSpan),
            'one_thousandth': swp_try(dev, RBWMode=T.RBWMode_TypeDef.RBW_OneThousandthSpan),
        }
        del span_modes
    finally:
        T.dll.Device_Close(pointer(dev))

    if args.json:
        print(json.dumps(out, indent=2))
    else:
        for section, rows in out.items():
            print(f'== {section}')
            if isinstance(rows, dict) and all(isinstance(v, dict) for v in rows.values()):
                for k, v in rows.items():
                    print(f'  {k}: {v}')
            else:
                print(f'  {rows}')


if __name__ == '__main__':
    main()
