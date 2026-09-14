"""Fake measurement sessions: synthetic RTA and SDR frames without the vendor library.

Used together with `hardware/fake_device.py` so the UI regression can run in CI (finding A1).
The swept path needs no fake session: `StdSession` only calls `dev.fetch_sweep()`, which the
fake device implements.
"""
from __future__ import annotations

import numpy as np

from .base import MeasurementSession
from .framer import encode_audio, encode_rta

RTA_POINTS = 1024
RTA_WATERFALL_WIDTH = 128
SDR_PAN_POINTS = 512
SDR_AUDIO_RATE = 48000
SDR_AUDIO_SAMPLES = 960          # 20 ms at 48 kHz, what the real SDR emits
AUDIO_TONE_HZ = 1000.0


class _FakeRtaBase(MeasurementSession):
    """Shared synthetic frame generation for the fake RTA/SDR sessions."""

    auto_ref_scope = 'rta'

    def __init__(self, dev):
        super().__init__(dev)
        self._ready = True
        self._tick = 0
        self._rng = np.random.default_rng(7)
        self._audio_seq = 0

    # lifecycle protocol the command layer relies on
    def request_stop(self) -> None:
        self._ready = False

    def is_ready(self) -> bool:
        return bool(self._ready)

    def acquisition_timeout(self) -> float:
        return 5.0

    def health(self) -> dict:
        return {'error_streak': 0, 'recovery_attempts': 0}

    def _spectrum(self, center: float, span: float, points: int):
        freq = np.linspace(center - span / 2, center + span / 2, points)
        powers = (-95.0 + self._rng.normal(0.0, 0.5, points)
                  + 70.0 * np.exp(-0.5 * ((np.arange(points) - points * 0.5) / 6.0) ** 2))
        return freq, powers.astype(np.float32)

    def _wf_row(self, width: int = RTA_WATERFALL_WIDTH) -> np.ndarray:
        return self._rng.integers(0, 4096, width, dtype=np.uint16)

    def _audio(self, samples: int = SDR_AUDIO_SAMPLES) -> bytes:
        t = (np.arange(samples) + self._tick * samples) / SDR_AUDIO_RATE
        pcm = (0.2 * np.sin(2 * np.pi * AUDIO_TONE_HZ * t) * 32767).astype(np.int16)
        self._audio_seq += 1
        return encode_audio(self._audio_seq, SDR_AUDIO_RATE, pcm)


class FakeRtaSession(_FakeRtaBase):
    """Real-time spectrum session emitting synthetic RTAF frames."""

    name = 'rta'

    def enter(self) -> None:
        super().enter()
        self._ready = True

    def reconfigure(self) -> None:
        self._ready = True

    def reset_defaults(self) -> None:
        self._ready = True

    def set_params(self, center=None, span=None) -> None:
        s = self.dev.state
        if center is not None:
            s.rta_center_hz = float(center)
        if span is not None:
            s.rta_span_hz = max(1000.0, min(float(span), 50.78125e6))
        self._ready = True

    def set_rbw(self, mode='auto', rbw=0.0) -> None:
        self.dev.state.rta_rbw_mode = mode
        self.dev.state.rta_rbw_hz = float(rbw or 0.0)

    def set_vbw(self, mode='equal', vbw=0.0) -> None:
        self.dev.state.rta_vbw_mode = mode
        self.dev.state.rta_vbw_hz = float(vbw or 0.0)

    def set_sweep(self, mode=0, time=0.0) -> None:
        self.dev.state.rta_sweep_time_mode = int(mode)
        self.dev.state.rta_sweep_time = float(time or 0.0)

    def set_reference(self, mode='manual', ref=None) -> None:
        self.dev.state.rta_ref_mode = mode
        if mode == 'manual' and ref is not None:
            self.dev.state.rta_ref_level = float(ref)

    def set_trigger(self) -> None:
        self.dev.state.trigger_actual = {'waiting': False, 'frames': self._tick, 'edges': 0,
                                         'triggered_bytes': 0, 'first_ts': 0, 'first_edge_ts': 0}

    def step(self):
        if not self._ready:
            return [], []
        self._tick += 1
        s = self.dev.state
        center, span = float(s.rta_center_hz), float(s.rta_span_hz)
        freq, spec = self._spectrum(center, span, RTA_POINTS)
        s.rta_actual = {
            'center': center, 'span': span, 'start': center - span / 2,
            'stop': center + span / 2, 'ref': float(s.rta_ref_level),
            'rbw': 7.5e3, 'vbw': 7.5e3, 'points': RTA_POINTS, 'frame_points': RTA_POINTS,
            'refclk': 100e6, 'refclk_src': 0, 'refclk_out': s.refclk_out,
            'atten': s.atten, 'preamp': s.preamplifier, 'ifgain': s.ifgain,
            'poi': 0.0005, 'time_resolution': 5.12e-7, 'packet_count': 1, 'packet_frame': 2,
        }
        frame = encode_rta(s.freq_version, freq, spec, self._wf_row(), 4095,
                           center - span / 2, center + span / 2)
        return [frame], []


class FakeSdrSession(_FakeRtaBase):
    """SDR session emitting a synthetic panadapter (RTAF) plus 20 ms audio frames (AUDF)."""

    name = 'sdr'
    auto_ref_scope = 'std'

    def enter(self) -> None:
        super().enter()
        self._ready = True

    def exit(self) -> None:
        self._ready = False
        snap = self.snapshot
        if snap is not None:
            s = self.dev.state
            s.center_hz, s.span_hz = snap.center, snap.span
            s.ref_level, s.ref_mode = snap.ref, snap.ref_mode
            s.rbw_mode, s.rbw_hz = snap.rbw_mode, snap.rbw_hz
            s.vbw_mode, s.vbw_hz = snap.vbw_mode, snap.vbw_hz
            s.atten, s.preamplifier = snap.atten, snap.preamp
            s.ifgain, s.gain_strategy = snap.ifgain, snap.gain_strategy
            s.window, s.spur_mode = snap.window, snap.spur
            self.snapshot = None

    def reconfigure(self) -> None:
        self._ready = True

    def set_params(self, center=None, decimate=None) -> None:
        s = self.dev.state
        if center is not None:
            s.sdr_center_hz = float(center)
            s.sdr_listen_hz = float(center)
        if decimate is not None:
            s.sdr_decimate = int(decimate)
        self._ready = True

    def set_tune(self, listen_hz) -> None:
        self.dev.state.sdr_listen_hz = float(listen_hz)

    def set_demod(self, mode=None, if_bw=None, squelch=None, volume=None, agc=None,
                  pitch=None, deemph_us=None) -> None:
        s = self.dev.state
        if mode is not None:
            s.sdr_demod = str(mode)
        if if_bw is not None:
            s.sdr_if_bw = float(if_bw)
        if squelch is not None:
            s.sdr_squelch = float(squelch)
        if volume is not None:
            s.sdr_volume = float(volume)
        if agc is not None:
            s.sdr_agc = bool(agc)
        if pitch is not None:
            s.sdr_pitch = float(pitch)
        if deemph_us is not None:
            s.sdr_deemph_us = float(deemph_us)

    def step(self):
        if not self._ready:
            return [], []
        self._tick += 1
        s = self.dev.state
        center = float(s.sdr_center_hz)
        # 0.8 * 62.5 MHz / decimate, as the vendor IQS reports it
        bandwidth = 50e6 / max(1, int(s.sdr_decimate or 16))
        freq, spec = self._spectrum(center, bandwidth, SDR_PAN_POINTS)
        s.sdr_actual = {
            'iq_rate': 62.5e6 / max(1, int(s.sdr_decimate or 16)),
            'bandwidth': bandwidth, 'iq_center': center,
            'decimate': int(s.sdr_decimate or 16), 'packet_samples': 16240,
            'packet_bytes': 64960, 'pan_points': SDR_PAN_POINTS, 'center': center,
            'capture_center': center, 'capture_start': center - bandwidth / 2,
            'capture_stop': center + bandwidth / 2, 'start': center - bandwidth / 2,
            'stop': center + bandwidth / 2, 'atten': s.atten, 'preamp': s.preamplifier,
            'ifgain': s.ifgain, 'ref_clock_source': 0, 'refclk_out': s.refclk_out,
            'listen': float(s.sdr_listen_hz), 'demod': s.sdr_demod,
            'if_bw': float(s.sdr_if_bw), 'ddc_offset': 0.0, 'mix_offset': 0.0,
            'ddc_decimate': 81, 'ddc_rate': 48225.3, 'ddc_delay': 102, 'ddc_batch': 1,
            'audio_rate': SDR_AUDIO_RATE, 'deemph_us': float(s.sdr_deemph_us),
        }
        s.sdr_level_dbfs = -60.0
        s.sdr_squelch_open = True
        pan = encode_rta(s.freq_version, freq, spec, self._wf_row(), 4095,
                         center - bandwidth / 2, center + bandwidth / 2)
        return [pan, self._audio()], []
