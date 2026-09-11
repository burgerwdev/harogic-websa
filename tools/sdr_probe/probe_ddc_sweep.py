#!/usr/bin/env python3
"""Map DSP_DDC offset/passband behaviour on a captured SAN-90 IQ frame.

Tone is fixed at +200 kHz from the SAN-90 center. DDC offset is swept; the output
peak position/level tells us the mixing sign, the usable passband and whether the
output is complex-float. Also verifies the vendor FFT on the raw int16 buffer.
"""
from __future__ import annotations

import os
import sys
import time
from ctypes import POINTER, addressof, byref, c_int16, c_void_p, cast, memmove

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tinysa import TinySA  # noqa: E402

import htra_api as T  # noqa: E402

CENTER, TONE = 100e6, 100.2e6
IN_DECIMATE = 16


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
        memmove(addressof(buf) + i * pkt * 4, cast(stream.AlternIQStream, c_void_p).value, n * 4)
    T.dll.IQS_BusTriggerStop(byref(dev))
    return buf, info


def read_float(outs, pts):
    arr = np.ctypeslib.as_array(
        cast(outs.AlternIQStream, POINTER(T.c_float * (pts * 2))).contents).copy()
    return arr[0::2].astype(np.float64), arr[1::2].astype(np.float64)


def ddc(dsp, buf, n, fs, offset, dec):
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
    return st, st2, int(dout.SamplePoints), float(dout.SampleRate), outs


def peak_of(i, q, fs):
    if len(i) < 8:
        return 0.0, -999.0, 0.0
    w = i + 1j * q
    w = w - w.mean()
    s = np.fft.fftshift(np.fft.fft(w * np.hanning(len(w))))
    f = np.fft.fftshift(np.fft.fftfreq(len(w), 1 / fs))
    k = int(np.argmax(np.abs(s)))
    return float(f[k]), float(20 * np.log10(abs(s[k]) / (len(w) / 2) + 1e-20)), float(np.sqrt(np.mean(np.abs(w) ** 2)))


def main():
    sa = TinySA('/dev/ttyACM0')
    sa.enter_low_output(); sa.cw(TONE, -25)
    time.sleep(0.3)
    dev, dsp = open_device()
    try:
        buf, info = capture(dev, CENTER, IN_DECIMATE, 131072)
        n = int(info.StreamSamples); fs = float(info.IQSampleRate)
        print(f'raw n={n} fs={fs} tone=+200 kHz')

        # 1) confirm DDC decimate=1 offset=0 reproduces raw exactly
        st, st2, pts, rate, outs = ddc(dsp, buf, n, fs, 0.0, 1)
        i, q = read_float(outs, pts)
        f0, l0, rms0 = peak_of(i, q, rate)
        print(f'[check] dec1 off0 -> pts={pts} rate={rate} peak={f0:+.1f} Hz lvl={l0:.1f} rms={rms0:.1f}')

        # 2) offset sweep at a fixed decimation; output Nyquist = rate/2 = 19.5 kHz
        dec = 100
        print(f'\n-- offset sweep, dec={dec} (output rate={fs/dec:.1f}, passband +-{fs/dec/2:.1f} Hz) --')
        print(f'{"offset_hz":>10} {"out_rate":>10} {"peak_hz":>12} {"level_db":>9} {"rms":>10}')
        for off in (-400e3, -300e3, -250e3, -220e3, -210e3, -205e3, -200e3, -195e3, -190e3,
                    -150e3, -100e3, 0.0, 100e3, 190e3, 195e3, 200e3, 205e3, 210e3, 220e3,
                    250e3, 300e3, 400e3):
            st, st2, pts, rate, outs = ddc(dsp, buf, n, fs, off, dec)
            i, q = read_float(outs, pts)
            pf, pl, pr = peak_of(i, q, rate)
            print(f'{off:>10.0f} {rate:>10.0f} {pf:>12.1f} {pl:>9.2f} {pr:>10.4f}')

        # 3) decimation sweep at offset = tone-center and at offset = -(tone-center)
        print('\n-- decimation sweep, offset = +200k --')
        print(f'{"dec":>7} {"out_rate":>10} {"peak_hz":>12} {"level_db":>9} {"rms":>10}')
        for dec in (2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000):
            st, st2, pts, rate, outs = ddc(dsp, buf, n, fs, 200e3, dec)
            i, q = read_float(outs, pts)
            pf, pl, pr = peak_of(i, q, rate)
            print(f'{dec:>7} {rate:>10.1f} {pf:>12.1f} {pl:>9.2f} {pr:>10.4f}')

        print('\n-- decimation sweep, offset = -200k --')
        for dec in (2, 100, 1000):
            st, st2, pts, rate, outs = ddc(dsp, buf, n, fs, -200e3, dec)
            i, q = read_float(outs, pts)
            pf, pl, pr = peak_of(i, q, rate)
            print(f'{dec:>7} {rate:>10.1f} {pf:>12.1f} {pl:>9.2f} {pr:>10.4f}')

        # 4) vendor FFT on the raw concatenated int16 buffer (correct stream setup)
        print('\n-- vendor DSP_FFT on the raw concatenated int16 buffer --')
        fi = T.DSP_FFT_TypeDef(); fo = T.DSP_FFT_TypeDef()
        tp = T.c_uint32(0); rr = T.c_double(0.0)
        T.dll.DSP_FFT_DeInit(byref(fi))
        fi.Calibration = 0; fi.DetectionRatio = 1
        fi.TraceDetector = T.TraceDetector_TypeDef.TraceDetector_PosPeak
        fi.FFTSize = n; fi.SamplePts = n; fi.Intercept = 1.0
        fi.WindowType = T.Window_TypeDef.FlatTop
        T.dll.DSP_FFT_Configuration(byref(dsp), byref(fi), byref(fo), byref(tp), byref(rr))
        pv = int(tp.value)
        freq = (T.c_double * pv)(); power = (T.c_float * pv)()
        raw_stream = T.IQStream_TypeDef()
        raw_stream.AlternIQStream = cast(buf, POINTER(c_void_p))
        raw_stream.IQS_StreamInfo.PacketSamples = n
        raw_stream.IQS_StreamInfo.IQSampleRate = fs
        st = T.dll.DSP_FFT_IQSToSpectrum(byref(dsp), byref(raw_stream), freq, power)
        pa = np.ctypeslib.as_array(power).copy(); fa = np.ctypeslib.as_array(freq).copy()
        k = int(np.argmax(pa))
        print(f'st={st} pts={pv} peak bin={k} freq={fa[k]:.1f} Hz level={pa[k]:.1f} dB')
        print(f'freq axis: {fa[0]:.1f} .. {fa[-1]:.1f}')
    finally:
        T.dll.Device_Close(byref(dev))
        sa.close()


if __name__ == '__main__':
    main()
