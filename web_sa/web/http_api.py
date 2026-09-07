"""
web/http_api.py -- REST routes + STATUS serialization
"""
from __future__ import annotations

import asyncio
import hmac
import logging
import math
import os
from pathlib import Path
from urllib.parse import urlsplit

from aiohttp import web

from .app_keys import COMMAND_LOCK, LOGGER
from .ws import CommandError, _dispatch


def _json_safe(value):
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {key: _json_safe(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_json_safe(item) for item in value]
    return value


def security_middleware(cfg):
    """Protect hardware-control endpoints and reject cross-origin WebSockets."""
    @web.middleware
    async def middleware(request, handler):
        if request.path.startswith('/api/') or request.path == '/ws':
            if cfg.auth_token:
                auth = request.headers.get('Authorization', '')
                supplied = auth[7:] if auth.startswith('Bearer ') else request.query.get('token', '')
                if not hmac.compare_digest(supplied, cfg.auth_token):
                    raise web.HTTPUnauthorized(text='authentication required')

        if request.path in ('/ws', '/api/config'):
            origin = request.headers.get('Origin')
            if origin:
                parsed = urlsplit(origin)
                normalized = f'{parsed.scheme}://{parsed.netloc}'.rstrip('/')
                same_host = parsed.scheme in ('http', 'https') and parsed.netloc == request.host
                if not same_host and normalized not in cfg.origin_allowlist:
                    raise web.HTTPForbidden(text='request origin not allowed')

        response = await handler(request)
        response.headers['X-Content-Type-Options'] = 'nosniff'
        response.headers['Referrer-Policy'] = 'no-referrer'
        response.headers['X-Frame-Options'] = 'DENY'
        return response

    return middleware


def build_status(dev) -> dict:
    s = dev.state
    return _json_safe({
        'cmd': 'STATUS', 'connected': s.connected, 'device': s.label,
        'device_detail': s.device_detail,
        'center': s.center_hz, 'span': s.span_hz, 'ref': s.ref_level,
        'rbw_mode': s.rbw_mode, 'rbw': s.rbw_hz, 'vbw_mode': s.vbw_mode, 'vbw': s.vbw_hz,
        'points': s.points_req, 'window': s.window, 'spur': s.spur_mode,
        'sweep_time_mode': s.sweep_time_mode, 'sweep_time': s.sweep_time,
        'mode': s.mode, 'pnm_supported': s.pnm_supported,
        'caps': dict(model=s.caps.model if s.caps else 0, name=s.caps.name if s.caps else '',
                     fmin=s.caps.freq_min_hz if s.caps else 0,
                     fmax=s.caps.freq_max_hz if s.caps else 0),
        'preset_defaults': dev.preset_defaults,
        'req': dict(center=s.center_hz, span=s.span_hz, points=s.points_req,
                    rbw_mode=s.rbw_mode, rbw=s.rbw_hz, vbw_mode=s.vbw_mode, vbw=s.vbw_hz,
                    ref=s.ref_level, spur=s.spur_mode,
                    rta_center=s.rta_center_hz),
        'actual': s.actual,
        'amp': dict(atten=s.atten, preamp=s.preamplifier, ifgain=s.ifgain,
                    gain_strategy=s.gain_strategy, atten_actual=s.amp_atten,
                    preamp_actual=s.preamplifier_actual, ifgain_actual=s.ifgain),
        'ref_clock': s.ref_clock, 'has_docxo': s.has_docxo,
        'refclk_ppm': s.refclk_ppm, 'calibrating': s.calibrating, 'refclk_out': s.refclk_out,
        'last_cal_freq': s.last_cal_freq,
        'gnss': s.gnss, 'last_error': s.last_error,
    })


def make_routes(app, dev, static_dir):
    """Register routes directly on app.router."""
    if COMMAND_LOCK not in app:
        app[COMMAND_LOCK] = asyncio.Lock()
    if LOGGER not in app:
        app[LOGGER] = logging.getLogger(__name__)

    async def state(request):
        async with app[COMMAND_LOCK]:
            status = build_status(dev)
        return web.json_response(status)

    async def config(request):
        try:
            data = await request.json()
        except Exception:
            return web.json_response({'error': 'bad json'}, status=400)
        if not isinstance(data, dict):
            return web.json_response({'error': 'JSON body must be an object'}, status=400)
        try:
            async with app[COMMAND_LOCK]:
                changed = await _dispatch(dev, data.get('cmd'), data)
        except CommandError as exc:
            return web.json_response({'error': str(exc)}, status=400)
        except Exception as exc:
            request.app[LOGGER].exception('HTTP command failed')
            return web.json_response({'error': str(exc)}, status=503)
        status = build_status(dev)
        status['changed'] = changed
        return web.json_response(status)

    static_root = Path(static_dir, 'modern', 'dist').resolve()

    async def index(request):
        index_file = static_root / 'index.html'
        if not index_file.is_file():
            raise web.HTTPServiceUnavailable(text='frontend is not built')
        return web.FileResponse(index_file)

    app.router.add_get('/api/state', state)
    app.router.add_post('/api/config', config)
    app.router.add_get('/', index)
    async def modern_static(request):
        try:
            target = (static_root / request.match_info['file']).resolve()
            if os.path.commonpath((str(static_root), str(target))) != str(static_root):
                raise web.HTTPForbidden(text='invalid static path')
        except (OSError, ValueError) as exc:
            raise web.HTTPForbidden(text='invalid static path') from exc
        if not target.is_file():
            raise web.HTTPNotFound()
        return web.FileResponse(target)
    app.router.add_get('/static/modern/dist/{file:.*}', modern_static)
