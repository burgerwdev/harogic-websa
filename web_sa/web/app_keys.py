"""Typed aiohttp application keys shared across backend modules."""
import asyncio
import logging

from aiohttp import web

WS_CLIENTS = web.AppKey('ws_clients', set)
DEVICE = web.AppKey('device', object)
COMMAND_LOCK = web.AppKey('command_lock', asyncio.Lock)
LOGGER = web.AppKey('logger', logging.Logger)
PUBLISHER_TASK = web.AppKey('publisher_task', asyncio.Task)
GNSS_TASK = web.AppKey('gnss_task', asyncio.Task)
