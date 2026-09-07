"""Restart the WebSA worker after native SDK crashes or fatal timeouts."""
from __future__ import annotations

import logging
import os
import signal
import subprocess
import sys
import time

from .logging_setup import setup_logging

log = logging.getLogger(__name__)
_stopping = False
_child: subprocess.Popen | None = None


def should_restart(return_code: int) -> bool:
    return return_code < 0 or return_code == 70


def _stop(signum, _frame) -> None:
    global _stopping
    _stopping = True
    if _child is not None and _child.poll() is None:
        _child.send_signal(signum)


def main() -> None:
    global _child
    setup_logging(os.getenv('WEBSA_LOG', 'INFO'), os.getenv('WEBSA_LOGFILE', ''))
    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    delay = 1.0
    while not _stopping:
        _child = subprocess.Popen([sys.executable, '-m', 'web_sa.main'])
        started = time.monotonic()
        return_code = _child.wait()
        runtime = time.monotonic() - started
        if _stopping:
            break
        if return_code == 0:
            log.info('WebSA worker exited normally')
            break
        if not should_restart(return_code):
            log.critical('WebSA worker exited with non-recoverable status %s', return_code)
            raise SystemExit(return_code)
        log.error(
            'WebSA worker exited with status %s; restarting in %.1fs',
            return_code, delay,
        )
        time.sleep(delay)
        delay = 1.0 if runtime >= 60 else min(10.0, delay * 2)


if __name__ == '__main__':
    main()
