"""Restart the WebSA worker after native SDK crashes or fatal timeouts."""
from __future__ import annotations

import logging
import os
import signal
import subprocess
import sys
import time

from .logging_setup import setup_logging
from .web.recovery import EXIT_FATAL

log = logging.getLogger(__name__)
_stopping = False
_child: subprocess.Popen | None = None

#: Consecutive crashes with a runtime below this count as "the worker cannot start at all".
#: A native SDK crash is deterministic until its cause is gone, so restarting forever only
#: spins the CPU and writes one core dump per attempt (measured: 7 cores in under a minute
#: while the analyzer was unplugged).
FAST_EXIT_S = 10.0
#: After this many consecutive fast exits the supervisor stops and says why. The link loop
#: inside the worker is what reconnects an analyzer that comes back; a worker that cannot
#: survive startup needs the cause fixed, not another restart.
FAST_EXIT_LIMIT = 5


def should_restart(return_code: int) -> bool:
    return return_code < 0 or return_code == EXIT_FATAL


def _stop(signum, _frame) -> None:
    global _stopping
    _stopping = True
    if _child is not None and _child.poll() is None:
        _child.send_signal(signum)


def main() -> None:
    global _child
    # rotate=False: the worker owns the rotation of the shared log file (logging_setup.py).
    setup_logging(os.getenv('WEBSA_LOG', 'INFO'), os.getenv('WEBSA_LOGFILE', ''),
                  rotate=False)
    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    delay = 1.0
    fast_exits = 0
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
        fast_exits = fast_exits + 1 if runtime < FAST_EXIT_S else 0
        if fast_exits >= FAST_EXIT_LIMIT:
            log.critical(
                'WebSA worker crashed %d times in a row within %.0fs of starting (last '
                'status %s); giving up instead of spinning. Check the analyzer (USB link, '
                'power), then restart the service. Log: tail -f /tmp/websa.log '
                '(crash dumps: /tmp/websa.err); recent '
                'crashes: coredumpctl list',
                fast_exits, FAST_EXIT_S, return_code,
            )
            raise SystemExit(return_code)
        log.error(
            'WebSA worker exited with status %s; restarting in %.1fs',
            return_code, delay,
        )
        time.sleep(delay)
        delay = 1.0 if runtime >= 60 else min(10.0, delay * 2)


if __name__ == '__main__':
    main()
