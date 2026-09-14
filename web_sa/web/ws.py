"""
web/ws.py -- WebSocket transport for the command table.

The command set, validation, mode/session guards and handlers live in `commands.py`; this
module adapts a WebSocket message to `commands.dispatch()` and streams the result back.
"""
from __future__ import annotations

import asyncio
import json
import logging

from aiohttp import WSMsgType, web

from . import commands
from .app_keys import COMMAND_LOCK, WS_CLIENTS
from .client_stream import ClientStream
from .commands import HW_CALL_TIMEOUT_S, CommandContext, CommandError, error_payload
from .recovery import fatal

log = logging.getLogger(__name__)

#: Command names accepted on this transport; derived from the table in commands.py so the
#: two cannot drift (report finding P1-2).
_COMMANDS = commands.command_names()


async def _dispatch(dev, cmd, data) -> bool:
    """Run one command through the declarative table in `commands.py`.

    Returns whether the configuration changed (a STATUS push is needed). The table owns the
    envelope/connection/mode/session guards, the payload validation and the handlers; this
    module only supplies the transport-facing hardware wrapper below (report P1-1/P1-2).
    """
    state = dev.state

    async def _hw_call(fn, *a, timeout=HW_CALL_TIMEOUT_S, **kw):
        try:
            return await asyncio.wait_for(asyncio.to_thread(fn, *a, **kw), timeout=timeout)
        except asyncio.TimeoutError:
            state.last_error = f'{getattr(fn, "__name__", "hardware call")} timed out'
            fatal(state.last_error)

    ctx = CommandContext(dev=dev, hw_call=_hw_call)
    return await commands.dispatch(ctx, cmd, data)


def _validate_command(dev, cmd, data) -> None:
    """Kept as the validator entry point (tests and `http_api` use it directly)."""
    commands.validate(dev, cmd, data)


def make_ws_handler(app, dev):
    async def handler(request):
        ws = web.WebSocketResponse(max_msg_size=32 * 1024 * 1024)
        await ws.prepare(request)
        channel = ClientStream(ws, audio_only=request.query.get('audio') == '1',
                               no_audio=request.query.get('noaudio') == '1')
        channel.start()
        app[WS_CLIENTS].add(channel)
        # New client connects: push the most recent frequency axis (FREQ frames are only
        # sent when the version changes, otherwise a new client would have no freq)
        if dev.last_freq is not None and not channel.audio_only:
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
                        raise CommandError('JSON message must be an object', 'json_object_required')
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
                    channel.publish_json({'cmd': 'ERROR', **error_payload(exc)})
                except Exception:
                    log.exception('WebSocket command failed')
                    channel.publish_json({'cmd': 'ERROR', 'msg': 'command failed'})
        finally:
            app[WS_CLIENTS].discard(channel)
            await channel.close()
        return ws

    return handler
