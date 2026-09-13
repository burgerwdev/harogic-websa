"""Data acquisition and non-blocking fan-out scheduler."""
from __future__ import annotations

import asyncio
import logging
import time

from ..config import GNSS_POLL_INTERVAL, PUBLISH_MIN_INTERVAL
from ..hardware.device import DeviceError
from ..measurements.base import _sweep_timeout
from .app_keys import COMMAND_LOCK, WS_CLIENTS
from .jsonutil import dumps_json, is_periodic_status
from .recovery import fatal

log = logging.getLogger(__name__)
STATUS_PUSH_INTERVAL = GNSS_POLL_INTERVAL
ERROR_LOG_INTERVAL = 5.0


def _acquisition_timeout(dev) -> float:
    """Watchdog for one acquisition step (the active session owns the policy)."""
    session = getattr(dev, 'session', None)
    if session is not None:
        return session.acquisition_timeout()
    return _sweep_timeout(dev.state)


def _acquisition_step(dev):
    nudge = getattr(dev, 'nudge_reference_out_of_overflow', None)
    if nudge is not None:
        # Must run every tick, not from the frame path: IF overflow (-12) means no frames.
        nudge()
    if dev.apply_pending_auto_reference():
        return [], []
    return dev.step()


async def publisher(app, dev):
    last_freq_ver = -1
    last_status_push = 0.0
    last_error_log = 0.0
    while True:
        t0 = time.monotonic()
        frames = []
        clients = app[WS_CLIENTS]
        if clients:
            if t0 - last_status_push >= STATUS_PUSH_INTERVAL:
                last_status_push = t0
                from .http_api import build_status
                async with app[COMMAND_LOCK]:
                    status = build_status(dev)
                status['stream'] = {
                    'clients': len(clients),
                    'dropped_frames': sum(client.dropped_frames for client in clients),
                    'dropped_control': sum(client.dropped_control for client in clients),
                    'dropped_audio': sum(client.dropped_audio for client in clients),
                }
                _send_json(app, status)
            try:
                # All SDK access runs outside the event loop and shares command_lock with
                # configuration/GNSS operations. This prevents old-data fetches from
                # interleaving with a state mutation and keeps HTTP/WS responsive.
                async with app[COMMAND_LOCK]:
                    try:
                        result = await asyncio.wait_for(
                            asyncio.to_thread(_acquisition_step, dev),
                            timeout=_acquisition_timeout(dev),
                        )
                    except asyncio.TimeoutError:
                        dev.state.last_error = 'acquisition timed out'
                        fatal('Acquisition timed out')
                frames, msgs = result if result is not None else ([], [])
                if dev.session is not None and dev.session.dedupe_freq:
                    for frame in frames:
                        magic = frame[:4]
                        version = int.from_bytes(frame[4:8], 'little')
                        if magic == b'FREQ':
                            if version == last_freq_ver:
                                continue
                            last_freq_ver = version
                        _send_bytes(app, frame)
                else:
                    for frame in frames:
                        _send_bytes(app, frame)
                for message in msgs:
                    _send_json(app, message)
            except asyncio.CancelledError:
                raise
            except DeviceError as exc:
                dev.state.last_error = f'fatal hardware error: {exc}'
                fatal(f'Fatal hardware error: {exc}')
            except Exception as exc:
                dev.state.last_error = f'publisher: {exc!r}'
                if t0 - last_error_log >= ERROR_LOG_INTERVAL:
                    last_error_log = t0
                    log.exception('Acquisition step failed')

        dt = time.monotonic() - t0
        if clients:
            dev.measure_sweep(dt)
        session = dev.session
        if session is not None:
            await asyncio.sleep(session.pacing(dt, bool(frames)))
        else:
            await asyncio.sleep(max(0.002, PUBLISH_MIN_INTERVAL - dt))


def _send_bytes(app, frame):
    for client in tuple(app[WS_CLIENTS]):
        client.publish_bytes(frame)


def _send_json(app, obj):
    """Serialize once and queue the same text on every client (report finding P1-11)."""
    try:
        text = dumps_json(obj)
    except (TypeError, ValueError):
        log.exception('Invalid JSON payload dropped')
        return
    coalesce = is_periodic_status(obj)
    for client in tuple(app[WS_CLIENTS]):
        client.publish_text(text, coalesce=coalesce)
