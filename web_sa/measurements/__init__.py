"""
measurements/__init__.py -- measurement session factory.

The session classes are imported lazily (PEP 562): only ``StdSession`` (pure SWP) is
hardware-free, while Harmonic/PNM/RTA/SDR pull in ``hardware.sdk_bindings`` and therefore
the vendor library. Keeping the import lazy means
``import web_sa.measurements.framer`` works on a machine without libhtraapi, so the frame
codec and the HTTP/WS layers stay testable in CI (see docs/*/ARCH_REVIEW.md G-3).
"""
from __future__ import annotations

from .base import ConfigSnapshot, MeasurementSession, StdSession

_SESSIONS = {
    'std': ('base', 'StdSession'),
    'harmonic': ('harmonic', 'HarmonicSession'),
    'pnm': ('phase_noise', 'PhaseNoiseSession'),
    'rta': ('rta', 'RtaSession'),
    'sdr': ('sdr', 'SdrSession'),
}


def session_class(name: str) -> type[MeasurementSession]:
    """Return the session class for ``name`` (unknown names fall back to the standard sweep)."""
    module_name, class_name = _SESSIONS.get(name, _SESSIONS['std'])
    if module_name == 'base':
        return StdSession
    import importlib

    return getattr(importlib.import_module('.' + module_name, __name__), class_name)


def make_session(dev, name: str) -> MeasurementSession:
    """Enter/switch a measurement session (automatically exits the old session and enters the new one)."""
    if dev.session is not None:
        dev.session.exit()
    cls = session_class(name)
    sess = cls(dev)
    if name != 'std':
        sess.enter()
    return sess


def __getattr__(name: str):
    """Resolve the session classes lazily (``from web_sa.measurements import RtaSession``)."""
    for module_name, class_name in _SESSIONS.values():
        if class_name == name:
            return session_class(module_name if module_name != 'base' else 'std')
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')


__all__ = [
    'ConfigSnapshot',
    'HarmonicSession',
    'MeasurementSession',
    'PhaseNoiseSession',
    'RtaSession',
    'SdrSession',
    'StdSession',
    'make_session',
    'session_class',
]
