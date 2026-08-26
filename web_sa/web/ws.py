"""
web/ws.py -- WebSocket routes (declarative command table)

Source: migrated from the WS command handling of web_sa/server.py (v0.11.1),
refactored into a command table.
"""
from __future__ import annotations

import json

from aiohttp import WSMsgType, web


def make_ws_handler(app, dev):
    async def handler(request):
        ws = web.WebSocketResponse(max_msg_size=32 * 1024 * 1024)
        await ws.prepare(request)
        app['ws'].add(ws)
        # New client connects: push the most recent frequency axis (FREQ frames are only
        # sent when the version changes, otherwise a new client would have no freq)
        if dev.last_freq is not None:
            try:
                from ..measurements.framer import encode_freq
                await ws.send_bytes(encode_freq(dev.last_freq_ver, dev.last_freq, 0.0))
            except Exception:
                pass
        try:
            async for msg in ws:
                if msg.type != WSMsgType.TEXT:
                    continue
                try:
                    data = json.loads(msg.data)
                except Exception:
                    continue
                cmd = data.get('cmd')
                if cmd == 'STATUS':
                    from .http_api import build_status
                    await ws.send_str(json.dumps(build_status(dev)))
                    continue
                changed = _dispatch(dev, cmd, data)
                if changed:
                    from .http_api import build_status
                    await ws.send_str(json.dumps(build_status(dev)))
        finally:
            app['ws'].discard(ws)
        return ws

    return handler


def _dispatch(dev, cmd, data) -> bool:
    """Command dispatch -- returns whether the config changed (a STATUS push is needed)."""
    s = dev.state
    if cmd == 'SET_PRESET':
        dev.apply_preset()
        return True
    if cmd == 'CAL_REFCLK':
        # Run GNSS 1PPS calibration on a background thread, pausing the publisher during
        # it; set the state synchronously for frontend feedback
        import asyncio
        async def _cal():
            cnt = int(data.get('count', 10))
            try:
                ok, freq = await asyncio.wait_for(
                    asyncio.to_thread(dev.calibrate_ref_clock, cnt),
                    timeout=cnt * 1.2 + 10)   # DLL may hang if GNSS 1PPS is unavailable; timeout protection
                if ok:
                    dev.state.last_cal_freq = freq
                    dev.state.refclk_ppm = (freq / 100e6 - 1.0) * 1e6
            except Exception:
                pass
            finally:
                dev.state.calibrating = False   # reset on timeout/failure (frontend button recovers)
        if not dev.state.calibrating:
            dev.state.calibrating = True
            asyncio.ensure_future(_cal())
        return True
    if cmd == 'STATUS':
        return False
    if cmd == 'CONNECT':
        return False
    if cmd == 'SET_FREQ':
        if 'center' in data:
            s.center_hz = float(data['center'])
        if 'span' in data:
            from ..config import fit_span
            s.span_hz = fit_span(s.center_hz, float(data['span']), s.caps)
        dev.configure_swp()
        return True
    if cmd == 'SET_REF':
        s.ref_level = float(data.get('ref', s.ref_level))
        # Note: previously tried "reading back the actual attenuation and locking it to a
        # fixed value" to mitigate auto attenuation oscillation under a noise source, but
        # in manual attenuation mode ref and atten are deeply coupled (ref=atten-10), so
        # users could not set ref level independently -> rolled back, keeping auto atten
        # (ref independent)
        dev.configure_swp()
        return True
    # In RTA mode, SWP-only params (RBW/VBW/window/points/amp) are not applicable;
    # applying them reconfigures the device behind the RTA session -> spectrum freezes.
    if dev.state.mode == 'rta' and cmd in ('SET_RBW', 'SET_VBW', 'SET_WINDOW', 'SET_POINTS', 'SET_AMP', 'SET_SPUR'):
        return False
    if cmd == 'SET_RBW':
        if 'mode' in data:
            s.rbw_mode = data['mode'] if data['mode'] in ('manual', 'auto') else s.rbw_mode
        if 'rbw' in data:
            s.rbw_hz = float(data['rbw'])
        dev.configure_swp()
        return True
    if cmd == 'SET_VBW':
        if 'mode' in data and data['mode'] in ('manual', 'equal', 'tenth', 'bypass'):
            s.vbw_mode = data['mode']
        if 'vbw' in data:
            s.vbw_hz = float(data['vbw'])
        dev.configure_swp()
        return True
    if cmd == 'SET_SWEEP':
        # RTA mode: sweep speed goes to the RTA session (RTA_Profile.SweepTimeMode)
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            sess.set_sweep(mode=int(data.get('mode', 0)), time=float(data.get('time', 0) or 0))
            return True
        if 'mode' in data:
            m = int(data['mode'])
            if 0 <= m <= 8:
                s.sweep_time_mode = m
        if 'time' in data:
            s.sweep_time = max(0.0, min(1e6, float(data['time'])))
        dev.configure_swp()
        return True
    if cmd == 'SET_POINTS':
        s.points_req = int(max(51, min(4000, int(data.get('points', 1000)))))
        dev.configure_swp()
        return True
    if cmd == 'SET_SPUR':
        if data.get('mode') in ('bypass', 'standard', 'enhanced'):
            s.spur_mode = data['mode']
            dev.configure_swp()
            return True
    if cmd == 'SET_WINDOW':
        if 0 <= int(data.get('window', 1)) <= 4:
            s.window = int(data['window'])
            dev.configure_swp()
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
        dev.configure_swp()
        return True
    if cmd == 'SET_REFCK':
        mode = data.get('mode', 'internal')
        if mode in ('internal', 'external', 'premium', 'external_forced'):
            s.ref_clock = mode
            dev.configure_swp()   # push the reference clock source to the device
            return True
    if cmd == 'SET_REFCKOUT':
        if 'on' in data:
            s.refclk_out = bool(data['on'])
            dev.configure_swp()   # push the reference clock output enable
            return True
    if cmd == 'SET_MODE':
        from ..measurements import make_session
        name = data.get('mode', 'std')
        if name in ('std', 'harmonic', 'pnm', 'rta'):
            dev.set_session(make_session(dev, name))
            return True
    if cmd == 'SET_RTA':
        sess = dev.session
        if sess is not None and sess.name == 'rta':
            sess.set_params(center=data.get('center'), span=data.get('span'))
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
