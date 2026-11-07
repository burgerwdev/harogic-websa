"""No native SDK threads survive stop; old-generation IQ cannot enter new tune."""
import ctypes as C
import sys
import threading
from types import SimpleNamespace

import numpy as np
import pytest

from tools import reference
from tools.reference.sdr_capture import SdrCapture


class DataFormat(C.c_int):
    Complex16bit = 0


class Profile(C.Structure):
    _fields_ = [('DataFormat', DataFormat)]


class StreamInfo(C.Structure):
    _fields_ = [('PacketSamples', C.c_uint32), ('PacketDataSize', C.c_uint32)]


class Stream(C.Structure):
    # Only the fields used by the reference reader. No vendor library or hardware
    # participates in these tests; this is not an SDK ABI/layout check.
    _fields_ = [('AlternIQStream', C.c_void_p), ('IQS_Profile', Profile),
                ('IQS_StreamInfo', StreamInfo)]


@pytest.fixture
def sb(monkeypatch):
    sdk = SimpleNamespace(IQStream_TypeDef=Stream, pointer=C.pointer, c_void_p=C.c_void_p,
                          DataFormat_TypeDef=DataFormat,
                          dll=SimpleNamespace(IQS_GetIQStream_PM1=None))
    monkeypatch.setitem(sys.modules, 'tools.reference.sdk_bindings', sdk)
    monkeypatch.setattr(reference, 'sdk_bindings', sdk, raising=False)
    return sdk


def test_capture_serializes_sdk_and_stops_with_full_queue(monkeypatch, sb):
    raw = (C.c_int16 * 8)(1, -1, 2, -2, 3, -3, 4, -4)
    calls = []

    def fetch(_dev, stream):
        calls.append(1)
        stream.contents.IQS_StreamInfo.PacketSamples = 4
        stream.contents.IQS_StreamInfo.PacketDataSize = C.sizeof(raw)
        stream.contents.AlternIQStream = C.addressof(raw)
        return 0

    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fetch)
    lock = threading.RLock()
    dev = SimpleNamespace(dev=sb.c_void_p(1))
    capture = SdrCapture(dev, lock, 4)
    with lock:
        capture.start()
        assert not calls
    try:
        first = capture.get(timeout=1)
        assert first is not None and first.index == 0 and first.epoch == 0
        np.testing.assert_array_equal(first.iq, [1, -1, 2, -2, 3, -3, 4, -4])
        assert not first.iq.flags.writeable
        capture.pause()
        count_after_pause = len(calls)
        threading.Event().wait(0.08)
        assert len(calls) <= count_after_pause + 2  # one in-flight SDK read, one queued packet
        assert not capture.active.is_set()
        capture.resume()
        with lock:
            capture.bump_epoch()
        newest = capture.get(timeout=1)
        assert newest is not None and newest.epoch == 1 and newest.index >= 0
        assert capture.queue_highwater <= 2
        assert capture.fetch_calls >= 2
        assert capture.captured_samples >= 8
    finally:
        capture.stop()
    assert capture.thread is None
    calls_at_stop = len(calls)
    threading.Event().wait(0.02)
    assert len(calls) == calls_at_stop


@pytest.mark.parametrize(('samples', 'size', 'data_format', 'address'), [
    (0, 0, 0, 1), (1, 4, 0, 1), (5, 20, 0, 1), (2**32 - 1, 12, 0, 1),
    (4, 0, 0, 1), (4, 15, 0, 1), (4, 17, 0, 1), (4, 2**32 - 1, 0, 1),
    (4, 16, 1, 1), (4, 16, 2, 1), (4, 16, 3, 1), (4, 16, 6, 1), (4, 16, 99, 1),
    (4, 16, 0, 0),
])
def test_capture_rejects_invalid_sdk_packet_before_copy(
        monkeypatch, sb, samples, size, data_format, address):
    copied = []

    def copy(*args):
        copied.append(args)
        return bytes(16)

    def fetch(_dev, stream):
        stream.contents.IQS_StreamInfo.PacketSamples = samples
        stream.contents.IQS_StreamInfo.PacketDataSize = size
        stream.contents.IQS_Profile.DataFormat = data_format
        stream.contents.AlternIQStream = address
        return 0

    monkeypatch.setattr(C, 'string_at', copy)
    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fetch)
    lock = threading.Lock()
    capture = SdrCapture(SimpleNamespace(dev=sb.c_void_p(1)), lock, 4)
    capture.start()
    try:
        capture.thread.join(timeout=0.2)
        assert not copied, 'untrusted SDK metadata reached ctypes.string_at'
        with pytest.raises(RuntimeError, match='capture failed'):
            capture.get(timeout=1)
        assert capture.sample_index == capture.captured_samples == 0
        assert capture.queue.empty()
    finally:
        capture.stop()
    assert lock.acquire(timeout=0.1), 'failed reader retained hardware lock'
    lock.release()
    assert capture.thread is None


@pytest.mark.parametrize('packet_samples', [0, 1, -1, 2**32, 2**63, 1.5, float('nan'),
                                           float('inf'), True])
def test_capture_rejects_unbounded_configuration(packet_samples):
    with pytest.raises(ValueError, match='packet_samples'):
        SdrCapture(None, threading.Lock(), packet_samples)


def test_capture_can_recover_with_new_owner_after_bad_packet(monkeypatch, sb):
    raw = (C.c_int16 * 8)(1, -1, 2, -2, 3, -3, 4, -4)
    malformed = True

    def fetch(_dev, stream):
        stream.contents.AlternIQStream = C.addressof(raw)
        stream.contents.IQS_StreamInfo.PacketSamples = 4
        stream.contents.IQS_StreamInfo.PacketDataSize = 1 if malformed else 16
        return 0

    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fetch)
    dev, lock = SimpleNamespace(dev=sb.c_void_p(1)), threading.Lock()
    failed = SdrCapture(dev, lock, 4)
    failed.start()
    try:
        failed.thread.join(timeout=0.2)
        with pytest.raises(RuntimeError, match='capture failed'):
            failed.get(timeout=0.1)
    finally:
        failed.stop()
    malformed = False
    recovered = SdrCapture(dev, lock, 4)
    recovered.start()
    try:
        packet = recovered.get(timeout=1)
        assert packet is not None and packet.index == 0
        np.testing.assert_array_equal(packet.iq, raw)
    finally:
        recovered.stop()


def test_waiting_consumer_observes_capture_failure(monkeypatch, sb):
    fetching, release_fetch, waiting = (threading.Event() for _ in range(3))

    def fetch(_dev, _stream):
        fetching.set()
        assert release_fetch.wait(1)
        return 0  # Empty, invalid SDK packet.

    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fetch)
    capture = SdrCapture(SimpleNamespace(dev=sb.c_void_p(1)), threading.Lock(), 4)
    original_get = capture.queue.get

    def get(**kwargs):
        waiting.set()
        return original_get(**kwargs)

    monkeypatch.setattr(capture.queue, 'get', get)
    observed = []

    def consume():
        try:
            observed.append(capture.get(timeout=0.1))
        except RuntimeError as exc:
            observed.append(exc)

    capture.start()
    consumer = threading.Thread(target=consume)
    try:
        assert fetching.wait(1)
        consumer.start()
        assert waiting.wait(1)
        release_fetch.set()
        consumer.join(timeout=1)
        assert len(observed) == 1 and isinstance(observed[0], RuntimeError)
        assert isinstance(observed[0].__cause__, RuntimeError)
    finally:
        release_fetch.set()
        capture.stop()
        if consumer.ident is not None:
            consumer.join(timeout=1)


def test_stop_keeps_sdk_owner_until_blocked_fetch_returns(monkeypatch, sb):
    entered, release = threading.Event(), threading.Event()

    def fetch(_dev, _stream):
        entered.set()
        assert release.wait(5)
        return -10

    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fetch)
    lock = threading.Lock()
    capture = SdrCapture(SimpleNamespace(dev=sb.c_void_p(1)), lock, 4)
    capture.start()
    try:
        assert entered.wait(1)
        with pytest.raises(RuntimeError, match='refusing concurrent Device_Close'):
            capture.stop()
        assert capture.thread is not None and capture.thread.is_alive()
        assert not lock.acquire(blocking=False), 'SDK owner must retain the hardware lock'
    finally:
        release.set()
        capture.stop()
    assert capture.thread is None and capture.queue.empty()
    assert lock.acquire(timeout=0.1)
    lock.release()
