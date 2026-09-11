#!/usr/bin/env python3
"""Verify DSP_DDC_* on real SAN-90 IQ.

Generates a known CW tone with the tinySA Ultra, captures SAN-90 IQS data, then
applies DSP_DDC with different offsets/decimations and FFTs the result with the
vendor DSP_FFT_IQSToSpectrum. Saves a multi-panel PNG for visual inspection.

Usage: python3 tools/sdr_probe/probe_ddc.py --tone 100.2e6 --center 100e6
"""
from __future__ import annotations

import argparse
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

PLOTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'plots')
os.makedirs(PLOTS, exist_ok=True)


def open_device():
    dev = c_void_p()
    bp = T.BootProfile_TypeDef()
    bi = T.BootInfo_TypeDef()
    bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
    bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
    st = T.dll.Device_Open(byref(dev), T.c_int(0), byref(bp), byref(bi))
    if st not in (0, -49):
        raise RuntimeError(f'Device_Open status={st}')
    dsp = c_void_p()
    dsp_st = T.dll.DSP_Open(byref(dsp))
    return dev, dsp, dsp_st


def capture(dev, center, decimate, trigger_len, dc_cancel=0):
    p = T.IQS_Profile_TypeDef()
    T.dll.IQS_ProfileDeInit(byref(dev), byref(p))
    p.CenterFreq_Hz = center
    p.RefLevel_dBm = 0.0
    p.DecimateFactor = int(decimate)
    p.DataFormat = T.DataFormat_TypeDef.Complex16bit
    p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
    p.TriggerMode = T.TriggerMode_TypeDef.FixedPoints
    p.TriggerLength = int(trigger_len)
    p.BusTimeout_ms = 5000
    p.DCCancelerMode = T.DCCancelerMode_TypeDef(dc_cancel)
    p.QDCMode = T.QDCMode_TypeDef.QDCOff
    out = T.IQS_Profile_TypeDef()
    info = T.IQS_StreamInfo_TypeDef()
    st = T.dll.IQS_Configuration(byref(dev), byref(p), byref(out), byref(info))
    if st != 0:
        raise RuntimeError(f'IQS_Configuration status={st}')
    total = int(info.StreamSamples)
    pkt = int(info.PacketSamples)
    buf = (c_int16 * (total * 2))()
    stream = T.IQStream_TypeDef()
    st = T.dll.IQS_BusTriggerStart(byref(dev))
    if st != 0:
        raise RuntimeError(f'IQS_BusTriggerStart status={st}')
    for i in range(int(info.PacketCount)):
        st = T.dll.IQS_GetIQStream_PM1(byref(dev), byref(stream))
        if st != 0:
            raise RuntimeError(f'IQS_GetIQStream_PM1 status={st} packet={i}')
        if i == int(info.PacketCount) - 1 and total % pkt != 0:
            n = total % pkt
        else:
            n = pkt
        src = cast(stream.AlternIQStream, c_void_p).value
        memmove(addressof(buf) + i * pkt * 2 * 2, src, n * 2 * 2)
    T.dll.IQS_BusTriggerStop(byref(dev))
    return buf, info, out, float(stream.IQS_ScaleToV)


def dsp_fft(dsp, iq_buf, n_samples, sample_rate, center_hz):
    """Run DSP_FFT_IQSToSpectrum and return (freq_axis, power_db)."""
    fft_in = T.DSP_FFT_TypeDef()
    fft_out = T.DSP_FFT_TypeDef()
    trace_points = T.c_uint32(0)
    rbw_ratio = T.c_double(0.0)
    T.dll.DSP_FFT_DeInit(byref(fft_in))
    fft_in.Calibration = 0
    fft_in.DetectionRatio = 1
    fft_in.TraceDetector = T.TraceDetector_TypeDef.TraceDetector_PosPeak
    fft_in.FFTSize = int(n_samples)
    fft_in.SamplePts = int(n_samples)
    fft_in.Intercept = 1.0
    fft_in.WindowType = T.Window_TypeDef.FlatTop
    st = T.dll.DSP_FFT_Configuration(byref(dsp), byref(fft_in), byref(fft_out),
                                     byref(trace_points), byref(rbw_ratio))
    pts = int(trace_points.value)
    freq = (T.c_double * pts)()
    power = (T.c_float * pts)()
    stream = T.IQStream_TypeDef()
    stream.AlternIQStream = cast(iq_buf, POINTER(c_void_p))
    stream.IQS_StreamInfo.PacketSamples = int(n_samples)
    stream.IQS_StreamInfo.IQSampleRate = float(sample_rate)
    st2 = T.dll.DSP_FFT_IQSToSpectrum(byref(dsp), byref(stream), freq, power)
    fa = np.ctypeslib.as_array(freq).copy()
    pa = np.ctypeslib.as_array(power).copy()
    return dict(status=(st, st2), trace_points=pts, rbw_ratio=float(rbw_ratio.value),
                freq=fa, power=pa, out=fft_out)


def np_fft(iq_i, iq_q, sample_rate):
    x = iq_i + 1j * iq_q
    n = len(x)
    x = x * np.hanning(n)
    spec = np.fft.fftshift(np.fft.fft(x))
    power = 20 * np.log10(np.abs(spec) / (n / 2) + 1e-20)
    freq = np.fft.fftshift(np.fft.fftfreq(n, d=1.0 / sample_rate))
    return freq, power


def dsp_fft_on_stream(dsp, stream, n_samples):
    """Vendor FFT directly on an existing IQStream (used for the DDC output)."""
    fft_in = T.DSP_FFT_TypeDef()
    fft_out = T.DSP_FFT_TypeDef()
    trace_points = T.c_uint32(0)
    rbw_ratio = T.c_double(0.0)
    T.dll.DSP_FFT_DeInit(byref(fft_in))
    fft_in.Calibration = 0
    fft_in.DetectionRatio = 1
    fft_in.TraceDetector = T.TraceDetector_TypeDef.TraceDetector_PosPeak
    fft_in.FFTSize = int(n_samples)
    fft_in.SamplePts = int(n_samples)
    fft_in.Intercept = 1.0
    fft_in.WindowType = T.Window_TypeDef.FlatTop
    st = T.dll.DSP_FFT_Configuration(byref(dsp), byref(fft_in), byref(fft_out),
                                     byref(trace_points), byref(rbw_ratio))
    pts = int(trace_points.value)
    freq = (T.c_double * pts)()
    power = (T.c_float * pts)()
    st2 = T.dll.DSP_FFT_IQSToSpectrum(byref(dsp), byref(stream), freq, power)
    return dict(status=(st, st2), trace_points=pts, rbw_ratio=float(rbw_ratio.value),
                freq=np.ctypeslib.as_array(freq).copy(),
                power=np.ctypeslib.as_array(power).copy())


def stream_info(stream):
    si = stream.IQS_StreamInfo
    return dict(
        fmt=int(getattr(stream.IQS_Profile.DataFormat, 'value', -1)),
        trigger_length=int(getattr(stream.IQS_Profile, 'TriggerLength', -1)),
        packet_samples=int(si.PacketSamples), iq_rate=float(si.IQSampleRate),
        stream_samples=int(si.StreamSamples), packet_data_size=int(si.PacketDataSize),
        bandwidth=float(si.Bandwidth))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--center', type=float, default=100e6)
    ap.add_argument('--tone', type=float, default=100.2e6)
    ap.add_argument('--level', type=float, default=-25.0)
    ap.add_argument('--decimate', type=int, default=16)
    ap.add_argument('--trigger-length', type=int, default=131072)
    ap.add_argument('--serial', default='/dev/ttyACM0')
    args = ap.parse_args()

    sa = TinySA(args.serial)
    print('tinySA:', sa.cmd('info').split('\n')[0])
    sa.enter_low_output()
    print('tinySA CW:', sa.cw(args.tone, args.level))
    time.sleep(0.5)

    dev, dsp, dsp_st = open_device()
    print('device open, DSP_Open=', dsp_st)
    try:
        buf, info, prof, scale = capture(
            dev, args.center, args.decimate, args.trigger_length, dc_cancel=0)
        n = int(info.StreamSamples)
        fs = float(info.IQSampleRate)
        print(f'captured: samples={n} fs={fs} bw={info.Bandwidth} scaleToV={scale}')

        all_i16 = np.ctypeslib.as_array(buf).copy()
        i_data = all_i16[0::2].astype(np.float32)
        q_data = all_i16[1::2].astype(np.float32)

        # ---- raw numpy FFT
        raw_f, raw_p = np_fft(i_data, q_data, fs)

        # ---- vendor FFT on raw IQ
        vraw = dsp_fft(dsp, buf, n, fs, args.center)

        # ---- DDC experiments (offline on the same captured buffer)
        results = {}
        for label, offset, dec in (
            ('off0_dec1', 0.0, 1),
            ('off0_dec8', 0.0, 8),
            ('off200k_dec100', args.tone - args.center, 100),
            ('off200k_dec1000', args.tone - args.center, 1000),
            ('offneg100k_dec100', -100e3, 100),
        ):
            din = T.DSP_DDC_TypeDef()
            dout = T.DSP_DDC_TypeDef()
            T.dll.DSP_DDC_DeInit(byref(din))
            din.DDCOffsetFrequency = float(offset)
            din.SampleRate = fs
            din.DecimateFactor = float(dec)
            din.SamplePoints = n
            ddc_st = T.dll.DSP_DDC_Configuration(byref(dsp), byref(din), byref(dout))
            delay = T.c_uint32(0)
            T.dll.DSP_DDC_GetDelay(byref(dsp), byref(delay))
            T.dll.DSP_DDC_Reset(byref(dsp))
            in_stream = T.IQStream_TypeDef()
            in_stream.AlternIQStream = cast(buf, POINTER(c_void_p))
            in_stream.IQS_StreamInfo.PacketSamples = n
            in_stream.IQS_StreamInfo.IQSampleRate = fs
            out_stream = T.IQStream_TypeDef()
            ex_st = T.dll.DSP_DDC_Execute(byref(dsp), byref(in_stream), byref(out_stream))
            out_pts = int(getattr(dout, 'SamplePoints', 0))
            out_rate = float(getattr(dout, 'SampleRate', 0.0))
            info_out = stream_info(out_stream)
            print(f'  {label}: out_stream {info_out}')
            vfft = None
            if out_pts > 2:
                vfft = dsp_fft_on_stream(dsp, out_stream, out_pts)
            # numpy view of the DDC output buffer, both interpretations
            np_i = np_q = None
            try:
                out_n = min(out_pts, 400000)
                if out_n > 0:
                    src_f = cast(out_stream.AlternIQStream, POINTER(T.c_float * (out_n * 2))).contents
                    oa = np.ctypeslib.as_array(src_f).copy()
                    np_i, np_q = oa[0::2].astype(np.float64), oa[1::2].astype(np.float64)
            except Exception as e:
                print('  read output err', repr(e))
            nfft = None
            if np_i is not None and out_rate > 0:
                f = np.fft.fftshift(np.fft.fftfreq(len(np_i), d=1.0 / out_rate))
                w = np_i + 1j * np_q
                sp = np.fft.fftshift(np.fft.fft(w * np.hanning(len(w))))
                p = 20 * np.log10(np.abs(sp) / (len(w) / 2) + 1e-20)
                nfft = (f, p)
            results[label] = dict(cfg_status=ddc_st, exec_status=ex_st, delay=int(delay.value),
                                  out_points=out_pts, out_rate=out_rate, info=info_out,
                                  nfft=nfft, vfft=vfft)
            print(f'{label}: ddc_st={ddc_st} ex_st={ex_st} delay={delay.value} '
                  f'out_pts={out_pts} out_rate={out_rate}')

        # ---- plot
        import matplotlib
        matplotlib.use('Agg')
        import matplotlib.pyplot as plt

        fig, axes = plt.subplots(4, 1, figsize=(12, 14))
        axes[0].plot((raw_f + args.center) / 1e6, raw_p, lw=0.6)
        axes[0].set_title(f'raw IQ numpy FFT  fs={fs/1e6:.3f} MSPS  center={args.center/1e6} MHz')
        axes[0].set_xlabel('MHz'); axes[0].set_ylabel('dB'); axes[0].set_ylim(-120, 20)
        axes[0].axvline(args.tone / 1e6, color='r', ls='--', lw=0.8)

        vf = vraw['freq'] if np.any(vraw['freq']) else np.linspace(-fs / 2, fs / 2, vraw['trace_points'])
        axes[1].plot(vf / 1e3, vraw['power'], lw=0.6)
        axes[1].set_title(f"vendor DSP_FFT raw  pts={vraw['trace_points']} rbwRatio={vraw['rbw_ratio']:.3g}")
        axes[1].set_xlabel('kHz (relative)'); axes[1].set_ylabel('dB'); axes[1].set_ylim(-120, 20)

        ax = axes[2]
        for label, r in results.items():
            if r['vfft'] is not None:
                p = r['vfft']['power']
                f = np.linspace(0, 1, len(p)) * r['out_rate']
                ax.plot(f / 1e3, p, lw=0.7, label=f"{label} rate={r['out_rate']/1e3:.2f}k")
        ax.set_title('DDC output VENDOR DSP_FFT (freq = index*rate/N)')
        ax.set_xlabel('kHz'); ax.set_ylabel('dB'); ax.set_ylim(-120, 80); ax.legend(fontsize=8)

        ax = axes[3]
        for label, r in results.items():
            if r['vfft'] is not None:
                p = r['vfft']['power']
                f = np.linspace(0, 1, len(p)) * r['out_rate']
                f = np.where(f > r['out_rate'] / 2, f - r['out_rate'], f)
                ax.plot(f, p, lw=0.7, label=label)
        ax.set_title('DDC output vendor FFT (fftshifted Hz)')
        ax.set_xlabel('Hz'); ax.set_ylabel('dB')
        ax.set_xlim(-max(20000, args.tone - args.center if False else 60000), 60000)
        ax.set_ylim(-120, 80); ax.legend(fontsize=8)

        fig.tight_layout()
        out_png = os.path.join(PLOTS, 'ddc_verify.png')
        fig.savefig(out_png, dpi=110)
        print('saved', out_png)

        # numeric tone location in each DDC output
        for label, r in results.items():
            if r['nfft'] is None:
                continue
            f, p = r['nfft']
            k = int(np.argmax(p))
            print(f'  {label}: numpy peak {f[k]:+.1f} Hz level {p[k]:.1f} dB | '
                  f'vendor peak bin {int(np.argmax(r["vfft"]["power"]))}/'
                  f'{len(r["vfft"]["power"])}')
    finally:
        T.dll.DSP_Close(byref(dsp))
        T.dll.Device_Close(byref(dev))
        sa.close()
        print('device + tinySA closed')


if __name__ == '__main__':
    main()
