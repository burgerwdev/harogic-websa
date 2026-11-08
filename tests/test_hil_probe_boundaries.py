"""Fault-injected HIL utility cleanup and response contracts; no RF or service I/O."""
import asyncio
from types import SimpleNamespace

import pytest

from tools.bench import hil_audio_check as hil
from tools.sdr_probe import probe_aux_live


class Context:
    def __init__(self, value=None):
        self.value = value if value is not None else self

    async def __aenter__(self):
        return self.value

    async def __aexit__(self, *_args):
        pass


@pytest.mark.parametrize('failure', ['generator', 'channel', 'capture'])
def test_hil_failure_stops_the_generator_and_leaves_sdr(monkeypatch, failure):
    events = []

    class Source:
        def __init__(self, _port):
            events.append('source-open')

        def close(self, disable_output):
            events.append(('source-close', disable_output))

    def configure(*_args, **_kwargs):
        events.append('generator-configure')
        if failure == 'generator':
            raise RuntimeError('injected generator failure')

    async def command(_session, _url, payload):
        events.append(payload)
        if payload['cmd'] == 'SET_SDR_CHANNEL' and failure == 'channel':
            raise RuntimeError('injected channel failure')
        return {'mode': payload.get('mode')}

    async def capture(*_args):
        raise RuntimeError('injected capture failure')

    async def no_wait(*_args):
        pass

    monkeypatch.setattr(hil, 'TinySaSource', Source)
    monkeypatch.setattr(hil, 'configure_tinysa', configure)
    monkeypatch.setattr(hil, 'command', command)
    monkeypatch.setattr(hil, 'capture', capture)
    monkeypatch.setattr(hil.aiohttp, 'ClientSession', Context)
    monkeypatch.setattr(hil.asyncio, 'sleep', no_wait)
    args = SimpleNamespace(url='http://unused', tinysa_port='fake', frequency=100e6,
                           modulation='cw', mod_freq=1000, deviation=3000, mode='cw',
                           pitch=700, ifbw=500, seconds=1)
    with pytest.raises(RuntimeError, match=f'injected {failure} failure'):
        asyncio.run(hil.run(args))
    assert events[-1] == ('source-close', True)
    if failure != 'generator':
        assert {'cmd': 'SET_MODE', 'mode': 'std'} in events


@pytest.mark.parametrize('failure', ['http', 'backend'])
def test_hil_command_rejects_failed_responses(failure):
    class Response(Context):
        def raise_for_status(self):
            if failure == 'http':
                raise RuntimeError('HTTP rejected configuration')

        async def json(self):
            return {'cmd': 'ERROR', 'error': 'backend rejected configuration'} if failure == 'backend' else {}

    session = SimpleNamespace(post=lambda *_args, **_kwargs: Response())
    with pytest.raises(RuntimeError, match='rejected'):
        asyncio.run(hil.command(session, 'http://unused', {'cmd': 'SET_MODE'}))


def test_aux_connection_deadline_bounds_a_silent_socket():
    class Silent:
        async def receive(self):
            await asyncio.Event().wait()

    async def run():
        # The outer timeout only prevents a broken implementation hanging the suite.
        with pytest.raises(TimeoutError, match='device never connected'):
            await asyncio.wait_for(probe_aux_live.wait_connected(Silent(), timeout=0.005), timeout=0.2)

    asyncio.run(run())
