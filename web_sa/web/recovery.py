"""Fatal-error exit path.

A native SDK crash or a hung DLL call cannot be recovered in-process (the vendor library
may hold locks or corrupt memory), so the worker exits with :data:`EXIT_FATAL` and
`web_sa.supervisor` restarts it. Every such path goes through :func:`fatal` so the exit
code and the log format are defined in exactly one place (report finding P1-10).
"""
from __future__ import annotations

import logging
import os
from typing import NoReturn

#: Exit status the supervisor treats as "restart me" (see supervisor.should_restart).
EXIT_FATAL = 70

log = logging.getLogger(__name__)


def fatal(reason: str) -> NoReturn:
    """Log ``reason`` and terminate the worker so the supervisor can restart it."""
    log.critical('%s; terminating worker for supervisor recovery', reason)
    os._exit(EXIT_FATAL)
