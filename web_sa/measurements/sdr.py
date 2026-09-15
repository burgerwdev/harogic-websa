"""
measurements/sdr.py -- SDR receive session (IQS streaming + channelizer + demod).

Data path (verified on SAN-90, see tools/sdr_probe/FINDINGS.md):
    IQS Adaptive IQ stream (int16, 62.5 MSPS / 2**n)
      -> Panadapter FFT            -> RTAF frame (spectrum + waterfall; reuses the
                                      existing RTA renderer on the frontend)
      -> DSP_DDC (offset + decimate)-> complex float narrowband
      -> AnalogDemod               -> AUDF frame (48 kHz int16 PCM)
      -> ADM_* (AM/FM only)        -> signal metrics in STATUS.sdr.adm
"""
from __future__ import annotations

import logging
import os
import time
from ctypes import cast as c_cast

import numpy as np

from ..demod import ANALOG_MODES, AnalogDemod, DdcChannel, Panadapter
from ..hardware import sdk_bindings as sb
from ..hardware.device import DeviceError
from .base import MeasurementSession
from .framer import encode_audio, encode_rta
from .iqs import BUS_RETRY_TRIES, IqsStream, sdk_call
from .iqs import round_decimate as _round_decimate

log = logging.getLogger(__name__)

# The IQS stream plumbing (profile, mode reset, post-configuration drain, one-packet fetch,
# the transient/fatal split and the "is the stream wedged?" verdict) lives in iqs.py, so the
# vector session reuses the same measured rules instead of re-deriving them. SDR keeps the
# DSP side: the vendor FFT, the DDC/demod chain, and its own recovery action.

ADM_ENABLED = os.getenv('WEBSA_SDR_ADM', '1').lower() not in ('0', 'false', 'no', 'off')

# Display-spectrum FFT. Set WEBSA_SDR_FFT=0 to fall back to the NumPy panadapter: the vendor
# FFT takes a hand-built IQStream whose metadata must match the buffer exactly, and a
# mismatch lets it overrun an internal buffer (corrupting the process heap).
VFFT_ENABLED = os.getenv('WEBSA_SDR_FFT', '1').lower() not in ('0', 'false', 'no', 'off')

# Staged tracing for native-crash diagnosis. Enable with WEBSA_TRACE=1. It walks the SDR
# configuration pipeline step by step so that the last line before a native abort names
# the offending SDK call. Per-frame calls use _tn(), which logs only the first few hits.
_TRACE = os.getenv('WEBSA_TRACE', '').lower() not in ('', '0', 'false', 'no', 'off')
_trace_seen: dict[str, int] = {}



def _t(msg: str, *args) -> None:
    if _TRACE:
        log.info('[trace] ' + msg, *args)


def _tn(key: str, msg: str, *args, n: int = 3) -> None:
    """Trace a hot-path milestone at most ``n`` times per key."""
    if not _TRACE:
        return
    hit = _trace_seen.get(key, 0)
    if hit < n:
        _trace_seen[key] = hit + 1
        log.info('[trace] ' + msg, *args)




def sdr_spectrum_windows(center_hz: float, capture_center_hz: float,
                         bandwidth: float) -> dict:
    """Display window (what the user asked for) and capture window (where the hardware is).

    The IQS capture is centred on ``capture_center_hz``, which normally equals the requested
    centre; it can still differ when the band edge clamps the capture. Publishing both ranges
    lets the front end map the bins by their true capture frequency and clip them to the
    display window, so the user's centre lands at the canvas centre and any uncovered edge is
    a real gap (never invented data). ``center`` is the display centre because every
    frequency the UI shows (freq axis, markers, limit window) must come from one source.
    """
    half = float(bandwidth) / 2.0
    return {
        'center': float(center_hz),
        'capture_center': float(capture_center_hz),
        'capture_start': float(capture_center_hz) - half,
        'capture_stop': float(capture_center_hz) + half,
        'start': float(center_hz) - half,
        'stop': float(center_hz) + half,
    }


class SdrSession(MeasurementSession):
    name = 'sdr'
    #: SDR drives its own reference tracker (its level is the IQS level, not a swept profile).
    auto_ref_scope = 'sdr'

    PAN_FFT = 2048
    PAN_MIN_INTERVAL = 1.0 / 20.0      # panadapter/waterfall ~20 fps
    AUDIO_RATE = 48000
    AUDIO_FRAME = 960                  # 20 ms
    ADM_MIN_INTERVAL = 1.0             # metrics update (matches the 1 s STATUS cadence)

    def __init__(self, dev):
        super().__init__(dev)
        self._lock = dev._hw
        self._ready = False
        self._pan = Panadapter(self.PAN_FFT)
        self._ddc = DdcChannel(dev.dsp)
        self._demod = AnalogDemod(self.AUDIO_RATE)
        self._adm = sb.c_void_p()
        self._adm_ok = False
        self._audio_buf = np.zeros(0, dtype=np.float32)
        self._audio_seq = 0
        self._audio_reset_pending = False
        self._ddc_batch = 1
        self._iqs_center_hz = 0.0
        # IQS stream state (packet geometry, counters, streaks, settle window) lives in the
        # shared layer; the accessors below keep this module, its health() and the bench
        # probe reading it under the same names as before.
        self.iqs = IqsStream(dev)
        self._vfft_ready = False
        self._vfft_points = 0
        self._vfft_freq = None
        self._vfft_power = None
        self._vfft_frame_samples = 0
        self._vfft_buffer = None
        self._vfft_stream = None
        self._last_pan = 0.0
        self._last_adm = 0.0
        self._discard_until = 0.0
        self._fade_start = 0.0
        self._fade_until = 0.0
        self._settle_pending = False
        # Squelch gate state: a bare level>=threshold comparator chattered at ~12 Hz when a
        # signal sat near the threshold (measured on hardware), so the gate has hysteresis,
        # a hold time and a smoothed gain for click-free open/close.
        self._squelch_gain = 1.0
        self._squelch_hold_until = 0.0
        self._mix_phase = 0.0
        self._mix_freq = 0.0
        self._applied_listen = None
        self._error_streak = 0
        self._recovery_attempts = 0

    # ---------------- IQS stream state (owned by measurements/iqs.py) ----------------
    #
    # These names predate the extraction; they are still how this module, its health() and the
    # bench probe read the stream state, but the storage is now the shared IqsStream.

    @property
    def _packet_samples(self) -> int:
        return self.iqs.packet_samples

    @property
    def _scale_to_v(self) -> float:
        return self.iqs.scale_to_v

    @property
    def _ready_at(self) -> float:
        return self.iqs.ready_at

    @_ready_at.setter
    def _ready_at(self, value: float) -> None:
        self.iqs.ready_at = value

    @property
    def _last_ok(self) -> float:
        return self.iqs.last_ok

    @_last_ok.setter
    def _last_ok(self, value: float) -> None:
        self.iqs.last_ok = value

    @property
    def _last_recovery(self) -> float:
        return self.iqs.last_recovery

    @_last_recovery.setter
    def _last_recovery(self, value: float) -> None:
        self.iqs.last_recovery = value

    @property
    def _last_status(self) -> int:
        return self.iqs.last_status

    @_last_status.setter
    def _last_status(self, value: int) -> None:
        self.iqs.last_status = value

    @property
    def _packets_ok(self) -> int:
        return self.iqs.packets_ok

    @_packets_ok.setter
    def _packets_ok(self, value: int) -> None:
        self.iqs.packets_ok = value

    @property
    def _packets_err(self) -> int:
        return self.iqs.packets_err

    @_packets_err.setter
    def _packets_err(self, value: int) -> None:
        self.iqs.packets_err = value

    @property
    def _transient_streak(self) -> int:
        return self.iqs.transient_streak

    @_transient_streak.setter
    def _transient_streak(self, value: int) -> None:
        self.iqs.transient_streak = value

    @property
    def _timeout_streak(self) -> int:
        return self.iqs.timeout_streak

    @_timeout_streak.setter
    def _timeout_streak(self, value: int) -> None:
        self.iqs.timeout_streak = value

    # ---------------- lifecycle ----------------
    def enter(self):
        super().enter()
        _t('enter: IQS configure begin')
        self._configure()
        _t('enter: IQS configure done')

    def exit(self):
        _t('exit: begin')
        with self._lock:
            self._ready = False
            try:
                sb.dll.IQS_BusTriggerStop(sb.pointer(self.dev.dev))
            except Exception:
                log.exception('SDR trigger stop failed during exit')
            _t('exit: trigger stopped, closing ADM')
            self._close_adm_locked()
            # A single SWP_Configuration after IQS does not take effect: SWP_GetFullSweep
            # then returns BusDataError (-9). A mode reset first makes the restored SWP
            # configuration actually work (bench-verified).
            try:
                self._reset_iqs_mode_locked()
            except Exception:
                log.exception('SDR mode reset failed during exit')
        super().exit()

    def _close_adm_locked(self):
        if self._adm.value:
            try:
                _t('adm: ADM_Close begin handle=%s', bool(self._adm.value))
                sb.dll.ADM_Close(sb.pointer(self._adm))
                _t('adm: ADM_Close ok')
            except Exception:
                log.exception('ADM_Close failed')
        self._adm = sb.c_void_p()
        self._adm_ok = False

    # ---------------- configuration ----------------
    def _configure(self):
        with self._lock:
            _t('_configure: begin')
            self._ready = False
            self._configure_iqs_locked()
            _t('_configure: IQS ok, configuring DDC/demod chain')
            self._configure_chain_locked()
            _t('_configure: chain ok, opening ADM')
            self._open_adm_locked()
            _t('_configure: ADM ok, starting trigger')
            self._start_trigger_locked()
            _t('_configure: done (ready)')
            self._last_pan = 0.0
            self._last_adm = 0.0
            self._transient_streak = 0
            self._recovery_attempts = 0
            # Settle window: the device needs a moment after IQS_Configuration; without it
            # the first fetches return BusDataError and would trip the stall recovery.
            self.iqs.arm_settle(delay=0.4)
            self._ready = True
        # Same hook as the swept/RTA paths: an IQS reconfiguration changes the capture geometry,
        # so stale observations are dropped and a settings change (centre / capture bandwidth /
        # IF bandwidth) arms ONE automatic re-fit (see auto_reference.begin_settle).
        self.dev.begin_auto_reference_settle('sdr')

    def _sdk_call(self, fn, what: str, retries: int = BUS_RETRY_TRIES):
        """Run an SDK configuration entry point, retrying the transient bus warnings.

        The device occasionally answers a configuration download with -11 (BusDownLoad)
        or -10 (BusTimeOut); the vendor guide's remedy is simply to call Configuration
        again. Raising on those made normal mode switches escalate to a worker restart.
        The retry loop itself lives in the shared IQS layer.
        """
        return sdk_call(fn, what, retries=retries)

    def _reset_iqs_mode_locked(self):
        """A second IQS_Configuration after the stream has started is rejected and
        permanently wedges the stream; an SWP_Configuration switches the device's mode and
        lets IQS be configured again (bench-verified: this makes reconfiguration safe).
        The sequence itself lives in the shared IQS layer."""
        self.iqs.mode_reset()

    def _stop_trigger_locked(self, *, required: bool = True) -> None:
        self.iqs.stop(required=required)

    def _configure_iqs_locked(self):
        s = self.dev.state
        self._stop_trigger_locked(required=False)
        s.sdr_decimate = _round_decimate(s.sdr_decimate)
        # Profile, mode reset and validation come from the shared IQS layer. DCC/QDC stay at
        # the bench-verified defaults (auto DC offset, QDC off; see tools/sdr_probe/FINDINGS).
        p = self.iqs.profile(center_hz=s.sdr_center_hz, decimate=s.sdr_decimate,
                             ref_level_dbm=s.ref_level, trigger_mode='adaptive',
                             bus_timeout_ms=250, dcc='auto', qdc='off',
                             atten=s.atten, preamplifier=s.preamplifier,
                             ifgain=s.ifgain, gain_strategy=s.gain_strategy)
        _t('iqs: ProfileDeInit ok')
        native_rate = float(p.NativeIQSampleRate_SPS)
        expected_bw = native_rate * 0.8 / s.sdr_decimate if native_rate > 0 else 0.0
        # Capture on the requested centre. The stream used to be tuned 200 kHz away from it
        # ("avoid the zero-IF DC centre"), which pushed the lowest 200 kHz of the requested
        # window out of the capture range and left a visible blank strip at the left edge of
        # the panadapter. A/B on hardware (AM and FM, -20..-75 dBm) showed no demod-quality
        # difference, so the shift was not worth a hole in the display.
        capture_center = float(s.sdr_center_hz)
        if s.caps and expected_bw > 0:
            half = expected_bw / 2.0
            capture_center = max(s.caps.freq_min_hz + half,
                                 min(s.caps.freq_max_hz - half, capture_center))
        p.CenterFreq_Hz = capture_center
        info = self.iqs.configure(p, mode_reset=True)
        _t('iqs: IQS_Configuration ok fs=%s bw=%s pts=%s dec=%s center=%s',
           info.fs, info.bandwidth, info.packet_samples, info.decimate, info.center_hz)
        fs = info.fs
        bandwidth = info.bandwidth
        half = bandwidth / 2.0
        configured_center = info.center_hz
        if configured_center > 0:
            self._iqs_center_hz = configured_center
        s.sdr_listen_hz = max(s.sdr_center_hz - half,
                              min(s.sdr_center_hz + half, float(s.sdr_listen_hz)))
        # One IQS packet per step. Fetching a second packet in the same step (an earlier
        # "high-rate" optimisation) overwrote the vendor's internal packet buffer: the
        # heap damage surfaced later as SIGSEGV / glibc "double free or corruption (out)"
        # during an unrelated numpy free, i.e. a random crash a few seconds after entering
        # SDR and on the next mode switch. Measured A/B: two packets per step crashes on
        # every run, one packet per step is stable over many mode switches. The official
        # examples also call IQS_GetIQStream once per iteration.
        self._ddc_batch = 1
        self._fs_in = fs
        s.sdr_actual = dict(
            iq_rate=fs, bandwidth=bandwidth,
            iq_center=self._iqs_center_hz,
            decimate=info.decimate, packet_samples=info.packet_samples,
            packet_bytes=info.packet_bytes,
            pan_points=min(
                self.PAN_FFT,
                max(2, 2 * int(np.floor(self.PAN_FFT * bandwidth / (2.0 * fs))) + 1),
            ),
            **sdr_spectrum_windows(s.sdr_center_hz, self._iqs_center_hz, bandwidth),
            atten=info.atten, preamp=info.preamp,
            ifgain=info.ifgain,
            ref_clock_source=info.refclk_source,
            refclk_out=info.refclk_out,
        )
        # Re-assert the vendor FFT only when the IQS packet geometry changed (it is the
        # only thing the FFT size depends on).
        self._configure_vendor_fft_locked()
        self.dev._read_amp_atten()
        self.dev.state.config_version += 1
        self.dev.state.freq_version += 1
        self.dev.state.last_error = ''
        self._pan.reset()

    def _configure_vendor_fft_locked(self) -> None:
        """Configure the vendor FFT over one full, contiguous IQ frame.

        The vendor FFT copies ``IQStream.IQS_StreamInfo.PacketSamples`` input samples into
        an internal buffer sized from ``DSP_FFT_TypeDef.SamplePts``. Configuring a small
        FFT while handing it a large IQS packet overflows that buffer and corrupts the
        heap (observed as ``munmap_chunk(): invalid pointer`` / SIGSEGV). Frame size, FFT
        size and stream metadata must therefore stay identical.
        """
        self._vfft_ready = False
        self._vfft_points = 0
        self._vfft_freq = None
        self._vfft_power = None
        self._vfft_frame_samples = 0
        self._vfft_buffer = None
        self._vfft_stream = None
        if not VFFT_ENABLED:
            log.info('SDR vendor FFT disabled (WEBSA_SDR_FFT=%s); using NumPy panadapter'
                     % os.getenv('WEBSA_SDR_FFT'))
            return
        try:
            frame_samples = int(self._packet_samples) * int(self._ddc_batch)
            if frame_samples < 64:
                raise RuntimeError(f'vendor FFT frame too small: {frame_samples}')
            fi = sb.DSP_FFT_TypeDef()
            fo = sb.DSP_FFT_TypeDef()
            points = sb.c_uint32(0)
            rbw_ratio = sb.c_double(0.0)
            sb.dll.DSP_FFT_DeInit(sb.pointer(fi))
            _t('vfft: DeInit ok, configuring size=%s', frame_samples)
            fi.Calibration = 0
            fi.DetectionRatio = 1
            fi.TraceDetector = sb.TraceDetector_TypeDef.TraceDetector_PosPeak
            fi.FFTSize = frame_samples
            fi.SamplePts = frame_samples
            fi.Intercept = 0.8
            fi.WindowType = sb.Window_TypeDef.FlatTop
            status = sb.dll.DSP_FFT_Configuration(
                sb.pointer(self.dev.dsp), sb.pointer(fi), sb.pointer(fo),
                sb.pointer(points), sb.pointer(rbw_ratio))
            if status != 0 or points.value < 2:
                raise RuntimeError(f'DSP_FFT_Configuration status={status}')
            _t('vfft: Configuration ok status=%s points=%s rbw=%s',
               status, points.value, rbw_ratio.value)
            self._vfft_frame_samples = frame_samples
            self._vfft_buffer = np.empty(frame_samples * 2, dtype=np.int16)
            self._vfft_stream = sb.IQStream_TypeDef()
            self._vfft_points = int(points.value)
            self._vfft_freq = np.empty(self._vfft_points, dtype=np.float64)
            self._vfft_power = np.empty(self._vfft_points, dtype=np.float32)
            self._vfft_ready = True
        except Exception as exc:
            log.warning('SDR vendor FFT unavailable; using NumPy fallback: %r', exc)

    def _vendor_spectrum_locked(self, template, frame):
        """Run the vendor FFT over a full contiguous IQ frame; return (freq_hz, dbm).

        ``frame`` must be int16 interleaved IQ of exactly the configured frame length;
        anything else falls back to the NumPy panadapter.
        """
        if not self._vfft_ready or frame is None:
            return None
        if frame.size != self._vfft_frame_samples * 2:
            return None
        try:
            self._vfft_buffer[:] = frame
            s = self.dev.state
            vstream = self._vfft_stream
            vstream.IQS_Profile = template.IQS_Profile
            vstream.IQS_StreamInfo = template.IQS_StreamInfo
            vstream.IQS_ScaleToV = template.IQS_ScaleToV
            vstream.IQS_Profile.CenterFreq_Hz = self._iqs_center_hz
            vstream.IQS_Profile.TriggerLength = self._vfft_frame_samples
            vstream.IQS_StreamInfo.PacketSamples = self._vfft_frame_samples
            vstream.IQS_StreamInfo.StreamSamples = self._vfft_frame_samples
            vstream.IQS_StreamInfo.PacketDataSize = self._vfft_frame_samples * 4
            vstream.IQS_StreamInfo.IQSampleRate = self._fs_in
            vstream.IQS_StreamInfo.Bandwidth = float(
                s.sdr_actual.get('bandwidth', self._fs_in))
            vstream.AlternIQStream = c_cast(
                sb.c_void_p(self._vfft_buffer.ctypes.data), sb.POINTER(sb.c_void_p))
            _tn('fft_iqstospec', 'vfft: IQSToSpectrum in frame=%s',
                self._vfft_frame_samples)
            status = sb.dll.DSP_FFT_IQSToSpectrum(
                sb.pointer(self.dev.dsp), sb.pointer(vstream),
                self._vfft_freq.ctypes.data_as(sb.POINTER(sb.c_double)),
                self._vfft_power.ctypes.data_as(sb.POINTER(sb.c_float)))
            _tn('fft_iqstospec_ok', 'vfft: IQSToSpectrum ok status=%s points=%s',
                status, self._vfft_points)
            if status != 0:
                return None
            power = self._vfft_power
            if not np.all(np.isfinite(power)):
                return None
            freq = self._vfft_freq
            target = max(2, int(s.sdr_actual.get('pan_points', self.PAN_FFT)))
            if freq.size > target:
                # Max-pool into display buckets so narrow carriers survive the decimation.
                starts = (np.arange(target) * freq.size // target).astype(np.int64)
                freq = freq[starts]
                power = np.maximum.reduceat(power, starts)
            return freq.copy(), power.copy()
        except Exception:
            log.exception('SDR vendor FFT frame failed')
            return None

    def _start_trigger_locked(self):
        """Start streaming. Kept separate so the slow DDC configuration can run BEFORE the
        trigger starts (otherwise the device buffer overflows while we are not fetching)."""
        self.iqs.start()
        _t('trigger: BusTriggerStart ok; streaming')
        self._last_ok = time.monotonic()

    def _chain_params(self):
        """IF bandwidth and the DDC decimate it requires. A wider DDC output (2.5x the IF
        bandwidth) gives a larger instant-tuning range for the software NCO, so adjacent
        channels do not need a (slow, stream-disrupting) full reconfiguration."""
        fs_in = self._fs_in or 1.0
        if_bw = float(max(200.0, min(self.dev.state.sdr_if_bw, fs_in * 0.4)))
        # Keep the DDC output (and hence the per-packet DSP) small: at a high capture
        # rate the loop runs every ~4 ms and a large demod block cannot keep up, which
        # showed up as choppy audio. 2.5x still leaves a usable software-tuning range and
        # anything beyond it uses the (safe) full reconfiguration.
        need = max(float(self.AUDIO_RATE), 2.5 * if_bw)
        return if_bw, max(1, min(65536, int(np.floor(fs_in / need))))

    def _chain_coarse(self, fs_out):
        """Coarse DDC offset (a multiple of ~fs_out) plus the residual for the software
        NCO. The DDC passband is +/-fs_out/2, so the residual stays well inside it."""
        s = self.dev.state
        rel = float(s.sdr_listen_hz) - float(self._iqs_center_hz or s.sdr_center_hz)
        # The DDC passband is flat only within ~0.4*fs_out (measured +-0.04 dB up to
        # 0.4*fs_out, -12 dB at 0.45*fs_out), so the coarse grid must keep the residual
        # (and the whole demod channel) well inside it. 0.4 keeps the residual <= 0.2*fs_out.
        grid = max(1000.0, (fs_out or 1.0) * 0.4)
        return rel, round(rel / grid) * grid

    SETTLE_DISCARD = 0.12
    SETTLE_FADE = 0.10
    # Squelch: open at the threshold, close 3 dB lower, keep open for 0.3 s after the last
    # above-threshold block, and ramp the gate gain (5 ms attack / 80 ms release).
    SQUELCH_HYST_DB = 3.0
    SQUELCH_HOLD_S = 0.30
    SQUELCH_ATTACK_S = 0.005
    SQUELCH_RELEASE_S = 0.080

    def _begin_audio_settle(self) -> None:
        """Arm a discard+fade window. It is applied when the first audio actually arrives
        after a reconfiguration (not at configure time), otherwise the settle window would
        elapse during the ~0.4 s acquisition settle and the transient would be audible."""
        self._settle_pending = True
        self._audio_buf = np.zeros(0, dtype=np.float32)
        self._audio_seq = 0
        self._audio_reset_pending = True

    def _configure_chain_locked(self):
        """DDC + demod. The DDC is configured with the coarse offset for the current
        listen frequency; it must run while the trigger is stopped (see _configure)."""
        s = self.dev.state
        fs_in = self._fs_in or 1.0
        if_bw, decimate = self._chain_params()
        fs_out = fs_in / decimate
        rel, coarse = self._chain_coarse(fs_out)
        # DDC output frequency = f_in + offset and its passband is |f_in + offset| <
        # fs_out/2, so passing `rel` needs offset = -coarse (bench-verified sign:
        # offset = center - listen). The residual rel - coarse goes to the software NCO.
        ddc_off = -coarse
        if (not self._ddc._ready) or self._ddc.decimate != decimate \
                or abs(self._ddc.offset_hz - ddc_off) > 1.0:
            # Deep filter design is expensive (~180 ms); run it only while stopped.
            _t('chain: DDC configure fs_in=%s offset=%s decimate=%s points=%s',
               fs_in, ddc_off, decimate, self._packet_samples * self._ddc_batch)
            self._ddc.configure(fs_in, ddc_off, decimate,
                                 self._packet_samples * self._ddc_batch)
            _t('chain: DDC configure ok out=%s fs_out=%s delay=%s',
               self._ddc.out_points, self._ddc.fs_out, self._ddc.delay)
            self._mix_phase = 0.0
        # The vendor FFT geometry depends only on the IQS packet geometry, so it is
        # (re)configured when that changes - not on every DDC offset/decimate change. Doing
        # it on every tuning step churned DSP_FFT_DeInit/Configuration against a live DSP
        # handle and caused native heap corruption ("corrupted size vs. prev_size").
        frame_samples = int(self._packet_samples) * int(self._ddc_batch)
        if (not self._vfft_ready) or self._vfft_frame_samples != frame_samples:
            _t('chain: vendor FFT configure frame_samples=%s', frame_samples)
            self._configure_vendor_fft_locked()
            _t('chain: vendor FFT ready=%s points=%s', self._vfft_ready, self._vfft_points)
        self._mix_freq = rel + self._ddc.offset_hz
        self._applied_listen = float(s.sdr_listen_hz)
        _t('chain: demod configure fs_out=%s mode=%s ifbw=%s',
           self._ddc.fs_out, s.sdr_demod, if_bw)
        _deemph = None if float(getattr(s, 'sdr_deemph_us', -1.0)) < 0 else float(s.sdr_deemph_us)
        self._demod.configure(self._ddc.fs_out, s.sdr_demod, if_bw, pitch=s.sdr_pitch,
                              deemph_us=_deemph)
        _t('chain: demod configure ok')
        self._begin_audio_settle()
        s.sdr_actual.update(
            listen=s.sdr_listen_hz, demod=s.sdr_demod, if_bw=if_bw,
            ddc_offset=self._ddc.offset_hz, mix_offset=self._mix_freq,
            ddc_decimate=self._ddc.decimate, ddc_rate=self._ddc.fs_out,
            ddc_delay=self._ddc.delay,
            ddc_batch=self._ddc_batch,
            audio_rate=self.AUDIO_RATE,
            deemph_us=self._demod.deemph_us,
        )
        half = float(s.sdr_actual.get('bandwidth', fs_in)) / 2.0
        s.sdr_actual.update(sdr_spectrum_windows(
            s.sdr_center_hz, self._iqs_center_hz or s.sdr_center_hz, half * 2.0))

    def _reconfigure_full_locked(self):
        """Full Stop -> Configuration -> DDC config -> Start. A DDC-only reconfiguration
        while streaming wedges the device, so any offset change goes through here."""
        self._configure_iqs_locked()
        self._configure_chain_locked()
        self._start_trigger_locked()
        self._ready_at = time.monotonic() + 0.3

    def _reconfigure_chain_runtime_locked(self) -> None:
        """Rebuild the host DDC/demod chain without allowing the IQS FIFO to overflow."""
        try:
            self._stop_trigger_locked()
            self._configure_chain_locked()
            self._start_trigger_locked()
            self._ready_at = time.monotonic() + 0.15
        except Exception as exc:
            log.warning('SDR runtime chain reconfiguration failed; doing full recovery: %r', exc)
            try:
                self._reconfigure_full_locked()
            except Exception as recovery_exc:
                raise DeviceError(
                    f'SDR runtime chain recovery failed: {recovery_exc}') from recovery_exc

    def _apply_tuning_locked(self) -> None:
        """Called from step(): if the tune moved outside the DDC passband, do a full
        reconfiguration; otherwise only update the cheap software NCO."""
        s = self.dev.state
        if not self._ddc._ready:
            return
        fs_out = self._ddc.fs_out or 1.0
        rel, coarse = self._chain_coarse(fs_out)
        ddc_off = -coarse
        if abs(self._ddc.offset_hz - ddc_off) > 1.0:
            # Host-only DDC reconfiguration: wrap it in a plain stop/start (which is safe)
            # so the device buffer cannot overflow while we are not fetching.
            try:
                self._stop_trigger_locked()
                self._ddc.configure(
                    self._fs_in, ddc_off, self._ddc.decimate,
                    self._packet_samples * self._ddc_batch)
                self._mix_phase = 0.0
                self._start_trigger_locked()
                self._last_ok = time.monotonic()
                self._ready_at = time.monotonic() + 0.15
            except Exception as exc:
                log.warning('SDR tuning reconfiguration failed; doing full recovery: %r', exc)
                try:
                    self._reconfigure_full_locked()
                except Exception as recovery_exc:
                    raise DeviceError(
                        f'SDR tuning recovery failed: {recovery_exc}') from recovery_exc
                return
        changed = abs((rel + self._ddc.offset_hz) - self._mix_freq) > 0.5
        self._mix_freq = rel + self._ddc.offset_hz
        if changed:
            # A new mix frequency means the FM discriminator's previous sample belongs to
            # another channel; clear it and flush queued audio from the old channel.
            self._demod.retune()
            self._begin_audio_settle()
        self._applied_listen = float(s.sdr_listen_hz)
        s.sdr_actual.update(listen=s.sdr_listen_hz, ddc_offset=self._ddc.offset_hz,
                            mix_offset=self._mix_freq, ddc_rate=fs_out,
                            ddc_delay=self._ddc.delay)

    def _mix(self, i, q):
        """Fine-tune by the residual offset with a continuous software NCO."""
        f = getattr(self, '_mix_freq', 0.0)
        if abs(f) < 1e-6 or i.size == 0:
            return i, q
        fs = self._ddc.fs_out or 1.0
        n = i.size
        inc = -2.0 * np.pi * f / fs
        ph = self._mix_phase + inc * np.arange(n)
        c = np.cos(ph)
        sn = np.sin(ph)
        i2 = i * c - q * sn
        q2 = i * sn + q * c
        self._mix_phase = (self._mix_phase + inc * n) % (2.0 * np.pi)
        return i2, q2

    def _open_adm_locked(self):
        self._close_adm_locked()
        if not sb.SDR_CAPS.get('adm'):
            return
        try:
            sb.dll.ADM_Open(sb.pointer(self._adm))
            self._adm_ok = bool(self._adm.value)
            _t('adm: ADM_Open ok=%s', self._adm_ok)
        except Exception:
            self._adm_ok = False

    # ---------------- parameter setters ----------------
    def set_params(self, center=None, decimate=None):
        s = self.dev.state
        if center is not None:
            s.sdr_center_hz = float(center)
        if decimate is not None:
            s.sdr_decimate = _round_decimate(decimate)
        self._configure()

    def set_tune(self, listen_hz):
        """Cheap, state-only: the new offset is applied in the next step() with a software
        NCO, so tuning does not block the acquisition loop or reconfigure the DDC."""
        s = self.dev.state
        bandwidth = float(s.sdr_actual.get('bandwidth', self._fs_in or 1.0))
        s.sdr_listen_hz = max(s.sdr_center_hz - bandwidth / 2.0,
                              min(s.sdr_center_hz + bandwidth / 2.0, float(listen_hz)))

    def set_demod(self, mode=None, if_bw=None, squelch=None, volume=None,
                  agc=None, pitch=None, deemph_us=None):
        s = self.dev.state
        old_demod = s.sdr_demod
        old_if_bw = s.sdr_if_bw
        old_pitch = s.sdr_pitch
        old_deemph = float(getattr(s, 'sdr_deemph_us', -1.0))
        reconfig = False
        if mode is not None and mode in ANALOG_MODES and mode != s.sdr_demod:
            s.sdr_demod = mode
            reconfig = True
        if if_bw is not None and abs(float(if_bw) - s.sdr_if_bw) > 0.5:
            s.sdr_if_bw = float(if_bw)
            reconfig = True
        if pitch is not None and abs(float(pitch) - s.sdr_pitch) > 0.5:
            s.sdr_pitch = float(pitch)
            reconfig = True
        if deemph_us is not None and abs(float(deemph_us) - old_deemph) > 1e-6:
            s.sdr_deemph_us = float(deemph_us)
            reconfig = True
        # Volume / squelch / AGC are applied live in step(); they must NOT reconfigure the
        # demod (that reset the filters and produced a pop/gap on every slider move).
        if squelch is not None:
            s.sdr_squelch = float(squelch)
        if volume is not None:
            s.sdr_volume = float(max(0.0, min(2.0, volume)))
        if agc is not None:
            s.sdr_agc = bool(agc)
        if reconfig:
            try:
                with self._lock:
                    self._reconfigure_chain_runtime_locked()
            except Exception:
                s.sdr_demod = old_demod
                s.sdr_if_bw = old_if_bw
                s.sdr_pitch = old_pitch
                s.sdr_deemph_us = old_deemph
                raise

    def reconfigure(self):
        self._configure()

    def request_stop(self) -> None:
        """Stop the fetch loop before the session is torn down."""
        self._ready = False

    def is_ready(self) -> bool:
        return bool(self._ready)

    def acquisition_timeout(self) -> float:
        return 5.0

    def pacing(self, dt: float, produced: bool) -> float:
        """The IQS Adaptive stream paces itself.

        IQS_GetIQStream_PM1 blocks until a packet is ready, so any extra sleep accumulates a
        backlog and the device then returns BusDataError on every fetch. Only back off when
        a step produced nothing (transient error), to avoid a busy spin.
        """
        return 0.0 if produced else 0.002

    def health(self) -> dict:
        return {'ok': self._packets_ok, 'err': self._packets_err,
                'last_status': self._last_status, 'transient_streak': self._transient_streak}

    # ---------------- acquisition ----------------
    def _recover_locked(self, reason) -> None:
        self.iqs.note_recovery()
        # In-place reconfiguration cannot always unwedge the device; after a few failed
        # attempts escalate to a fatal error so the supervisor restarts the worker with a
        # fresh Device_Open (which does recover).
        self._recovery_attempts += 1
        if self._recovery_attempts > 3:
            raise DeviceError('SDR stream unrecoverable; restarting worker')
        log.warning('SDR stream stalled (%s); reconfiguring %d/3', reason, self._recovery_attempts)
        try:
            self._reconfigure_full_locked()
        except DeviceError:
            raise
        except Exception as exc:
            log.warning('SDR stall recovery failed: %r', exc)

    def _step_failed_locked(self, stage, status):
        self._error_streak += 1
        if self._error_streak < 8:
            return
        now = time.monotonic()
        if now - self._last_recovery < 1.0:
            return
        message = f'SDR {stage} failed repeatedly (status={status})'
        self.dev.state.last_error = message
        if self._recovery_attempts >= 2:
            raise DeviceError(message)
        self._recovery_attempts += 1
        self._last_recovery = now
        log.warning('%s; reconfigure attempt %d/2', message, self._recovery_attempts)
        try:
            self._reconfigure_full_locked()
        except Exception as exc:
            raise DeviceError(f'SDR recovery configuration failed: {exc}') from exc

    def _adm_metrics_locked(self, i, q):
        """Call the vendor analog demod for AM/FM metrics (not for the audio path)."""
        s = self.dev.state
        if not self._adm_ok or s.sdr_demod not in ('am', 'fm', 'nfm', 'wfm'):
            return
        if not ADM_ENABLED:
            return
        if len(i) < 64:
            return
        T = sb
        arr = np.empty(2 * len(i), dtype=np.float32)
        arr[0::2] = i
        arr[1::2] = q
        n = len(i)
        try:
            if s.sdr_demod == 'am':
                p = T.AMDemodParam_TypeDef()
                st = T.dll.ADM_AMDemod_PM1(T.pointer(self._adm), T.c_void_p(arr.ctypes.data),
                                           int(T.DataFormat_TypeDef.Complexfloat), n,
                                           self._ddc.fs_out, self._scale_to_v, T.pointer(p))
                if st == 0:
                    s.sdr_adm = dict(kind='am', mod_rate=float(p.ModRate),
                                     mod_depth=float(p.ModDepth) * 100.0,
                                     carrier_dbm=float(p.CarrierPower),
                                     sinad=float(p.SINAD), snr=float(p.SNR),
                                     thd=float(p.THD))
            else:
                p = T.FMDemodParam_TypeDef()
                st = T.dll.ADM_FMDemod_PM1(T.pointer(self._adm), T.c_void_p(arr.ctypes.data),
                                           int(T.DataFormat_TypeDef.Complexfloat), n,
                                           self._ddc.fs_out, self._scale_to_v, False, T.pointer(p))
                if st == 0:
                    s.sdr_adm = dict(kind='fm', mod_rate=float(p.ModRate),
                                     deviation=float(p.Deviation),
                                     carrier_err=float(p.CarrierFreqErr),
                                     sinad=float(p.SINAD), snr=float(p.SNR),
                                     thd=float(p.THD))
        except Exception:
            self._adm_ok = False

    def step(self):
        if not self._ready:
            return [], []
        now = time.monotonic()
        frames = []
        dev = self.dev
        with self._lock:
            if not self._ready:
                return [], []
            s = dev.state
            # Apply a pending tune BEFORE fetching: a full reconfiguration (needed when the
            # tune leaves the DDC passband) invalidates any buffer fetched before it.
            if s.sdr_listen_hz != self._applied_listen:
                self._apply_tuning_locked()
            if self._audio_reset_pending:
                frames.append(encode_audio(
                    self._audio_seq, self.AUDIO_RATE, np.zeros(0, dtype=np.int16)))
                self._audio_reset_pending = False
            # Watchdog: if no good packet arrived for a while the stream is wedged; a full
            # reconfigure recovers it. This bounds any freeze to ~1.5 s.
            if self.iqs.watchdog_expired(now):
                self._recover_locked('watchdog')
            got = self.iqs.fetch(now)
            if not got.ok:
                if got.error:
                    self._step_failed_locked('get exception', got.error)
                    return [], []
                if got.transient:
                    if got.warn:
                        s.status_warning = int(got.status)
                    # A timeout blocks for BusTimeout each call, so recover fast on those;
                    # BusDataError returns immediately, so a longer streak is fine. The
                    # streak limits and the cooldown live in the shared IQS layer.
                    if got.recover:
                        self._recover_locked(got.status)
                    return [], []
                self._step_failed_locked('get', got.status)
                return [], []
            s.status_warning = 0
            self._recovery_attempts = 0
            if self.iqs.packets_ok == 1 or self.iqs.packets_ok % 100 == 0:
                _t('step: packets_ok=%d err=%d status=%d', self.iqs.packets_ok,
                   self.iqs.packets_err, self.iqs.last_status)
            if self.iqs.settling(now):
                # Keep draining IQS while settling so the device FIFO cannot overflow, but
                # preserve a pending audio reset marker for the clients.
                return frames, []
            self._error_streak = 0
            self._recovery_attempts = 0
            n = got.samples
            if n < 2:
                return [], []
            arrays = [got.raw]
            total_n = n
            # Kept for the record: batching two packets per step is NOT safe -- it
            # overwrote the vendor's internal packet buffer and corrupted the heap -- so
            # `_ddc_batch` stays 1 and this loop does not run.
            for _ in range(1, self._ddc_batch):
                nxt = self.iqs.fetch(now)
                if not nxt.ok:
                    if nxt.error:
                        self._step_failed_locked('get exception', nxt.error)
                    return frames, []
                arrays.append(nxt.raw)
                total_n += nxt.samples
            arr = arrays[0] if len(arrays) == 1 else np.concatenate(arrays)
            src = arr
            n = total_n
            s = dev.state

            # ---- panadapter / waterfall ----
            if now - self._last_pan >= self.PAN_MIN_INTERVAL:
                # Rate-limit first: a failed vendor frame must not become a busy retry.
                self._last_pan = now
                vendor = self._vendor_spectrum_locked(self.iqs.stream, arr)
                if vendor is not None:
                    freq, spec = vendor
                    row = self._pan.waterfall_row(spec)
                    res = (freq, spec, row)
                else:
                    res = self._pan.process(
                        arr[0::2], arr[1::2], self._fs_in, self._iqs_center_hz,
                        self._scale_to_v, bandwidth=s.sdr_actual.get('bandwidth'))
                if res is not None:
                    freq, spec, row = res
                    finite = spec[np.isfinite(spec)]
                    if finite.size:
                        # Same observation the swept paths hand the reference loop, so an SDR
                        # Auto Scale uses the identical placement rule.
                        floor_index = int((finite.size - 1) * 0.3)
                        dev.observe_reference_peak(
                            'sdr', float(np.max(finite)),
                            float(np.partition(finite, floor_index)[floor_index]))
                    frames.append(encode_rta(dev.state.freq_version, freq, spec, row,
                                              65535, s.sdr_actual['start'],
                                              s.sdr_actual['stop']))

            # ---- channelizer + demod ----
            try:
                i, q = self._ddc.process(src, n)  # pass the ctypes buffer directly
            except (RuntimeError, ValueError) as exc:
                self._step_failed_locked('ddc', repr(exc))
                return frames, []
            i, q = self._mix(i, q)                # software fine tuning
            # Arm the settle window BEFORE the demod so the AGC is held while the chain
            # transient is discarded; otherwise it winds up on audio that is never
            # published and the first real block comes out far too loud.
            if self._settle_pending and i.size:
                self._settle_pending = False
                self._discard_until = now + self.SETTLE_DISCARD
                self._fade_start = self._discard_until
                self._fade_until = self._discard_until + self.SETTLE_FADE
            settling = now < self._discard_until
            audio, power_dbfs = self._demod.process(
                i, q, use_agc=s.sdr_agc, agc_hold=settling)
            if audio.size and now < self._discard_until:
                audio = np.zeros(0, dtype=np.float32)   # discard the settling transient
            elif audio.size and now < self._fade_until:
                span = max(1e-3, self._fade_until - self._fade_start)
                gain = max(0.0, min(1.0, (now - self._fade_start) / span))
                audio = audio * gain
            level_dbfs = float(power_dbfs) - 90.31     # int16 full-scale reference
            s.sdr_level_dbfs = float(level_dbfs)
            thr = float(s.sdr_squelch)
            if level_dbfs >= thr:
                self._squelch_hold_until = now + self.SQUELCH_HOLD_S
                s.sdr_squelch_open = True
            elif (level_dbfs < thr - self.SQUELCH_HYST_DB
                    and now >= self._squelch_hold_until):
                s.sdr_squelch_open = False
            if audio.size:
                # Smooth the gate instead of hard-zeroing one block: a step from full level
                # to zero audibly clicks, and a bare comparator chatters at the threshold.
                target = 1.0 if s.sdr_squelch_open else 0.0
                dt = audio.size / max(1.0, float(self.AUDIO_RATE))
                tau = (self.SQUELCH_ATTACK_S if target > self._squelch_gain
                       else self.SQUELCH_RELEASE_S)
                alpha = 1.0 - float(np.exp(-dt / max(1e-4, tau)))
                new_gain = self._squelch_gain + (target - self._squelch_gain) * alpha
                gate = np.linspace(self._squelch_gain, new_gain, audio.size)
                self._squelch_gain = new_gain
                audio = audio * float(s.sdr_volume) * gate
                self._audio_buf = np.concatenate([self._audio_buf, audio])
                while self._audio_buf.size >= self.AUDIO_FRAME:
                    chunk = self._audio_buf[:self.AUDIO_FRAME]
                    self._audio_buf = self._audio_buf[self.AUDIO_FRAME:]
                    pcm = np.clip(chunk, -1.0, 1.0) * 32767.0
                    frames.append(encode_audio(
                        self._audio_seq, self.AUDIO_RATE, pcm.astype(np.int16)))
                    self._audio_seq = (self._audio_seq + 1) & 0xFFFFFFFF
                if self._audio_buf.size > self.AUDIO_RATE:
                    self._audio_buf = self._audio_buf[-self.AUDIO_FRAME:]

            # ---- vendor demod metrics (AM/FM) ----
            if now - self._last_adm >= self.ADM_MIN_INTERVAL and len(i) >= 64:
                self._adm_metrics_locked(i, q)
                self._last_adm = now

        return frames, []
