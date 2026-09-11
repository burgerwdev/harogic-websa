#!/usr/bin/env python3
"""Live test of the SDR chain: IQS Adaptive -> DSP_DDC -> AnalogDemod -> WAV.

Reuses the real web_sa.demod modules. tinySA generates AM/FM at +200 kHz from the
SAN-90 center so the wideband DC spray is out of band. Writes /tmp/*.wav and prints
the demodulated audio tone.
"""
from __future__ import annotations

import os
import sys
import time
import wave
from ctypes import POINTER, byref, c_int16, c_void_p, cast

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from tinysa import TinySA  # noqa: E402

import htra_api as T  # noqa: E402
from web_sa.demod import AnalogDemod, DdcChannel, Panadapter  # noqa: E402

CENTER = 100e6
SIGNAL = 100.2e6
FS_AUDIO = 48000


def open_device():
    dev = c_void_p(); bp = T.BootProfile_TypeDef(); bi = T.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    st = T.dll.Device_Open(byref(dev), T.c_int(0), byref(bp), byref(bi))
    if st not in (0, -49):
        raise RuntimeError(f'Device_Open status={st}')
    dsp = c_void_p(); T.dll.DSP_Open(byref(dsp))
    return dev, dsp


def configure_iqs(dev, decimate=32, dc_cancel=0):
    p = T.IQS_Profile_TypeDef(); T.dll.IQS_ProfileDeInit(byref(dev), byref(p))
    p.CenterFreq_Hz = CENTER; p.RefLevel_dBm = 0.0; p.DecimateFactor = decimate
    p.DataFormat = T.DataFormat_TypeDef.Complex16bit
    p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
    p.TriggerMode = T.TriggerMode_TypeDef.Adaptive
    p.BusTimeout_ms = 2000
    p.DCCancelerMode = T.DCCancelerMode_TypeDef(dc_cancel)
    p.QDCMode = T.QDCMode_TypeDef.QDCOff
    out = T.IQS_Profile_TypeDef(); info = T.IQS_StreamInfo_TypeDef()
    st = T.dll.IQS_Configuration(byref(dev), byref(p), byref(out), byref(info))
    if st != 0:
        raise RuntimeError(f'IQS_Configuration status={st}')
    return out, info


def write_wav(path, audio, rate):
    a = np.clip(audio, -1, 1)
    pcm = (a * 32767).astype(np.int16)
    with wave.open(path, 'wb') as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(rate)
        w.writeframes(pcm.tobytes())


def tone_of(audio, rate):
    audio = np.asarray(audio)
    if len(audio) < 256:
        return 0.0
    w = np.hanning(len(audio))
    s = np.abs(np.fft.rfft((audio - audio.mean()) * w))
    f = np.fft.rfftfreq(len(audio), 1 / rate)
    return float(f[int(np.argmax(s))])


def run_case(name, setup, decimate, if_bw, seconds=2.0):
    sa = TinySA('/dev/ttyACM0')
    sa.enter_low_output()
    setup(sa)
    time.sleep(0.4)
    dev, dsp = open_device()
    try:
        out, info = configure_iqs(dev, decimate=decimate, dc_cancel=0)
        fs = float(info.IQSampleRate)
        pkt = int(info.PacketSamples)
        pan = Panadapter(2048)
        ddc = DdcChannel(dsp)
        offset = CENTER - SIGNAL
        need = max(FS_AUDIO, 2.2 * if_bw)
        ddec = max(1, int(np.floor(fs / need)))
        ddc.configure(fs, offset, ddec, pkt)
        demod = AnalogDemod(FS_AUDIO)
        demod.configure(ddc.fs_out, name, if_bw, pitch=700)
        print(f'[{name}] fs={fs/1e6:.3f} MSPS pkt={pkt} | ddc dec={ddec} rate={ddc.fs_out:.1f} '
              f'delay={ddc.delay}')
        audios = []
        pan_peaks = []
        stream = T.IQStream_TypeDef()
        T.dll.IQS_BusTriggerStart(byref(dev))
        t0 = time.monotonic()
        npkt = 0
        while time.monotonic() - t0 < seconds:
            st = T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream))
            if st != 0:
                break
            n = int(stream.IQS_StreamInfo.PacketSamples)
            src = cast(stream.AlternIQStream, POINTER(c_int16 * (n * 2))).contents
            i16 = np.ctypeslib.as_array(src).copy()
            scale = float(stream.IQS_ScaleToV)
            res = pan.process(i16[0::2], i16[1::2], fs, CENTER, scale)
            if res is not None and npkt % 8 == 0:
                _, spec, _ = res
                pan_peaks.append(float(np.max(spec)))
            i, q = ddc.process(i16, n)
            audio, pwr = demod.process(i, q)
            audios.append(audio)
            npkt += 1
        T.dll.IQS_BusTriggerStop(byref(dev))
        audio = np.concatenate(audios) if audios else np.zeros(0)
        path = f'/tmp/sdr_{name}.wav'
        write_wav(path, audio, FS_AUDIO)
        print(f'[{name}] packets={npkt} audio={len(audio)/FS_AUDIO:.2f}s '
              f'tone={tone_of(audio, FS_AUDIO):.1f} Hz  pan_peak={np.mean(pan_peaks) if pan_peaks else 0:.1f} dBm'
              f' -> {path}')
    finally:
        T.dll.Device_Close(byref(dev))


def main():
    run_case('am', lambda sa: sa.am(SIGNAL, -25, 1000, 50), decimate=32, if_bw=6000)
    run_case('fm', lambda sa: sa.fm(SIGNAL, -25, 1000, 25000), decimate=32, if_bw=25000)
    print('done')


if __name__ == '__main__':
    main()
