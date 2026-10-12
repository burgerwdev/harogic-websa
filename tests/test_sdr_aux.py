"""Aux DDC channels (G1', official 3-channel parity): wire format, routing, command surface.

Official evidence (findings/rea-deep.md): SAStudio's "3 DDC" is one IQS stream feeding
three host-side DSP_DDC instances (libIQS.so, channels 1..3). Here channel 1 stays the
listen path; channels 2/3 stream as IQAX to ?iqaux=1 sockets only, so the browser
demodulator can never mistake another channel for a retune.
"""
from __future__ import annotations

import asyncio
import struct
from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.hardware.fake_device import FakeDevice
from web_sa.measurements import framer
from web_sa.web.client_stream import AUX_MAGIC, ClientStream
from web_sa.web.ws import CommandError, _validate_command


class FakeWebSocket:
    def __init__(self):
        self.binary = []
        self.text = []
        self.closed = False

    async def send_bytes(self, value):
        self.binary.append(value)

    async def send_str(self, value):
        self.text.append(value)

    async def close(self):
        self.closed = True


def test_encode_aux_shares_the_iqbf_layout_with_its_own_magic():
    iq = np.array([1.0, -1.0, 0.5, -0.5], dtype=np.float32)
    frame = framer.encode_aux(framer.BASEBAND_VERSION, 7, 48000.0, 101.7e6, iq)
    assert frame[:4] == b'IQAX'
    ver, seq, samples, rate, center = struct.unpack_from('<IIIdd', frame, 4)
    assert (ver, seq, samples) == (framer.BASEBAND_VERSION, 7, 2)
    assert rate == 48000.0 and center == 101.7e6
    assert np.frombuffer(frame[32:], dtype=np.float32).tolist() == iq.tolist()


@pytest.mark.asyncio
async def test_aux_frames_are_dropped_without_a_subscription():
    ws = FakeWebSocket()
    stream = ClientStream(ws)
    stream.publish_bytes(framer.encode_aux(framer.BASEBAND_VERSION, 1, 48e3, 100e6,
                                           np.zeros(4, dtype=np.float32)))
    stream.publish_bytes(framer.encode_baseband(framer.BASEBAND_VERSION, 1, 48e3, 100e6,
                                                np.zeros(4, dtype=np.float32)))
    stream.start()
    await asyncio.sleep(0.02)
    await stream.close()
    assert all(f[:4] != AUX_MAGIC for f in ws.binary)
    assert any(f[:4] == b'IQBF' for f in ws.binary)


@pytest.mark.asyncio
async def test_aux_frames_flow_to_a_subscribed_socket():
    ws = FakeWebSocket()
    stream = ClientStream(ws, aux_iq=True)
    stream.publish_bytes(framer.encode_aux(framer.BASEBAND_VERSION, 3, 48e3, 101.7e6,
                                           np.ones(4, dtype=np.float32)))
    stream.start()
    await asyncio.sleep(0.02)
    await stream.close()
    aux = [f for f in ws.binary if f[:4] == AUX_MAGIC]
    assert len(aux) == 1
    assert struct.unpack_from('<d', aux[0], 24)[0] == 101.7e6


def test_set_sdr_ddc_validates_channel_and_sdr_mode():
    stub = SimpleNamespace(state=SimpleNamespace(connected=True, mode="sdr", caps=None), session=None)
    with pytest.raises(CommandError):
        _validate_command(stub, 'SET_SDR_DDC', {'cmd': 'SET_SDR_DDC', 'channel': 1})
    with pytest.raises(CommandError):
        _validate_command(stub, 'SET_SDR_DDC', {'cmd': 'SET_SDR_DDC', 'channel': 4})
    _validate_command(stub, 'SET_SDR_DDC', {'cmd': 'SET_SDR_DDC', 'channel': 2})


@pytest.mark.asyncio
async def test_fake_session_aux_bookkeeping_round_trips_status():
    from web_sa.hardware.fake_device import install_fake_sessions
    from web_sa.measurements import make_session

    install_fake_sessions()
    dev = FakeDevice()
    ok, _err = dev.open()
    assert ok
    session = make_session(dev, 'sdr')
    assert session is not None and session.name == 'sdr'
    session.set_aux(2, enabled=True, freq_hz=98.0e6)
    session.set_aux(3, enabled=False, freq_hz=0.0)
    assert dev.state.sdr_aux[2] == {'enabled': True, 'freq': 98.0e6}
    assert dev.state.sdr_aux[3] == {'enabled': False, 'freq': 0.0}
    session.exit()

