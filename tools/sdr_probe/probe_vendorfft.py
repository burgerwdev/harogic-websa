#!/usr/bin/env python3
"""Verify vendor DSP_FFT_IQSToSpectrum with the official stream pattern, plus
DDC->FFT chaining. Answers: does the vendor FFT work on concatenated raw IQ and
on the complex-float DDC output?"""
from __future__ import annotations

import os
import sys
import time
from ctypes import POINTER, addressof, byref, cast, c_int16, c_void_p, memmove

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import htra_api as T  # noqa: E402
from tinysa import TinySA  # noqa: E402

CENTER, TONE = 100e6, 100.2e6


def open_device():
    dev = c_void_p(); bp = T.BootProfile_TypeDef(); bi = T.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    T.dll.Device_Open(byref(dev), T.c_int(0), byref(bp), byref(bi))
    dsp = c_void_p(); T.dll.DSP_Open(byref(dsp))
    return dev, dsp


def capture(dev, center, decimate, trigger_len):
    p = T.IQS_Profile_TypeDef(); T.dll.IQS_ProfileDeInit(byref(dev), byref(p))
    p.CenterFreq_Hz = center; p.RefLevel_dBm = 0.0; p.DecimateFactor = int(decimate)
    p.DataFormat = T.DataFormat_TypeDef.Complex16bit
    p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
    p.TriggerMode = T.TriggerMode_TypeDef.FixedPoints
    p.TriggerLength = int(trigger_len); p.BusTimeout_ms = 5000
    p.DCCancelerMode = T.DCCancelerMode_TypeDef.DCCOff; p.QDCMode = T.QDCMode_TypeDef.QDCOff
    out = T.IQS_Profile_TypeDef(); info = T.IQS_StreamInfo_TypeDef()
    assert T.dll.IQS_Configuration(byref(dev), byref(p), byref(out), byref(info)) == 0
    total = int(info.StreamSamples); pkt = int(info.PacketSamples)
    buf = (c_int16 * (total * 2))()
    stream = T.IQStream_TypeDef()
    assert T.dll.IQS_BusTriggerStart(byref(dev)) == 0
    for i in range(int(info.PacketCount)):
        assert T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream)) == 0
        n = total % pkt if (i == int(info.PacketCount) - 1 and total % pkt) else pkt
        memmove(addressof(buf) + i * pkt * 4, cast(stream.AlternIQStream, c_void_p).value, n * 4)
    T.dll.IQS_BusTriggerStop(byref(dev))
    return buf, info, stream


def fft(stream, n):
    fi = T.DSP_FFT_TypeDef(); fo = T.DSP_FFT_TypeDef()
    tp = T.c_uint32(0); rr = T.c_double(0.0)
    T.dll.DSP_FFT_DeInit(byref(fi))
    fi.Calibration = 0; fi.DetectionRatio = 1
    fi.TraceDetector = T.TraceDetector_TypeDef.TraceDetector_PosPeak
    fi.FFTSize = n; fi.SamplePts = n; fi.Intercept = 1.0
    fi.WindowType = T.Window_TypeDef.FlatTop
    st = T.dll.DSP_FFT_Configuration(byref(dsp_global), byref(fi), byref(fo), byref(tp), byref(rr))
    pts = int(tp.value)
    freq = (T.c_double * pts)(); power = (T.c_float * pts)()
    st2 = T.dll.DSP_FFT_IQSToSpectrum(byref(dsp_global), byref(stream), freq, power)
    fa = np.ctypeslib.as_array(freq).copy(); pa = np.ctypeslib.as_array(power).copy()
    return st, st2, pts, fa, pa


dsp_global = None


def report(tag, st, st2, pts, fa, pa):
    if not np.any(pa):
        print(f'{tag}: st={st},{st2} pts={pts} ALL-ZERO'); return
    k = int(np.argmax(pa))
    nz = np.count_nonzero(pa)
    print(f'{tag}: st={st},{st2} pts={pts} nonzero={nz} peak bin={k} freq={fa[k]:.1f} '
          f'level={pa[k]:.2f} axis={fa[0]:.1f}..{fa[-1]:.1f}')


def main():
    global dsp_global
    sa = TinySA('/dev/ttyACM0')
    sa.enter_low_output(); sa.cw(TONE, -25); time.sleep(0.3)
    dev, dsp = open_device(); dsp_global = dsp
    try:
        buf, info, stream = capture(dev, CENTER, 16, 131072)
        n = int(info.StreamSamples); fs = float(info.IQSampleRate)
        print(f'n={n} fs={fs}')

        # official pattern for raw concatenated IQ
        stream.AlternIQStream = cast(buf, POINTER(c_void_p))
        stream.IQS_StreamInfo.PacketSamples = n
        report('vendor FFT raw', *fft(stream, n))

        # DDC offset=-200k dec=100 then vendor FFT on out_stream
        din = T.DSP_DDC_TypeDef(); dout = T.DSP_DDC_TypeDef()
        T.dll.DSP_DDC_DeInit(byref(din))
        din.DDCOffsetFrequency = -200e3; din.SampleRate = fs
        din.DecimateFactor = 100.0; din.SamplePoints = n
        cst = T.dll.DSP_DDC_Configuration(byref(dsp), byref(din), byref(dout))
        T.dll.DSP_DDC_Reset(byref(dsp))
        ins = T.IQStream_TypeDef()
        ins.AlternIQStream = cast(buf, POINTER(c_void_p))
        ins.IQS_StreamInfo.PacketSamples = n; ins.IQS_StreamInfo.IQSampleRate = fs
        outs = T.IQStream_TypeDef()
        est = T.dll.DSP_DDC_Execute(byref(dsp), byref(ins), byref(outs))
        opts = int(dout.SamplePoints)
        print(f'DDC cfg={cst} exec={est} opts={opts} rate={dout.SampleRate} '
              f'out center={outs.IQS_Profile.CenterFreq_Hz} fmt='
              f'{int(getattr(outs.IQS_Profile.DataFormat, "value", -1))} '
              f'pktSamples={outs.IQS_StreamInfo.PacketSamples} '
              f'pktBytes={outs.IQS_StreamInfo.PacketDataSize}')
        report('vendor FFT on DDC out', *fft(outs, opts))
    finally:
        T.dll.Device_Close(byref(dev))
        sa.close()


if __name__ == '__main__':
    main()
