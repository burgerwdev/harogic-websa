"""
main.py -- application entry point (web_sa_new)

Single-process aiohttp + serial SDK calls; SIGINT/SIGTERM -> os._exit(0)
(to avoid crashes during library destruction).
"""
from __future__ import annotations

import asyncio
import logging
import os
import signal

from aiohttp import web

from .config import AppConfig
from .logging_setup import setup_logging
from .measurements import SessionManager, SessionNotReady, make_session
from .web import http_api, publisher
from .web import ws as ws_module
from .web.app_keys import (
    COMMAND_LOCK,
    DEVICE,
    GNSS_TASK,
    LINK_TASK,
    LOGGER,
    PUBLISHER_TASK,
    WS_CLIENTS,
)

log = logging.getLogger(__name__)


def _make_device():
    """Real device, or the synthetic one when WEBSA_FAKE=1 (CI runs the UI regression there).

    The vendor module is imported lazily so the fake path never loads libhtraapi.
    """
    if os.getenv('WEBSA_FAKE', '').lower() in ('1', 'true', 'yes'):
        from .hardware.fake_device import create_device, install_fake_sessions

        install_fake_sessions()
        return create_device()
    from .hardware.device import HarogicDevice

    return HarogicDevice()


def create_app(dev, cfg: AppConfig) -> web.Application:
    app = web.Application(middlewares=[http_api.security_middleware(cfg)])
    app[WS_CLIENTS] = set()
    app[DEVICE] = dev
    app[COMMAND_LOCK] = asyncio.Lock()
    dev.command_lock = app[COMMAND_LOCK]
    app[LOGGER] = logging.getLogger('web_sa')

    async def start_background(app):
        dev.set_session(make_session(dev, 'std'))
        app[PUBLISHER_TASK] = asyncio.create_task(publisher.publisher(app, dev))
        app[GNSS_TASK] = asyncio.create_task(_gnss_loop(app, dev))
        app[LINK_TASK] = asyncio.create_task(_link_loop(app, dev))

    async def cleanup_background(app):
        tasks = [app[PUBLISHER_TASK], app[GNSS_TASK], app[LINK_TASK]]
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        await asyncio.to_thread(dev.close)

    app.on_startup.append(start_background)
    app.on_cleanup.append(cleanup_background)

    app.router.add_get('/ws', ws_module.make_ws_handler(app, dev))
    http_api.make_routes(app, dev, cfg.static_dir)
    return app


async def _gnss_loop(app, dev):
    from .config import GNSS_POLL_INTERVAL
    while True:
        if dev.state.connected and not dev.state.calibrating:
            async with app[COMMAND_LOCK]:
                dev.state.gnss = await asyncio.to_thread(dev.query_gnss)
        await asyncio.sleep(GNSS_POLL_INTERVAL)


async def _link_loop(app, dev):
    """Reopen the device after a bus error / unplug and re-enter the active mode.

    The acquisition path already declares the link lost (`connected=false` in STATUS), so the
    page reports the disconnect right away; this loop is what makes the page come back on its
    own when the analyzer is plugged in again, instead of requiring a service restart. The
    session is re-entered in place, so the mode and its settings survive the outage.
    """

    def said(previous, reason):
        """Keep retrying quietly: report a failure only when it changes."""
        if reason != previous:
            log.warning('%s', reason)
        return reason

    from .config import LINK_CALL_TIMEOUT, LINK_POLL_INTERVAL
    last_reason = None
    stuck = False
    while True:
        await asyncio.sleep(LINK_POLL_INTERVAL)
        if dev.state.connected:
            last_reason = None
            stuck = False
            continue
        if stuck:
            # A previous reopen attempt timed out; its worker thread cannot be killed and still
            # owns the hardware lock, so a new attempt would only block (and leak a thread).
            # Wait for it: if it ever completes it flips `connected` back on its own.
            continue
        # Serialized with acquisition/commands: reopening closes and reopens the one device
        # handle, so no other SDK call may be in flight (the session re-entry configures the
        # device too, so it stays inside the lock).
        async with app[COMMAND_LOCK]:
            try:
                ok, err = await asyncio.wait_for(
                    asyncio.to_thread(dev.reopen), timeout=LINK_CALL_TIMEOUT)
            except asyncio.TimeoutError:
                stuck = True
                last_reason = said(last_reason, 'device reopen timed out after %.0fs; '
                                   'waiting for the outstanding attempt' % LINK_CALL_TIMEOUT)
                continue
            except Exception as exc:
                last_reason = said(last_reason, 'device reopen failed: %r' % exc)
                continue
            if not ok:
                # A missing analyzer is the normal state until the cable is back; say it when
                # it changes, then keep retrying quietly (a dev box without hardware stays quiet).
                last_reason = said(last_reason, 'device still unreachable: %s' % err)
                continue
            name = getattr(getattr(dev, 'session', None), 'name', 'std') or 'std'
            try:
                await asyncio.to_thread(SessionManager(dev).switch, name)
                log.info('device link restored; %s mode resumed', name)
            except SessionNotReady:
                log.warning('link restored but %s mode did not become ready; falling back to std',
                            name)
                try:
                    await asyncio.to_thread(SessionManager(dev).switch, 'std')
                except Exception:
                    log.exception('fallback to std failed after link restore')
            except Exception:
                log.exception('link restored but re-entering %s failed', name)


def main() -> None:
    # Dump the Python stack of every thread if the vendor SDK aborts the process
    # (SIGSEGV/SIGABRT from native heap corruption). The dump names the exact ctypes call
    # that was executing, which is the only reliable way to localise a native crash.
    import faulthandler
    faulthandler.enable()
    cfg = AppConfig()
    try:
        cfg.validate()
    except ValueError as exc:
        raise SystemExit(f'configuration error: {exc}') from None
    setup_logging(cfg.log_level, cfg.log_file)
    if not cfg.static_dir:
        cfg.static_dir = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                                      'frontend')
    dev = _make_device()
    ok, err = dev.open()
    print('Device open:', err if not ok else dev.state.label)

    def sig(sig_, frame_):
        # Fast exit. Device handle is released by the OS; official SDK docs require
        # open/close once per session - the process exits so the handle is freed on
        # process teardown (verified: manual Ctrl+C then restart works).
        os._exit(0)

    signal.signal(signal.SIGINT, sig)
    signal.signal(signal.SIGTERM, sig)
    app = create_app(dev, cfg)
    print('======================================================')
    print(' web_sa_new (SAN series) — http://localhost:%d' % cfg.port)
    print('======================================================')
    web.run_app(app, host=cfg.host, port=cfg.port)


if __name__ == '__main__':
    main()
