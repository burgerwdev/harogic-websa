"""Bounded IQS capture for offline Python reference tools (not the Rust service).

One SDK reader owns each packet until copied. Consumers see immutable IQ, a
configuration epoch, and a monotonic sample index. Queue saturation is counted
and backpressures the reader until the consumer catches up or capture stops.
"""
from __future__ import annotations

import ctypes as C
import operator
import queue
import threading
import time
from dataclasses import dataclass

import numpy as np

# A reference packet may own at most 4 MiB; SDK metadata never sizes an
# unbounded ctypes read. The normal SAN packet contains 16,240 complex samples.
MAX_PACKET_BYTES = 4 * 1024 * 1024
IQ_SAMPLE_BYTES = 2 * C.sizeof(C.c_int16)


def validate_iq_packet(stream, max_samples: int, expected_format: int,
                       sample_bytes: int = IQ_SAMPLE_BYTES) -> tuple[int, int]:
    """Validate SDK metadata before a reference tool reads foreign IQ memory."""
    count = int(stream.IQS_StreamInfo.PacketSamples)
    size = int(stream.IQS_StreamInfo.PacketDataSize)
    data_format = stream.IQS_Profile.DataFormat.value
    if (sample_bytes not in (2, 4) or not 2 <= count <= max_samples
            or size > MAX_PACKET_BYTES or not stream.AlternIQStream
            or data_format != expected_format or size != count * sample_bytes):
        raise RuntimeError(
            f'IQS invalid packet: samples={count}, bytes={size}, format={data_format}')
    return count, size


@dataclass(frozen=True)
class CapturedIq:
    epoch: int
    index: int
    status: int
    stream: object
    iq: np.ndarray | None
    received_at: float


class SdrCapture:
    def __init__(self, dev, lock, packet_samples: int):
        try:
            bounded_samples = operator.index(packet_samples)
        except TypeError as exc:
            raise ValueError('packet_samples must be an integer') from exc
        if isinstance(packet_samples, bool) or not 2 <= bounded_samples <= MAX_PACKET_BYTES // IQ_SAMPLE_BYTES:
            raise ValueError('packet_samples exceeds the reference capture packet budget')
        self.dev = dev
        self.lock = lock
        self.packet_samples = bounded_samples
        self.queue: queue.Queue[CapturedIq] = queue.Queue(maxsize=2)
        self.stop_event = threading.Event()
        self.active = threading.Event()
        self.active.set()
        self.thread: threading.Thread | None = None
        self.epoch = 0
        self.sample_index = 0
        self.queue_blocked = 0
        self.queue_highwater = 0
        self.queue_full = 0
        self.queue_wait_s = 0.0
        self.lock_wait_s = 0.0
        self.fetch_s = 0.0
        self.fetch_max_s = 0.0
        self.fetch_calls = 0
        self.captured_samples = 0
        self.idle_discarded = 0
        self.failure: Exception | None = None

    def start(self):
        if self.thread is not None:
            raise RuntimeError('capture already started')
        self.thread = threading.Thread(target=self._run, name='sdr-iqs-capture')
        self.thread.start()

    def bump_epoch(self):
        # Called with the hardware lock while reconfiguring the IQS stream.
        self.epoch += 1
        self.sample_index = 0

    def pause(self):
        if not self.active.is_set():
            return
        self.active.clear()
        while True:
            try:
                self.queue.get_nowait()
                self.idle_discarded += 1
            except queue.Empty:
                break

    def resume(self):
        self.active.set()

    def signal_stop(self):
        self.stop_event.set()

    def stop(self):
        self.signal_stop()
        if self.thread is not None:
            self.thread.join(timeout=2.0)
            if self.thread.is_alive():
                raise RuntimeError('IQS capture still in SDK; refusing concurrent Device_Close')
            self.thread = None

    def get(self, timeout: float = 0.5) -> CapturedIq | None:
        end = time.monotonic() + timeout
        while True:
            if self.failure:
                raise RuntimeError('IQS capture failed') from self.failure
            remaining = end - time.monotonic()
            if remaining <= 0:
                return None
            try:
                packet = self.queue.get(timeout=remaining)
            except queue.Empty:
                if self.failure:
                    raise RuntimeError('IQS capture failed') from self.failure
                return None
            if packet.epoch == self.epoch:
                return packet
            # Explicit retune/mode reconfiguration invalidated old-frequency IQ.

    def _run(self):
        try:
            from . import sdk_bindings as sb

            while not self.stop_event.is_set():
                if not self.active.wait(timeout=0.05):
                    continue
                stream = sb.IQStream_TypeDef()
                started_wait = time.perf_counter()
                with self.lock:
                    self.lock_wait_s += time.perf_counter() - started_wait
                    if self.stop_event.is_set():
                        break
                    if not self.active.is_set():
                        continue
                    started_fetch = time.perf_counter()
                    status = int(sb.dll.IQS_GetIQStream_PM1(
                        sb.pointer(self.dev.dev), sb.pointer(stream)))
                    elapsed_fetch = time.perf_counter() - started_fetch
                    self.fetch_s += elapsed_fetch
                    self.fetch_max_s = max(self.fetch_max_s, elapsed_fetch)
                    self.fetch_calls += 1
                    epoch = self.epoch
                    now = time.monotonic()
                    iq = None
                    index = self.sample_index
                    if status == 0:
                        count, size = validate_iq_packet(
                            stream, self.packet_samples, sb.DataFormat_TypeDef.Complex16bit)
                        raw = C.string_at(stream.AlternIQStream, size)
                        iq = np.frombuffer(raw, dtype=np.int16)
                        self.sample_index += count
                        self.captured_samples += count
                packet = CapturedIq(epoch, index, status, stream, iq, now)
                while not self.stop_event.is_set():
                    try:
                        self.queue.put_nowait(packet)
                        self.queue_highwater = max(self.queue_highwater, self.queue.qsize())
                        break
                    except queue.Full:
                        self.queue_full += 1
                        started_wait = time.perf_counter()
                        try:
                            self.queue.put(packet, timeout=0.01)
                            self.queue_wait_s += time.perf_counter() - started_wait
                            self.queue_highwater = max(self.queue_highwater, self.queue.qsize())
                            break
                        except queue.Full:
                            self.queue_wait_s += time.perf_counter() - started_wait
                            self.queue_blocked += 1
        except Exception as exc:
            self.failure = exc
