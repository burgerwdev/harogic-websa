#!/usr/bin/env python3
"""Verify the vendor ADM_ analog demod API (AM/FM) on real SAN-90 IQ, driven by
known tinySA AM/FM signals. Also tests the DDC -> ADM chain (complex-float DDC
output fed to ADM).

Structs are declared here because the repo's htra_api.py does not export
AMDemodParam_TypeDef / FMDemodParam_TypeDef and declares no ADM_* bindings.
"""
from __future__ import annotations

import os
import sys
import time
from ctypes import (
    POINTER,
    Structure,
    addressof,
    byref,
    c_bool,
    c_double,
    c_float,
    c_int,
    c_int16,
    c_uint32,
    c_uint64,
    c_void_p,
    cast,
    memmove,
)

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tinysa import TinySA  # noqa: E402

import htra_api as T  # noqa: E402


class AMDemodParam(Structure):
    _fields_ = [
        ('DemodWaveform', POINTER(c_float)), ('AFSpectrum_ModDepth', POINTER(c_float)),
        ('AFSpectrum_Freq', POINTER(c_double)), ('DemodWaveformSize', c_uint32),
        ('ModDepth', c_float), ('ModDepthPeakPos', c_float), ('ModDepthPeakNeg', c_float),
        ('ModDepthHalfPeak', c_float), ('ModDepthRMS', c_float), ('CarrierPower', c_float),
        ('ModRate', c_double), ('SINAD', c_float), ('RMSPower', c_float),
        ('FreqError', c_double), ('SNR', c_float), ('DistTotalVrms', c_float),
        ('THD', c_float), ('PEP', c_float),
    ]


class FMDemodParam(Structure):
    _fields_ = [
        ('DemodWaveform', POINTER(c_float)), ('AFSpectrum_Deviation', POINTER(c_float)),
        ('AFSpectrum_Freq', POINTER(c_double)), ('DemodWaveformSize', c_uint32),
        ('Deviation', c_float), ('DeviationPeakPos', c_float), ('DeviationPeakNeg', c_float),
        ('DeviationHalfPeak', c_float), ('DeviationRMS', c_float), ('CarrierPower', c_float),
        ('CarrierFreqErr', c_double), ('ModRate', c_double), ('SINAD', c_float),
        ('SNR', c_float), ('DistTotalVrms', c_float), ('THD', c_float),
    ]


def bind_adm():
    T.dll.ADM_Open.argtypes = [POINTER(c_void_p)]; T.dll.ADM_Open.restype = None
    T.dll.ADM_Close.argtypes = [POINTER(c_void_p)]; T.dll.ADM_Close.restype = None
    T.dll.ADM_AMDemod.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64, c_double,
                                  POINTER(c_float)]
    T.dll.ADM_AMDemod.restype = c_int
    T.dll.ADM_AMDemod_PM1.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64, c_double,
                                      c_float, POINTER(AMDemodParam)]
    T.dll.ADM_AMDemod_PM1.restype = c_int
    T.dll.ADM_FMDemod.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64, c_double,
                                  c_bool, POINTER(c_float)]
    T.dll.ADM_FMDemod.restype = c_int
    T.dll.ADM_FMDemod_PM1.argtypes = [POINTER(c_void_p), c_void_p, c_int, c_uint64, c_double,
                                      c_float, c_bool, POINTER(FMDemodParam)]
    T.dll.ADM_FMDemod_PM1.restype = c_int


def open_device():
    dev = c_void_p(); bp = T.BootProfile_TypeDef(); bi = T.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    T.dll.Device_Open(byref(dev), c_int(0), byref(bp), byref(bi))
    dsp = c_void_p(); T.dll.DSP_Open(byref(dsp))
    adm = c_void_p(); T.dll.ADM_Open(byref(adm))
    return dev, dsp, adm


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
    return buf, info, float(stream.IQS_ScaleToV)


def tone_of(wave, rate):
    if wave is None or len(wave) < 32:
        return 0.0
    w = wave - np.mean(wave)
    s = np.abs(np.fft.rfft(w * np.hanning(len(w))))
    f = np.fft.rfftfreq(len(w), 1 / rate)
    return float(f[int(np.argmax(s))])


def main():
    bind_adm()
    center = 100e6
    sa = TinySA('/dev/ttyACM0')
    sa.enter_low_output()
    dev, dsp, adm = open_device()
    try:
        # ---------- AM ----------
        sa.am(center, -20, mod_hz=1000, depth=50)
        time.sleep(0.4)
        buf, info, scale = capture(dev, center, 32, 131072)
        n = int(info.StreamSamples); fs = float(info.IQSampleRate)
        print(f'AM capture: n={n} fs={fs} scaleToV={scale}')
        wf = (c_float * n)()
        p = AMDemodParam()
        st = T.dll.ADM_AMDemod_PM1(byref(adm), cast(buf, c_void_p),
                                   int(T.DataFormat_TypeDef.Complex16bit), n, fs, scale, byref(p))
        wfp = np.ctypeslib.as_array(p.DemodWaveform, shape=(n,)) if p.DemodWaveform else None
        print(f'ADM_AMDemod_PM1 st={st} size={p.DemodWaveformSize} ModRate={p.ModRate:.2f} Hz '
              f'(expect ~1000) ModDepth={p.ModDepth:.2f}% (expect ~50) Carrier={p.CarrierPower:.2f} dBm '
              f'SINAD={p.SINAD:.2f} SNR={p.SNR:.2f} THD={p.THD:.3f}')
        if wfp is not None:
            print(f'  waveform tone = {tone_of(wfp, fs):.1f} Hz (expect ~1000)')
        st2 = T.dll.ADM_AMDemod(byref(adm), cast(buf, c_void_p),
                                int(T.DataFormat_TypeDef.Complex16bit), n, fs, wf)
        w2 = np.ctypeslib.as_array(wf)
        print(f'ADM_AMDemod st={st2} waveform tone={tone_of(w2, fs):.1f} Hz')

        # ---------- FM ----------
        sa.fm(center, -20, mod_hz=1000, dev_hz=25000)
        time.sleep(0.4)
        buf, info, scale = capture(dev, center, 32, 131072)
        n = int(info.StreamSamples); fs = float(info.IQSampleRate)
        print(f'\nFM capture: n={n} fs={fs}')
        fp = FMDemodParam()
        st = T.dll.ADM_FMDemod_PM1(byref(adm), cast(buf, c_void_p),
                                   int(T.DataFormat_TypeDef.Complex16bit), n, fs, scale, True, byref(fp))
        wfp = np.ctypeslib.as_array(fp.DemodWaveform, shape=(n,)) if fp.DemodWaveform else None
        print(f'ADM_FMDemod_PM1 st={st} size={fp.DemodWaveformSize} ModRate={fp.ModRate:.2f} Hz '
              f'(expect ~1000) Deviation={fp.Deviation:.1f} Hz (expect ~25000) '
              f'CarrierErr={fp.CarrierFreqErr:.1f} SINAD={fp.SINAD:.2f} SNR={fp.SNR:.2f} THD={fp.THD:.3f}')
        if wfp is not None:
            print(f'  waveform tone = {tone_of(wfp, fs):.1f} Hz (expect ~1000)')

        # ---------- DDC -> ADM (complex-float output) ----------
        print('\n-- DDC float output -> ADM (Complexfloat) --')
        din = T.DSP_DDC_TypeDef(); dout = T.DSP_DDC_TypeDef()
        T.dll.DSP_DDC_DeInit(byref(din))
        din.DDCOffsetFrequency = 0.0; din.SampleRate = fs
        din.DecimateFactor = 50.0; din.SamplePoints = n
        T.dll.DSP_DDC_Configuration(byref(dsp), byref(din), byref(dout))
        T.dll.DSP_DDC_Reset(byref(dsp))
        ins = T.IQStream_TypeDef()
        ins.AlternIQStream = cast(buf, POINTER(c_void_p))
        ins.IQS_StreamInfo.PacketSamples = n; ins.IQS_StreamInfo.IQSampleRate = fs
        outs = T.IQStream_TypeDef()
        T.dll.DSP_DDC_Execute(byref(dsp), byref(ins), byref(outs))
        opts = int(dout.SamplePoints); orate = float(dout.SampleRate)
        fp2 = FMDemodParam()
        st = T.dll.ADM_FMDemod_PM1(byref(adm), cast(outs.AlternIQStream, c_void_p),
                                   int(T.DataFormat_TypeDef.Complexfloat), opts, orate, scale, True, byref(fp2))
        print(f'FMDemod on DDC float: st={st} out_pts={opts} rate={orate} ModRate={fp2.ModRate:.2f} '
              f'Deviation={fp2.Deviation:.1f}')
        # convert DDC float -> int16 and retry
        arr = np.ctypeslib.as_array(
            cast(outs.AlternIQStream, POINTER(c_float * (opts * 2))).contents).copy()
        mx = max(1e-9, float(np.max(np.abs(arr))))
        i16 = (arr / mx * 30000).astype(np.int16)
        cbuf = (c_int16 * (opts * 2))(*i16.tolist())
        fp3 = FMDemodParam()
        st = T.dll.ADM_FMDemod_PM1(byref(adm), cast(cbuf, c_void_p),
                                   int(T.DataFormat_TypeDef.Complex16bit), opts, orate, scale, True, byref(fp3))
        print(f'FMDemod on DDC->int16: st={st} ModRate={fp3.ModRate:.2f} Deviation={fp3.Deviation:.1f}')
    finally:
        T.dll.ADM_Close(byref(adm))
        T.dll.Device_Close(byref(dev))
        sa.close()


if __name__ == '__main__':
    main()
