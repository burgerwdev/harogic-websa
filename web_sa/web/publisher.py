"""Data acquisition and non-blocking fan-out scheduler."""
from __future__ import annotations

import asyncio
import logging
import os
import time

from ..config import GNSS_POLL_INTERVAL, PUBLISH_MIN_INTERVAL
from ..hardware.device import DeviceError
from .app_keys import COMMAND_LOCK, WS_CLIENTS

log = logging.getLogger(__name__)
STATUS_PUSH_INTERVAL = GNSS_POLL_INTERVAL
ERROR_LOG_INTERVAL = 5.0


def _acquisition_timeout(dev) -> float:
    if dev.state.mode == 'rta':
        return 5.0
    estimated = float(dev.state.actual.get('est_min', 0.0) or 0.0)
    configured = dev.state.sweep_time if dev.state.sweep_time_mode == 7 else 0.0
    return max(10.0, min(180.0, max(estimated, configured) * 1.5 + 5.0))


def _acquisition_step(dev):
    if dev.apply_pending_auto_reference():
        return [], []
    return dev.step()


async def publisher(app, dev):
    last_freq_ver = -1
    last_status_push = 0.0
    last_error_log = 0.0
    while True:
        t0 = time.monotonic()
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
                        log.critical('Acquisition timed out; terminating worker for recovery')
                        os._exit(70)
                frames, msgs = result if result is not None else ([], [])
                if dev.state.mode == 'std':
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
                log.critical('Fatal hardware error: %s; terminating worker', exc)
                os._exit(70)
            except Exception as exc:
                dev.state.last_error = f'publisher: {exc!r}'
                if t0 - last_error_log >= ERROR_LOG_INTERVAL:
                    last_error_log = t0
                    log.exception('Acquisition step failed')

        dt = time.monotonic() - t0
        if clients:
            dev.measure_sweep(dt)
        await asyncio.sleep(max(0.002, PUBLISH_MIN_INTERVAL - dt))


def _send_bytes(app, frame):
    for client in tuple(app[WS_CLIENTS]):
        client.publish_bytes(frame)


def _send_json(app, obj):
    for client in tuple(app[WS_CLIENTS]):
        try:
            client.publish_json(obj)
        except (TypeError, ValueError):
            log.exception('Invalid JSON payload dropped')
