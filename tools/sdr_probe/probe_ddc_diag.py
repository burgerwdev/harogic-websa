#!/usr/bin/env python3
"""Focused numeric diagnosis of DSP_DDC output layout and DSP_FFT axis."""
from __future__ import annotations

import os
import sys
import time
from ctypes import POINTER, addressof, byref, c_int16, c_void_p, cast, memmove, string_at

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tinysa import TinySA  # noqa: E402

import htra_api as T  # noqa: E402


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
    p.DCCancelerMode = T.DCCancelerMode_TypeDef.DCCOff
    p.QDCMode = T.QDCMode_TypeDef.QDCOff
    out = T.IQS_Profile_TypeDef(); info = T.IQS_StreamInfo_TypeDef()
    assert T.dll.IQS_Configuration(byref(dev), byref(p), byref(out), byref(info)) == 0
    total = int(info.StreamSamples); pkt = int(info.PacketSamples)
    buf = (c_int16 * (total * 2))()
    stream = T.IQStream_TypeDef()
    assert T.dll.IQS_BusTriggerStart(byref(dev)) == 0
    for i in range(int(info.PacketCount)):
        assert T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream)) == 0
        n = total % pkt if (i == int(info.PacketCount) - 1 and total % pkt) else pkt
        src = cast(stream.AlternIQStream, c_void_p).value
        memmove(addressof(buf) + i * pkt * 4, src, n * 4)
    T.dll.IQS_BusTriggerStop(byref(dev))
    return buf, info, stream


def vendor_fft(dsp, stream, n):
    fi = T.DSP_FFT_TypeDef(); fo = T.DSP_FFT_TypeDef()
    tp = T.c_uint32(0); rr = T.c_double(0.0)
    T.dll.DSP_FFT_DeInit(byref(fi))
    fi.Calibration = 0; fi.DetectionRatio = 1
    fi.TraceDetector = T.TraceDetector_TypeDef.TraceDetector_PosPeak
    fi.FFTSize = n; fi.SamplePts = n; fi.Intercept = 1.0
    fi.WindowType = T.Window_TypeDef.FlatTop
    st = T.dll.DSP_FFT_Configuration(byref(dsp), byref(fi), byref(fo), byref(tp), byref(rr))
    pts = int(tp.value)
    freq = (T.c_double * pts)(); power = (T.c_float * pts)()
    st2 = T.dll.DSP_FFT_IQSToSpectrum(byref(dsp), byref(stream), freq, power)
    fa = np.ctypeslib.as_array(freq).copy(); pa = np.ctypeslib.as_array(power).copy()
    # top peaks
    idx = np.argsort(pa)[-6:][::-1]
    peaks = [(int(i), float(pa[i]), float(fa[i])) for i in idx]
    return dict(st=(st, st2), pts=pts, rbw=float(rr.value), freq0=fa[:4].tolist(),
                freqN=fa[-4:].tolist(), peaks=peaks, power=pa)


def run_ddc(dsp, buf, n, fs, offset, dec):
    din = T.DSP_DDC_TypeDef(); dout = T.DSP_DDC_TypeDef()
    T.dll.DSP_DDC_DeInit(byref(din))
    din.DDCOffsetFrequency = float(offset); din.SampleRate = fs
    din.DecimateFactor = float(dec); din.SamplePoints = n
    st = T.dll.DSP_DDC_Configuration(byref(dsp), byref(din), byref(dout))
    T.dll.DSP_DDC_Reset(byref(dsp))
    ins = T.IQStream_TypeDef()
    ins.AlternIQStream = cast(buf, POINTER(c_void_p))
    ins.IQS_StreamInfo.PacketSamples = n
    ins.IQS_StreamInfo.IQSampleRate = fs
    outs = T.IQStream_TypeDef()
    st2 = T.dll.DSP_DDC_Execute(byref(dsp), byref(ins), byref(outs))
    out_pts = int(dout.SamplePoints); out_rate = float(dout.SampleRate)
    addr = cast(outs.AlternIQStream, c_void_p).value
    raw = string_at(addr, 32) if addr else b''
    as_f = np.frombuffer(raw, dtype=np.float32)[:8]
    as_i = np.frombuffer(raw, dtype=np.int16)[:16]
    return dict(st=(st, st2), out_pts=out_pts, out_rate=out_rate, addr=addr,
                as_f=as_f.tolist(), as_i=as_i.tolist(), outs=outs, dout=dout)


def main():
    center, tone = 100e6, 100.2e6
    sa = TinySA('/dev/ttyACM0')
    sa.enter_low_output(); sa.cw(tone, -25)
    time.sleep(0.3)
    dev, dsp = open_device()
    try:
        buf, info, stream = capture(dev, center, 16, 131072)
        n = int(info.StreamSamples); fs = float(info.IQSampleRate)
        print(f'raw n={n} fs={fs}')

        # numpy raw
        a = np.ctypeslib.as_array(buf).copy()
        x = a[0::2].astype(np.float64) + 1j * a[1::2].astype(np.float64)
        sp = np.fft.fftshift(np.fft.fft(x * np.hanning(n)))
        pf = np.fft.fftshift(np.fft.fftfreq(n, 1 / fs))
        k = int(np.argmax(np.abs(sp)))
        print(f'numpy raw peak: {pf[k]:+.1f} Hz, |X|={abs(sp[k]):.1f}')

        # vendor FFT on raw
        r = vendor_fft(dsp, stream, n)
        print('vendor raw FFT: st=', r['st'], 'pts=', r['pts'], 'rbw=', r['rbw'])
        print('  freq[0:4]=', r['freq0'], ' freq[-4:]=', r['freqN'])
        print('  top peaks (bin, dB, freq):', r['peaks'])

        for label, off, dec in (('off0_dec1', 0.0, 1), ('off200k_dec100', tone - center, 100)):
            d = run_ddc(dsp, buf, n, fs, off, dec)
            print(f'DDC {label}: st={d["st"]} pts={d["out_pts"]} rate={d["out_rate"]} addr=0x{d["addr"]:x}')
            print('  first bytes as float32:', [round(v, 5) for v in d['as_f']])
            print('  first bytes as int16  :', d['as_i'])
            v = vendor_fft(dsp, d['outs'], d['out_pts'])
            print('  vendor FFT on DDC out: st=', v['st'], 'pts=', v['pts'])
            print('  freq[0:4]=', v['freq0'], ' freq[-4:]=', v['freqN'])
            print('  top peaks (bin, dB, freq):', v['peaks'])
            # also numpy interpretation
            try:
                arr = np.ctypeslib.as_array(
                    cast(d['outs'].AlternIQStream, POINTER(T.c_float * (d['out_pts'] * 2))).contents).copy()
                w = arr[0::2] + 1j * arr[1::2]
                s2 = np.fft.fftshift(np.fft.fft(w * np.hanning(len(w))))
                f2 = np.fft.fftshift(np.fft.fftfreq(len(w), 1 / d['out_rate']))
                k2 = int(np.argmax(np.abs(s2)))
                print(f'  numpy(float) peak {f2[k2]:+.1f} Hz |X|={abs(s2[k2]):.1f} '
                      f'rms={np.sqrt(np.mean(np.abs(w)**2)):.3e}')
            except Exception as e:
                print('  numpy read err', repr(e))
    finally:
        T.dll.Device_Close(byref(dev))
        sa.close()


if __name__ == '__main__':
    main()
