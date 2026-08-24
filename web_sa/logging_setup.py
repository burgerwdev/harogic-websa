"""logging_setup.py —— 结构化日志(占位, P2 完善轮转)。"""
import logging
import sys


def setup_logging(level: str = 'INFO') -> None:
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format='%(asctime)s %(levelname)s %(name)s: %(message)s',
        stream=sys.stdout,
    )
