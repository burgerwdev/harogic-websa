"""
measurements/__init__.py -- session factory
"""
from __future__ import annotations

from .base import ConfigSnapshot, MeasurementSession, StdSession
from .harmonic import HarmonicSession
from .phase_noise import PhaseNoiseSession
from .rta import RtaSession

_SESSIONS = {'std': StdSession, 'harmonic': HarmonicSession, 'pnm': PhaseNoiseSession, 'rta': RtaSession}


def make_session(dev, name: str) -> MeasurementSession:
    """Enter/switch a measurement session (automatically exits the old session and enters the new one)."""
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
    'RtaSession',
    'StdSession',
    'make_session',
]
