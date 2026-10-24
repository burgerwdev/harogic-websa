"""No native SDK threads survive stop; old-generation IQ cannot enter new tune."""
import ctypes as C
import threading
from types import SimpleNamespace

import numpy as np
import pytest

from tools.reference import sdk_bindings as sb
from tools.reference.sdr_capture import SdrCapture


def test_capture_serializes_sdk_and_stops_with_full_queue(monkeypatch):
    raw = (C.c_int16 * 8)(1, -1, 2, -2, 3, -3, 4, -4)
    calls = []

    def fetch(_dev, stream):
        calls.append(1)
        stream.contents.IQS_StreamInfo.PacketSamples = 4
        stream.contents.AlternIQStream = C.cast(raw, C.POINTER(C.c_void_p))
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
        assert capture.queue_wait_s >= 0
    finally:
        capture.stop()
    assert capture.thread is None
    assert capture.get(timeout=0.01) is None or capture.stop_event.is_set()


def test_capture_rejects_invalid_sdk_packet(monkeypatch):
    def fetch(_dev, stream):
        stream.contents.IQS_StreamInfo.PacketSamples = 99
        return 0

    monkeypatch.setattr(sb.dll, 'IQS_GetIQStream_PM1', fetch)
    capture = SdrCapture(SimpleNamespace(dev=sb.c_void_p(1)), threading.RLock(), 4)
    capture.start()
    try:
        with pytest.raises(RuntimeError, match='capture failed'):
            capture.get(timeout=1)
    finally:
        capture.stop()
