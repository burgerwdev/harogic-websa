"""Bounded per-client WebSocket sender tests."""
import asyncio
import json
import struct

import pytest

import web_sa.web.client_stream as client_stream_module
from web_sa.measurements import framer
from web_sa.web.client_stream import DEFAULT_POLICY, FRAME_POLICY, ClientStream


class FakeWebSocket:
    def __init__(self):
        self.binary = []
        self.text = []
        self.first_send_started = asyncio.Event()
        self.release_first_send = asyncio.Event()
        self.block_first = False
        self.closed = False

    async def send_bytes(self, value):
        if self.block_first and not self.binary:
            self.first_send_started.set()
            await self.release_first_send.wait()
        self.binary.append(value)

    async def send_str(self, value):
        self.text.append(value)

    async def close(self):
        self.closed = True


@pytest.mark.asyncio
async def test_latest_data_frame_wins_for_slow_client():
    ws = FakeWebSocket()
    ws.block_first = True
    stream = ClientStream(ws)
    stream.start()
    stream.publish_bytes(b'POWR-one')
    await ws.first_send_started.wait()

    stream.publish_bytes(b'POWR-two')
    stream.publish_bytes(b'POWR-three')
    assert stream.dropped_frames == 1

    ws.release_first_send.set()
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert ws.binary == [b'POWR-one', b'POWR-three']
    await stream.close()


@pytest.mark.asyncio
async def test_frequency_frame_precedes_latest_power_frame():
    ws = FakeWebSocket()
    stream = ClientStream(ws)
    stream.start()
    stream.publish_bytes(b'FREQ-axis')
    stream.publish_bytes(b'POWR-old')
    stream.publish_bytes(b'POWR-new')

    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert ws.binary == [b'FREQ-axis', b'POWR-new']
    assert stream.dropped_frames == 1
    await stream.close()


@pytest.mark.asyncio
async def test_status_messages_are_coalesced():
    ws = FakeWebSocket()
    stream = ClientStream(ws)
    stream.publish_json({'cmd': 'STATUS', 'version': 1})
    stream.publish_json({'cmd': 'STATUS', 'version': 2})
    stream.start()

    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert [json.loads(item)['version'] for item in ws.text] == [2]
    await stream.close()


@pytest.mark.asyncio
async def test_command_status_is_not_coalesced_by_periodic_status():
    ws = FakeWebSocket()
    stream = ClientStream(ws)
    stream.publish_json({'cmd': 'STATUS', 'response_to': 'SET_FREQ', 'version': 2})
    stream.publish_json({'cmd': 'STATUS', 'version': 2})
    stream.start()

    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert len(ws.text) == 2
    assert json.loads(ws.text[0])['response_to'] == 'SET_FREQ'
    await stream.close()


@pytest.mark.asyncio
async def test_publish_text_uses_the_publishers_serialized_message():
    """The publisher serializes once and queues the same text on every client (P1-11)."""
    ws = FakeWebSocket()
    stream = ClientStream(ws)
    stream.publish_text('{"cmd":"STATUS","version":7}', coalesce=True)
    stream.publish_text('{"cmd":"STATUS","version":8}', coalesce=True)   # supersedes 7
    stream.start()

    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert [json.loads(item)['version'] for item in ws.text] == [8]
    await stream.close()


def test_zero_audio_sequence_flushes_queued_channel_audio():
    ws = FakeWebSocket()
    stream = ClientStream(ws)

    def audio(seq):
        return struct.pack('<4sIII', b'AUDF', seq, 48000, 0)

    stream.publish_bytes(audio(5))
    stream.publish_bytes(audio(6))
    stream.publish_bytes(audio(0))

    assert list(stream._audio) == [audio(0)]
    assert stream.dropped_audio == 2


def _iq(seq, samples=2):
    """Minimal IQDF frame: magic + ver + seq + samples + rate(f8) + centre(f8) + payload."""
    return (struct.pack('<4sIIIdd', b'IQDF', 1, seq, samples, 62.5e6 / 1024, 100.2e6)
            + b'\x00' * (samples * 4))


def test_zero_iq_sequence_flushes_the_iq_queue_not_the_audio_one():
    """IQDF carries its sequence at offset 8 (after the version), AUDF at offset 4.

    Reading the wrong offset would flush on an unrelated field: the two streaming frames keep
    their own queue and their own counter.
    """
    stream = ClientStream(FakeWebSocket())
    stream.publish_bytes(_iq(5))
    stream.publish_bytes(_iq(6))
    stream.publish_bytes(_iq(0))

    assert list(stream._iq) == [_iq(0)]
    assert stream.dropped_iq == 2
    assert stream.dropped_audio == 0
    assert not stream._audio


def test_iq_queue_is_bounded_and_drops_the_oldest_block():
    stream = ClientStream(FakeWebSocket())
    for seq in range(1, ClientStream.IQ_LIMIT + 4):
        stream.publish_bytes(_iq(seq))

    assert len(stream._iq) == ClientStream.IQ_LIMIT
    assert stream._iq[0] == _iq(4)              # drop-oldest: the newest blocks survive
    assert stream.dropped_iq == 3


def test_short_iq_frame_is_counted_and_dropped():
    stream = ClientStream(FakeWebSocket())
    stream.publish_bytes(b'IQDF' + b'\x00' * 8)   # below FIFO_MIN_BYTES[IQDF] = 32
    assert not stream._iq
    assert stream.dropped_iq == 1


def test_stream_filters_route_audio_and_iq_to_their_own_connections():
    audio = struct.pack('<4sIII', b'AUDF', 1, 48000, 0)
    iq = _iq(1)
    display = ClientStream(FakeWebSocket(), no_audio=True, no_iq=True)
    display.publish_bytes(audio)
    display.publish_bytes(iq)
    assert not display._audio and not display._iq
    assert not display.accepts_iq

    audio_only = ClientStream(FakeWebSocket(), audio_only=True)
    audio_only.publish_bytes(audio)
    audio_only.publish_bytes(iq)
    assert list(audio_only._audio) == [audio] and not audio_only._iq
    assert not audio_only.accepts_iq          # an audio socket must not pull IQ either

    iq_only = ClientStream(FakeWebSocket(), iq_only=True)
    iq_only.publish_bytes(audio)
    iq_only.publish_bytes(iq)
    assert list(iq_only._iq) == [iq] and not iq_only._audio
    assert iq_only.accepts_iq
    # A control-only socket is never written to a stream socket's channel either.
    iq_only.publish_text('{"cmd":"STATUS"}')
    assert not iq_only._control


@pytest.mark.asyncio
async def test_stalled_client_is_closed(monkeypatch):
    monkeypatch.setattr(client_stream_module, 'SEND_TIMEOUT', 0.01)
    ws = FakeWebSocket()
    ws.block_first = True
    stream = ClientStream(ws)
    stream.start()
    stream.publish_bytes(b'POWR-stalled')

    await asyncio.wait_for(ws.first_send_started.wait(), timeout=1)
    await asyncio.sleep(0.03)
    assert stream.closed
    assert ws.closed


def test_every_frame_type_has_a_retention_policy():
    """A new frame type must declare how it is retained (report finding E-4).

    The magics come from the production encoders, so adding an encoder without a policy row
    (and therefore silently getting latest-wins) fails here.
    """
    encoded = {
        framer.MAGIC_FREQ, framer.MAGIC_POWR, framer.MAGIC_RTA, framer.MAGIC_AUDIO,
        framer.MAGIC_IQ,
    }
    assert encoded == set(FRAME_POLICY), (
        f'frame types without a policy: {sorted(encoded - set(FRAME_POLICY))}; '
        f'policies for unknown types: {sorted(set(FRAME_POLICY) - encoded)}')
    # Unknown magics fall back to latest-wins rather than being dropped silently.
    assert DEFAULT_POLICY == 'latest'


def test_fifo_policy_is_what_the_audio_path_expects():
    assert FRAME_POLICY[framer.MAGIC_AUDIO] == 'fifo'
    assert FRAME_POLICY[framer.MAGIC_IQ] == 'fifo'
    assert FRAME_POLICY[framer.MAGIC_FREQ] == 'retain'
    assert FRAME_POLICY[framer.MAGIC_POWR] == 'latest'
