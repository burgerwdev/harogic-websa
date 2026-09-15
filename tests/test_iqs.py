"""IQS shared layer: configuration, the post-configuration drain, transient retries and
the wedge decision, all without hardware.

A fake ``sdk_bindings`` stands in for the vendor library: it records every call and fills
the profile/stream structures the way the real ``IQS_Configuration`` does. ``iqs.py``
resolves the SDK lazily, so this module imports and runs on a machine with no
``libhtraapi`` (that is what puts these gates in CI rather than only on the bench).
"""
from __future__ import annotations

import ctypes
from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.measurements import iqs as I


class _Enum:
    """Stand-in for an htra_api enum member (only ``.value`` is ever read)."""

    def __init__(self, name, value=0):
        self.name = name
        self.value = value

    def __repr__(self):
        return f'<{self.name}>'


class _Profile:
    """Permissive profile/stream struct: every field exists, none is pre-set."""

    def __init__(self, **kw):
        self._fields = {}
        self.__dict__.update(kw)
        self.NativeIQSampleRate_SPS = 62.5e6

    def __getattr__(self, name):                     # unset fields read as 0
        if name.startswith('_'):
            raise AttributeError(name)
        return 0


class _StreamInfo:
    def __init__(self, packet_samples=8):
        self.PacketSamples = packet_samples
        self.PacketDataSize = packet_samples * 4
        self.IQSampleRate = 3.90625e6
        self.Bandwidth = 3.125e6
        self.PacketCount = 1
        self.StreamSamples = packet_samples


class _Stream:
    def __init__(self, samples=8, scale=2e-5):
        self.IQS_StreamInfo = _StreamInfo(samples)
        self.IQS_ScaleToV = scale
        self.IQS_Profile = _Profile()
        self._buf = (ctypes.c_int16 * (samples * 2))(*([1, -1] * samples))
        self.AlternIQStream = ctypes.cast(self._buf,
                                          ctypes.POINTER(ctypes.c_void_p))


class _Dll:
    """Scripted SDK entry points; every call is recorded on the fake sdk."""

    def __init__(self, sdk):
        self.sdk = sdk

    def IQS_ProfileDeInit(self, _dev, _p):
        self.sdk.calls.append('IQS_ProfileDeInit')
        return 0

    def SWP_ProfileDeInit(self, _dev, _p):
        self.sdk.calls.append('SWP_ProfileDeInit')
        return 0

    def SWP_Configuration(self, _dev, _p, _o, _ti):
        self.sdk.calls.append('SWP_Configuration')
        return self.sdk.next_status('SWP_Configuration')

    def IQS_Configuration(self, _dev, p, out, info):
        self.sdk.calls.append('IQS_Configuration')
        st = self.sdk.next_status('IQS_Configuration')
        if st != 0:
            return st
        out.CenterFreq_Hz = p.CenterFreq_Hz
        out.DecimateFactor = p.DecimateFactor
        out.Atten = getattr(p, 'Atten', 0)
        out.Preamplifier = _Enum('preamp', 0)
        out.IFGainGrade = getattr(p, 'IFGainGrade', 0)
        out.ReferenceClockSource = _Enum('refclk', 0)
        out.EnableReferenceClockOut = 0
        info.IQSampleRate = self.sdk.fs
        info.Bandwidth = self.sdk.bandwidth
        info.PacketSamples = self.sdk.packet_samples
        info.PacketDataSize = self.sdk.packet_samples * 4
        info.PacketCount = 1
        info.StreamSamples = self.sdk.packet_samples
        return 0

    def IQS_BusTriggerStart(self, _dev):
        self.sdk.calls.append('start')
        return 0

    def IQS_BusTriggerStop(self, _dev):
        self.sdk.calls.append('stop')
        return self.sdk.stop_status

    def IQS_GetIQStream_PM1(self, _dev, stream):
        self.sdk.calls.append('fetch')
        return self.sdk.next_fetch()


class FakeSdk:
    def __init__(self, fetches=(), fs=3.90625e6, bandwidth=3.125e6, packet_samples=8):
        self.calls: list[str] = []
        self.fetches = list(fetches)          # statuses for IQS_GetIQStream_PM1
        self.statuses: dict[str, list[int]] = {}
        self.fs = fs
        self.bandwidth = bandwidth
        self.packet_samples = packet_samples
        self.stop_status = 0
        self.WARN_STATUS = frozenset({-8, -9, -10, -12})
        self.dll = _Dll(self)
        self.enums = {
            'DataFormat_TypeDef': {n: _Enum(n) for n in
                                   ('Complex8bit', 'Complex16bit', 'Complex32bit',
                                    'Complexfloat')},
            'TriggerMode_TypeDef': {n: _Enum(n) for n in ('Adaptive', 'FixedPoints')},
            'DCCancelerMode_TypeDef': {n: _Enum(n) for n in
                                       ('DCCOff', 'DCCHighPassFilterMode',
                                        'DCCManualOffsetMode', 'DCCAutoOffsetMode')},
            'QDCMode_TypeDef': {n: _Enum(n) for n in ('QDCOff', 'QDCAutoMode',
                                                      'QDCManualMode')},
            'PreamplifierState_TypeDef': {n: _Enum(n) for n in ('AutoOn', 'ForcedOff')},
            'GainStrategy_TypeDef': {n: _Enum(n) for n in ('LowNoisePreferred',
                                                           'HighLinearityPreferred')},
            'IQS_TriggerSource_TypeDef': {n: _Enum(n) for n in ('Bus',)},
        }
        self._stream = _Stream(packet_samples)

    # -- enum access: sdk.DataFormat_TypeDef.Complex16bit --
    def __getattr__(self, name):
        if name in self.enums:
            return SimpleNamespace(**self.enums[name])
        raise AttributeError(name)

    # -- helpers the test drives --
    def script(self, name, statuses):
        self.statuses[name] = list(statuses)

    def next_status(self, name):
        seq = self.statuses.get(name)
        return seq.pop(0) if seq else 0

    def next_fetch(self):
        return self.fetches.pop(0) if self.fetches else 0

    # -- structs --
    def IQS_Profile_TypeDef(self):
        return _Profile()

    def IQS_StreamInfo_TypeDef(self):
        return _StreamInfo(self.packet_samples)

    def SWP_Profile_TypeDef(self):
        return _Profile()

    def SWP_TraceInfo_TypeDef(self):
        return _Profile()

    def IQStream_TypeDef(self):
        return _Stream(self.packet_samples)

    @staticmethod
    def pointer(obj):
        return obj


class FakeDev:
    def __init__(self):
        self.dev = object()
        self.dsp = object()
        self.state = SimpleNamespace(last_error='')


@pytest.fixture
def sdk():
    return FakeSdk()


@pytest.fixture
def stream(sdk):
    return I.IqsStream(FakeDev(), sdk=sdk, ready_delay=0.0)


# ---------------- configuration ----------------

def test_profile_carries_the_requested_geometry(stream, sdk):
    p = stream.profile(center_hz=100.2e6, decimate=16, ref_level_dbm=-10.0,
                       trigger_mode='fixed_points', trigger_length=262144,
                       bus_timeout_ms=5000, dcc='high_pass', qdc='off')
    assert p.CenterFreq_Hz == 100.2e6
    assert p.RefLevel_dBm == -10.0
    assert p.DecimateFactor == 16
    assert p.TriggerMode.name == 'FixedPoints'
    assert p.TriggerLength == 262144
    assert p.BusTimeout_ms == 5000
    assert p.DCCancelerMode.name == 'DCCHighPassFilterMode'
    assert p.QDCMode.name == 'QDCOff'
    assert 'IQS_ProfileDeInit' in sdk.calls


def test_configure_reports_stream_info(stream):
    info = stream.configure(stream.profile(center_hz=100e6, decimate=16))
    assert (info.fs, info.bandwidth, info.packet_samples) == (3.90625e6, 3.125e6, 8)
    assert info.valid()
    assert stream.packet_samples == 8


def test_configure_resets_the_device_mode_first(stream, sdk):
    """Without the SWP_Configuration reset a reconfiguration wedges the stream."""
    stream.configure(stream.profile(center_hz=100e6, decimate=16))
    assert sdk.calls.index('SWP_Configuration') < sdk.calls.index('IQS_Configuration')


def test_configure_rejects_invalid_stream_info(stream, sdk):
    sdk.fs = 0
    with pytest.raises(RuntimeError, match='invalid stream info'):
        stream.configure(stream.profile(center_hz=100e6, decimate=16))


def test_sdk_call_retries_the_transient_bus_warnings():
    seq = [-10, -11, 0]
    slept = []
    assert I.sdk_call(lambda: seq.pop(0), 'IQS_Configuration',
                      sleep=slept.append) == 0
    assert slept == [I.BUS_RETRY_DELAY, I.BUS_RETRY_DELAY]


def test_sdk_call_raises_after_the_retries_and_on_fatal_statuses():
    with pytest.raises(RuntimeError, match=r'X status=-11'):
        I.sdk_call(lambda: -11, 'X', sleep=lambda _s: None)
    calls = []
    with pytest.raises(RuntimeError, match=r'X status=-5'):
        I.sdk_call(lambda: (calls.append(1), -5)[1], 'X', sleep=lambda _s: None)
    assert len(calls) == 1                     # a fatal status is not retried


# ---------------- stream ----------------

def test_fetch_returns_samples_and_volts(stream):
    r = stream.fetch()
    assert r.ok and r.samples == 8
    volts = I.IqsStream.to_volts(r.raw, r.scale_to_v)
    assert volts.dtype == np.complex128
    assert volts[0].real == pytest.approx(2e-5, rel=1e-6)      # I sample * ScaleToV
    assert volts[0].imag == pytest.approx(-2e-5, rel=1e-6)     # Q sample * ScaleToV
    assert stream.packets_ok == 1 and stream.packets_err == 0


def test_fetch_survives_a_raising_sdk_call(stream, sdk):
    sdk.dll.IQS_GetIQStream_PM1 = lambda *a: (_ for _ in ()).throw(RuntimeError('hang'))
    r = stream.fetch()
    assert not r.ok and r.status == -1 and 'hang' in r.error
    assert stream.packets_err == 1


def test_a_transient_run_asks_for_recovery_only_after_the_limits(sdk):
    sdk.fetches = [-9] * (I.TRANSIENT_STREAK_LIMIT + 2)
    s = I.IqsStream(FakeDev(), sdk=sdk, ready_delay=0.0)
    verdicts = [s.fetch(now=100.0).recover for _ in range(I.TRANSIENT_STREAK_LIMIT)]
    assert verdicts[:-1] == [False] * (I.TRANSIENT_STREAK_LIMIT - 1)
    assert verdicts[-1] is True                # the 60th consecutive bad packet
    assert s.transient_streak == I.TRANSIENT_STREAK_LIMIT


def test_three_consecutive_timeouts_are_enough(sdk):
    sdk.fetches = [-10, -10, -10]
    s = I.IqsStream(FakeDev(), sdk=sdk, ready_delay=0.0)
    assert [s.fetch(now=100.0).recover for _ in range(3)] == [False, False, True]
    assert s.timeout_streak == I.TIMEOUT_STREAK_LIMIT


def test_a_good_packet_clears_the_streaks(sdk):
    sdk.fetches = [-9, -9, 0, -9]
    s = I.IqsStream(FakeDev(), sdk=sdk, ready_delay=0.0)
    s.fetch(now=100.0)
    s.fetch(now=100.0)
    assert s.transient_streak == 2
    s.fetch(now=100.0)
    assert s.transient_streak == 0 and s.timeout_streak == 0
    assert s.last_ok == 100.0
    assert s.fetch(now=100.0).recover is False


def test_recovery_cooldown_blocks_a_second_recovery(sdk):
    sdk.fetches = [-9] * (I.TRANSIENT_STREAK_LIMIT * 2)
    s = I.IqsStream(FakeDev(), sdk=sdk, ready_delay=0.0)
    for _ in range(I.TRANSIENT_STREAK_LIMIT):
        s.fetch(now=100.0)
    assert s.fetch(now=100.0).recover is True      # limit reached, cooldown long passed
    s.note_recovery(now=100.0)
    assert s.transient_streak == 0
    assert [s.fetch(now=100.0).recover
            for _ in range(I.TRANSIENT_STREAK_LIMIT)] == [False] * I.TRANSIENT_STREAK_LIMIT


def test_watchdog_fires_when_no_packet_arrives(stream):
    stream.last_ok = 10.0
    stream.last_recovery = 0.0
    assert stream.watchdog_expired(now=10.0 + I.WATCHDOG_S / 2) is False
    assert stream.watchdog_expired(now=10.0 + I.WATCHDOG_S * 2) is True


# ---------------- settle / drain ----------------

def test_settle_window_is_armed_by_configure(sdk):
    s = I.IqsStream(FakeDev(), sdk=sdk, ready_delay=0.2)
    s.configure(s.profile(center_hz=100e6, decimate=16), now=50.0)
    assert s.settling(now=50.1) is True
    assert s.settling(now=50.3) is False


def test_drain_fetches_until_the_settle_window_ends(stream, sdk, monkeypatch):
    """The measured 0.25 s of discarded packets: without them a rate change failed."""
    clock = [100.0]
    monkeypatch.setattr(I.time, 'monotonic', lambda: clock[0])
    fetches = []

    def fake_fetch(_dev, _stream):
        fetches.append(clock[0])
        clock[0] += 0.01
        return 0

    monkeypatch.setattr(sdk.dll, 'IQS_GetIQStream_PM1', fake_fetch)
    stream.arm_settle(now=100.0, delay=0.05)
    n = stream.drain()
    assert n == len(fetches)
    assert n >= 5                                   # kept fetching until the window closed
    assert clock[0] >= 100.05


def test_stop_swallows_a_failure_only_when_not_required(stream, sdk):
    sdk.stop_status = -5
    stream.stop(required=False)                # exit path: never raise
    with pytest.raises(RuntimeError, match='IQS_BusTriggerStop status=-5'):
        stream.stop()
