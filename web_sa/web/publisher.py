"""
web/publisher.py -- data push scheduler

Source: migrated from the publisher of web_sa/server.py (v0.11.1); after session
objectification it only does scheduling.
"""
from __future__ import annotations

import asyncio
import json
import time

from ..config import PUBLISH_MIN_INTERVAL, GNSS_POLL_INTERVAL

STATUS_PUSH_INTERVAL = GNSS_POLL_INTERVAL   # periodic STATUS push interval (aligned with GNSS polling ~2s)


async def publisher(app, dev):
    last_freq_ver = -1
    last_status_push = 0.0
    while True:
        t0 = time.monotonic()
        if app['ws']:
            # Push STATUS periodically (aligned with the GNSS polling rhythm):
            # keeps device states like GNSS lock / reference clock output / calibration
            # status automatically refreshed, so the frontend page need not be reloaded
            if time.monotonic() - last_status_push >= STATUS_PUSH_INTERVAL:
                last_status_push = time.monotonic()
                from .http_api import build_status
                await _send_json(app, build_status(dev))
            try:
                # RTA Get transfers ~2MB/frame; run on a worker thread (libhtraapi
                # RTA calls behave differently on the asyncio thread -> crash)
                if dev.state.mode == 'rta':
                    frames, msgs = await asyncio.to_thread(dev.step)
                else:
                    frames, msgs = dev.step()
                # send FREQ frames only when the version changes
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
        # measured sweep time EMA (frame interval -> SWT display)
        if app['ws']:
            try:
                dev.measure_sweep(dt)
            except Exception:
                pass
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
