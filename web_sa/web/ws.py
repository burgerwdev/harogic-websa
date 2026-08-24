"""
web/ws.py —— WebSocket 路由 (声明式命令表)

来源: web_sa/server.py (v0.11.1) WS 命令处理迁移, 重构为命令表。
"""
from __future__ import annotations

import json

from aiohttp import WSMsgType, web


def make_ws_handler(app, dev):
    async def handler(request):
        ws = web.WebSocketResponse(max_msg_size=32 * 1024 * 1024)
        await ws.prepare(request)
        app['ws'].add(ws)
        # 新客户端连接: 下发最近频率轴(FREQ 帧只在版本变化时推,否则新客户端无 freq)
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
    """命令分发 —— 返回是否配置变更(需回发 STATUS)。"""
    s = dev.state
    if cmd == 'SET_PRESET':
        dev.apply_preset()
        return True
    if cmd == 'CAL_REFCLK':
        # 后台线程执行 GNSS 1PPS 校准, 期间 publisher 暂停; 同步置状态供前端反馈
        import asyncio
        async def _cal():
            cnt = int(data.get('count', 10))
            try:
                ok, freq = await asyncio.wait_for(
                    asyncio.to_thread(dev.calibrate_ref_clock, cnt),
                    timeout=cnt * 1.2 + 10)   # GNSS 1PPS 不可用时 DLL 可能挂起, 超时保护
                if ok:
                    dev.state.last_cal_freq = freq
                    dev.state.refclk_ppm = (freq / 100e6 - 1.0) * 1e6
            except Exception:
                pass
            finally:
                dev.state.calibrating = False   # 超时/失败都复位(前端按钮恢复)
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
        dev.configure_swp()
        return True
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
            dev.configure_swp()   # 下发参考时钟源到设备
            return True
    if cmd == 'SET_REFCKOUT':
        if 'on' in data:
            s.refclk_out = bool(data['on'])
            dev.configure_swp()   # 下发参考时钟输出使能
            return True
    if cmd == 'SET_MODE':
        from ..measurements import make_session
        name = data.get('mode', 'std')
        if name in ('std', 'harmonic', 'pnm'):
            dev.set_session(make_session(dev, name))
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
