"""measurements/vsa.py -- vector signal analysis: IQ capture plus Tier 1 analysis.

The mode answers "what is in this signal" from an IQ capture: spectrum, power versus time,
CCDF, spectrogram and -- with the demodulation chain -- a constellation. See
docs/*/VSA_ROADMAP.md for the staged plan and docs/*/VSA_FEASIBILITY.md for the measured
numbers behind every rule quoted here.

Two acquisition paths, both on the shared stream plumbing (`measurements/iqs.py`):

* ``capture`` (the default): one ``FixedPoints`` frame at the requested depth, analysed
  once and published once. The transfer is roughly real time (measured ratio 1.00-1.01 of
  the signal duration for 2^20..2^24 samples, and up to ~2.5x when the frame is short
  enough to be USB-bandwidth bound), so the session reports progress in ``STATUS.vsa``
  instead of pretending to be live, and arms the next frame as soon as the previous one is
  published.
* The frame is read **packet after packet with no reads before the trigger**: measured, a
  fetch issued while a fixed frame is filling tears the frame down (first fetch inside the
  settle window: 0 samples, ``-10`` forever) and one issued mid-frame loses exactly one
  packet, while back-to-back reads return the whole frame with 0 errors. So ``capture``
  never drains (the Bus trigger start is the flush) while ``stream`` drains as SDR does.
* ``stream``: an ``Adaptive`` stream feeding a Tier 1 spectrum/waterfall at <=20 Hz, reusing
  the RTAF frame and the RTA renderer.

Recovery is a re-arm (stop -> configure -> start) driven by the shared verdict (60
transients or 3 bus timeouts, see iqs.py); after a few failed attempts it raises
``DeviceError`` so the supervisor restarts the worker with a fresh ``Device_Open`` --
the same escalation the SDR session uses.
"""
from __future__ import annotations

import logging
import time

import numpy as np

from ..demod import vector
from ..demod.spectrum import Panadapter
from ..hardware.errors import DeviceError
from .base import MeasurementSession
from .framer import encode_rta
from .iqs import IqsStream, round_decimate

log = logging.getLogger(__name__)


class VsaSession(MeasurementSession):
    name = 'vsa'
    #: VSA's spectrum is an IQ-domain display with the same placement problem SDR solved
    #: (peak plus floor, display-scaled reference), so it drives that tracker.
    auto_ref_scope = 'sdr'

    PAN_FFT = 2048
    PAN_MIN_INTERVAL = 1.0 / 20.0       # panadapter / waterfall ~20 fps
    #: Failed in-place re-arms before escalating to a worker restart.
    RECOVERY_LIMIT = 3
    #: Trigger-wait window, not a frame-length limit: the probe captured 2^24 samples (4.3 s
    #: of signal, ratio 1.00) with 2000 ms and the stream uses SDR's measured 250 ms.
    CAPTURE_BUS_TIMEOUT_MS = 2000
    STREAM_BUS_TIMEOUT_MS = 250

    def __init__(self, dev, *, iqs: IqsStream | None = None, sdk=None):
        super().__init__(dev)
        self._lock = dev._hw
        self._ready = False
        self._pan = Panadapter(self.PAN_FFT)
        self.iqs = iqs if iqs is not None else IqsStream(dev, sdk=sdk)
        self._phase = 'idle'            # idle | settle | capture | stream
        self._chunks: list[np.ndarray] = []
        self._samples = 0
        self._stop = False
        self._last_pan = 0.0
        self._error_streak = 0
        self._recovery_attempts = 0
        self._frames = 0

    # ---------------- lifecycle ----------------
    def enter(self):
        """Snapshot the standard config, then arm the first capture."""
        super().enter()
        prepare = getattr(self.dev, 'prepare_auto_reference_retune', None)
        if prepare is not None:
            prepare(self.auto_ref_scope)
        self._configure()

    def exit(self):
        with self._lock:
            self._ready = False
            try:
                self.iqs.stop(required=False)
            except Exception:
                log.exception('VSA trigger stop failed during exit')
            # A single SWP_Configuration after IQS does not take effect (SWP_GetFullSweep
            # then returns -9); a mode reset first makes the restored configuration work.
            # Bench-verified in the SDR session, same device path here.
            try:
                self.iqs.mode_reset()
            except Exception:
                log.exception('VSA mode reset failed during exit')
        super().exit()

    def request_stop(self) -> None:
        self._stop = True

    def is_ready(self) -> bool:
        return self._ready

    # ---------------- configuration ----------------
    def _configure(self, *, recovery: bool = False) -> None:
        with self._lock:
            s = self.dev.state
            s.vsa_decimate = round_decimate(s.vsa_decimate)
            self._ready = False
            self._stop = False
            self._chunks = []
            self._samples = 0
            self.iqs.stop(required=False)
            capture = s.vsa_view == 'capture'
            # DCC stays at the probe-verified default (high-pass): the measured DC effect is
            # 0.2-1.6 dB and drive-dependent, not a notch to work around.
            p = self.iqs.profile(
                center_hz=s.vsa_center_hz, decimate=s.vsa_decimate,
                ref_level_dbm=s.ref_level,
                trigger_mode='fixed_points' if capture else 'adaptive',
                trigger_length=int(s.vsa_depth) if capture else 0,
                bus_timeout_ms=self.CAPTURE_BUS_TIMEOUT_MS if capture
                else self.STREAM_BUS_TIMEOUT_MS,
                dcc='high_pass', qdc='off', atten=s.atten,
                preamplifier=s.preamplifier, ifgain=s.ifgain,
                gain_strategy=s.gain_strategy)
            info = self.iqs.configure(p, mode_reset=True)
            self.iqs.start()
            half = info.bandwidth / 2.0
            s.vsa_actual = dict(
                iq_rate=info.fs, bandwidth=info.bandwidth, iq_center=info.center_hz,
                decimate=info.decimate, packet_samples=info.packet_samples,
                packet_bytes=info.packet_bytes,
                start=s.vsa_center_hz - half, stop=s.vsa_center_hz + half,
                depth=int(s.vsa_depth) if capture else 0,
                packets=info.packet_count if capture else 0,
            )
            s.vsa_busy = capture
            s.vsa_progress = 0.0
            s.config_version += 1
            s.freq_version += 1
            s.last_error = ''
            self._pan.reset()
            self._phase = 'settle'
            self._ready = True
            if not recovery:
                self._recovery_attempts = 0
        self.dev.begin_auto_reference_settle(self.auto_ref_scope)

    def reconfigure(self) -> None:
        """Re-apply the current geometry (mode entry, settings change, link restore)."""
        self._configure()

    def set_params(self, center=None, decimate=None, view=None, depth=None, measure=None,
                   modulation=None, symbol_rate=None, rolloff=None, phase_rot=None):
        """Apply the VSA parameters; anything given re-arms the acquisition."""
        s = self.dev.state
        if center is not None:
            s.vsa_center_hz = float(center)
        if decimate is not None:
            s.vsa_decimate = round_decimate(decimate)
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

    # ---------------- acquisition ----------------
    def acquisition_timeout(self) -> float:
        """A deep capture transfers for about as long as the signal lasts."""
        s = self.dev.state
        if s.vsa_view != 'capture':
            return 5.0
        rate = float(s.vsa_actual.get('iq_rate', 0.0) or 0.0)
        if rate <= 0:
            return 30.0
        return max(10.0, 2.5 * float(s.vsa_depth) / rate + 5.0)

    def pacing(self, dt: float, produced: bool) -> float:
        """The IQS fetch paces the loop (same reasoning as the SDR session)."""
        return 0.0 if produced else 0.002

    def health(self) -> dict:
        s = self.dev.state
        return {
            'ok': self.iqs.packets_ok, 'err': self.iqs.packets_err,
            'last_status': self.iqs.last_status,
            'transient_streak': self.iqs.transient_streak,
            'phase': self._phase, 'progress': s.vsa_progress, 'frames': self._frames,
            'recovery_attempts': self._recovery_attempts,
        }

    def step(self):
        s = self.dev.state
        if not self._ready or self._stop:
            return [], []
        with self._lock:
            if not self._ready or self._stop:
                return [], []
            now = time.monotonic()
            if s.vsa_view == 'capture':
                # No settle drain here on purpose: reading a fixed frame while it is filling
                # destroys it (measured). The Bus trigger start already flushed the old
                # geometry, so the first fetch waits for the new frame's first packet.
                return self._step_capture(s, now)
            if self.iqs.settling(now):
                self._handle(s, self.iqs.fetch(now), now)
                return [], []
            return self._step_stream(s, now)

    def _handle(self, s, got, now) -> bool:
        """Shared fetch handling: returns True when the packet is usable."""
        if got.ok:
            s.status_warning = 0
            self._error_streak = 0
            return True
        if got.error:
            self._step_failed('get exception', got.error)
            return False
        if got.transient:
            if got.warn:
                s.status_warning = int(got.status)
            if got.recover:
                self._rearm(got.status)
            return False
        self._step_failed('get', got.status)
        return False

    def _step_capture(self, s, now):
        """Accumulate one frame, then analyse and publish it, then arm the next."""
        got = self.iqs.fetch(now)
        if not self._handle(s, got, now):
            return [], []
        if got.samples:
            self._chunks.append(got.raw)
            self._samples += got.samples
        depth = int(s.vsa_depth)
        s.vsa_progress = min(1.0, self._samples / depth) if depth else 0.0
        if self._samples < depth:
            return [], []
        packets = len(self._chunks)
        raw = np.concatenate(self._chunks) if packets > 1 else self._chunks[0]
        self._chunks = []
        self._samples = 0
        s.vsa_busy = False
        s.vsa_progress = 1.0
        self._phase = 'capture'
        frames = self._analyse(s, raw, packets=packets)
        self._frames += 1
        # Arm the next frame: a capture-then-analyse mode refreshes frame by frame rather
        # than pretending to be a gapless stream (the transfer is about real time).
        self._configure(recovery=True)
        return frames, []

    def _step_stream(self, s, now):
        got = self.iqs.fetch(now)
        if not self._handle(s, got, now):
            return [], []
        if not got.samples:
            return [], []
        if now - self._last_pan < self.PAN_MIN_INTERVAL:
            return [], []
        self._last_pan = now
        self._phase = 'stream'
        return self._analyse(s, got.raw, accumulate=True), []

    def _analyse(self, s, raw, accumulate: bool = False, packets: int = 0):
        """Tier 1 analysis of the block; publishes one RTAF frame (the shared renderer).

        The capture path runs the measurement `SET_VSA measure` selects through the Tier 1
        module (`demod/vector.py`), which always includes the Welch spectrum: that spectrum
        is the RTAF frame and the reference tracker's input, so it is paid for once. The
        streaming path stays on the shared panadapter, which is what the SDR display draws
        and what fits the 20 Hz budget -- a spectrogram (120 %) or a constellation (300 %+)
        needs the whole capture, so those are capture-only (VSA_ROADMAP.md 2.3).
        """
        fs = float(s.vsa_actual['iq_rate'])
        volts = IqsStream.to_volts(raw, self.iqs.scale_to_v)
        if accumulate:
            res = self._pan.process(volts.real, volts.imag, fs,
                                    float(s.vsa_actual['iq_center']), 1.0,
                                    bandwidth=float(s.vsa_actual['bandwidth']))
            if res is None:
                return []
            freq, spec, row = res
            extra = {'kind': 'spectrum'}
        else:
            measured = vector.measure(volts, fs, kind=s.vsa_measure,
                                      symbol_rate=s.vsa_symbol_rate or None,
                                      rolloff=s.vsa_rolloff, modulation=s.vsa_modulation)
            freq = measured['spectrum'][0] + float(s.vsa_actual['iq_center'])
            spec = measured['spectrum'][1]
            keep = np.abs(freq - s.vsa_actual['iq_center']) <= s.vsa_actual['bandwidth'] / 2.0
            freq, spec = freq[keep], spec[keep]
            row = self._pan.waterfall_row(spec)
            extra = vector.summary(measured)
            extra.pop('peak_dbm', None)          # recomputed below on the display window
            extra.pop('floor_dbm', None)
            extra.pop('peak_hz', None)
        peak_bin = float(np.max(spec))
        # The capture's trace is the Tier 1 Welch spectrum (power *per bin*), so the
        # strongest tone reads as its main-lobe integral; the streaming trace is the
        # shared panadapter (equivalent-sinusoid amplitude, SDR's convention), where the
        # peak bin already is the tone's level. Both end up as "the tone's level".
        peak = peak_bin if accumulate else vector.tone_level_dbm(spec)
        floor_index = int((spec.size - 1) * 0.3)
        floor = float(np.partition(spec, floor_index)[floor_index])
        self.dev.observe_reference_peak(self.auto_ref_scope, peak, floor)
        s.vsa_last = {**extra, 'points': int(spec.size), 'peak_dbm': peak,
                      'peak_bin_dbm': peak_bin, 'floor_dbm': floor,
                      'peak_hz': float(freq[int(np.argmax(spec))]), 'mode': s.vsa_view,
                      # Evidence of "no shortfall": what the frame actually delivered
                      # (raw holds interleaved int16 I/Q, so two values per IQ sample).
                      'samples': int(raw.size // 2) if not accumulate else int(raw.size),
                      'packets': packets or None}
        return [encode_rta(s.freq_version, freq, spec.astype(np.float32), row, 65535,
                           float(s.vsa_actual['start']), float(s.vsa_actual['stop']))]

    # ---------------- recovery ----------------
    def _rearm(self, reason) -> None:
        """In-place recovery after the shared verdict; escalates like the SDR session."""
        self.iqs.note_recovery()
        self._recovery_attempts += 1
        if self._recovery_attempts > self.RECOVERY_LIMIT:
            raise DeviceError('VSA stream unrecoverable; restarting worker')
        log.warning('VSA stream stalled (%s); re-arming %d/%d',
                    reason, self._recovery_attempts, self.RECOVERY_LIMIT)
        try:
            self._configure(recovery=True)
        except DeviceError:
            raise
        except Exception as exc:
            log.warning('VSA re-arm failed: %r', exc)

    def _step_failed(self, stage: str, status) -> None:
        self._error_streak += 1
        if self._error_streak < 8:
            return
        now = time.monotonic()
        if now - self.iqs.last_recovery < 1.0:
            return
        message = f'VSA {stage} failed repeatedly (status={status})'
        self.dev.state.last_error = message
        if self._recovery_attempts >= 2:
            raise DeviceError(message)
        self._recovery_attempts += 1
        self.iqs.note_recovery(now)
        log.warning('%s; re-arm attempt %d/2', message, self._recovery_attempts)
        try:
            self._configure(recovery=True)
        except Exception as exc:
            raise DeviceError(f'VSA recovery configuration failed: {exc}') from exc
