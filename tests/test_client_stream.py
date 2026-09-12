"""Bounded per-client WebSocket sender tests."""
import asyncio
import json
import struct

import pytest

import web_sa.web.client_stream as client_stream_module
from web_sa.web.client_stream import ClientStream


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
