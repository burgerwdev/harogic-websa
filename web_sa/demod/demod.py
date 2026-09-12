"""
demod/demod.py -- analog demodulators for the SDR mode (numpy).

Input is complex baseband (from DSP_DDC, carrier at DC) and output is real audio at
`audio_rate`. Modes: AM, FM/NFM/WFM, USB, LSB, CW. Sideband selection uses a
complex FIR; CW is filtered at zero IF and then shifted to the configured sidetone.
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
            band = (-w, w)
            kind = 'cw'
        else:
            band = (-if_bw / 2.0, if_bw / 2.0)
            kind = 'am' if mode == 'am' else 'fm'
        self.kind = kind
        self.band = StreamFilter(design_complex_bandpass(fs, band[0], band[1], ntaps=257))
        if mode == 'wfm':
            audio_cut = 15000.0
        elif mode == 'nfm':
            audio_cut = min(5000.0, if_bw / 2.0)
        elif mode in ('am', 'fm'):
            audio_cut = min(15000.0, if_bw / 2.0)
        elif mode == 'cw':
            audio_cut = min(5000.0, max(1000.0, self.pitch + if_bw / 2.0 + 200.0))
        else:
            audio_cut = min(20000.0, if_bw)
        audio_cut = max(100.0, min(audio_cut, 0.45 * self.audio_rate, 0.45 * fs))
        self.audio_lp = StreamFilter(design_lowpass(fs, audio_cut, ntaps=129))
        self.resampler = LinearResampler(fs, self.audio_rate)
        self.agc = Agc(target=0.2, attack=0.2, release=0.08)
        self._prev_z = None
        self._prev_env = 0.0
        self._dc_y = 0.0
        self._deemph_alpha = np.exp(-1.0 / (fs * 50e-6)) if mode == 'wfm' else 0.0
        self._deemph_y = 0.0
        self._cw_phase = 0.0
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
        self._dc_y = 0.0
        self._deemph_y = 0.0
        self._cw_phase = 0.0

    def retune(self) -> None:
        """Retune to a new offset: clear the filter tail and demodulator history so the
        first samples are clean, but KEEP the AGC gain (no re-settle, no pop)."""
        if not self._configured:
            return
        self.band.reset()
        self._prev_z = None
        self._prev_env = 0.0
        self._dc_y = 0.0
        self._deemph_y = 0.0
        self._cw_phase = 0.0

    # ---- processing ----
    def process(self, i, q, use_agc: bool = True):
        """Returns (audio float32 @ audio_rate, power_dbfs)."""
        if not self._configured:
            return np.zeros(0, dtype=np.float32), -120.0
        z = np.asarray(i, dtype=np.float64) + 1j * np.asarray(q, dtype=np.float64)
        if z.size == 0:
            return np.zeros(0, dtype=np.float32), -120.0
        zf = self.band.process(z)
        if zf.size == 0:
            return np.zeros(0, dtype=np.float32), -120.0
        # Channel power (after the IF filter) -> meaningful S-meter / squelch.
        power_dbfs = float(10.0 * np.log10(float(np.mean(np.abs(zf) ** 2)) + 1e-20))

        if self.kind == 'am':
            env = np.abs(zf)
            # DC block (one-pole high-pass); state carried across blocks
            out = np.empty_like(env)
            prev_x = self._prev_env
            prev_y = self._dc_y
            a = 0.9995
            for k in range(env.size):
                y = a * (prev_y + env[k] - prev_x)
                out[k] = y
                prev_x = env[k]
                prev_y = y
            self._prev_env = prev_x
            self._dc_y = prev_y
            audio = out
        elif self.kind == 'fm':
            if self._prev_z is not None:
                zf = np.concatenate([[self._prev_z], zf])
            self._prev_z = zf[-1]
            d = np.angle(zf[1:] * np.conj(zf[:-1]))
            audio = d * (self.fs / (2.0 * np.pi))
        elif self.kind == 'cw':
            inc = 2.0 * np.pi * self.pitch / self.fs
            phase = self._cw_phase + inc * np.arange(zf.size)
            audio = (zf * np.exp(1j * phase)).real
            self._cw_phase = (self._cw_phase + inc * zf.size) % (2.0 * np.pi)
        else:  # SSB: real part of the sideband-selected complex signal
            audio = zf.real

        if self._deemph_alpha > 0.0 and audio.size:
            # Keep the stateful one-pole recurrence in-place to avoid an extra audio-sized
            # allocation on every high-rate DDC packet.
            alpha = self._deemph_alpha
            feed = 1.0 - alpha
            prev = self._deemph_y
            for k in range(audio.size):
                prev = alpha * prev + feed * audio[k]
                audio[k] = prev
            self._deemph_y = float(prev)

        a = self.audio_lp.process(audio.astype(np.float32))
        a = self.resampler.process(a)
        if use_agc and a.size:
            a = self.agc.process(a)
        return np.ascontiguousarray(a, dtype=np.float32), power_dbfs
