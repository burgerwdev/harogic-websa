"""HTTP API 路由单测: /api/state + /api/config (stub 设备, 无需真硬件)"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from web_sa.config import AppConfig, DeviceCapabilities
from web_sa.hardware.device import DeviceState
from web_sa.web import http_api
from web_sa.web import ws as ws_module
from web_sa.web.app_keys import WS_CLIENTS


class StubDevice:
    def __init__(self):
        self.state = DeviceState(
            connected=True,
            center_hz=1e9,
            span_hz=100e6,
            caps=DeviceCapabilities.from_model(67),
        )
        self.preset_defaults = {'center': 1e9, 'span': 100e6}
        self.last_freq = None


def make_client(*, token=''):
    dev = StubDevice()
    cfg = AppConfig(auth_token=token)
    app = web.Application(middlewares=[http_api.security_middleware(cfg)])
    app[WS_CLIENTS] = set()
    app.router.add_get('/ws', ws_module.make_ws_handler(app, dev))
    http_api.make_routes(app, dev, os.path.join(os.path.dirname(__file__), '..', 'frontend'))
    return TestClient(TestServer(app))


@pytest.mark.asyncio
async def test_state_endpoint():
    client = make_client()
    async with client:
        resp = await client.get('/api/state')
        assert resp.status == 200
        data = await resp.json()
        assert data['cmd'] == 'STATUS'
        assert data['connected'] is True
        assert data['center'] == 1e9


@pytest.mark.asyncio
async def test_config_endpoint_echoes_status():
    client = make_client()
    async with client:
        resp = await client.post('/api/config', json={'cmd': 'STATUS'})
        assert resp.status == 200
        data = await resp.json()
        assert data['connected'] is True


@pytest.mark.asyncio
async def test_config_bad_json():
    client = make_client()
    async with client:
        resp = await client.post('/api/config', data='{bad json', headers={'Content-Type': 'application/json'})
        assert resp.status == 400


@pytest.mark.asyncio
async def test_config_rejects_non_object_and_unknown_command():
    client = make_client()
    async with client:
        resp = await client.post('/api/config', json=[])
        assert resp.status == 400
        resp = await client.post('/api/config', json={'cmd': 'NO_SUCH_COMMAND'})
        assert resp.status == 400
        resp = await client.post('/api/config', json={'cmd': 'SET_FREQ', 'center': 'NaN'})
        assert resp.status == 400
        resp = await client.post('/api/config', json={'cmd': 'SET_SWEEP', 'mode': 7})
        assert resp.status == 400


@pytest.mark.asyncio
async def test_static_route_rejects_encoded_absolute_path():
    client = make_client()
    async with client:
        resp = await client.get('/static/modern/dist/%2Fetc%2Fhostname')
        assert resp.status == 403


@pytest.mark.asyncio
async def test_control_api_requires_configured_token():
    client = make_client(token='secret-token')
    async with client:
        resp = await client.get('/api/state')
        assert resp.status == 401
        resp = await client.get('/api/state', headers={'Authorization': 'Bearer secret-token'})
        assert resp.status == 200


@pytest.mark.asyncio
async def test_websocket_rejects_cross_origin_and_missing_token():
    client = make_client(token='secret-token')
    async with client:
        with pytest.raises(Exception) as exc_info:
            await client.ws_connect('/ws', headers={'Origin': 'http://evil.example'})
        assert getattr(exc_info.value, 'status', None) == 401

        with pytest.raises(Exception) as exc_info:
            await client.ws_connect(
                '/ws?token=secret-token', headers={'Origin': 'http://evil.example'})
        assert getattr(exc_info.value, 'status', None) == 403

        response = await client.post(
            '/api/config?token=secret-token',
            json={'cmd': 'STATUS'},
            headers={'Origin': 'http://evil.example'},
        )
        assert response.status == 403
