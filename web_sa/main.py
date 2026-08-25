"""
main.py -- application entry point (web_sa_new)

Single-process aiohttp + serial SDK calls; SIGINT/SIGTERM -> os._exit(0)
(to avoid crashes during library destruction).
"""
from __future__ import annotations

import asyncio
import os
import signal

from aiohttp import web

from .config import AppConfig
from .hardware.device import HarogicDevice
from .logging_setup import setup_logging
from .measurements import make_session
from .web import http_api, publisher
from .web import ws as ws_module


def create_app(dev: HarogicDevice, cfg: AppConfig) -> web.Application:
    app = web.Application()
    app['ws'] = set()
    app['dev'] = dev

    async def start_background(app):
        app['publisher'] = asyncio.create_task(publisher.publisher(app, dev))
        app['gnss'] = asyncio.create_task(_gnss_loop(app, dev))
        dev.set_session(make_session(dev, 'std'))

    async def cleanup_background(app):
        app['publisher'].cancel()
        app['gnss'].cancel()
        try:
            await app['publisher']
        except Exception:
            pass
        dev.close()

    app.on_startup.append(start_background)
    app.on_cleanup.append(cleanup_background)

    app.router.add_get('/ws', ws_module.make_ws_handler(app, dev))
    http_api.make_routes(app, dev, cfg.static_dir)
    return app


async def _gnss_loop(app, dev):
    from .config import GNSS_POLL_INTERVAL
    while True:
        if dev.state.connected:
            dev.state.gnss = dev.query_gnss()
        await asyncio.sleep(GNSS_POLL_INTERVAL)


def main() -> None:
    setup_logging()
    cfg = AppConfig()
    if not cfg.static_dir:
        cfg.static_dir = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                                      'frontend')
    dev = HarogicDevice()
    ok, err = dev.open()
    print('Device open:', err if not ok else dev.state.label)

    def sig(sig_, frame_):
        os._exit(0)   # do not call Device_Close inside the signal handler (may hang on USB)

    signal.signal(signal.SIGINT, sig)
    signal.signal(signal.SIGTERM, sig)
    app = create_app(dev, cfg)
    print('======================================================')
    print(' web_sa_new (SAN series) — http://localhost:%d' % cfg.port)
    print('======================================================')
    web.run_app(app, host=cfg.host, port=cfg.port)


if __name__ == '__main__':
    main()
