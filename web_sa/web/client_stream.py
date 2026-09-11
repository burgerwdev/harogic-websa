"""Per-client bounded WebSocket output with latest-frame backpressure."""
from __future__ import annotations

import asyncio
import json
import logging
import math
from collections import deque

log = logging.getLogger(__name__)
SEND_TIMEOUT = 5.0


def _finite_json(value):
    """Replace NaN/Inf with null so one bad measurement value cannot drop a whole message."""
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {key: _finite_json(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_finite_json(item) for item in value]
    return value


class ClientStream:
    """Own the only writer for one WebSocket.

    Control JSON messages are bounded and STATUS is coalesced. Frequency frames are
    retained separately so a dropped power frame can never orphan a new frequency axis.
    High-rate POWR/RTAF frames use latest-wins semantics. SDR audio (AUDF) is kept in a
    small FIFO so it is never reordered/dropped in normal operation (drop-oldest only on
    overrun, to bound latency).
    """

    CONTROL_LIMIT = 32
    AUDIO_LIMIT = 80          # ~1.6 s of 20 ms frames

    def __init__(self, ws):
        self.ws = ws
        self._control: deque[str] = deque()
        self._freq: bytes | None = None
        self._data: bytes | None = None
        self._audio: deque[bytes] = deque()
        self._event = asyncio.Event()
        self._task: asyncio.Task | None = None
        self.dropped_frames = 0
        self.dropped_control = 0
        self.dropped_audio = 0
        self.closed = False

    def start(self) -> None:
        self._task = asyncio.create_task(self._sender())

    def publish_bytes(self, frame: bytes) -> None:
        if self.closed:
            return
        magic = frame[:4]
        if magic == b'FREQ':
            self._freq = frame
        elif magic == b'AUDF':
            self._audio.append(frame)
            if len(self._audio) > self.AUDIO_LIMIT:
                self._audio.popleft()
                self.dropped_audio += 1
        else:
            if self._data is not None:
                self.dropped_frames += 1
            self._data = frame
        self._event.set()

    def publish_json(self, obj: dict) -> None:
        if self.closed:
            return
        text = json.dumps(_finite_json(obj), allow_nan=False, separators=(',', ':'))
        if obj.get('cmd') == 'STATUS' and not obj.get('response_to'):
            self._control = deque(
                item for item in self._control
                if '"cmd":"STATUS"' not in item or '"response_to":' in item
            )
        if len(self._control) >= self.CONTROL_LIMIT:
            self._control.popleft()
            self.dropped_control += 1
        self._control.append(text)
        self._event.set()

    async def close(self) -> None:
        self.closed = True
        self._event.set()
        if self._task is not None:
            self._task.cancel()
            await asyncio.gather(self._task, return_exceptions=True)

    async def _sender(self) -> None:
        try:
            while not self.closed:
                await self._event.wait()
                self._event.clear()
                while not self.closed:
                    if self._control:
                        await asyncio.wait_for(
                            self.ws.send_str(self._control.popleft()), timeout=SEND_TIMEOUT)
                    elif self._freq is not None:
                        frame, self._freq = self._freq, None
                        await asyncio.wait_for(self.ws.send_bytes(frame), timeout=SEND_TIMEOUT)
                    elif self._audio:
                        await asyncio.wait_for(
                            self.ws.send_bytes(self._audio.popleft()), timeout=SEND_TIMEOUT)
                    elif self._data is not None:
                        frame, self._data = self._data, None
                        await asyncio.wait_for(self.ws.send_bytes(frame), timeout=SEND_TIMEOUT)
                    else:
                        break
        except asyncio.CancelledError:
            raise
        except Exception:
            log.debug('WebSocket sender stopped', exc_info=True)
            self.closed = True
            await self.ws.close()
