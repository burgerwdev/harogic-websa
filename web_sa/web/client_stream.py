"""Per-client bounded WebSocket output with latest-frame backpressure."""
from __future__ import annotations

import asyncio
import json
import logging
from collections import deque

log = logging.getLogger(__name__)
SEND_TIMEOUT = 5.0


class ClientStream:
    """Own the only writer for one WebSocket.

    Control JSON messages are bounded and STATUS is coalesced. Frequency frames are
    retained separately so a dropped power frame can never orphan a new frequency axis.
    High-rate POWR/RTAF frames use latest-wins semantics.
    """

    CONTROL_LIMIT = 32

    def __init__(self, ws):
        self.ws = ws
        self._control: deque[str] = deque()
        self._freq: bytes | None = None
        self._data: bytes | None = None
        self._event = asyncio.Event()
        self._task: asyncio.Task | None = None
        self.dropped_frames = 0
        self.dropped_control = 0
        self.closed = False

    def start(self) -> None:
        self._task = asyncio.create_task(self._sender())

    def publish_bytes(self, frame: bytes) -> None:
        if self.closed:
            return
        if frame[:4] == b'FREQ':
            self._freq = frame
        else:
            if self._data is not None:
                self.dropped_frames += 1
            self._data = frame
        self._event.set()

    def publish_json(self, obj: dict) -> None:
        if self.closed:
            return
        text = json.dumps(obj, allow_nan=False, separators=(',', ':'))
        if obj.get('cmd') == 'STATUS':
            self._control = deque(
                item for item in self._control if '"cmd":"STATUS"' not in item)
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
