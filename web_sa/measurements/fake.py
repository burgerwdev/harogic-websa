"""Fake measurement sessions: synthetic RTA and SDR frames without the vendor library.

Used together with `hardware/fake_device.py` so the UI regression can run in CI (finding A1).
The swept path needs no fake session: `StdSession` only calls `dev.fetch_sweep()`, which the
fake device implements.
"""
from __future__ import annotations

import os
from pathlib import Path

import numpy as np

from .base import MeasurementSession
from .framer import BASEBAND_VERSION, encode_audio, encode_baseband, encode_rta
from .results import pnm_payload

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
#: Mirror of `sdr.SDR_MAX_CAPTURE_DECIMATE` (the fake cannot import the real session without
#: libhtraapi): a narrower *requested* capture is floored here, so the fake publishes the same
#: request-vs-capture geometry the hardware session does.
FAKE_MAX_CAPTURE_DECIMATE = 128
SDR_AUDIO_RATE = 48000
SDR_AUDIO_SAMPLES = 960          # 20 ms at 48 kHz, what the real SDR emits
AUDIO_TONE_HZ = 1000.0
#: Rate of the channelized baseband the fake publishes: what the real backend's DDC produces, and
#: low enough that the whole demodulator path is exercised at its real rate.
SDR_BASEBAND_RATE = 48_000.0
#: One baseband block per acquisition step (85 ms at 48 kHz).
SDR_BASEBAND_SAMPLES = 4096
#: The modulation the synthetic baseband carries: a carrier at DC (where the DDC puts the tuned
#: channel) with an AM envelope, so the analog demodulators and the audio chain have something real
#: to work on (a bare carrier has no envelope to detect).
SDR_BASEBAND_MOD_HZ = 1.0e3
#: The Morse the fake CW session keys (a fixed message so an e2e can assert the decoded text) and
#: the speed it is sent at.
CW_MESSAGE = 'TEST DE N0CALL'
CW_WPM = 20
#: The sidetone pitch of the fake carrier, above the dial. The CW demodulator selects its band at
#: this frequency. A carrier at the dial (zero beat) is not demodulated, because the DC cancellation
#: of the receiver operates there.
#:
#: `WEBSA_FAKE_CW_OFFSET_HZ` moves the carrier from this pitch. An e2e test uses this variable to
#: examine the frequency tolerance of the decoder. The operator's Pitch stays at CW_PITCH_HZ, and the
#: signal arrives a few hundred Hz away. The old decoder decoded nothing in this condition. A phone
#: application decoded the same audio correctly.
CW_PITCH_HZ = 700.0
CW_OFFSET_HZ = float(os.environ.get('WEBSA_FAKE_CW_OFFSET_HZ') or 0.0)
_CW_MORSE = {
    'A': '.-', 'B': '-...', 'C': '-.-.', 'D': '-..', 'E': '.', 'F': '..-.', 'G': '--.',
    'H': '....', 'I': '..', 'J': '.---', 'K': '-.-', 'L': '.-..', 'M': '--', 'N': '-.',
    'O': '---', 'P': '.--.', 'Q': '--.-', 'R': '.-.', 'S': '...', 'T': '-', 'U': '..-',
    'V': '...-', 'W': '.--', 'X': '-..-', 'Y': '-.--', 'Z': '--..',
    '0': '-----', '1': '.----', '2': '..---', '3': '...--', '4': '....-',
    '5': '.....', '6': '-....', '7': '--...', '8': '---..', '9': '----.',
}


def cw_keying(text: str, wpm: float, rate: float) -> np.ndarray:
    """A 0/1 keying envelope for `text` at `wpm`, with the ITU element ratios (1/3 dots, 1-dot
    element gaps, 3-dot character gaps, 7-dot word gaps) and a 10-dot silence between repeats."""
    dot = int(rate * 1.2 / max(1.0, wpm))
    runs: list[tuple[int, int]] = []          # (key, samples)
    words = text.split()
    for wi, word in enumerate(words):
        for ci, ch in enumerate(word):
            code = _CW_MORSE.get(ch.upper(), '')
            for si, sym in enumerate(code):
                runs.append((1, dot * (3 if sym == '-' else 1)))
                if si < len(code) - 1:
                    runs.append((0, dot))
            if ci < len(word) - 1:
                runs.append((0, dot * 3))
        if wi < len(words) - 1:
            runs.append((0, dot * 7))
    runs.append((0, dot * 10))
    key = np.concatenate([np.full(n, k, dtype=np.float32) for k, n in runs])
    return key


class _FakeRtaBase(MeasurementSession):
    """Shared synthetic frame generation for the fake RTA/SDR sessions."""

    auto_ref_scope = 'rta'

    def __init__(self, dev):
        super().__init__(dev)
        self._ready = True
        self._tick = 0
        self._rng = np.random.default_rng(7)
        self._audio_seq = 0
        self._audio_pos = 0
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
        t = (np.arange(samples) + self._audio_pos) / SDR_AUDIO_RATE
        self._audio_pos += samples
        pcm = (0.2 * np.sin(2 * np.pi * AUDIO_TONE_HZ * t) * 32767).astype(np.int16)
        self._audio_seq += 1
        return encode_audio(self._audio_seq, SDR_AUDIO_RATE, pcm)

    def _baseband(self, rate: float, center_hz: float) -> bytes:
        """One channelized baseband block, interleaved complex float32, as the DDC produces it.

        For a digital mode this replays the committed FT8 fixture (the fixture *is* 48 kHz baseband),
        so the whole browser path — decoder, UI readout — runs against a real transmission.
        """
        if str(self.dev.state.sdr_demod) == 'ft8':
            return self._ft8_baseband()
        if str(self.dev.state.sdr_demod) == 'cw':
            return self._cw_baseband(rate, center_hz)
        n = SDR_BASEBAND_SAMPLES
        t = (np.arange(n) + self._tick * n) / max(1.0, rate)
        envelope = 0.25 * (1.0 + 0.5 * np.cos(2 * np.pi * SDR_BASEBAND_MOD_HZ * t))
        block = np.empty(n * 2, dtype=np.float32)
        block[0::2] = envelope          # the carrier sits at DC after the channelizer's mix
        block[1::2] = 0.0
        self._iq_seq = (self._iq_seq % 0xFFFFFFFF) + 1
        return encode_baseband(BASEBAND_VERSION, self._iq_seq, rate, center_hz, block)


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


class FakePnmSession(MeasurementSession):
    """Phase noise without the vendor DLL: a synthetic 1/f curve, published in a few partials.

    The fake backend exists so the UI can be exercised with no hardware, and PNM was the one session
    it could not enter: `PhaseNoiseSession._configure` talks to the DLL through `dev.dev`, which
    `FakeDevice` does not have, so every UI path that started phase noise raised an AttributeError
    that the client showed as "Device: command failed" (found by the e2e once it listened for
    dialogs; RTA and SDR already had fake sessions, harmonic happens to only use device methods).
    """

    name = 'pnm'

    def __init__(self, dev):
        super().__init__(dev)
        self.center = 1e9
        self.threshold = -50.0
        self.traceavg = 4
        self.start_offset = 100.0
        self.stop_offset = 10e6
        self._ready = True
        self._partials = 4
        self._i = 0

    # lifecycle protocol the command layer relies on
    def request_stop(self) -> None:
        self._ready = False

    def is_ready(self) -> bool:
        return bool(self._ready)

    def acquisition_timeout(self) -> float:
        return 5.0

    def health(self) -> dict:
        return {'error_streak': 0, 'recovery_attempts': 0}

    def enter(self) -> None:
        super().enter()
        self._configure()

    def reconfigure(self) -> None:
        self._configure()

    def exit(self) -> None:
        self._i = 0
        super().exit()

    def set_params(self, center=None, threshold=None, traceavg=None,
                   start=None, stop=None) -> None:
        """Same knobs as the real session, so SET_PNM validates and the curve follows them."""
        caps = self.dev.state.caps
        if center is not None:
            self.center = float(max(caps.freq_min_hz, min(caps.freq_max_hz, center)))
        if threshold is not None:
            self.threshold = float(threshold)
        if traceavg is not None:
            self.traceavg = int(max(1, min(1000, traceavg)))
        if start is not None:
            self.start_offset = float(max(1.0, min(9e6, start)))
        if stop is not None:
            self.stop_offset = float(max(10.0, min(1e7, stop)))

    def _configure(self) -> None:
        # The real session reconfigures a vendor profile here; the fake has nothing to do but
        # restart the sweep, which is what a reconfiguration means for the display.
        self._i = 0

    def _curve(self, n: int) -> tuple[list[float], list[float]]:
        """A plausible phase-noise trace: the carrier at 0 dBc and about -30 dBc/Hz at 100 kHz."""
        offset = np.geomspace(max(1.0, self.start_offset), max(10.0, self.stop_offset), n)
        pn = (-95.0 - 10.0 * np.log10(offset / 1000.0)
              + 0.4 * np.random.default_rng(11).normal(0.0, 1.0, n))
        return [float(x) for x in offset], [float(x) for x in pn]

    def step(self):
        if not self._ready:
            return [], []
        self._i += 1
        done = self._i >= self._partials
        offset, pn = self._curve(64)
        res = pnm_payload(
            carrier_freq=self.center,
            carrier_power=float(self.dev.state.ref_level) - 6.0,
            offset=offset, pn=pn, ref=float(self.threshold),
            traceavg=float(self.traceavg), done=done,
            progress=round(self._i / self._partials * 100),
        )
        if done:
            self.dev.state.pnm_last = res
            self._i = 0
        return [], [res]


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

    def _ft8_baseband(self) -> bytes:
        if FakeSdrSession._ft8_i is None:
            raw = np.frombuffer(FT8_FIXTURE.read_bytes(), dtype='<f4').reshape(-1, 2)
            # The fixture is baseband at unit scale, which is the scale the DDC output arrives in.
            FakeSdrSession._ft8_i = raw[:, 0].astype(np.float32)
            FakeSdrSession._ft8_q = raw[:, 1].astype(np.float32)
        i = FakeSdrSession._ft8_i
        q = FakeSdrSession._ft8_q
        slot = int(FT8_IQ_RATE * FT8_SLOT_SECONDS)
        start = FakeSdrSession._ft8_pos
        position = (np.arange(SDR_BASEBAND_SAMPLES) + start) % slot
        inside = position < i.size                       # the transmission occupies the slot's head
        index = np.where(inside, position % i.size, 0)
        block = np.empty(SDR_BASEBAND_SAMPLES * 2, dtype=np.float32)
        block[0::2] = np.where(inside, i[index], 0.0)
        block[1::2] = np.where(inside, q[index], 0.0)
        FakeSdrSession._ft8_pos = (start + SDR_BASEBAND_SAMPLES) % slot
        self._iq_seq = (self._iq_seq % 0xFFFFFFFF) + 1
        return encode_baseband(BASEBAND_VERSION, self._iq_seq, FT8_IQ_RATE, 100.2e6, block)

    #: The keying envelope (built once; the samples are the fixture, like the FT8 one).
    _cw_key: np.ndarray | None = None
    _cw_pos = 0

    def _cw_baseband(self, rate: float, center_hz: float) -> bytes:
        """A keyed carrier at the sidetone pitch, or at this pitch plus `WEBSA_FAKE_CW_OFFSET_HZ`.
        The CW demodulator sends this signal to the decoder."""
        if FakeSdrSession._cw_key is None:
            FakeSdrSession._cw_key = cw_keying(CW_MESSAGE, CW_WPM, rate)
        key = FakeSdrSession._cw_key
        start = FakeSdrSession._cw_pos
        index = (np.arange(SDR_BASEBAND_SAMPLES) + start) % key.size
        t = (np.arange(SDR_BASEBAND_SAMPLES) + start) / rate
        carrier = np.exp(2j * np.pi * (CW_PITCH_HZ + CW_OFFSET_HZ) * t)
        block = np.empty(SDR_BASEBAND_SAMPLES * 2, dtype=np.float32)
        block[0::2] = (key[index] * carrier).real
        block[1::2] = (key[index] * carrier).imag
        FakeSdrSession._cw_pos = (start + SDR_BASEBAND_SAMPLES) % key.size
        self._iq_seq = (self._iq_seq % 0xFFFFFFFF) + 1
        return encode_baseband(BASEBAND_VERSION, self._iq_seq, rate, center_hz, block)

    def pacing(self, dt: float, produced: bool) -> float:
        """Pace the synthetic stream to the rate it declares.

        The generic loop runs at ~250 Hz, and one step carries 85 ms of baseband: the browser then
        received about 20x real time, its playback buffer pinned at the ceiling and every audio
        assertion measured the overflow path instead of playback. A real analyzer paces itself by
        the packet (see `SdrSession.pacing`), so the fake does the same with the block it produces.
        """
        block_seconds = SDR_BASEBAND_SAMPLES / max(1.0, SDR_BASEBAND_RATE)
        return max(0.0, block_seconds - dt)

    def step(self):
        if not self._ready:
            return [], []
        self._tick += 1
        s = self.dev.state
        center = float(s.sdr_center_hz)
        # 0.8 * 62.5 MHz / decimate, as the vendor IQS reports it: the requested capture, which
        # is the DISPLAY window even when the device capture is floored (FAKE_MAX_CAPTURE_DECIMATE).
        bandwidth = 48_000.0 if str(s.sdr_demod) in DIGITAL_DEMODS else 50e6 / max(1, int(s.sdr_decimate or 16))
        freq, spec = self._spectrum(center, bandwidth, SDR_PAN_POINTS)
        finite = np.sort(spec[np.isfinite(spec)])
        if finite.size:
            self.dev.observe_reference_peak(
                'sdr', float(finite[-1]), float(finite[int((finite.size - 1) * 0.3)]))
        digital = str(s.sdr_demod) in DIGITAL_DEMODS
        requested = max(1, int(s.sdr_decimate or 16))
        capture_decimate = min(requested, FAKE_MAX_CAPTURE_DECIMATE)
        capture_bandwidth = 48_000.0 if digital else 50e6 / capture_decimate
        s.sdr_actual = {
            'iq_rate': FT8_IQ_RATE if digital else 62.5e6 / capture_decimate,
            # The rate of the channelized baseband this session actually publishes.
            'ddc_rate': FT8_IQ_RATE if digital else SDR_BASEBAND_RATE,
            'bandwidth': bandwidth, 'capture_bandwidth': capture_bandwidth,
            'iq_center': center,
            'decimate': requested, 'capture_decimate': capture_decimate,
            'packet_samples': 16240,
            'packet_bytes': 64960, 'pan_points': SDR_PAN_POINTS, 'center': center,
            'capture_center': center, 'capture_start': center - capture_bandwidth / 2,
            'capture_stop': center + capture_bandwidth / 2, 'start': center - bandwidth / 2,
            'stop': center + bandwidth / 2, 'atten': s.atten, 'preamp': s.preamplifier,
            'ifgain': s.ifgain, 'ref_clock_source': 0, 'refclk_out': s.refclk_out,
            'listen': float(s.sdr_listen_hz), 'demod': s.sdr_demod,
            'if_bw': float(s.sdr_if_bw), 'ddc_offset': 0.0, 'mix_offset': 0.0,
            'ddc_decimate': 81, 'ddc_delay': 102, 'ddc_batch': 1,
            'audio_rate': SDR_AUDIO_RATE, 'deemph_us': float(s.sdr_deemph_us),
        }
        s.sdr_level_dbfs = -60.0
        s.sdr_squelch_open = True
        pan = encode_rta(s.freq_version, freq, spec, self._wf_row(), 4095,
                         center - bandwidth / 2, center + bandwidth / 2)
        frames = [pan]
        if getattr(self.dev, 'iq_clients', 0):
            frames.append(self._baseband(s.sdr_actual['ddc_rate'], center))
        # The audio that belongs to this block's worth of time (the stream is paced to the baseband,
        # so one 20 ms frame per step would be four times too slow).
        block_seconds = SDR_BASEBAND_SAMPLES / max(1.0, SDR_BASEBAND_RATE)
        for _ in range(max(1, int(round(block_seconds * SDR_AUDIO_RATE / SDR_AUDIO_SAMPLES)))):
            frames.append(self._audio())
        return frames, []
