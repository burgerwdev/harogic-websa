#!/usr/bin/env python3
"""Read-only IQS capability probe for the SAN-90.

Run with the WebSA service stopped. Opens the device, reads the hardware state,
then enumerates IQS DecimateFactor / DataFormat combinations and records the
actual StreamInfo (Bandwidth, IQSampleRate, packet sizes). Nothing is calibrated
or written persistently; closing the device restores nothing beyond RAM state.

Usage:  python3 tools/sdr_probe/probe_iqs_caps.py [--out caps.json]
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

import htra_api as T  # noqa: E402


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


def read_hw_state(dev):
    out = {}
    try:
        hs = T.HardWareState_TypeDef() if hasattr(T, 'HardWareState_TypeDef') else None
    except Exception:
        hs = None
    # htra_api.py may not declare HardWareState_TypeDef; call with raw struct if missing
    if hs is None:
        try:
            import ctypes as C

            class HardWareState(C.Structure):
                _fields_ = [
                    ('GNSSPeriphType', C.c_int), ('GNSSType', C.c_int), ('OCXOType', C.c_int),
                    ('InternalOCXO', C.c_uint8), ('SignalSourceEn', C.c_uint8),
                    ('ADC_VariableRateEn', C.c_uint8), ('IM3_filter', C.c_uint8),
                ]
            hs = HardWareState()
            T.dll.Device_GetHardwareState.argtypes = [T.POINTER(T.c_void_p), T.POINTER(HardWareState)]
            T.dll.Device_GetHardwareState.restype = T.c_int
        except Exception as e:
            return {'error': repr(e)}
    st = T.dll.Device_GetHardwareState(pointer(dev), pointer(hs))
    out['status'] = st
    for f, _ in getattr(hs, '_fields_', []):
        try:
            v = getattr(hs, f)
            out[f] = int(v.value) if hasattr(v, 'value') else int(v)
        except Exception:
            pass
    return out


def make_iqs_profile(decimate, fmt, trigger_length):
    p = T.IQS_Profile_TypeDef()
    T.dll.IQS_ProfileDeInit(pointer(dev_global), pointer(p))
    defaults = {}
    for f, _ in p._fields_:
        try:
            v = getattr(p, f)
            defaults[f] = int(v.value) if hasattr(v, 'value') else (float(v) if isinstance(v, float) else v)
        except Exception:
            pass
    p.CenterFreq_Hz = 100e6
    p.RefLevel_dBm = 0.0
    p.DecimateFactor = int(decimate)
    p.DataFormat = fmt
    p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
    p.TriggerMode = T.TriggerMode_TypeDef.FixedPoints
    p.TriggerLength = int(trigger_length)
    p.BusTimeout_ms = 5000
    return p, defaults


dev_global = None


def probe_one(decimate, fmt, trigger_length):
    p, defaults = make_iqs_profile(decimate, fmt, trigger_length)
    out = T.IQS_Profile_TypeDef()
    info = T.IQS_StreamInfo_TypeDef()
    t0 = time.monotonic()
    st = T.dll.IQS_Configuration(pointer(dev_global), pointer(p), pointer(out), pointer(info))
    dt = time.monotonic() - t0
    rec = {
        'req_decimate': int(decimate),
        'req_format': int(fmt.value if hasattr(fmt, 'value') else fmt),
        'status': int(st),
        'config_s': round(dt, 4),
    }
    if st == 0:
        rec.update(
            out_decimate=int(out.DecimateFactor),
            out_center=float(out.CenterFreq_Hz),
            out_ref=float(out.RefLevel_dBm),
            out_format=int(getattr(out.DataFormat, 'value', out.DataFormat)),
            bandwidth=float(info.Bandwidth),
            iq_sample_rate=float(info.IQSampleRate),
            packet_count=int(info.PacketCount),
            stream_samples=int(info.StreamSamples),
            stream_data_size=int(info.StreamDataSize),
            packet_samples=int(info.PacketSamples),
            packet_data_size=int(info.PacketDataSize),
            gain_parameter=int(info.GainParameter),
        )
    return rec, defaults


def main():
    global dev_global
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', default=os.path.join(os.path.dirname(__file__), 'caps.json'))
    ap.add_argument('--trigger-length', type=int, default=65536)
    args = ap.parse_args()

    dev, bi = open_device()
    dev_global = dev
    di = bi.DeviceInfo
    report = {
        'device': dict(uid='%012X' % di.DeviceUID, model=int(di.Model),
                       hw=int(di.HardwareVersion), mfw=int(di.MFWVersion), ffw=int(di.FFWVersion)),
        'boot': dict(bus_speed=int(bi.BusSpeed), bus_ver=int(bi.BusVersion),
                     api_ver=int(bi.APIVersion), errors=int(bi.Errors), warnings=int(bi.Warnings)),
        'hw_state': read_hw_state(dev),
        'iqs_defaults': None,
        'sleep_off': None,
        'decimate_scan_c16': [],
        'format_scan': [],
    }
    try:
        # default profile
        _, defaults = make_iqs_profile(1, T.DataFormat_TypeDef.Complex16bit, args.trigger_length)
        report['iqs_defaults'] = defaults

        # decimate scan (Complex16bit)
        for d in (1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192):
            try:
                rec, _ = probe_one(d, T.DataFormat_TypeDef.Complex16bit, args.trigger_length)
            except Exception as e:
                rec = {'req_decimate': d, 'error': repr(e)}
            report['decimate_scan_c16'].append(rec)
            print('decimate', d, '->', {k: rec.get(k) for k in
                  ('status', 'out_decimate', 'iq_sample_rate', 'bandwidth',
                   'packet_count', 'packet_samples', 'packet_data_size')})

        # format scan at a moderate decimate
        for name, fmt in (('Complex8bit', T.DataFormat_TypeDef.Complex8bit),
                          ('Complex16bit', T.DataFormat_TypeDef.Complex16bit),
                          ('Complex32bit', T.DataFormat_TypeDef.Complex32bit)):
            try:
                rec, _ = probe_one(256, fmt, args.trigger_length)
                rec['format_name'] = name
            except Exception as e:
                rec = {'format_name': name, 'error': repr(e)}
            report['format_scan'].append(rec)
            print('format', name, '->', {k: rec.get(k) for k in
                  ('status', 'out_decimate', 'iq_sample_rate', 'bandwidth',
                   'packet_samples', 'packet_data_size')})

        # IQS_EZProfile defaults (simplified API)
        try:
            ez = T.IQS_EZProfile_TypeDef()
            T.dll.IQS_EZProfileDeInit(pointer(dev), pointer(ez))
            report['ez_defaults'] = {f: (int(getattr(ez, f).value) if hasattr(getattr(ez, f), 'value')
                                         else float(getattr(ez, f)))
                                     for f, _ in ez._fields_}
        except Exception as e:
            report['ez_defaults'] = {'error': repr(e)}
    finally:
        T.dll.Device_Close(pointer(dev))

    with open(args.out, 'w') as fh:
        json.dump(report, fh, indent=2)
    print('\nWrote', args.out)


if __name__ == '__main__':
    main()
