"""
web/publisher.py —— 数据推送调度

来源: web_sa/server.py (v0.11.1) publisher 迁移; 会话对象化后只做调度。
"""
from __future__ import annotations

import asyncio
import json
import time

from ..config import PUBLISH_MIN_INTERVAL


async def publisher(app, dev):
    last_freq_ver = -1
    while True:
        t0 = time.monotonic()
        if app['ws']:
            try:
                frames, msgs = dev.step()
                # FREQ 帧按版本变化发送
                if dev.state.mode == 'std':
                    for fr in frames:
                        magic = fr[:4]
                        ver = int.from_bytes(fr[4:8], "little")
                        if magic == b'FREQ':
                            if ver == last_freq_ver:
                                continue
                            last_freq_ver = ver
                        await _send_bytes(app, fr)
                else:
                    for fr in frames:
                        await _send_bytes(app, fr)
                for m in msgs:
                    await _send_json(app, m)
            except Exception:
                pass
        dt = time.monotonic() - t0
        if dt < PUBLISH_MIN_INTERVAL:
            await asyncio.sleep(PUBLISH_MIN_INTERVAL - dt)
        else:
            await asyncio.sleep(0.002)


async def _send_bytes(app, frame):
    for ws in list(app['ws']):
        try:
            await ws.send_bytes(frame)
        except Exception:
            pass


async def _send_json(app, obj):
    for ws in list(app['ws']):
        try:
            await ws.send_str(json.dumps(obj))
        except Exception:
            pass
