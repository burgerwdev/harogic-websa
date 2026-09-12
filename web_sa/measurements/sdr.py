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
import struct
import time

import numpy as np

from ..demod import ANALOG_MODES, AnalogDemod, DdcChannel, Panadapter
from ..hardware import sdk_bindings as sb
from ..hardware.device import DeviceError
from .base import MeasurementSession
from .rta import _encode_rta

log = logging.getLogger(__name__)

MAGIC_AUDIO = b'AUDF'
_AUDIO_HEADER = struct.Struct('<4sIII')   # magic, seq, rate, samples

# IQS return codes that are transient (bad packet / timeout) rather than fatal.
# The official examples never check IQS_GetIQStream's return value; a bad packet is
# simply skipped and the next one is used.
_TRANSIENT_IQS = {-8, -9, -10, -12}

ADM_ENABLED = os.getenv('WEBSA_SDR_ADM', '1').lower() not in ('0', 'false', 'no', 'off')


def encode_audio(seq: int, rate: int, pcm: np.ndarray) -> bytes:
    pcm = np.ascontiguousarray(pcm, dtype=np.int16)
    return _AUDIO_HEADER.pack(MAGIC_AUDIO, int(seq), int(rate), pcm.size) + pcm.tobytes()


def _round_decimate(value) -> int:
    """IQS DecimateFactor is power-of-two only (verified); clamp to 1..2048."""
    try:
        v = int(value)
    except (TypeError, ValueError):
        v = 16
    v = max(1, min(2048, v))
    return 1 << (v.bit_length() - 1) if v & (v - 1) else v


class SdrSession(MeasurementSession):
    name = 'sdr'

    PAN_FFT = 2048
    PAN_MIN_INTERVAL = 1.0 / 20.0      # panadapter/waterfall ~20 fps
    AUDIO_RATE = 48000
    AUDIO_FRAME = 960                  # 20 ms
    ADM_MIN_INTERVAL = 0.5             # metrics update

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
        self._packet_samples = 0
        self._ddc_batch = 1
        self._scale_to_v = 1.0
        self._last_pan = 0.0
        self._last_adm = 0.0
        self._discard_until = 0.0
        self._fade_start = 0.0
        self._fade_until = 0.0
        self._settle_pending = False
        self._mix_phase = 0.0
        self._mix_freq = 0.0
        self._applied_listen = None
        self._ready_at = 0.0
        self._error_streak = 0
        self._recovery_attempts = 0
        self._last_recovery = 0.0
        self._last_status = 0
        self._packets_ok = 0
        self._packets_err = 0
        self._transient_streak = 0
        self._timeout_streak = 0
        self._last_ok = 0.0

    # ---------------- lifecycle ----------------
    def enter(self):
        super().enter()
        self._configure()

    def exit(self):
        with self._lock:
            self._ready = False
            try:
                sb.dll.IQS_BusTriggerStop(sb.pointer(self.dev.dev))
            except Exception:
                log.exception('SDR trigger stop failed during exit')
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
                sb.dll.ADM_Close(sb.pointer(self._adm))
            except Exception:
                pass
        self._adm = sb.c_void_p()
        self._adm_ok = False

    # ---------------- configuration ----------------
    def _configure(self):
        with self._lock:
            self._ready = False
            self._configure_iqs_locked()
            self._configure_chain_locked()
            self._open_adm_locked()
            self._start_trigger_locked()
            self._last_pan = 0.0
            self._last_adm = 0.0
            self._transient_streak = 0
            self._recovery_attempts = 0
            # Settle window: the device needs a moment after IQS_Configuration; without it
            # the first fetches return BusDataError and would trip the stall recovery.
            self._ready_at = time.monotonic() + 0.4
            self._last_ok = time.monotonic()
            self._ready = True

    def _reset_iqs_mode_locked(self):
        """A second IQS_Configuration after the stream has started is rejected and
        permanently wedges the stream; an SWP_Configuration switches the device's mode and
        lets IQS be configured again (bench-verified: this makes reconfiguration safe)."""
        T = sb
        p = T.SWP_Profile_TypeDef()
        o = T.SWP_Profile_TypeDef()
        ti = T.SWP_TraceInfo_TypeDef()
        st = T.dll.SWP_ProfileDeInit(T.pointer(self.dev.dev), T.pointer(p))
        if st != 0:
            raise RuntimeError(f'SWP_ProfileDeInit status={st}')
        st = T.dll.SWP_Configuration(T.pointer(self.dev.dev), T.pointer(p),
                                     T.pointer(o), T.pointer(ti))
        if st != 0:
            raise RuntimeError(f'SWP_Configuration mode reset status={st}')

    def _stop_trigger_locked(self, *, required: bool = True) -> None:
        try:
            st = sb.dll.IQS_BusTriggerStop(sb.pointer(self.dev.dev))
        except Exception:
            if required:
                raise
            return
        if required and st != 0:
            raise RuntimeError(f'IQS_BusTriggerStop status={st}')

    def _configure_iqs_locked(self):
        dev = self.dev
        s = dev.state
        T = sb
        self._stop_trigger_locked(required=False)
        s.sdr_decimate = _round_decimate(s.sdr_decimate)
        p = T.IQS_Profile_TypeDef()
        out = T.IQS_Profile_TypeDef()
        info = T.IQS_StreamInfo_TypeDef()
        # Reset the device mode first so IQS_Configuration is accepted (see the note above).
        self._reset_iqs_mode_locked()
        st = T.dll.IQS_ProfileDeInit(T.pointer(dev.dev), T.pointer(p))
        if st != 0:
            raise RuntimeError(f'IQS_ProfileDeInit status={st}')
        native_rate = float(p.NativeIQSampleRate_SPS)
        expected_bw = native_rate * 0.8 / s.sdr_decimate if native_rate > 0 else 0.0
        if s.caps and expected_bw > 0:
            half = expected_bw / 2.0
            s.sdr_center_hz = max(s.caps.freq_min_hz + half,
                                  min(s.caps.freq_max_hz - half, s.sdr_center_hz))
        p.CenterFreq_Hz = float(s.sdr_center_hz)
        p.RefLevel_dBm = float(s.ref_level)
        p.DecimateFactor = int(s.sdr_decimate)
        p.DataFormat = T.DataFormat_TypeDef.Complex16bit
        p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
        p.TriggerMode = T.TriggerMode_TypeDef.Adaptive
        p.BusTimeout_ms = 250
        p.Atten = int(s.atten)
        p.Preamplifier = (T.PreamplifierState_TypeDef.AutoOn if s.preamplifier == 0
                          else T.PreamplifierState_TypeDef.ForcedOff)
        p.IFGainGrade = int(s.ifgain)
        p.GainStrategy = (T.GainStrategy_TypeDef.LowNoisePreferred if s.gain_strategy == 0
                          else T.GainStrategy_TypeDef.HighLinearityPreferred)
        p.DCCancelerMode = T.DCCancelerMode_TypeDef.DCCHighPassFilterMode
        p.QDCMode = T.QDCMode_TypeDef.QDCOff
        st = T.dll.IQS_Configuration(T.pointer(dev.dev), T.pointer(p), T.pointer(out), T.pointer(info))
        if st != 0:
            raise RuntimeError(f'IQS_Configuration status={st}')
        fs = float(info.IQSampleRate)
        bandwidth = float(info.Bandwidth) or fs
        if fs <= 0 or bandwidth <= 0 or int(info.PacketSamples) <= 0:
            raise RuntimeError(
                f'IQS_Configuration returned invalid stream info '
                f'rate={fs} bandwidth={bandwidth} samples={int(info.PacketSamples)}')
        half = bandwidth / 2.0
        configured_center = float(out.CenterFreq_Hz)
        if configured_center > 0:
            s.sdr_center_hz = configured_center
        s.sdr_listen_hz = max(s.sdr_center_hz - half,
                              min(s.sdr_center_hz + half, float(s.sdr_listen_hz)))
        self._packet_samples = int(info.PacketSamples)
        self._ddc_batch = 2 if fs >= 3.0e6 else 1
        self._fs_in = fs
        s.sdr_actual = dict(
            center=s.sdr_center_hz, iq_rate=fs, bandwidth=bandwidth,
            decimate=int(out.DecimateFactor), packet_samples=self._packet_samples,
            packet_bytes=int(info.PacketDataSize),
            pan_points=min(
                self.PAN_FFT,
                max(2, 2 * int(np.floor(self.PAN_FFT * bandwidth / (2.0 * fs))) + 1),
            ),
            start=s.sdr_center_hz - half, stop=s.sdr_center_hz + half,
            atten=int(out.Atten), preamp=int(getattr(out.Preamplifier, 'value', 0)),
            ifgain=int(out.IFGainGrade),
            ref_clock_source=int(getattr(out.ReferenceClockSource, 'value', -1)),
            refclk_out=bool(out.EnableReferenceClockOut),
        )
        dev._read_amp_atten()
        dev.state.config_version += 1
        dev.state.freq_version += 1
        dev.state.last_error = ''
        self._pan.reset()

    def _start_trigger_locked(self):
        """Start streaming. Kept separate so the slow DDC configuration can run BEFORE the
        trigger starts (otherwise the device buffer overflows while we are not fetching)."""
        T = sb
        start = T.dll.IQS_BusTriggerStart(T.pointer(self.dev.dev))
        if start != 0:
            raise RuntimeError(f'IQS_BusTriggerStart status={start}')
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
        rel = float(s.sdr_listen_hz) - float(s.sdr_center_hz)
        # The DDC passband is flat only within ~0.4*fs_out (measured +-0.04 dB up to
        # 0.4*fs_out, -12 dB at 0.45*fs_out), so the coarse grid must keep the residual
        # (and the whole demod channel) well inside it. 0.4 keeps the residual <= 0.2*fs_out.
        grid = max(1000.0, (fs_out or 1.0) * 0.4)
        return rel, round(rel / grid) * grid

    SETTLE_DISCARD = 0.12
    SETTLE_FADE = 0.10

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
            self._ddc.configure(fs_in, ddc_off, decimate,
                                 self._packet_samples * self._ddc_batch)
            self._mix_phase = 0.0
        self._mix_freq = rel + self._ddc.offset_hz
        self._applied_listen = float(s.sdr_listen_hz)
        self._demod.configure(self._ddc.fs_out, s.sdr_demod, if_bw, pitch=s.sdr_pitch)
        self._begin_audio_settle()
        s.sdr_actual.update(
            listen=s.sdr_listen_hz, demod=s.sdr_demod, if_bw=if_bw,
            ddc_offset=self._ddc.offset_hz, mix_offset=self._mix_freq,
            ddc_decimate=self._ddc.decimate, ddc_rate=self._ddc.fs_out,
            ddc_delay=self._ddc.delay,
            ddc_batch=self._ddc_batch,
            audio_rate=self.AUDIO_RATE,
        )
        half = float(s.sdr_actual.get('bandwidth', fs_in)) / 2.0
        s.sdr_actual['start'] = float(s.sdr_center_hz) - half
        s.sdr_actual['stop'] = float(s.sdr_center_hz) + half

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
                  agc=None, pitch=None):
        s = self.dev.state
        old_demod = s.sdr_demod
        old_if_bw = s.sdr_if_bw
        old_pitch = s.sdr_pitch
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
                raise

    def reconfigure(self):
        self._configure()

    # ---------------- acquisition ----------------
    def _recover_locked(self, reason) -> None:
        self._last_recovery = time.monotonic()
        self._transient_streak = 0
        self._timeout_streak = 0
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
        import ctypes as C

        T = sb
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
            stream = T.IQStream_TypeDef()
            # Watchdog: if no good packet arrived for a while the stream is wedged; a full
            # reconfigure recovers it. This bounds any freeze to ~1.5 s.
            if now - self._last_ok > 1.5 and now - self._last_recovery >= 1.0:
                self._recover_locked('watchdog')
            try:
                st = T.dll.IQS_GetIQStream_PM1(T.pointer(dev.dev), T.pointer(stream))
            except Exception as exc:
                self._step_failed_locked('get exception', repr(exc))
                return [], []
            if st != 0:
                self._last_status = int(st)
                self._packets_err += 1
                if st in _TRANSIENT_IQS:
                    self._transient_streak += 1
                    self._timeout_streak = self._timeout_streak + 1 if st == -10 else 0
                    # A timeout blocks for BusTimeout each call, so recover fast on those;
                    # BusDataError returns immediately, so a longer streak is fine.
                    if ((self._transient_streak >= 60 or self._timeout_streak >= 3)
                            and now - self._last_recovery >= 1.0):
                        self._recover_locked(st)
                    return [], []
                self._step_failed_locked('get', st)
                return [], []
            self._last_status = 0
            self._transient_streak = 0
            self._timeout_streak = 0
            self._last_ok = now
            self._recovery_attempts = 0
            self._packets_ok += 1
            if time.monotonic() < self._ready_at:
                # Keep draining IQS while settling so the device FIFO cannot overflow, but
                # preserve a pending audio reset marker for the clients.
                return frames, []
            self._error_streak = 0
            self._recovery_attempts = 0
            n = int(stream.IQS_StreamInfo.PacketSamples)
            if n < 2:
                return [], []
            self._scale_to_v = float(stream.IQS_ScaleToV)
            src = C.cast(stream.AlternIQStream, C.POINTER(C.c_int16 * (n * 2))).contents
            arrays = [np.ctypeslib.as_array(src).copy()]
            total_n = n
            # At high IQ input rates the DSP work fits within one packet only barely.
            # Fetch a second packet before invoking NumPy so two packet periods absorb
            # scheduler jitter without changing the IQ capture bandwidth or IF filter.
            for _ in range(1, self._ddc_batch):
                next_stream = T.IQStream_TypeDef()
                try:
                    next_st = T.dll.IQS_GetIQStream_PM1(
                        T.pointer(dev.dev), T.pointer(next_stream))
                except Exception as exc:
                    self._step_failed_locked('get exception', repr(exc))
                    return frames, []
                if next_st != 0:
                    self._last_status = int(next_st)
                    self._packets_err += 1
                    if next_st in _TRANSIENT_IQS:
                        self._transient_streak += 1
                        self._timeout_streak = (
                            self._timeout_streak + 1 if next_st == -10 else 0)
                        if ((self._transient_streak >= 60 or self._timeout_streak >= 3)
                                and time.monotonic() - self._last_recovery >= 1.0):
                            self._recover_locked(next_st)
                    else:
                        self._step_failed_locked('get', next_st)
                    return frames, []
                self._last_status = 0
                self._transient_streak = 0
                self._timeout_streak = 0
                self._last_ok = time.monotonic()
                self._packets_ok += 1
                next_n = int(next_stream.IQS_StreamInfo.PacketSamples)
                if next_n < 2:
                    return frames, []
                next_src = C.cast(
                    next_stream.AlternIQStream,
                    C.POINTER(C.c_int16 * (next_n * 2))).contents
                arrays.append(np.ctypeslib.as_array(next_src).copy())
                total_n += next_n
            arr = arrays[0] if len(arrays) == 1 else np.concatenate(arrays)
            src = arr
            n = total_n
            s = dev.state

            # ---- panadapter / waterfall ----
            if now - self._last_pan >= self.PAN_MIN_INTERVAL:
                res = self._pan.process(
                    arr[0::2], arr[1::2], self._fs_in, s.sdr_center_hz,
                    self._scale_to_v, bandwidth=s.sdr_actual.get('bandwidth'))
                if res is not None:
                    freq, spec, row = res
                    frames.append(_encode_rta(dev.state.freq_version, freq, spec, row,
                                              65535, s.sdr_actual['start'],
                                              s.sdr_actual['stop']))
                    self._last_pan = now

            # ---- channelizer + demod ----
            try:
                i, q = self._ddc.process(src, n)  # pass the ctypes buffer directly
            except (RuntimeError, ValueError) as exc:
                self._step_failed_locked('ddc', repr(exc))
                return frames, []
            i, q = self._mix(i, q)                # software fine tuning
            audio, power_dbfs = self._demod.process(i, q, use_agc=s.sdr_agc)
            if audio.size and self._settle_pending:
                # First audio after a reconfiguration: start the discard+fade window now.
                self._settle_pending = False
                self._discard_until = now + self.SETTLE_DISCARD
                self._fade_start = self._discard_until
                self._fade_until = self._discard_until + self.SETTLE_FADE
            if audio.size and now < self._discard_until:
                audio = np.zeros(0, dtype=np.float32)   # discard the settling transient
            elif audio.size and now < self._fade_until:
                span = max(1e-3, self._fade_until - self._fade_start)
                gain = max(0.0, min(1.0, (now - self._fade_start) / span))
                audio = audio * gain
            level_dbfs = float(power_dbfs) - 90.31     # int16 full-scale reference
            s.sdr_level_dbfs = float(level_dbfs)
            s.sdr_squelch_open = bool(level_dbfs >= float(s.sdr_squelch))
            if audio.size:
                audio = (audio * float(s.sdr_volume) if s.sdr_squelch_open
                         else np.zeros_like(audio))
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
