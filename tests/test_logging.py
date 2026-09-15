"""Logging policy: one rotation owner, one ceiling, no handler stacking.

`run.sh` sets WEBSA_LOGFILE and drops stdout, so these tests pin the contract that makes
that safe: the worker's handler rotates at 5 MiB and keeps 3 backups (20 MiB total), the
supervisor's handler never rotates (it just reopens the file the worker renamed), and
repeated setup calls replace rather than stack handlers.
"""
from __future__ import annotations

import logging
import logging.handlers as lh
from pathlib import Path

import pytest

from web_sa import logging_setup as L


@pytest.fixture(autouse=True)
def restore_root_logger():
    """setup_logging(force=True) rewrites the root logger; put it back for other tests."""
    root = logging.getLogger()
    handlers, level = root.handlers[:], root.level
    yield
    for handler in root.handlers[:]:
        if handler not in handlers:
            handler.close()
    root.handlers[:] = handlers
    root.setLevel(level)


def _rotating() -> list[lh.RotatingFileHandler]:
    return [h for h in logging.getLogger().handlers
            if isinstance(h, lh.RotatingFileHandler)]


def test_file_handler_uses_the_documented_policy(tmp_path):
    log = tmp_path / 'websa.log'
    L.setup_logging('INFO', str(log))
    [handler] = _rotating()
    assert handler.maxBytes == 5 * 1024 * 1024
    assert handler.backupCount == 3
    assert Path(handler.baseFilename) == log
    assert L.MAX_BYTES * (L.BACKUP_COUNT + 1) == 20 * 1024 * 1024     # the 20 MiB ceiling


def test_supervisor_watches_instead_of_rotating(tmp_path):
    log = tmp_path / 'websa.log'
    L.setup_logging('INFO', str(log), rotate=False)
    assert _rotating() == []
    [watcher] = [h for h in logging.getLogger().handlers
                 if isinstance(h, lh.WatchedFileHandler)]
    assert Path(watcher.baseFilename) == log


def test_repeated_setup_does_not_stack_handlers(tmp_path):
    log = tmp_path / 'websa.log'
    L.setup_logging('INFO', str(log))
    L.setup_logging('INFO', str(log))
    assert len(_rotating()) == 1
    assert len(logging.getLogger().handlers) == 2      # stdout + the file


def test_no_file_handler_without_a_path():
    L.setup_logging('INFO', '')
    assert _rotating() == []
    assert len(logging.getLogger().handlers) == 1


def test_level_is_applied():
    L.setup_logging('warning', '')
    assert logging.getLogger().level == logging.WARNING


def test_rotation_keeps_the_cap_and_exactly_the_backup_count(tmp_path):
    """Drive the real handler built by setup_logging with a small budget.

    The stdlib does the renaming; what is worth pinning is that *this* configuration
    keeps ``BACKUP_COUNT`` backups and never lets one file exceed ``maxBytes`` - the
    property the 20 MiB ceiling depends on.
    """
    log = tmp_path / 'websa.log'
    L.setup_logging('INFO', str(log))
    [handler] = _rotating()
    handler.maxBytes = 4096                          # same handler, cheap budget
    record = 'x' * 200
    writer = logging.getLogger('websa.rotation.test')
    for _ in range(300):                             # ~60 KiB through the handler
        writer.info(record)
    handler.close()

    live = log.stat().st_size
    backups = sorted(tmp_path.glob('websa.log.*'))
    assert live <= handler.maxBytes + 300            # one record of overshoot, no more
    assert len(backups) == L.BACKUP_COUNT            # oldest dropped, never more than 3
    assert all(p.stat().st_size <= handler.maxBytes + 300 for p in backups)
    assert live + sum(p.stat().st_size for p in backups) <= (L.BACKUP_COUNT + 1) * (
        handler.maxBytes + 300)


def test_supervisor_watcher_reopens_after_a_worker_rotation(tmp_path):
    """The point of WatchedFileHandler: the supervisor keeps logging after a rotation."""
    log = tmp_path / 'websa.log'
    L.setup_logging('INFO', str(log), rotate=False)
    [watcher] = [h for h in logging.getLogger().handlers
                 if isinstance(h, lh.WatchedFileHandler)]
    logger = logging.getLogger('websa.supervisor.test')
    logger.info('before rotation')
    log.rename(tmp_path / 'websa.log.1')             # what the worker does
    log.write_text('')                               # the fresh file the worker creates
    logger.info('after rotation')
    assert 'after rotation' in log.read_text()
    watcher.close()
