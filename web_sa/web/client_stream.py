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
    b'IQBF': FIFO,
    b'POWR': LATEST,
    b'RTAF': LATEST,
}
DEFAULT_POLICY = LATEST
#: Markers the connection filters use (audio and baseband have their own sockets in the frontend).
AUDIO_MAGIC = b'AUDF'
IQ_MAGIC = b'IQBF'
#: Byte offset of the u32 sequence number in each streaming (FIFO) frame. AUDF puts it first
#: (seq, rate, samples); IQBF keeps the version first (ver, seq, samples, rate, centre).
FIFO_SEQ_OFFSET = {AUDIO_MAGIC: 4, IQ_MAGIC: 8}
#: Dropped-frame counter per streaming frame type, for STATUS.stream.
FIFO_DROPPED_FIELD = {AUDIO_MAGIC: 'dropped_audio', IQ_MAGIC: 'dropped_iq'}


class ClientStream:
    """Own the only writer for one WebSocket.

    Control JSON messages are bounded and STATUS is coalesced. Frequency frames are
    retained separately so a dropped power frame can never orphan a new frequency axis.
    High-rate POWR/RTAF frames use latest-wins semantics. The SDR streaming frames (AUDF
    audio, IQBF channelized baseband) are kept in small FIFOs so they are never
    reordered/dropped in normal operation (drop-oldest only on overrun, to bound latency).
    """

    CONTROL_LIMIT = 32
    AUDIO_LIMIT = 20          # 400 ms of 20 ms frames; seq=0 flushes stale audio
    #: Baseband blocks are per acquisition step (~8 ms at the DDC output rate). The reader on the
    #: browser side is a dedicated worker that never blocks (sdr/ft8Worker.ts owns only the
    #: socket; the WASM decode runs in a second worker fed over a MessagePort), so the FIFO only
    #: has to ride out scheduling jitter — 48 blocks ~= 0.4 s. History: when the decode ran on
    #: the socket-owning thread, seconds-long decodes stalled the reader, this FIFO overran, and
    #: every decode attempt came back as a sequence gap that reset the decoder's window
    #: (measured live: 8 of ~18 slots decoded, `dropped=2142`). Raising the bound was the wrong
    #: fix — splitting the workers was the right one (measured after: 14 of 14 slots, `dropped=0`).
    IQ_LIMIT = 48
    #: Minimum sane header length per streaming frame type (IQBF's header is 32 bytes).
    FIFO_MIN_BYTES = {AUDIO_MAGIC: 16, IQ_MAGIC: 32}

    def __init__(self, ws, audio_only: bool = False, no_audio: bool = False,
                 iq_only: bool = False, no_iq: bool = False):
        self.ws = ws
        # Per-connection stream filter. The main UI connection uses no_audio/no_iq (it
        # handles neither AUDF nor IQBF); the SDR audio worker uses audio_only and the SDR
        # IQ worker uses iq_only, so neither is fed the display frames. Default keeps the
        # historical behaviour (everything).
        self.audio_only = audio_only
        self.no_audio = no_audio
        self.iq_only = iq_only
        self.no_iq = no_iq
        self._control: deque[str] = deque()
        self._freq: bytes | None = None
        self._data: bytes | None = None
        self._audio: deque[bytes] = deque()
        self._iq: deque[bytes] = deque()
        self._event = asyncio.Event()
        self._task: asyncio.Task | None = None
        self.dropped_frames = 0
        self.dropped_control = 0
        self.dropped_audio = 0
        self.dropped_iq = 0
        self.closed = False

    @property
    def accepts_audio(self) -> bool:
        """True when this connection would actually keep an AUDF frame.

        The SDR session asks before running its Python demodulator: with the browser doing the
        demodulation (its own socket carries the channelized baseband) nobody subscribes to audio,
        and an audio chain computed for a socket that discards it would be pure CPU.
        """
        return not self.closed and not self.no_audio and not self.iq_only

    @property
    def accepts_iq(self) -> bool:
        """True when this connection would actually keep an IQBF frame.

        The publisher asks this before the session encodes an IQ block, so the raw-IQ
        encode+fan-out only happens while a browser DSP socket is really subscribed: an
        audio-only socket discards IQ, and counting it would encode megabytes for nobody.
        """
        return not self.closed and not self.no_iq and not self.audio_only

    def start(self) -> None:
        self._task = asyncio.create_task(self._sender())

    def publish_bytes(self, frame: bytes) -> None:
        if self.closed:
            return
        magic = frame[:4]
        if self.audio_only and magic != AUDIO_MAGIC:
            return
        if self.iq_only and magic != IQ_MAGIC:
            return
        if self.no_audio and magic == AUDIO_MAGIC:
            return
        if self.no_iq and magic == IQ_MAGIC:
            return
        policy = FRAME_POLICY.get(magic, DEFAULT_POLICY)
        if policy is RETAIN:
            self._freq = frame
        elif policy is FIFO:
            if len(frame) < self.FIFO_MIN_BYTES[magic]:
                self._drop_fifo(magic, 1)
                return
            queue = self._iq if magic == IQ_MAGIC else self._audio
            limit = self.IQ_LIMIT if magic == IQ_MAGIC else self.AUDIO_LIMIT
            offset = FIFO_SEQ_OFFSET[magic]
            if int.from_bytes(frame[offset:offset + 4], 'little') == 0:
                self._drop_fifo(magic, len(queue))
                queue.clear()
            queue.append(frame)
            if len(queue) > limit:
                queue.popleft()
                self._drop_fifo(magic, 1)
        else:
            if self._data is not None:
                self.dropped_frames += 1
            self._data = frame
        self._event.set()

    def _drop_fifo(self, magic: bytes, count: int) -> None:
        """Count dropped streaming frames on the same counter STATUS.stream reports."""
        field = FIFO_DROPPED_FIELD[magic]
        setattr(self, field, getattr(self, field) + count)

    def publish_json(self, obj: dict) -> None:
        """Serialize and queue one message for this client."""
        self.publish_text(dumps_json(obj), coalesce=is_periodic_status(obj))

    def publish_text(self, text: str, *, coalesce: bool = False) -> None:
        """Queue an already-serialized message.

        The publisher uses this to serialize once for all clients; ``coalesce`` marks the
        1 Hz periodic STATUS, which supersedes an older queued one.
        """
        if self.closed or self.audio_only or self.iq_only:
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
                    elif self._iq:
                        await asyncio.wait_for(
                            self.ws.send_bytes(self._iq.popleft()), timeout=SEND_TIMEOUT)
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
