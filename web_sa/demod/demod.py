"""
demod/demod.py -- analog demodulators for the SDR mode (numpy).

Input is complex baseband (from DSP_DDC, carrier at DC) and output is real audio at
`audio_rate`. Modes: AM, FM/NFM/WFM, USB, LSB, CW. Sideband selection is done with a
complex FIR (positive band = USB, negative = LSB, offset band = CW sidetone).
"""
from __future__ import annotations

import numpy as np

from .filters import Agc, LinearResampler, StreamFilter, design_complex_bandpass, design_lowpass

ANALOG_MODES = ('am', 'fm', 'nfm', 'wfm', 'usb', 'lsb', 'cw')


class AnalogDemod:
    def __init__(self, audio_rate: float = 48000.0):
        self.audio_rate = float(audio_rate)
        self._configured = False
        self.configure(fs=48000.0, mode='am', if_bw=6000.0)

    # ---- configuration ----
    def configure(self, fs: float, mode: str, if_bw: float, pitch: float = 700.0) -> None:
        mode = mode if mode in ANALOG_MODES else 'am'
        fs = float(fs)
        if_bw = float(max(50.0, min(if_bw, fs * 0.45)))
        self.fs = fs
        self.mode = mode
        self.if_bw = if_bw
        self.pitch = float(pitch)
        if mode == 'usb':
            band = (max(100.0, min(300.0, if_bw * 0.1)), if_bw)
            kind = 'ssb'
        elif mode == 'lsb':
            band = (-if_bw, -max(100.0, min(300.0, if_bw * 0.1)))
            kind = 'ssb'
        elif mode == 'cw':
            w = max(50.0, if_bw) / 2.0
            band = (self.pitch - w, self.pitch + w)
            kind = 'ssb'
        else:
            band = (-if_bw / 2.0, if_bw / 2.0)
            kind = 'am' if mode == 'am' else 'fm'
        self.kind = kind
        self.band = StreamFilter(design_complex_bandpass(fs, band[0], band[1], ntaps=257))
        audio_cut = min(if_bw, 20000.0, 0.45 * self.audio_rate)
        self.audio_lp = StreamFilter(design_lowpass(fs, audio_cut, ntaps=129))
        self.resampler = LinearResampler(fs, self.audio_rate)
        self.agc = Agc(target=0.2, attack=0.05, release=0.01)
        self._prev_z = None
        self._prev_env = 0.0
        self._configured = True

    def reset(self) -> None:
        if not self._configured:
            return
        self.band.reset()
        self.audio_lp.reset()
        self.resampler = LinearResampler(self.fs, self.audio_rate)
        self.agc.reset()
        self._prev_z = None
        self._prev_env = 0.0

    # ---- processing ----
    def process(self, i, q, use_agc: bool = True):
        """Returns (audio float32 @ audio_rate, power_dbfs)."""
        if not self._configured:
            return np.zeros(0, dtype=np.float32), -120.0
        z = np.asarray(i, dtype=np.float64) + 1j * np.asarray(q, dtype=np.float64)
        if z.size == 0:
            return np.zeros(0, dtype=np.float32), -120.0
        power = float(np.mean(np.abs(z) ** 2) + 1e-20)
        power_dbfs = float(10.0 * np.log10(power + 1e-20))
        zf = self.band.process(z)
        if zf.size == 0:
            return np.zeros(0, dtype=np.float32), power_dbfs

        if self.kind == 'am':
            env = np.abs(zf)
            # DC block (one-pole high-pass) so the carrier term is removed
            out = np.empty_like(env)
            prev_x = self._prev_env
            prev_y = 0.0
            a = 0.9995
            for k in range(env.size):
                y = a * (prev_y + env[k] - prev_x)
                out[k] = y
                prev_x = env[k]
                prev_y = y
            self._prev_env = prev_x
            audio = out
        elif self.kind == 'fm':
            if self._prev_z is not None:
                zf = np.concatenate([[self._prev_z], zf])
            self._prev_z = zf[-1]
            d = np.angle(zf[1:] * np.conj(zf[:-1]))
            audio = d * (self.fs / (2.0 * np.pi))
        else:  # ssb / cw: real part of the sideband-selected complex signal
            audio = zf.real
            self._prev_z = zf[-1]

        a = self.audio_lp.process(audio.astype(np.float32))
        a = self.resampler.process(a)
        if use_agc and a.size:
            a = self.agc.process(a)
        return np.ascontiguousarray(a, dtype=np.float32), power_dbfs
