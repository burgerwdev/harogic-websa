"""Application logging configuration."""
import logging
import sys
from logging.handlers import RotatingFileHandler


def setup_logging(level: str = 'INFO', log_file: str = '') -> None:
    handlers: list[logging.Handler] = [logging.StreamHandler(sys.stdout)]
    if log_file:
        handlers.append(RotatingFileHandler(log_file, maxBytes=5 * 1024 * 1024, backupCount=3))
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format='%(asctime)s %(levelname)s %(name)s: %(message)s',
        handlers=handlers,
        force=True,
    )
