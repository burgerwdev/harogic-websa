#!/usr/bin/env python3
"""Verify the vendor ADM_ analog demod API (AM/FM) on real SAN-90 IQ, driven by
known tinySA AM/FM signals. Also tests the DDC -> ADM chain (complex-float DDC
output fed to ADM).

Structs are declared here because the repo's htra_api.py does not export
AMDemodParam_TypeDef / FMDemodParam_TypeDef and declares no ADM_* bindings.
"""
from __future__ import annotations

import math
import os
import sys
import time
from contextlib import suppress
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
from tools.reference import sdk_bindings as sb  # noqa: E402  (restored BootInfo)
from tools.reference.sdr_capture import MAX_PACKET_BYTES, validate_iq_packet  # noqa: E402

# This manual probe captures one bounded block, independent of the live service.
MAX_CAPTURE_BYTES = 64 * 1024 * 1024


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
    dev = c_void_p(); bp = T.BootProfile_TypeDef(); bi = sb.BootInfo_TypeDef()
    dsp = c_void_p(); adm = c_void_p()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    try:
        if T.dll.Device_Open(byref(dev), c_int(0), byref(bp), byref(bi)) != 0 or not dev:
            raise RuntimeError('Device_Open failed')
        if T.dll.DSP_Open(byref(dsp)) != 0 or not dsp:
            raise RuntimeError('DSP_Open failed')
        T.dll.ADM_Open(byref(adm))
        if not adm:
            raise RuntimeError('ADM_Open returned a null handle')
    except BaseException:
        for handle, close in [(adm, T.dll.ADM_Close), (dsp, T.dll.DSP_Close), (dev, T.dll.Device_Close)]:
            if handle:
                with suppress(Exception):
                    close(byref(handle))
        raise
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
    if T.dll.IQS_Configuration(byref(dev), byref(p), byref(out), byref(info)) != 0:
        raise RuntimeError('IQS configuration failed')
    total = int(info.StreamSamples); pkt = int(info.PacketSamples)
    packets = int(info.PacketCount)
    if (not 2 <= total <= MAX_CAPTURE_BYTES // 4 or not 2 <= pkt <= MAX_PACKET_BYTES // 4
            or packets != (total + pkt - 1) // pkt
            or int(info.PacketDataSize) != pkt * 4
            or out.DataFormat.value != T.DataFormat_TypeDef.Complex16bit
            or not math.isfinite(info.IQSampleRate) or info.IQSampleRate <= 0):
        raise RuntimeError('IQS invalid configuration geometry')
    buf = (c_int16 * (total * 2))()
    stream = T.IQStream_TypeDef()
    try:
        if T.dll.IQS_BusTriggerStart(byref(dev)) != 0:
            raise RuntimeError('IQS_BusTriggerStart failed')
        copied = 0
        for _ in range(packets):
            if T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream)) != 0:
                raise RuntimeError('IQS_GetIQStream_PM1 failed')
            n, size = validate_iq_packet(stream, pkt, T.DataFormat_TypeDef.Complex16bit)
            if n != min(pkt, total - copied):
                raise RuntimeError('IQS packet count does not match the remaining capture')
            memmove(addressof(buf) + copied * 4, cast(stream.AlternIQStream, c_void_p).value, size)
            copied += n
    finally:
        stop = T.dll.IQS_BusTriggerStop(byref(dev))
        if stop != 0:
            if sys.exc_info()[0] is None:
                raise RuntimeError(f'IQS_BusTriggerStop failed: {stop}')
            print(f'IQS_BusTriggerStop also failed: {stop}', file=sys.stderr)
    return buf, info, float(stream.IQS_ScaleToV)


def tone_of(wave, rate):
    if wave is None or len(wave) < 32:
        return 0.0
    w = wave - np.mean(wave)
    s = np.abs(np.fft.rfft(w * np.hanning(len(w))))
    f = np.fft.rfftfreq(len(w), 1 / rate)
    return float(f[int(np.argmax(s))])


def demod_waveform(params, input_samples, status):
    """Read the ADM result while its owning handle is still open."""
    if status != 0:
        raise RuntimeError(f'ADM demodulation failed: {status}')
    count = int(params.DemodWaveformSize)
    if (not 1 <= count <= input_samples <= MAX_CAPTURE_BYTES // 4
            or not params.DemodWaveform):
        raise RuntimeError(f'ADM invalid waveform geometry: {count} samples')
    return np.ctypeslib.as_array(params.DemodWaveform, shape=(count,)).copy()


def ddc_float_block(dsp, buf, input_samples, rate):
    """Run the manual probe's single DDC block and own the returned samples."""
    if (not math.isfinite(rate) or rate <= 0
            or not 1 <= input_samples <= MAX_CAPTURE_BYTES // 4
            or len(buf) < input_samples * 2):
        raise ValueError('DDC invalid input geometry')
    din = T.DSP_DDC_TypeDef(); dout = T.DSP_DDC_TypeDef()
    status = T.dll.DSP_DDC_DeInit(byref(din))
    if status != 0:
        raise RuntimeError(f'DSP_DDC_DeInit failed: {status}')
    din.DDCOffsetFrequency = 0.0; din.SampleRate = rate
    din.DecimateFactor = 50.0; din.SamplePoints = input_samples
    status = T.dll.DSP_DDC_Configuration(byref(dsp), byref(din), byref(dout))
    if status != 0:
        raise RuntimeError(f'DSP_DDC_Configuration failed: {status}')
    points = int(dout.SamplePoints); output_rate = float(dout.SampleRate)
    if (not math.isfinite(output_rate) or not 0 < output_rate <= rate
            or not 1 <= points <= min(input_samples, MAX_CAPTURE_BYTES // 8)):
        raise RuntimeError('DDC invalid configuration geometry')
    status = T.dll.DSP_DDC_Reset(byref(dsp))
    if status != 0:
        raise RuntimeError(f'DSP_DDC_Reset failed: {status}')
    ins = T.IQStream_TypeDef()
    ins.AlternIQStream = cast(buf, POINTER(c_void_p))
    ins.IQS_StreamInfo.PacketSamples = input_samples; ins.IQS_StreamInfo.IQSampleRate = rate
    outs = T.IQStream_TypeDef()
    status = T.dll.DSP_DDC_Execute(byref(dsp), byref(ins), byref(outs))
    if status != 0:
        raise RuntimeError(f'DSP_DDC_Execute failed: {status}')
    info = outs.IQS_StreamInfo
    if (not outs.AlternIQStream or int(info.PacketSamples) != points
            or int(info.PacketDataSize) != points * 8
            or outs.IQS_Profile.DataFormat.value != T.DataFormat_TypeDef.Complexfloat):
        raise RuntimeError('DDC invalid output packet geometry')
    samples = np.ctypeslib.as_array(
        cast(outs.AlternIQStream, POINTER(c_float * (points * 2))).contents).copy()
    if not np.all(np.isfinite(samples)):
        raise RuntimeError('DDC non-finite output samples')
    return samples, output_rate


def main():
    bind_adm()
    center = 100e6
    sa = TinySA('/dev/ttyACM0')
    dev = dsp = adm = None
    try:
        sa.enter_low_output()
        dev, dsp, adm = open_device()
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
        wfp = demod_waveform(p, n, st)
        print(f'ADM_AMDemod_PM1 st={st} size={p.DemodWaveformSize} ModRate={p.ModRate:.2f} Hz '
              f'(expect ~1000) ModDepth={p.ModDepth:.2f}% (expect ~50) Carrier={p.CarrierPower:.2f} dBm '
              f'SINAD={p.SINAD:.2f} SNR={p.SNR:.2f} THD={p.THD:.3f}')
        if wfp is not None:
            print(f'  waveform tone = {tone_of(wfp, fs):.1f} Hz (expect ~1000)')
        st2 = T.dll.ADM_AMDemod(byref(adm), cast(buf, c_void_p),
                                int(T.DataFormat_TypeDef.Complex16bit), n, fs, wf)
        if st2 != 0:
            raise RuntimeError(f'ADM_AMDemod failed: {st2}')
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
        wfp = demod_waveform(fp, n, st)
        print(f'ADM_FMDemod_PM1 st={st} size={fp.DemodWaveformSize} ModRate={fp.ModRate:.2f} Hz '
              f'(expect ~1000) Deviation={fp.Deviation:.1f} Hz (expect ~25000) '
              f'CarrierErr={fp.CarrierFreqErr:.1f} SINAD={fp.SINAD:.2f} SNR={fp.SNR:.2f} THD={fp.THD:.3f}')
        if wfp is not None:
            print(f'  waveform tone = {tone_of(wfp, fs):.1f} Hz (expect ~1000)')

        # ---------- DDC -> ADM (complex-float output) ----------
        print('\n-- DDC float output -> ADM (Complexfloat) --')
        arr, orate = ddc_float_block(dsp, buf, n, fs)
        opts = arr.size // 2
        fp2 = FMDemodParam()
        st = T.dll.ADM_FMDemod_PM1(byref(adm), arr.ctypes.data_as(c_void_p),
                                   int(T.DataFormat_TypeDef.Complexfloat), opts, orate, scale, True, byref(fp2))
        if st != 0:
            raise RuntimeError(f'ADM_FMDemod_PM1 on float failed: {st}')
        print(f'FMDemod on DDC float: st={st} out_pts={opts} rate={orate} ModRate={fp2.ModRate:.2f} '
              f'Deviation={fp2.Deviation:.1f}')
        # convert DDC float -> int16 and retry
        mx = max(1e-9, float(np.max(np.abs(arr))))
        i16 = (arr / mx * 30000).astype(np.int16)
        cbuf = (c_int16 * (opts * 2))(*i16.tolist())
        fp3 = FMDemodParam()
        st = T.dll.ADM_FMDemod_PM1(byref(adm), cast(cbuf, c_void_p),
                                   int(T.DataFormat_TypeDef.Complex16bit), opts, orate, scale, True, byref(fp3))
        if st != 0:
            raise RuntimeError(f'ADM_FMDemod_PM1 on int16 failed: {st}')
        print(f'FMDemod on DDC->int16: st={st} ModRate={fp3.ModRate:.2f} Deviation={fp3.Deviation:.1f}')
    finally:
        for handle, close in [(adm, T.dll.ADM_Close), (dsp, T.dll.DSP_Close), (dev, T.dll.Device_Close)]:
            if handle:
                with suppress(Exception):
                    close(byref(handle))
        sa.close()


if __name__ == '__main__':
    main()
