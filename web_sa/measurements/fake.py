"""Fake measurement sessions: synthetic RTA and SDR frames without the vendor library.

Used together with `hardware/fake_device.py` so the UI regression can run in CI (finding A1).
The swept path needs no fake session: `StdSession` only calls `dev.fetch_sweep()`, which the
fake device implements.
"""
from __future__ import annotations

from pathlib import Path

import numpy as np

from .base import MeasurementSession
from .framer import IQ_VERSION, encode_audio, encode_iq, encode_rta

#: The committed FT8 fixture, replayed when the demod is a digital protocol. The fake backend is the
#: only place a protocol waveform can come from in CI, and using the real fixture keeps the browser
#: path honest: the decoder sees an actual FT8 transmission, not a second encoder's output.
FT8_FIXTURE = Path(__file__).resolve().parents[2] / 'tests' / 'fixtures' / 'ft8' / 'ft8_cq_iq.bin'
#: Rate the fixture is replayed at (it is 48 kHz baseband): low enough that a 12.64 s transmission
#: arrives quickly in a test, and a rate the analyzer can legitimately be configured for.
FT8_IQ_RATE = 48_000.0
#: The stream is SLOT-shaped: one 12.64 s transmission followed by the rest of the 15 s slot as
#: silence, repeating. A real FT8 band looks like this, and the period matters: replaying the
#: transmission back-to-back makes the stream periodic in exactly the amount a decoder discards
#: between attempts, so a mis-aligned window would never sweep into alignment.
FT8_SLOT_SECONDS = 15.0

#: Demod ids the fake treats as digital protocols (they get the protocol fixture instead of a tone).
DIGITAL_DEMODS = ('ft8',)

RTA_POINTS = 1024
RTA_WATERFALL_WIDTH = 128
SDR_PAN_POINTS = 512
SDR_AUDIO_RATE = 48000
SDR_AUDIO_SAMPLES = 960          # 20 ms at 48 kHz, what the real SDR emits
AUDIO_TONE_HZ = 1000.0
#: Synthetic IQ block: the browser DSP's input, so the fake backend exercises the same path
#: as the analyzer (a tone offset from the capture centre, i.e. a signal to tune to).
SDR_IQ_SAMPLES = 4096
#: An amplitude-modulated tone inside the DDC's output band, so the analog demodulators and the
#: audio chain have something real to work on (a bare carrier has no envelope to detect).
SDR_IQ_TONE_HZ = 8.0e3
SDR_IQ_MOD_HZ = 1.0e3


class _FakeRtaBase(MeasurementSession):
    """Shared synthetic frame generation for the fake RTA/SDR sessions."""

    auto_ref_scope = 'rta'

    def __init__(self, dev):
        super().__init__(dev)
        self._ready = True
        self._tick = 0
        self._rng = np.random.default_rng(7)
        self._audio_seq = 0
        self._iq_seq = 0

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

    def _iq(self, rate: float, center_hz: float) -> bytes:
        """One IQ block, interleaved int16, as the IQS stream delivers it.

        For a digital mode this replays the committed FT8 fixture (at the fixture's rate), so the
        whole browser path — DDC, decoder, UI readout — runs against a real transmission.
        """
        if str(self.dev.state.sdr_demod) == 'ft8':
            return self._ft8_iq()
        n = SDR_IQ_SAMPLES
        t = (np.arange(n) + self._tick * n) / max(1.0, rate)
        ph = 2 * np.pi * SDR_IQ_TONE_HZ * t
        envelope = 0.25 * (1.0 + 0.5 * np.cos(2 * np.pi * SDR_IQ_MOD_HZ * t))
        i = (envelope * np.cos(ph) * 32767).astype(np.int16)
        q = (envelope * np.sin(ph) * 32767).astype(np.int16)
        self._iq_seq = (self._iq_seq % 0xFFFFFFFF) + 1
        return encode_iq(IQ_VERSION, self._iq_seq, rate, center_hz,
                         np.stack([i, q], axis=1).reshape(-1))


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

    # Replay position into the FT8 fixture (looped: the decoder consumes one transmission per
    # decode attempt and then waits for the next).
    _ft8_i: np.ndarray | None = None
    _ft8_q: np.ndarray | None = None
    _ft8_pos = 0

    def _ft8_iq(self) -> bytes:
        if FakeSdrSession._ft8_i is None:
            raw = np.frombuffer(FT8_FIXTURE.read_bytes(), dtype='<f4').reshape(-1, 2)
            scale = 0.25 * 32767.0
            FakeSdrSession._ft8_i = np.clip(raw[:, 0] * scale, -32768, 32767).astype(np.int16)
            FakeSdrSession._ft8_q = np.clip(raw[:, 1] * scale, -32768, 32767).astype(np.int16)
        i = FakeSdrSession._ft8_i
        q = FakeSdrSession._ft8_q
        slot = int(FT8_IQ_RATE * FT8_SLOT_SECONDS)
        start = FakeSdrSession._ft8_pos
        position = (np.arange(SDR_IQ_SAMPLES) + start) % slot
        inside = position < i.size                       # the transmission occupies the slot's head
        index = np.where(inside, position % i.size, 0)
        block = np.empty(SDR_IQ_SAMPLES * 2, dtype=np.int16)
        block[0::2] = np.where(inside, i[index], 0)
        block[1::2] = np.where(inside, q[index], 0)
        FakeSdrSession._ft8_pos = (start + SDR_IQ_SAMPLES) % slot
        self._iq_seq = (self._iq_seq % 0xFFFFFFFF) + 1
        return encode_iq(IQ_VERSION, self._iq_seq, FT8_IQ_RATE, 100.2e6, block)

    def step(self):
        if not self._ready:
            return [], []
        self._tick += 1
        s = self.dev.state
        center = float(s.sdr_center_hz)
        # 0.8 * 62.5 MHz / decimate, as the vendor IQS reports it
        bandwidth = 48_000.0 if str(s.sdr_demod) in DIGITAL_DEMODS else 50e6 / max(1, int(s.sdr_decimate or 16))
        freq, spec = self._spectrum(center, bandwidth, SDR_PAN_POINTS)
        finite = np.sort(spec[np.isfinite(spec)])
        if finite.size:
            self.dev.observe_reference_peak(
                'sdr', float(finite[-1]), float(finite[int((finite.size - 1) * 0.3)]))
        digital = str(s.sdr_demod) in DIGITAL_DEMODS
        s.sdr_actual = {
            'iq_rate': FT8_IQ_RATE if digital else 62.5e6 / max(1, int(s.sdr_decimate or 16)),
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
        frames = [pan]
        if getattr(self.dev, 'iq_clients', 0):
            frames.append(self._iq(s.sdr_actual['iq_rate'], center))
        frames.append(self._audio())
        return frames, []
