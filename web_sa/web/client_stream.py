"""Per-client bounded WebSocket output with latest-frame backpressure."""
from __future__ import annotations

import asyncio
import logging
from collections import deque

from .jsonutil import dumps_json, is_periodic_status

log = logging.getLogger(__name__)
SEND_TIMEOUT = 5.0

#: Retention policy per frame type (report finding E-4). A new frame type is one row here
#: instead of another branch in the send path; anything unlisted behaves like data.
RETAIN = 'retain'      # keep the newest and drop older ones (a frequency axis is context)
LATEST = 'latest'      # newest wins, the previous frame is dropped (traces, bitmaps)
FIFO = 'fifo'          # ordered queue, drop-oldest on overrun (audio)
FRAME_POLICY = {
    b'FREQ': RETAIN,
    b'AUDF': FIFO,
    b'POWR': LATEST,
    b'RTAF': LATEST,
    b'VSAD': LATEST,
}
DEFAULT_POLICY = LATEST
#: Marker the connection filters use (audio has its own socket in the frontend).
AUDIO_MAGIC = b'AUDF'


class ClientStream:
    """Own the only writer for one WebSocket.

    Control JSON messages are bounded and STATUS is coalesced. Frequency frames are
    retained separately so a dropped power frame can never orphan a new frequency axis.
    High-rate frames (POWR/RTAF/VSAD) use latest-wins semantics *per frame type*: a VSA
    capture publishes a spectrum (RTAF) and a measurement (VSAD) together, so one slot per
    magic keeps the newest of each instead of letting them evict each other. SDR audio
    (AUDF) is kept in a small FIFO so it is never reordered/dropped in normal operation
    (drop-oldest only on overrun, to bound latency).
    """

    CONTROL_LIMIT = 32
    AUDIO_LIMIT = 20          # 400 ms of 20 ms frames; seq=0 flushes stale audio

    def __init__(self, ws, audio_only: bool = False, no_audio: bool = False):
        self.ws = ws
        # Per-connection stream filter. The main UI connection uses no_audio (it never
        # handles AUDF); the SDR audio worker uses audio_only so it is not fed the display
        # frames. Default keeps the historical behaviour (everything).
        self.audio_only = audio_only
        self.no_audio = no_audio
        self._control: deque[str] = deque()
        self._freq: bytes | None = None
        #: Newest frame per magic, in first-inserted order (insertion order is the send
        #: order, so a slow client still gets each frame type in a fair rotation).
        self._data: dict[bytes, bytes] = {}
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
        if self.audio_only and magic != AUDIO_MAGIC:
            return
        if self.no_audio and magic == AUDIO_MAGIC:
            return
        policy = FRAME_POLICY.get(magic, DEFAULT_POLICY)
        if policy is RETAIN:
            self._freq = frame
        elif policy is FIFO:
            if len(frame) < 16:
                self.dropped_audio += 1
                return
            seq = int.from_bytes(frame[4:8], 'little')
            if seq == 0:
                self.dropped_audio += len(self._audio)
                self._audio.clear()
            self._audio.append(frame)
            if len(self._audio) > self.AUDIO_LIMIT:
                self._audio.popleft()
                self.dropped_audio += 1
        else:
            if magic in self._data:
                self.dropped_frames += 1
            self._data[magic] = frame
        self._event.set()

    def publish_json(self, obj: dict) -> None:
        """Serialize and queue one message for this client."""
        self.publish_text(dumps_json(obj), coalesce=is_periodic_status(obj))

    def publish_text(self, text: str, *, coalesce: bool = False) -> None:
        """Queue an already-serialized message.

        The publisher uses this to serialize once for all clients; ``coalesce`` marks the
        1 Hz periodic STATUS, which supersedes an older queued one.
        """
        if self.closed or self.audio_only:
            return
        if coalesce:
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
                    elif self._data:
                        magic, frame = next(iter(self._data.items()))
                        del self._data[magic]
                        await asyncio.wait_for(self.ws.send_bytes(frame), timeout=SEND_TIMEOUT)
                    else:
                        break
        except asyncio.CancelledError:
            raise
        except Exception:
            log.debug('WebSocket sender stopped', exc_info=True)
            self.closed = True
            await self.ws.close()
