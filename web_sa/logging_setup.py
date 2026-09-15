"""Application logging configuration.

Rotation policy: **5 MiB active file + 3 backups (a 20 MiB ceiling)**, enforced in exactly
one process. The worker writes almost every record, so it gets the
``RotatingFileHandler``; the supervisor appends its rare messages through a
``WatchedFileHandler``, which notices the inode change and reopens the file after the
worker has rotated it. Two ``RotatingFileHandler``s on one file would count and rename
independently - each would keep its own backups and the cap would not hold.

Both processes calling this with the same file is therefore expected and safe; only the
process that passes ``rotate=True`` owns rotation.
"""
from __future__ import annotations

import logging
import sys
from logging.handlers import RotatingFileHandler, WatchedFileHandler

#: 5 MiB active file plus this many backups -> 20 MiB per log file (see docs/*/FAQ_NOTES.md).
MAX_BYTES = 5 * 1024 * 1024
BACKUP_COUNT = 3


def setup_logging(level: str = 'INFO', log_file: str = '', rotate: bool = True) -> None:
    """Install stdout plus, when ``log_file`` is given, the file handler.

    ``rotate=False`` is for the supervisor: it must not compete with the worker for
    rotation (see the module docstring). Repeated calls replace the previous handlers
    (``force=True``) instead of stacking them.
    """
    handlers: list[logging.Handler] = [logging.StreamHandler(sys.stdout)]
    if log_file:
        handlers.append(
            RotatingFileHandler(log_file, maxBytes=MAX_BYTES, backupCount=BACKUP_COUNT)
            if rotate else WatchedFileHandler(log_file)
        )
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format='%(asctime)s %(levelname)s %(name)s: %(message)s',
        handlers=handlers,
        force=True,
    )
