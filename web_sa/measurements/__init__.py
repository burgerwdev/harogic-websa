"""
measurements/__init__.py —— 会话工厂
"""
from __future__ import annotations

from .base import ConfigSnapshot, MeasurementSession, StdSession
from .harmonic import HarmonicSession
from .phase_noise import PhaseNoiseSession

_SESSIONS = {'std': StdSession, 'harmonic': HarmonicSession, 'pnm': PhaseNoiseSession}


def make_session(dev, name: str) -> MeasurementSession:
    """进入/切换测量会话 (自动处理旧会话退出与新会话进入)。"""
    if dev.session is not None:
        dev.session.exit()
    cls = _SESSIONS.get(name, StdSession)
    sess = cls(dev)
    if name != 'std':
        sess.enter()
    return sess


__all__ = [
    'ConfigSnapshot',
    'HarmonicSession',
    'MeasurementSession',
    'PhaseNoiseSession',
    'StdSession',
    'make_session',
]
