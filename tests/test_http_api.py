"""HTTP API 路由单测: /api/state + /api/config (stub 设备, 无需真硬件)"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer
from web_sa.hardware.device import DeviceState
from web_sa.web import http_api


class StubDevice:
    def __init__(self):
        self.state = DeviceState(connected=True, center_hz=1e9, span_hz=100e6)
        self.preset_defaults = {'center': 1e9, 'span': 100e6}


def make_client():
    dev = StubDevice()
    app = web.Application()
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
