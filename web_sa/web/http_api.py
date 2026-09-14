"""
web/http_api.py -- REST routes + STATUS serialization
"""
from __future__ import annotations

import asyncio
import hmac
import logging
import os
from pathlib import Path
from urllib.parse import urlsplit

from aiohttp import web

from ..config import (
    DEFAULT_RTA_CENTER_HZ,
    DEFAULT_RTA_POINTS,
    DEFAULT_RTA_REF_DBM,
    DEFAULT_RTA_SPAN_HZ,
    ref_bounds,
)
from .app_keys import COMMAND_LOCK, LOGGER
from .commands import build_schema
from .jsonutil import finite_json
from .ws import CommandError, _dispatch, error_payload


def _json_safe(value):
    """Backwards-compatible alias (the implementation lives in jsonutil)."""
    return finite_json(value)


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


def _sdr_health(session) -> dict:
    """Kept for API compatibility; the session reports its own counters."""
    return {} if session is None else session.health()


def build_status(dev) -> dict:
    s = dev.state
    swp_req = {
        'center': s.center_hz,
        'span': s.span_hz,
        'points': s.points_req,
        'rbw_mode': s.rbw_mode,
        'rbw': s.rbw_hz,
        'vbw_mode': s.vbw_mode,
        'vbw': s.vbw_hz,
        'ref_mode': s.ref_mode,
        'ref': s.ref_level,
        'sweep_time_mode': s.sweep_time_mode,
        'sweep_time': s.sweep_time,
        'spur': s.spur_mode,
        'detector': s.detector,
        'window': s.window,
    }
    rta_req = {
        'center': s.rta_center_hz,
        'span': s.rta_span_hz,
        'points': 3328,
        'rbw_mode': s.rta_rbw_mode,
        'rbw': s.rta_rbw_hz,
        'vbw_mode': s.rta_vbw_mode,
        'vbw': s.rta_vbw_hz,
        'ref_mode': s.rta_ref_mode,
        'ref': s.rta_ref_level,
        'sweep_time_mode': s.rta_sweep_time_mode,
        'sweep_time': s.rta_sweep_time,
        'trigger_source': s.trigger_source,
        'trigger_edge': s.trigger_edge,
        'trigger_level': s.trigger_level_dbm,
        'trigger_safetime': s.trigger_safe_time_s,
        'trigger_delay': s.trigger_delay_s,
        'trigger_pretime': s.trigger_pre_time_s,
        'trigger_acqtime': s.trigger_acq_time_s,
        'trigger_retrigger': s.trigger_retrigger_count,
        'trigger_retriggerperiod': s.trigger_retrigger_period_s,
        'trigger_out': s.trigger_out,
        'trigger_outpolarity': s.trigger_out_polarity,
        'trigger_actual': s.trigger_actual,
    }
    is_rta = s.mode == 'rta'
    sdr_req = {
        'center': s.sdr_center_hz, 'decimate': s.sdr_decimate, 'listen': s.sdr_listen_hz,
        'demod': s.sdr_demod, 'if_bw': s.sdr_if_bw, 'squelch': s.sdr_squelch,
        'volume': s.sdr_volume, 'agc': s.sdr_agc, 'pitch': s.sdr_pitch,
        # The requested de-emphasis (-1 = per-mode default). It has to be reported like the
        # other SDR requests, otherwise the UI slot never confirms the user's choice and it
        # expires back to Auto after the TTL.
        'deemph_us': s.sdr_deemph_us,
    }
    if s.mode == 'sdr':
        bandwidth = float(s.sdr_actual.get('bandwidth', 0.0) or s.span_hz)
        points = int(s.sdr_actual.get('pan_points', 0) or s.points_req)
        bin_width = bandwidth / max(1, points)
        active_req = {
            **swp_req,
            'center': s.sdr_center_hz,
            'span': bandwidth,
            'points': points,
            'ref_mode': s.ref_mode,
            'ref': s.ref_level,
            'rbw_mode': 'auto',
            'rbw': bin_width,
            'vbw_mode': 'equal',
            'vbw': bin_width,
        }
        active_actual = s.sdr_actual
    else:
        active_req = rta_req if is_rta else swp_req
        active_actual = s.rta_actual if is_rta else s.actual
    auto_ref = dev.auto_reference_view()
    session = getattr(dev, 'session', None)
    session_health = dev.session_health() if session is not None else {}
    rta_health = {
        'error_streak': session_health.get('error_streak', 0) if s.mode == 'rta' else 0,
        'recovery_attempts': session_health.get('recovery_attempts', 0) if s.mode == 'rta' else 0,
    }

    def effective(name):
        value = active_actual.get(name)
        return active_req.get(name) if value is None else value

    request = dict(active_req)
    request['rta_center'] = s.rta_center_hz  # protocol compatibility
    request['swp'] = swp_req
    request['rta'] = rta_req
    request['sdr'] = sdr_req
    return _json_safe({
        'cmd': 'STATUS', 'connected': s.connected, 'device': s.label,
        'device_detail': s.device_detail,
        'center': effective('center'), 'span': effective('span'),
        'ref_mode': active_req['ref_mode'], 'ref': effective('ref'),
        'rbw_mode': active_req['rbw_mode'], 'rbw': effective('rbw'),
        'vbw_mode': active_req['vbw_mode'], 'vbw': effective('vbw'),
        'points': effective('points'), 'window': s.window, 'spur': s.spur_mode,
        'detector': s.detector,
        'sweep_time_mode': active_req['sweep_time_mode'],
        'sweep_time': active_req['sweep_time'],
        'mode': s.mode, 'pnm_supported': s.pnm_supported,
        'config_version': s.config_version,
        'caps': dict(model=s.caps.model if s.caps else 0, name=s.caps.name if s.caps else '',
                     fmin=s.caps.freq_min_hz if s.caps else 0,
                     fmax=s.caps.freq_max_hz if s.caps else 0,
                     # The numeric limits the client needs (it used to hard-code them).
                     ref_min=ref_bounds(s.caps)[0], ref_max=ref_bounds(s.caps)[1],
                     rta_span_max=s.caps.rta_span_max_hz if s.caps else 0,
                     rta_points=DEFAULT_RTA_POINTS),
        'preset_defaults': dev.preset_defaults,
        'rta_defaults': {
            # The device's own RTA defaults, from the constants the session uses.
            'center': DEFAULT_RTA_CENTER_HZ, 'span': DEFAULT_RTA_SPAN_HZ,
            'ref': DEFAULT_RTA_REF_DBM, 'ref_mode': 'manual',
            'rbw_mode': 'auto', 'rbw': 0.0, 'vbw_mode': 'equal', 'vbw': 0.0,
            'sweep_time_mode': 2, 'sweep_time': 0.0,
        },
        'req': request,
        'actual': active_actual,
        'swp_actual': s.actual,
        'rta_actual': s.rta_actual,
        'sdr': {
            **sdr_req,
            'actual': s.sdr_actual,
            'level_dbfs': s.sdr_level_dbfs,
            'squelch_open': s.sdr_squelch_open,
            'adm': s.sdr_adm,
            'health': session_health if s.mode == 'sdr' else {},
        },
        'auto_ref': {
            'last_peak': auto_ref['last_peak'],
            'last_noise_floor': auto_ref['last_noise_floor'],
            'target': auto_ref['target'],
            'result': auto_ref['result'],
            'seq': auto_ref['seq'],
            'pending': auto_ref['pending'],
            'adjusting': auto_ref['adjusting'],
        },
        'rta_health': rta_health,
        'amp': dict(atten=s.atten, preamp=s.preamplifier, ifgain=s.ifgain,
                    gain_strategy=s.gain_strategy, atten_actual=s.amp_atten,
                    preamp_actual=s.preamplifier_actual, ifgain_actual=s.ifgain),
        'ref_clock': s.ref_clock, 'has_docxo': s.has_docxo,
        'refclk_ppm': s.refclk_ppm, 'calibrating': s.calibrating, 'refclk_out': s.refclk_out,
        'ifagc': s.ifagc, 'ifagc_gain': s.ifagc_gain,
        'status_warning': s.status_warning, 'ref_range_db': s.ref_range_db,
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

    async def schema(request):
        """Machine-readable command/parameter schema (see commands.build_schema)."""
        return web.json_response(build_schema(dev))

    async def config(request):
        try:
            data = await request.json()
        except Exception:
            return web.json_response({'error': 'bad json', 'code': 'bad_json'}, status=400)
        if not isinstance(data, dict):
            return web.json_response(
                {'error': 'JSON body must be an object', 'code': 'json_object_required'}, status=400)
        try:
            async with app[COMMAND_LOCK]:
                changed = await _dispatch(dev, data.get('cmd'), data)
                status = build_status(dev)
        except CommandError as exc:
            payload = error_payload(exc)
            body = {'error': payload.pop('msg')}
            body.update(payload)
            return web.json_response(body, status=400)
        except Exception as exc:
            request.app[LOGGER].exception('HTTP command failed')
            return web.json_response({'error': str(exc)}, status=503)
        status['changed'] = changed
        status['response_to'] = data.get('cmd')
        return web.json_response(status)

    static_root = Path(static_dir, 'modern', 'dist').resolve()

    async def index(request):
        index_file = static_root / 'index.html'
        if not index_file.is_file():
            raise web.HTTPServiceUnavailable(text='frontend is not built')
        return web.FileResponse(index_file)

    app.router.add_get('/api/state', state)
    app.router.add_get('/api/schema', schema)
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
