"""
web/http_api.py -- REST routes + STATUS serialization
"""
from __future__ import annotations

import os

from aiohttp import web

from .ws import _dispatch


def build_status(dev) -> dict:
    s = dev.state
    return {
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
    }


def make_routes(app, dev, static_dir):
    """Register routes directly on app.router."""

    async def state(request):
        return web.json_response(build_status(dev))

    async def config(request):
        try:
            data = await request.json()
        except Exception:
            return web.json_response({'error': 'bad json'}, status=400)
        await _dispatch(dev, data.get('cmd'), data)
        return web.json_response(build_status(dev))

    # Frontend: modern (TS rewrite, i18n + theming)
    async def index(request):
        return web.FileResponse(os.path.join(static_dir, 'modern', 'dist', 'index.html'))

    app.router.add_get('/api/state', state)
    app.router.add_post('/api/config', config)
    app.router.add_get('/', index)
    # modern frontend assets: /static/modern/dist/...
    async def modern_static(request):
        name = request.match_info['file'].split('?')[0]
        safe = os.path.normpath(name)
        if safe.startswith('..'):
            return web.Response(status=403)
        return web.FileResponse(os.path.join(static_dir, 'modern', 'dist', safe))
    app.router.add_get('/static/modern/dist/{file:.*}', modern_static)
