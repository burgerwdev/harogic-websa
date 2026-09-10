"""
web/ws.py -- WebSocket routes (declarative command table)

Source: migrated from the WS command handling of web_sa/server.py (v0.11.1),
refactored into a command table.
"""
from __future__ import annotations

import asyncio
import json
import logging
import math
import os

from aiohttp import WSMsgType, web

from .app_keys import COMMAND_LOCK, WS_CLIENTS
from .client_stream import ClientStream

log = logging.getLogger(__name__)


class CommandError(ValueError):
    """A client command is malformed or cannot be applied in the current state."""


_COMMANDS = {
    'STATUS', 'CONNECT', 'SET_PRESET', 'CAL_REFCLK', 'SET_FREQ', 'SET_REF',
    'SET_RBW', 'SET_VBW', 'SET_SWEEP', 'SET_POINTS', 'SET_SPUR', 'SET_WINDOW',
    'SET_AMP', 'SET_REFCK', 'SET_REFCKOUT', 'SET_MODE', 'SET_RTA', 'SET_HARM',
    'SET_PNM',
}


def _number(data, key, *, minimum=None, maximum=None, required=False):
    if key not in data:
        if required:
            raise CommandError(f'missing {key}')
        return None
    value = data[key]
    if isinstance(value, bool):
        raise CommandError(f'{key} must be a number')
    try:
        value = float(value)
    except (TypeError, ValueError):
        raise CommandError(f'{key} must be a number') from None
    if not math.isfinite(value):
        raise CommandError(f'{key} must be finite')
    if minimum is not None and value < minimum:
        raise CommandError(f'{key} must be >= {minimum}')
    if maximum is not None and value > maximum:
        raise CommandError(f'{key} must be <= {maximum}')
    data[key] = value
    return value


def _integer(data, key, *, minimum=None, maximum=None, required=False):
    value = _number(data, key, minimum=minimum, maximum=maximum, required=required)
    if value is None:
        return None
    if not value.is_integer():
        raise CommandError(f'{key} must be an integer')
    data[key] = int(value)
    return data[key]


def _choice(data, key, choices, *, required=False):
    if key not in data:
        if required:
            raise CommandError(f'missing {key}')
        return None
    value = data[key]
    if value not in choices:
        raise CommandError(f'{key} must be one of {", ".join(map(str, choices))}')
    return value


def _validate_command(dev, cmd, data):
    if not isinstance(data, dict) or not isinstance(cmd, str) or cmd not in _COMMANDS:
        raise CommandError('unknown or missing command')
    if cmd not in ('STATUS', 'CONNECT') and not dev.state.connected:
        raise CommandError('device is not connected')

    caps = dev.state.caps
    if cmd == 'CAL_REFCLK':
        _integer(data, 'count', minimum=3, maximum=120)
    elif cmd == 'SET_FREQ':
        if caps is None:
            raise CommandError('device capabilities are unavailable')
        has_center_span = 'center' in data or 'span' in data
        has_start_stop = 'start' in data or 'stop' in data
        if has_center_span and has_start_stop:
            raise CommandError('use center/span or start/stop, not both')
        if has_start_stop:
            _number(data, 'start', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz,
                    required=True)
            _number(data, 'stop', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz,
                    required=True)
            if data['stop'] - data['start'] < 100.0:
                raise CommandError('stop - start must be >= 100 Hz')
        elif has_center_span:
            _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
            _number(data, 'span', minimum=100.0,
                    maximum=caps.freq_max_hz - caps.freq_min_hz)
        else:
            raise CommandError('SET_FREQ requires center/span or start/stop')
    elif cmd == 'SET_REF':
        mode = _choice(data, 'mode', ('manual', 'auto')) or 'manual'
        if mode == 'manual':
            _number(data, 'ref', minimum=-50.0, maximum=30.0, required=True)
    elif cmd == 'SET_RBW':
        mode = _choice(data, 'mode', ('manual', 'auto')) or 'auto'
        if mode == 'manual':
            _number(data, 'rbw', minimum=100.0, maximum=10e6, required=True)
    elif cmd == 'SET_VBW':
        mode = _choice(data, 'mode', ('manual', 'equal', 'tenth', 'bypass', 'onethousandth')) or 'bypass'
        if mode == 'manual':
            _number(data, 'vbw', minimum=10.0, maximum=10e6, required=True)
    elif cmd == 'SET_SWEEP':
        mode = _integer(data, 'mode', minimum=0, maximum=8)
        current_mode = (
            dev.state.rta_sweep_time_mode
            if dev.state.mode == 'rta'
            else dev.state.sweep_time_mode
        )
        mode = current_mode if mode is None else mode
        if mode == 7:
            _number(data, 'time', minimum=0.001, maximum=60.0, required=True)
        elif mode in (6, 8):
            _number(data, 'time', minimum=1.0, maximum=1000.0, required=True)
        else:
            _number(data, 'time', minimum=0.0, maximum=60.0)
    elif cmd == 'SET_POINTS':
        _integer(data, 'points', minimum=51, maximum=4000, required=True)
    elif cmd == 'SET_SPUR':
        _choice(data, 'mode', ('bypass', 'standard', 'enhanced'), required=True)
    elif cmd == 'SET_WINDOW':
        _integer(data, 'window', minimum=0, maximum=4, required=True)
    elif cmd == 'SET_AMP':
        _integer(data, 'atten', minimum=-1, maximum=33)
        _integer(data, 'preamp', minimum=0, maximum=1)
        _integer(data, 'ifgain', minimum=0, maximum=3)
        _integer(data, 'gain_strategy', minimum=0, maximum=1)
    elif cmd == 'SET_REFCK':
        _choice(data, 'mode', ('internal', 'external', 'premium', 'external_forced'), required=True)
    elif cmd == 'SET_REFCKOUT':
        if not isinstance(data.get('on'), bool):
            raise CommandError('on must be a boolean')
    elif cmd == 'SET_MODE':
        mode = _choice(data, 'mode', ('std', 'harmonic', 'pnm', 'rta'), required=True)
        if mode == 'pnm' and not dev.state.pnm_supported:
            raise CommandError('phase-noise measurement is not supported')
    elif cmd == 'SET_RTA':
        if caps is None:
            raise CommandError('device capabilities are unavailable')
        _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
        _number(data, 'span', minimum=1000.0, maximum=50.78125e6)
        if 'center' not in data and 'span' not in data:
            raise CommandError('SET_RTA requires center or span')
    elif cmd == 'SET_HARM':
        if caps is None:
            raise CommandError('device capabilities are unavailable')
        _number(data, 'f0', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
        _integer(data, 'count', minimum=1, maximum=10)
        _number(data, 'span', minimum=1.0, maximum=100e6)
    elif cmd == 'SET_PNM':
        if caps is None:
            raise CommandError('device capabilities are unavailable')
        _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
        _number(data, 'threshold', minimum=-150.0, maximum=30.0)
        _integer(data, 'traceavg', minimum=1, maximum=1000)
        start = _number(data, 'start', minimum=1.0, maximum=9e6)
        stop = _number(data, 'stop', minimum=10.0, maximum=10e6)
        if start is not None and stop is not None and start >= stop:
            raise CommandError('start must be lower than stop')


def make_ws_handler(app, dev):
    async def handler(request):
        ws = web.WebSocketResponse(max_msg_size=32 * 1024 * 1024)
        await ws.prepare(request)
        channel = ClientStream(ws)
        channel.start()
        app[WS_CLIENTS].add(channel)
        # New client connects: push the most recent frequency axis (FREQ frames are only
        # sent when the version changes, otherwise a new client would have no freq)
        if dev.last_freq is not None:
            try:
                from ..measurements.framer import encode_freq
                channel.publish_bytes(encode_freq(dev.last_freq_ver, dev.last_freq, 0.0))
            except Exception:
                pass
        try:
            async for msg in ws:
                if msg.type != WSMsgType.TEXT:
                    continue
                try:
                    data = json.loads(msg.data)
                    if not isinstance(data, dict):
                        raise CommandError('JSON message must be an object')
                    cmd = data.get('cmd')
                    if cmd == 'STATUS':
                        from .http_api import build_status
                        async with app[COMMAND_LOCK]:
                            channel.publish_json(build_status(dev))
                        continue
                    from .http_api import build_status
                    async with app[COMMAND_LOCK]:
                        changed = await _dispatch(dev, cmd, data)
                        status = build_status(dev) if changed else None
                    if status is not None:
                        status['response_to'] = cmd
                        channel.publish_json(status)
                except CommandError as exc:
                    channel.publish_json({'cmd': 'ERROR', 'msg': str(exc)})
                except Exception:
                    log.exception('WebSocket command failed')
                    channel.publish_json({'cmd': 'ERROR', 'msg': 'command failed'})
        finally:
            app[WS_CLIENTS].discard(channel)
            await channel.close()
        return ws

    return handler


async def _dispatch(dev, cmd, data) -> bool:
    """Command dispatch -- returns whether the config changed (a STATUS push is needed).

    Slow/possibly-hanging device reconfigurations (RTA session: mode enter, center/span/
    sweep/RBW) run on a worker thread with a timeout so the asyncio loop (publisher,
    STATUS pushes, other clients) never blocks on a DLL call that the device firmware
    answers slowly. The device watchdog recovers the hardware; the next reconfigure
    succeeds on its own.
    """
    _validate_command(dev, cmd, data)
    s = dev.state

    # Harmonic/PNM sessions own the device configuration while they run. Applying an
    # SWP-owned command would reconfigure the device behind the session and silently break
    # its acquisition, so reject it with an explicit error instead.
    sess = getattr(dev, 'session', None)
    if sess is not None and sess.name in ('harmonic', 'pnm'):
        swp_owned = {
            'SET_FREQ', 'SET_REF', 'SET_RBW', 'SET_VBW', 'SET_SWEEP', 'SET_POINTS',
            'SET_SPUR', 'SET_WINDOW', 'SET_AMP', 'SET_REFCK', 'SET_REFCKOUT',
        }
        if cmd in swp_owned:
            raise CommandError(
                f'{cmd} is not available while the {sess.name} measurement is active')

    async def _hw_call(fn, *a, timeout=20.0, **kw):
        try:
            return await asyncio.wait_for(asyncio.to_thread(fn, *a, **kw), timeout=timeout)
        except asyncio.TimeoutError:
            s.last_error = f'{getattr(fn, "__name__", "hardware call")} timed out'
            log.critical('%s; terminating worker for supervisor recovery', s.last_error)
            os._exit(70)

    async def _configure_swp():
        result = await _hw_call(dev.configure_swp)
        if result and result[0] is False:
            raise CommandError(result[1])

    async def _configure_active():
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            await _hw_call(sess.reconfigure)
        else:
            await _configure_swp()

    if cmd == 'SET_PRESET':
        # Preset covers BOTH modes without switching the current one:
        # 1) SWP parameters <- device defaults (preset_state, no reconfigure)
        # 2) RTA parameters  <- RTA defaults (RtaSession.reset_defaults)
        # Only the ACTIVE mode is reconfigured/put into effect now; the other mode's
        # defaults apply automatically the next time it is entered.
        dev.preset_state()
        dev.reset_rta_state()
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            # Preset supersedes the SWP restore point captured when RTA was entered.
            sess.snapshot_current()
            # RTA active: reset + reconfigure on a worker thread (device reconfigure on
            # the asyncio thread stalls the publisher/data flow)
            await _hw_call(sess.reset_defaults)
            return True
        await _configure_swp()   # SWP active: apply SWP defaults now
        return True
    if cmd == 'CAL_REFCLK':
        # Run GNSS 1PPS calibration on a background thread, pausing the publisher during
        # it; set the state synchronously for frontend feedback
        async def _cal():
            cnt = int(data.get('count', 10))
            try:
                command_lock = getattr(dev, 'command_lock', None)
                if command_lock is None:
                    task = asyncio.to_thread(dev.calibrate_ref_clock, cnt)
                    ok, freq = await asyncio.wait_for(task, timeout=cnt * 1.2 + 10)
                else:
                    async with command_lock:
                        task = asyncio.to_thread(dev.calibrate_ref_clock, cnt)
                        ok, freq = await asyncio.wait_for(task, timeout=cnt * 1.2 + 10)
                if ok:
                    dev.state.last_cal_freq = freq
                    dev.state.refclk_ppm = (freq / 100e6 - 1.0) * 1e6
            except asyncio.TimeoutError:
                s.last_error = 'reference calibration timed out'
                log.critical('%s; terminating worker for supervisor recovery', s.last_error)
                os._exit(70)
            except Exception as exc:
                s.last_error = f'reference calibration failed: {exc}'
                log.exception('Reference-clock calibration failed')
            finally:
                dev.state.calibrating = False
        if not dev.state.calibrating:
            dev.state.calibrating = True
            asyncio.create_task(_cal())
        return True
    if cmd == 'STATUS':
        return False
    if cmd == 'CONNECT':
        if s.connected:
            return False
        ok, err = await _hw_call(dev.open)
        if not ok:
            raise CommandError(err)
        return True
    if cmd == 'SET_FREQ':
        from ..config import fit_center_span, fit_start_stop

        if 'start' in data:
            s.center_hz, s.span_hz = fit_start_stop(
                data['start'], data['stop'], s.caps)
        else:
            center = data.get('center', s.center_hz)
            span = data.get('span', s.span_hz)
            s.center_hz, s.span_hz = fit_center_span(center, span, s.caps)
        dev.prepare_auto_reference_retune('std')
        await _configure_swp()
        return True
    if cmd == 'SET_REF':
        mode = data.get('mode', 'manual')
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            await _hw_call(sess.set_reference, mode=mode, ref=data.get('ref'))
            return True
        s.ref_mode = mode
        dev.reset_auto_reference('std')
        if mode == 'manual':
            s.ref_level = data['ref']
            await _configure_swp()
        return True
    # In RTA mode, SWP-only params (window/points/spur) are not applicable. Shared
    # RF/front-end settings are re-applied through the active RTA profile.
    # SET_RBW/SET_VBW are NOT intercepted: RTA supports both via the session
    # (set_rbw/set_vbw below, independent from SWP and restored on exit).
    if s.mode == 'rta' and cmd in ('SET_WINDOW', 'SET_POINTS', 'SET_SPUR'):
        raise CommandError(f'{cmd} is only available in SWP mode')
    if cmd == 'SET_RBW':
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            await _hw_call(sess.set_rbw, mode=data.get('mode', 'auto'), rbw=data.get('rbw', 0))
            return True
        if 'mode' in data:
            s.rbw_mode = data['mode']
        if 'rbw' in data:
            s.rbw_hz = data['rbw']
        await _configure_swp()
        return True
    if cmd == 'SET_VBW':
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            await _hw_call(sess.set_vbw, mode=data.get('mode', 'equal'), vbw=data.get('vbw', 0))
            return True
        if 'mode' in data:
            s.vbw_mode = data['mode']
        if 'vbw' in data:
            s.vbw_hz = data['vbw']
        await _configure_swp()
        return True
    if cmd == 'SET_SWEEP':
        # RTA mode: sweep speed goes to the RTA session (RTA_Profile.SweepTimeMode)
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            await _hw_call(
                sess.set_sweep,
                mode=int(data.get('mode', s.rta_sweep_time_mode)),
                time=float(data.get('time', s.rta_sweep_time) or 0),
            )
            return True
        if 'mode' in data:
            m = int(data['mode'])
            if 0 <= m <= 8:
                s.sweep_time_mode = m
        if 'time' in data:
            s.sweep_time = max(0.0, min(1e6, float(data['time'])))
        await _configure_swp()
        return True
    if cmd == 'SET_POINTS':
        s.points_req = data['points']
        await _configure_swp()
        return True
    if cmd == 'SET_SPUR':
        s.spur_mode = data['mode']
        await _configure_swp()
        return True
    if cmd == 'SET_WINDOW':
        s.window = data['window']
        await _configure_swp()
        return True
    if cmd == 'SET_AMP':
        if 'atten' in data:
            s.atten = int(max(-1, min(33, int(data['atten']))))
        if 'preamp' in data:
            s.preamplifier = 1 if data['preamp'] else 0
        if 'ifgain' in data:
            s.ifgain = int(max(0, min(3, int(data['ifgain']))))
        if 'gain_strategy' in data:
            s.gain_strategy = 1 if data['gain_strategy'] else 0
        await _configure_active()
        return True
    if cmd == 'SET_REFCK':
        s.ref_clock = data['mode']
        await _configure_active()
        return True
    if cmd == 'SET_REFCKOUT':
        s.refclk_out = data['on']
        await _configure_active()
        return True
    if cmd == 'SET_MODE':
        from ..measurements import make_session
        name = data.get('mode', 'std')
        if name in ('std', 'harmonic', 'pnm', 'rta'):
            def _sw():
                old_sess = dev.session
                if old_sess is not None and old_sess.name == 'rta':
                    old_sess._ready = False   # stop the RTA worker loop first (best-effort)
                dev.set_session(make_session(dev, name))
            await _hw_call(_sw)
            return True
    if cmd == 'SET_RTA':
        sess = dev.session
        if sess is None or sess.name != 'rta':
            raise CommandError('SET_RTA requires RTA mode')
        await _hw_call(
            sess.set_params, center=data.get('center'), span=data.get('span'))
        return True
    if cmd == 'SET_HARM':
        sess = dev.session
        if sess is not None and sess.name == 'harmonic':
            sess.set_params(f0=data.get('f0'), count=data.get('count'), span=data.get('span'))
            return True
    if cmd == 'SET_PNM':
        sess = dev.session
        if sess is not None and sess.name == 'pnm':
            sess.set_params(center=data.get('center'), threshold=data.get('threshold'),
                            traceavg=data.get('traceavg'), start=data.get('start'),
                            stop=data.get('stop'))
            return True
    return False
