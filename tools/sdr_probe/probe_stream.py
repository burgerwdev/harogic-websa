#!/usr/bin/env python3
"""Measure sustained IQS streaming throughput (Adaptive vs FixedPoints) and the
per-frame DSP_DDC cost. Determines what continuous SDR bandwidth the SAN-90 can
deliver over USB."""
from __future__ import annotations

import os
import sys
import time
from ctypes import POINTER, byref, c_int16, c_void_p, cast

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tinysa import TinySA  # noqa: E402

import htra_api as T  # noqa: E402

CENTER, TONE = 100e6, 100.2e6


def open_device():
    dev = c_void_p(); bp = T.BootProfile_TypeDef(); bi = T.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    T.dll.Device_Open(byref(dev), T.c_int(0), byref(bp), byref(bi))
    dsp = c_void_p(); T.dll.DSP_Open(byref(dsp))
    return dev, dsp


def config(dev, center, decimate, mode, trigger_len=65536):
    p = T.IQS_Profile_TypeDef(); T.dll.IQS_ProfileDeInit(byref(dev), byref(p))
    p.CenterFreq_Hz = center; p.RefLevel_dBm = 0.0; p.DecimateFactor = int(decimate)
    p.DataFormat = T.DataFormat_TypeDef.Complex16bit
    p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
    p.TriggerMode = mode
    p.TriggerLength = int(trigger_len)
    p.BusTimeout_ms = 2000
    p.DCCancelerMode = T.DCCancelerMode_TypeDef.DCCOff
    p.QDCMode = T.QDCMode_TypeDef.QDCOff
    out = T.IQS_Profile_TypeDef(); info = T.IQS_StreamInfo_TypeDef()
    st = T.dll.IQS_Configuration(byref(dev), byref(p), byref(out), byref(info))
    return st, out, info


def measure_stream(dev, decimate, mode, seconds=3.0, trigger_len=65536):
    st, out, info = config(dev, CENTER, decimate, mode, trigger_len)
    if st != 0:
        return dict(decimate=decimate, mode=str(mode), error=f'config st={st}')
    stream = T.IQStream_TypeDef()
    pkt = int(info.PacketSamples)
    samples = 0
    packets = 0
    t0 = time.monotonic()
    tstart = T.dll.IQS_BusTriggerStart(byref(dev))
    last = t0
    gaps = 0
    try:
        while time.monotonic() - t0 < seconds:
            a = time.monotonic()
            r = T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream))
            b = time.monotonic()
            if r != 0:
                break
            if b - a > 0.05:
                gaps += 1
            packets += 1
            samples += pkt
            last = b
        dur = last - t0
    finally:
        T.dll.IQS_BusTriggerStop(byref(dev))
    eff = samples / dur if dur > 0 else 0.0
    return dict(decimate=decimate, out_decimate=int(out.DecimateFactor),
                mode=str(mode), config_st=st, trigger_st=tstart,
                iq_rate=float(info.IQSampleRate), packet_samples=pkt,
                packets=packets, samples=samples, seconds=round(dur, 3),
                eff_sample_rate=eff, mbps=samples * 4 / dur / 1e6 if dur else 0,
                long_gaps=gaps,
                stream_ok=abs(eff - float(info.IQSampleRate)) / max(1.0, float(info.IQSampleRate)) < 0.05)


def measure_ddc(dsp, n, fs, dec):
    din = T.DSP_DDC_TypeDef(); dout = T.DSP_DDC_TypeDef()
    T.dll.DSP_DDC_DeInit(byref(din))
    din.DDCOffsetFrequency = -200e3; din.SampleRate = fs
    din.DecimateFactor = float(dec); din.SamplePoints = n
    T.dll.DSP_DDC_Configuration(byref(dsp), byref(din), byref(dout))
    T.dll.DSP_DDC_Reset(byref(dsp))
    buf = (c_int16 * (n * 2))()
    ins = T.IQStream_TypeDef()
    ins.AlternIQStream = cast(buf, POINTER(c_void_p))
    ins.IQS_StreamInfo.PacketSamples = n; ins.IQS_StreamInfo.IQSampleRate = fs
    reps = 20
    t0 = time.monotonic()
    for _ in range(reps):
        outs = T.IQStream_TypeDef()
        T.dll.DSP_DDC_Execute(byref(dsp), byref(ins), byref(outs))
    dt = (time.monotonic() - t0) / reps
    return dt, int(dout.SamplePoints), float(dout.SampleRate)


def main():
    sa = TinySA('/dev/ttyACM0')
    sa.enter_low_output(); sa.cw(TONE, -25); time.sleep(0.3)
    dev, dsp = open_device()
    try:
        print('== Adaptive mode (continuous) ==')
        for dec in (2, 4, 8, 16, 64, 256):
            r = measure_stream(dev, dec, T.TriggerMode_TypeDef.Adaptive, seconds=3.0)
            print(f'dec={dec:5d} rate={r.get("iq_rate",0)/1e6:9.3f} MSPS '
                  f'eff={r.get("eff_sample_rate",0)/1e6:9.3f} MSPS '
                  f'{r.get("mbps",0):8.2f} MB/s pkts={r.get("packets",0)} '
                  f'gaps={r.get("long_gaps",0)} ok={r.get("stream_ok")} {r.get("error","")}')
        print('\n== FixedPoints mode (trigger per frame) ==')
        for dec in (2, 4, 8, 16, 64):
            r = measure_stream(dev, dec, T.TriggerMode_TypeDef.FixedPoints, seconds=3.0,
                               trigger_len=65536)
            print(f'dec={dec:5d} rate={r.get("iq_rate",0)/1e6:9.3f} MSPS '
                  f'eff={r.get("eff_sample_rate",0)/1e6:9.3f} MSPS '
                  f'{r.get("mbps",0):8.2f} MB/s pkts={r.get("packets",0)} '
                  f'gaps={r.get("long_gaps",0)} ok={r.get("stream_ok")} {r.get("error","")}')

        print('\n== per-call DSP_DDC cost (131072 in) ==')
        for dec in (4, 16, 100, 1000):
            dt, opts, rate = measure_ddc(dsp, 131072, 3906250.0, dec)
            signal_s = 131072 / 3906250.0
            print(f'dec={dec:5d} -> {dt*1e3:7.3f} ms/call out_pts={opts} out_rate={rate:.1f} '
                  f'realtime_ratio={dt/signal_s*100:.2f}%')
    finally:
        T.dll.Device_Close(byref(dev))
        sa.close()


if __name__ == '__main__':
    main()
