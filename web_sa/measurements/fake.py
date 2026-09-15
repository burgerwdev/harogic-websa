"""Fake measurement sessions: synthetic RTA and SDR frames without the vendor library.

Used together with `hardware/fake_device.py` so the UI regression can run in CI (finding A1).
The swept path needs no fake session: `StdSession` only calls `dev.fetch_sweep()`, which the
fake device implements.
"""
from __future__ import annotations

import numpy as np

from ..demod import digital as digital_dsp
from ..demod import vector as vector_dsp
from .base import MeasurementSession
from .framer import encode_audio, encode_rta, encode_vsa

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
        self._configure()

    def reconfigure(self) -> None:
        self._configure()

    def reset_defaults(self) -> None:
        self._ready = True

    def set_params(self, center=None, span=None) -> None:
        s = self.dev.state
        if center is not None:
            s.rta_center_hz = float(center)
        if span is not None:
            s.rta_span_hz = max(1000.0, min(float(span), 50.78125e6))
        self._configure()

    def set_rbw(self, mode='auto', rbw=0.0) -> None:
        self.dev.state.rta_rbw_mode = mode
        self.dev.state.rta_rbw_hz = float(rbw or 0.0)
        self._configure()

    def set_vbw(self, mode='equal', vbw=0.0) -> None:
        self.dev.state.rta_vbw_mode = mode
        self.dev.state.rta_vbw_hz = float(vbw or 0.0)
        self._configure()

    def set_sweep(self, mode=0, time=0.0) -> None:
        self.dev.state.rta_sweep_time_mode = int(mode)
        self.dev.state.rta_sweep_time = float(time or 0.0)
        self._configure()

    def set_reference(self, mode='manual', ref=None) -> None:
        self.dev.state.rta_ref_mode = mode
        self.dev.reset_auto_reference('rta')
        if mode == 'manual' and ref is not None:
            self.dev.state.rta_ref_level = float(ref)
            self._configure()

    def _configure(self) -> None:
        """Re-apply the profile (the real session's call; the fake just stays ready).

        It still runs the same auto-ref settle hook as `measurements/rta.py::_configure`, so an
        RTA settings change arms the one-shot re-fit in the e2e too (the CI checks would
        otherwise exercise a rule the real session never triggers).
        """
        self._ready = True
        self.dev.begin_auto_reference_settle(self.name)

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
        finite = np.sort(spec[np.isfinite(spec)])
        if finite.size:
            self.dev.observe_reference_peak(
                'rta', float(finite[-1]), float(finite[int((finite.size - 1) * 0.3)]))
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


class FakeVsaSession(_FakeRtaBase):
    """Vector session for the fake backend: a synthetic capture cycle and one RTAF frame.

    It models what the real session reports while a frame transfers (``vsa_busy`` and a
    ``vsa_progress`` that climbs to 1), because the UI has to show that state instead of
    pretending the capture is live. The frame itself is the same synthetic spectrum the fake
    RTA emits.
    """

    name = 'vsa'
    #: Same IQ-domain display as SDR (see measurements/vsa.py).
    auto_ref_scope = 'sdr'
    #: Steps a fake capture takes to "transfer" its frame.
    CAPTURE_STEPS = 3

    def __init__(self, dev):
        super().__init__(dev)
        self._progress = 0
        self._phase = 'idle'

    def enter(self) -> None:
        super().enter()
        self._configure()

    def reconfigure(self) -> None:
        self._configure()

    def reset_defaults(self) -> None:
        self._ready = True

    def set_params(self, center=None, decimate=None, view=None, depth=None, measure=None,
                   modulation=None, symbol_rate=None, rolloff=None, phase_rot=None) -> None:
        s = self.dev.state
        if center is not None:
            s.vsa_center_hz = float(center)
        if decimate is not None:
            s.vsa_decimate = int(decimate)
        if view is not None:
            s.vsa_view = str(view)
        if depth is not None:
            s.vsa_depth = int(depth)
        if measure is not None:
            s.vsa_measure = str(measure)
        if modulation is not None:
            s.vsa_modulation = str(modulation)
        if symbol_rate is not None:
            s.vsa_symbol_rate = float(symbol_rate)
        if rolloff is not None:
            s.vsa_rolloff = float(rolloff)
        if phase_rot is not None:
            s.vsa_phase_rot_deg = float(phase_rot) % 360.0
        self._configure()

    def _configure(self) -> None:
        s = self.dev.state
        self._ready = True
        self._progress = 0
        capture = s.vsa_view == 'capture'
        self._phase = 'capture' if capture else 'stream'
        s.vsa_busy = capture
        s.vsa_progress = 0.0
        s.vsa_actual = {
            'iq_rate': 62.5e6 / max(1, int(s.vsa_decimate)),
            'bandwidth': 50e6 / max(1, int(s.vsa_decimate)),
            'iq_center': float(s.vsa_center_hz),
            'decimate': int(s.vsa_decimate),
            'packet_samples': 16240, 'packet_bytes': 16240 * 4,
            'start': float(s.vsa_center_hz) - 25e6 / max(1, int(s.vsa_decimate)),
            'stop': float(s.vsa_center_hz) + 25e6 / max(1, int(s.vsa_decimate)),
            'depth': int(s.vsa_depth) if capture else 0,
            'packets': (int(s.vsa_depth) + 16239) // 16240 if capture else 0,
        }
        self.dev.begin_auto_reference_settle(self.auto_ref_scope)

    def health(self) -> dict:
        return {'ok': self._tick, 'err': 0, 'last_status': 0, 'transient_streak': 0,
                'phase': self._phase, 'progress': float(self.dev.state.vsa_progress),
                'frames': self._tick, 'recovery_attempts': 0}

    def step(self):
        if not self._ready:
            return [], []
        s = self.dev.state
        if s.vsa_view == 'capture' and self._progress < self.CAPTURE_STEPS:
            self._progress += 1
            s.vsa_progress = self._progress / self.CAPTURE_STEPS
            s.vsa_busy = self._progress < self.CAPTURE_STEPS
            if self._progress < self.CAPTURE_STEPS:
                return [], []
        self._tick += 1
        center = float(s.vsa_center_hz)
        span = float(s.vsa_actual['bandwidth'])
        freq, spec = self._spectrum(center, span, RTA_POINTS)
        finite = np.sort(spec[np.isfinite(spec)])
        if finite.size:
            self.dev.observe_reference_peak(
                self.auto_ref_scope, float(finite[-1]), float(finite[int((finite.size - 1) * 0.3)]))
        s.vsa_progress = 1.0
        s.vsa_busy = False
        s.vsa_last = {'points': RTA_POINTS, 'peak_dbm': float(finite[-1]) if finite.size else 0.0,
                      'floor_dbm': float(finite[int((finite.size - 1) * 0.3)]) if finite.size else 0.0,
                      'mode': s.vsa_view,
                      'samples': int(s.vsa_actual.get('depth') or 16240),
                      'packets': int(s.vsa_actual.get('packets') or 1)}
        frame = encode_rta(s.freq_version, freq, spec, self._wf_row(), 4095,
                           float(s.vsa_actual['start']), float(s.vsa_actual['stop']))
        frames = [frame]
        if s.vsa_view == 'capture' and s.vsa_measure != 'spectrum':
            frames.append(self._vsad_frame(s))
            self._progress = 0                     # arm the next frame like the real session
            s.vsa_busy = True
        return frames, []

    def _vsad_frame(self, s) -> bytes:
        """Synthetic measurement payload: the fake has no capture, only *a* frame to draw.

        It goes through the production encoder and the production payload mapping, so the
        UI smoke exercises the same wire format and the same keys as a real capture would.
        """
        rng = np.random.default_rng(self._tick + 1)
        pts = digital_dsp.nominal_points(s.vsa_modulation)
        rms = 1e-3
        cloud = (np.repeat(pts, 64) * rms
                 + (rng.normal(0.0, rms * 0.03, 256) + 1j * rng.normal(0.0, rms * 0.03, 256)))
        result = {'kind': 'constellation', 'symbols': cloud, 'nominal': pts * rms,
                  'symbol_rate_used': 250e3, 'cfo_hz': 12.5, 'timing_samples': 0.25,
                  'rms_v': rms, 'symbols_n': len(cloud), 'mean_dbm': -30.0,
                  'peak_dbm': -30.0, 'peak_bin_dbm': -30.0, 'peak_hz': 0.0,
                  'floor_dbm': -80.0, 'floor_1hz_dbm': -110.0, 'centroid_hz': 0.0,
                  'duty': 1.0, 'points': RTA_POINTS}
        payload = vector_dsp.frame_payload(result)
        return encode_vsa(s.freq_version, payload['kind'], payload['data'],
                          ideal=payload['ideal'], scalars=payload['scalars'],
                          measurements=payload['measurements'])


class FakeSdrSession(_FakeRtaBase):
    """SDR session emitting a synthetic panadapter (RTAF) plus 20 ms audio frames (AUDF)."""

    name = 'sdr'
    auto_ref_scope = 'sdr'

    def enter(self) -> None:
        super().enter()
        self._configure()

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
        self._configure()

    def set_params(self, center=None, decimate=None) -> None:
        s = self.dev.state
        if center is not None:
            s.sdr_center_hz = float(center)
            s.sdr_listen_hz = float(center)
        if decimate is not None:
            s.sdr_decimate = int(decimate)
        self._configure()

    def _configure(self) -> None:
        """Re-apply the capture (the fake just stays ready) and run the auto-ref settle hook.

        Mirrors `measurements/sdr.py::_configure`: an IQS reconfiguration changes the capture
        geometry, so a centre/bandwidth change arms the one-shot auto-ref re-fit.
        """
        self._ready = True
        self.dev.begin_auto_reference_settle(self.name)

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
        finite = np.sort(spec[np.isfinite(spec)])
        if finite.size:
            self.dev.observe_reference_peak(
                'sdr', float(finite[-1]), float(finite[int((finite.size - 1) * 0.3)]))
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
