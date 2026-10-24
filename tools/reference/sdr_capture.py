"""Bounded IQS capture for the opt-in wideband SDR pipeline.

One SDK reader owns each packet until copied. Consumers see immutable IQ, a
configuration epoch, and a monotonic sample index. No packet is dropped while
subscribed: queue saturation is counted and treated as a throughput failure.
"""
from __future__ import annotations

import ctypes as C
import queue
import threading
import time
from dataclasses import dataclass

import numpy as np


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
        self.dev = dev
        self.lock = lock
        self.packet_samples = packet_samples
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
                        count = int(stream.IQS_StreamInfo.PacketSamples)
                        if count < 2 or count > self.packet_samples or not stream.AlternIQStream:
                            raise RuntimeError(f'IQS invalid packet: samples={count}')
                        raw = C.string_at(stream.AlternIQStream, count * 4)
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
