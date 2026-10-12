"""DDC channels 1..3 (ux-2): wire format, routing, listen switching, activity.

Official evidence (findings/rea-deep.md): SAStudio's "3 DDC" is one IQS stream feeding
three host-side DSP_DDC instances (libIQS.so). Here the LISTENED channel streams as IQBF
(the browser demodulator), the others as IQAX to ?iqaux=1 sockets only — so the demodulator
can never mistake another channel for a retune, and SET_SDR_LISTEN switches with zero
device calls (config_version unchanged).
"""
from __future__ import annotations

import asyncio
import json
import os
import signal
import socket
import struct
import sys
import time
from types import SimpleNamespace

import numpy as np
import pytest

from web_sa.hardware.fake_device import FakeDevice
from web_sa.measurements import framer
from web_sa.measurements.sdr import _chan_activity
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
    frame = framer.encode_aux(framer.BASEBAND_VERSION, 7, 48000.0, 48000.0, iq)
    assert frame[:4] == b'IQAX'
    ver, seq, samples, rate, center = struct.unpack_from('<IIIdd', frame, 4)
    assert (ver, seq, samples) == (framer.BASEBAND_VERSION, 7, 2)
    assert rate == 48000.0 and center == 48000.0
    assert np.frombuffer(frame[32:], dtype=np.float32).tolist() == iq.tolist()


@pytest.mark.asyncio
async def test_aux_frames_are_dropped_without_a_subscription():
    ws = FakeWebSocket()
    stream = ClientStream(ws)
    stream.publish_bytes(framer.encode_aux(framer.BASEBAND_VERSION, 1, 48e3, 48e3,
                                           np.zeros(4, dtype=np.float32)))
    stream.publish_bytes(framer.encode_baseband(framer.BASEBAND_VERSION, 1, 48e3, 48e3,
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
    stream.publish_bytes(framer.encode_aux(framer.BASEBAND_VERSION, 3, 48e3, 48e3,
                                           np.ones(4, dtype=np.float32)))
    stream.start()
    await asyncio.sleep(0.02)
    await stream.close()
    aux = [f for f in ws.binary if f[:4] == AUX_MAGIC]
    assert len(aux) == 1
    assert struct.unpack_from('<d', aux[0], 24)[0] == 48e3


def _stub(mode='sdr'):
    return SimpleNamespace(state=SimpleNamespace(connected=True, mode=mode, caps=None),
                           session=None)


def test_set_sdr_ddc_validates_channel_and_range():
    _validate_command(_stub(), 'SET_SDR_DDC', {'cmd': 'SET_SDR_DDC', 'channel': 1})
    with pytest.raises(CommandError):
        _validate_command(_stub(), 'SET_SDR_DDC', {'cmd': 'SET_SDR_DDC', 'channel': 0})
    with pytest.raises(CommandError):
        _validate_command(_stub(), 'SET_SDR_DDC',
                          {'cmd': 'SET_SDR_DDC', 'channel': 2, 'threshold': 99.0})


def test_set_sdr_listen_validates_channel():
    _validate_command(_stub(), 'SET_SDR_LISTEN', {'cmd': 'SET_SDR_LISTEN', 'channel': 3})
    with pytest.raises(CommandError):
        _validate_command(_stub(), 'SET_SDR_LISTEN', {'cmd': 'SET_SDR_LISTEN', 'channel': 4})


def test_fake_channel_bookkeeping_and_listen_switch():
    install = __import__('web_sa.hardware.fake_device', fromlist=['install_fake_sessions'])
    install.install_fake_sessions()
    dev = FakeDevice()
    ok, _err = dev.open()
    assert ok
    from web_sa.measurements import make_session
    session = make_session(dev, 'sdr')
    assert session.name == 'sdr'
    session.set_channel(2, enabled=True, freq_hz=88.0e6)
    session.set_channel(3, enabled=False, freq_hz=0.0)
    assert dev.state.sdr_chans[2]['enabled'] and dev.state.sdr_chans[2]['freq'] == 88.0e6
    assert not dev.state.sdr_chans[3]['enabled']
    session.set_listen(2)
    assert dev.state.sdr_listen_channel == 2
    assert dev.state.sdr_listen_hz == 88.0e6
    with pytest.raises(ValueError):
        session.set_channel(2, enabled=False)     # listen channel: refuse
    session.set_listen(1)
    assert dev.state.sdr_listen_channel == 1
    session.exit()


def test_chan_activity_hysteresis():
    floor = []
    # Quiet band settles the noise floor; a spike opens; only dropping 3 dB below the
    # threshold closes (the hysteresis band must not chatter at the threshold).
    active = False
    seen = []
    for lvl in [-130.0] * 6 + [-100.0, -98.0, -99.0, -96.0] + [-97.5] * 2 + [-110.0]:
        active, _floor = _chan_activity(lvl, floor, 6.0, active)
        seen.append(active)
    # -130s: inactive; -100/-98 above floor(-130)+6: active; -99/-96 stay active (hysteresis
    # closes at floor+6-3 = -127); -110 closes.
    assert seen[:6] == [False] * 6
    assert seen[6] is True
    assert seen[-1] is False
    assert any(seen)


def test_chan_activity_empty_floor_uses_the_first_level():
    active, base = _chan_activity(-95.0, [], 6.0, False)
    assert active is False and base == -95.0     # first sample defines the floor


@pytest.mark.asyncio
async def test_fake_server_listen_switch_flips_iqbf_center():
    """Fake-server e2e: enabling ch2 streams IQAX on ?iqaux=1; SET_SDR_LISTEN 2 flips the
    main IQBF to the ch2 centre with config_version unchanged (zero device calls)."""
    # A free port per run: stale listeners from an earlier crashed run would otherwise
    # answer the handshake with a dead worker (measured: empty IQBF, silent timeouts).
    import socket as _socket
    import subprocess

    import aiohttp
    with _socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
    env = dict(os.environ, WEBSA_FAKE='1', WEBSA_PORT=str(port))
    proc = subprocess.Popen([sys.executable, '-m', 'web_sa.supervisor'], cwd=os.getcwd(),
                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            preexec_fn=os.setsid)
    try:
        deadline = time.monotonic() + 25.0
        while time.monotonic() < deadline:
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=1.0):
                    break
            except OSError:
                if proc.poll() is not None:
                    raise RuntimeError('fake server exited early') from None
                await asyncio.sleep(0.4)
        base = f'http://127.0.0.1:{port}'
        async with aiohttp.ClientSession() as http, \
                http.ws_connect(f'{base}/ws') as ws:
            await ws.send_str(json.dumps({'cmd': 'CONNECT'}))
            end = time.monotonic() + 10
            while time.monotonic() < end:
                raw = await ws.receive()
                if raw.type != aiohttp.WSMsgType.TEXT:
                    continue
                m = json.loads(raw.data)
                if m.get('cmd') == 'STATUS' and m.get('connected'):
                    break
            for cmd in ({'cmd': 'SET_MODE', 'mode': 'sdr'},
                        {'cmd': 'SET_SDR', 'center': 100e6, 'decimate': 64},
                        {'cmd': 'SET_SDR_TUNE', 'listen': 100e6},
                        {'cmd': 'SET_SDR_DDC', 'channel': 2, 'enabled': True,
                         'freq': 88.0e6},
                        {'cmd': 'SET_SDR_DDC', 'channel': 3, 'enabled': True,
                         'freq': 100.3e6, 'threshold': 3.0},
                        {'cmd': 'SET_SDR_LISTEN', 'channel': 2}):
                await ws.send_str(json.dumps(cmd))
                await asyncio.sleep(0.4)
            cv0 = None
            end = time.monotonic() + 10
            while time.monotonic() < end:
                raw = await ws.receive()
                if raw.type != aiohttp.WSMsgType.TEXT:
                    continue
                m = json.loads(raw.data)
                if m.get('cmd') == 'STATUS' and m.get('response_to') == 'SET_SDR_LISTEN':
                    cv0 = m.get('config_version')
                    assert m['req']['sdr']['listen_channel'] == 2
                    break
            assert cv0 is not None
            await asyncio.sleep(2.0)
            # The enabled ch3 row is echoed by STATUS; the active flip itself is proven
            # by the hysteresis unit test and the live ux-5 probe (real signal).
            seen3 = None
            end = time.monotonic() + 5.0
            async with http.ws_connect(f'{base}/ws') as ws3:
                while time.monotonic() < end and seen3 is None:
                    raw = await ws3.receive()
                    if raw.type != aiohttp.WSMsgType.TEXT:
                        continue
                    m = json.loads(raw.data)
                    if m.get('cmd') == 'STATUS':
                        seen3 = (m.get('req', {}).get('sdr', {}).get('chans') or {}).get('3')
            assert seen3 is not None, 'chans echo missing ch3'
            assert seen3.get('enabled') is True
            assert isinstance(seen3.get('active'), bool)
            assert isinstance(seen3.get('level_db'), (int, float))
            # The listened channel's IQBF now carries the ch2 centre.
            centers = set()
            async with http.ws_connect(f'{base}/ws?iq=1') as iq:
                end = time.monotonic() + 3.0
                while time.monotonic() < end:
                    try:
                        raw = await asyncio.wait_for(iq.receive(), timeout=0.5)
                    except (asyncio.TimeoutError, TimeoutError):
                        continue
                    if raw.type == aiohttp.WSMsgType.BINARY and raw.data[:4] == b'IQBF':
                        centers.add(struct.unpack_from('<d', raw.data, 24)[0])
            print('IQBF centers seen:', sorted(centers), file=sys.stderr)
            assert centers == {88.0e6}, centers
    finally:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
