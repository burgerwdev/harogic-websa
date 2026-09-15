"""measurements/iqs.py -- the IQ-stream plumbing the SDR and VSA sessions share.

Not a session: no demodulation, no frames, no display state. What it owns is the part
that is expensive to get wrong twice, and every rule here was bought with a bench
measurement (see tools/vsa_probe/FINDINGS.md):

* the profile ``IQS_Configuration`` needs, and the **mode reset** that makes a
  reconfiguration take effect at all -- a second ``IQS_Configuration`` after the stream has
  started is rejected and wedges the stream, while an ``SWP_Configuration`` switches the
  device's mode and makes IQS configurable again;
* the **transient/fatal** split: ``-8/-9/-10/-12`` are bus warnings and bad packets, the
  rest are errors;
* the ``-10/-11`` retry: the vendor guide's remedy is to re-issue the configuration call
  (treating ``-11`` as fatal turned a recoverable stall into a worker restart);
* the post-configuration **settle/drain**: without discarding the queued packets a rate
  change failed 10824 out of 10824 fetches, with it 0;
* one packet fetch, int16 -> volts through the vendor ``IQS_ScaleToV``;
* the **recovery decision**: 60 consecutive transient returns, or 3 bus timeouts, with a
  1 s cooldown, means the stream is wedged (2 of 6 decimate changes wedged permanently in
  the multi-rate soaks until recovery was reused).

The recovery *action* stays with the session -- SDR rebuilds its FFT/DDC/demod chain, a
vector session does something else -- so :meth:`IqsStream.fetch` reports the verdict and the
session reconfigures.

``sdk_bindings`` is resolved lazily (or injected) so this module and its tests import
without the vendor library present.
"""
from __future__ import annotations

import ctypes
import time

import numpy as np

#: Bus warnings / bad packets: skip and retry, never fatal.
TRANSIENT = frozenset({-8, -9, -10, -12})
#: Documented transient bus warnings whose remedy is to re-issue the call
#: (-10 BusTimeOut, -11 BusDownLoad).
BUS_RETRY = (-10, -11)
BUS_RETRY_TRIES = 4
BUS_RETRY_DELAY = 0.05
#: A run of these with no success in between means the stream is wedged.
TRANSIENT_STREAK_LIMIT = 60
TIMEOUT_STREAK_LIMIT = 3
RECOVERY_COOLDOWN_S = 1.0
#: No good packet for this long: the stream is wedged even without error returns.
WATCHDOG_S = 1.5
#: Default settle window after a configuration (SDR uses the same value on entry).
DEFAULT_READY_DELAY = 0.4

DATA_FORMATS = {'complex8': 'Complex8bit', 'complex16': 'Complex16bit',
                'complex32': 'Complex32bit', 'complexfloat': 'Complexfloat'}
TRIGGER_MODES = {'adaptive': 'Adaptive', 'fixed_points': 'FixedPoints'}
DCC_MODES = {'off': 'DCCOff', 'high_pass': 'DCCHighPassFilterMode',
             'manual': 'DCCManualOffsetMode', 'auto': 'DCCAutoOffsetMode'}
QDC_MODES = {'off': 'QDCOff', 'auto': 'QDCAutoMode', 'manual': 'QDCManualMode'}


def default_sdk():
    """The vendor binding layer, imported on first use (it loads libhtraapi at import)."""
    from ..hardware import sdk_bindings as sb
    return sb


def sdk_call(fn, what: str, *, retries: int = BUS_RETRY_TRIES,
             delay: float = BUS_RETRY_DELAY, sleep=time.sleep) -> int:
    """Run an SDK configuration entry point, retrying the transient bus warnings."""
    last = 0
    for attempt in range(retries + 1):
        last = int(fn())
        if last == 0:
            return 0
        if last in BUS_RETRY and attempt < retries:
            sleep(delay)
            continue
        raise RuntimeError(f'{what} status={last}')
    raise RuntimeError(f'{what} status={last}')


class IqsInfo:
    """What ``IQS_Configuration`` reported back, in the units the sessions need."""

    __slots__ = ('fs', 'bandwidth', 'packet_samples', 'packet_bytes', 'packet_count',
                 'stream_samples', 'center_hz', 'decimate', 'atten', 'preamp', 'ifgain',
                 'refclk_source', 'refclk_out')

    def __init__(self, out, info):
        self.fs = float(info.IQSampleRate)
        self.bandwidth = float(info.Bandwidth) or self.fs
        self.packet_samples = int(info.PacketSamples)
        self.packet_bytes = int(info.PacketDataSize)
        self.packet_count = int(info.PacketCount)
        self.stream_samples = int(info.StreamSamples)
        self.center_hz = float(out.CenterFreq_Hz)
        self.decimate = int(out.DecimateFactor)
        self.atten = int(out.Atten)
        self.preamp = int(getattr(out.Preamplifier, 'value', 0))
        self.ifgain = int(out.IFGainGrade)
        self.refclk_source = int(getattr(out.ReferenceClockSource, 'value', -1))
        self.refclk_out = bool(out.EnableReferenceClockOut)

    def valid(self) -> bool:
        return self.fs > 0 and self.bandwidth > 0 and self.packet_samples > 0


class Fetch:
    """One ``IQS_GetIQStream_PM1`` outcome."""

    __slots__ = ('status', 'raw', 'samples', 'scale_to_v', 'transient', 'recover', 'error')

    def __init__(self, status, raw=None, samples=0, scale_to_v=1.0,
                 transient=False, recover=False, error=''):
        self.status = status
        self.raw = raw
        self.samples = samples
        self.scale_to_v = scale_to_v
        self.transient = transient
        self.recover = recover
        #: repr() of an exception raised by the SDK call (a DLL hang/abort), else ''.
        self.error = error

    @property
    def ok(self) -> bool:
        """The SDK call succeeded (a 0-sample packet is still a successful call)."""
        return self.status == 0


class IqsStream:
    """One IQS stream on a device: configure, drain, fetch, and say when it is wedged."""

    def __init__(self, dev, *, sdk=None, ready_delay: float = DEFAULT_READY_DELAY):
        self.dev = dev
        self.sb = sdk or default_sdk()
        #: Header-correct IQStream (728 bytes) -- see web_sa/hardware/sdk_bindings.py: the
        #: vendor wrapper's own struct is 8 bytes short and the SDK writes past it.
        self.stream = self.sb.IQStream_TypeDef()
        self.ready_delay = ready_delay
        self.packet_samples = 0
        self.scale_to_v = 1.0
        self.packets_ok = 0
        self.packets_err = 0
        self.transient_streak = 0
        self.timeout_streak = 0
        self.last_status = 0
        self.last_ok = 0.0
        self.last_recovery = 0.0
        self.ready_at = 0.0

    # ---------------- configuration ----------------
    def profile(self, *, center_hz: float, decimate: int, ref_level_dbm: float = 0.0,
                data_format: str = 'complex16', trigger_mode: str = 'adaptive',
                trigger_length: int = 0, bus_timeout_ms: int = 250,
                dcc: str = 'auto', qdc: str = 'off', atten: int = -1,
                preamplifier: int = 0, ifgain: int = 2, gain_strategy: int = 0):
        """Build an IQS profile (and let the device fill in its own defaults first)."""
        T = self.sb
        p = T.IQS_Profile_TypeDef()
        T.dll.IQS_ProfileDeInit(T.pointer(self.dev.dev), T.pointer(p))
        p.CenterFreq_Hz = float(center_hz)
        p.RefLevel_dBm = float(ref_level_dbm)
        p.DecimateFactor = int(decimate)
        p.DataFormat = getattr(T.DataFormat_TypeDef, DATA_FORMATS[data_format])
        p.TriggerSource = T.IQS_TriggerSource_TypeDef.Bus
        p.TriggerMode = getattr(T.TriggerMode_TypeDef, TRIGGER_MODES[trigger_mode])
        p.TriggerLength = int(trigger_length)
        p.BusTimeout_ms = int(bus_timeout_ms)
        p.Atten = int(atten)
        p.Preamplifier = (T.PreamplifierState_TypeDef.AutoOn if preamplifier == 0
                          else T.PreamplifierState_TypeDef.ForcedOff)
        p.IFGainGrade = int(ifgain)
        p.GainStrategy = (T.GainStrategy_TypeDef.LowNoisePreferred if gain_strategy == 0
                          else T.GainStrategy_TypeDef.HighLinearityPreferred)
        p.DCCancelerMode = getattr(T.DCCancelerMode_TypeDef, DCC_MODES[dcc])
        p.QDCMode = getattr(T.QDCMode_TypeDef, QDC_MODES[qdc])
        return p

    def mode_reset(self) -> None:
        """Switch the device out of IQS so ``IQS_Configuration`` is accepted again.

        Bench-verified: a second ``IQS_Configuration`` after the stream has started is
        rejected and permanently wedges the stream; an ``SWP_Configuration`` switches the
        device's mode and lets IQS be configured again.
        """
        T = self.sb
        p = T.SWP_Profile_TypeDef()
        o = T.SWP_Profile_TypeDef()
        ti = T.SWP_TraceInfo_TypeDef()
        sdk_call(lambda: T.dll.SWP_ProfileDeInit(T.pointer(self.dev.dev), T.pointer(p)),
                 'SWP_ProfileDeInit')
        sdk_call(lambda: T.dll.SWP_Configuration(
            T.pointer(self.dev.dev), T.pointer(p), T.pointer(o), T.pointer(ti)),
            'SWP_Configuration mode reset')

    def configure(self, p, *, mode_reset: bool = True,
                  now: float | None = None) -> IqsInfo:
        """Apply a profile (from :meth:`profile`) and arm the settle window."""
        T = self.sb
        if mode_reset:
            self.mode_reset()
        out = T.IQS_Profile_TypeDef()
        info = T.IQS_StreamInfo_TypeDef()
        sdk_call(lambda: T.dll.IQS_Configuration(
            T.pointer(self.dev.dev), T.pointer(p), T.pointer(out), T.pointer(info)),
            'IQS_Configuration')
        parsed = IqsInfo(out, info)
        if not parsed.valid():
            raise RuntimeError(
                f'IQS_Configuration returned invalid stream info rate={parsed.fs} '
                f'bandwidth={parsed.bandwidth} samples={parsed.packet_samples}')
        self.packet_samples = parsed.packet_samples
        self.transient_streak = 0
        self.last_status = 0
        self.arm_settle(now)
        return parsed

    def arm_settle(self, now: float | None = None, delay: float | None = None) -> None:
        """Start (or restart) the window in which fetched packets are discarded."""
        base = time.monotonic() if now is None else now
        self.ready_at = base + (self.ready_delay if delay is None else delay)
        self.last_ok = base

    # ---------------- stream ----------------
    def start(self) -> None:
        sdk_call(lambda: self.sb.dll.IQS_BusTriggerStart(self.sb.pointer(self.dev.dev)),
                 'IQS_BusTriggerStart')

    def stop(self, *, required: bool = True) -> None:
        try:
            st = self.sb.dll.IQS_BusTriggerStop(self.sb.pointer(self.dev.dev))
        except Exception:
            if required:
                raise
            return
        if required and st != 0:
            raise RuntimeError(f'IQS_BusTriggerStop status={st}')

    def settling(self, now: float | None = None) -> bool:
        """True while a freshly configured stream should be drained, not analysed."""
        return (time.monotonic() if now is None else now) < self.ready_at

    def drain(self, seconds: float | None = None, *, now=None) -> int:
        """Fetch and drop packets for ``seconds`` (default: until the settle window ends).

        A blocking drain for callers that configure a stream and then do something else
        before analysing; ``settling()`` is the non-blocking form used inside a step loop.
        Errors are ignored on purpose -- the point is to keep the device FIFO empty.
        """
        t = time.monotonic() if now is None else now
        deadline = self.ready_at if seconds is None else t + seconds
        n = 0
        while time.monotonic() < deadline:
            self.fetch()
            n += 1
        return n

    def fetch(self, now: float | None = None) -> Fetch:
        """One packet, with the counters, streaks and recovery verdict updated.

        Never raises for a bus warning: the caller decides what to do with the verdict
        (``recover``), because the recovery action is session-specific.
        """
        t = time.monotonic() if now is None else now
        T = self.sb
        try:
            st = int(T.dll.IQS_GetIQStream_PM1(T.pointer(self.dev.dev),
                                               T.pointer(self.stream)))
        except Exception as exc:
            self.packets_err += 1
            self.last_status = -1
            return Fetch(-1, error=repr(exc))
        self.last_status = st
        if st != 0:
            self.packets_err += 1
            if st not in TRANSIENT:
                return Fetch(st)
            self.transient_streak += 1
            self.timeout_streak = self.timeout_streak + 1 if st == -10 else 0
            due = (self.transient_streak >= TRANSIENT_STREAK_LIMIT
                   or self.timeout_streak >= TIMEOUT_STREAK_LIMIT)
            recover = due and (t - self.last_recovery) >= RECOVERY_COOLDOWN_S
            return Fetch(st, transient=True, recover=recover)
        self.transient_streak = 0
        self.timeout_streak = 0
        self.last_ok = t
        self.packets_ok += 1
        n = int(self.stream.IQS_StreamInfo.PacketSamples)
        if n <= 0:
            return Fetch(0)
        self.scale_to_v = float(self.stream.IQS_ScaleToV)
        src = ctypes.cast(self.stream.AlternIQStream,
                          ctypes.POINTER(ctypes.c_int16 * (n * 2))).contents
        raw = np.ctypeslib.as_array(src).copy()
        return Fetch(0, raw=raw, samples=n, scale_to_v=self.scale_to_v)

    def watchdog_expired(self, now: float | None = None) -> bool:
        """No good packet for :data:`WATCHDOG_S` (and past the cooldown): wedged."""
        t = time.monotonic() if now is None else now
        return (t - self.last_ok) > WATCHDOG_S and (t - self.last_recovery) >= RECOVERY_COOLDOWN_S

    def note_recovery(self, now: float | None = None) -> None:
        self.last_recovery = time.monotonic() if now is None else now
        self.transient_streak = 0
        self.timeout_streak = 0

    # ---------------- convenience ----------------
    @staticmethod
    def to_volts(raw: np.ndarray, scale_to_v: float) -> np.ndarray:
        """Interleaved int16 -> complex128 volts (I + jQ)."""
        x = (np.asarray(raw)[0::2].astype(np.float64)
             + 1j * np.asarray(raw)[1::2].astype(np.float64))
        return x * (scale_to_v or 1.0)
