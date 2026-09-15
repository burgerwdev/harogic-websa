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
    'vsa': ('vsa', 'VsaSession'),
}


def session_class(name: str) -> type[MeasurementSession]:
    """Return the session class for ``name`` (unknown names fall back to the standard sweep)."""
    module_name, class_name = _SESSIONS.get(name, _SESSIONS['std'])
    if module_name == 'base':
        return StdSession
    import importlib

    return getattr(importlib.import_module('.' + module_name, __name__), class_name)


class SessionNotReady(RuntimeError):
    """The session was constructed but never became usable (its configure path failed)."""


class SessionManager:
    """Explicit session lifecycle: stop -> exit -> construct -> enter -> ready (finding P1-9).

    The previous code did this inline in the command handler and asked the session for a
    private `_ready` flag; a failure inside a session's configure path therefore looked like
    a successful switch. `switch()` owns the order and the readiness check.
    """

    def __init__(self, dev):
        self.dev = dev

    def current(self) -> MeasurementSession | None:
        return self.dev.session

    def switch(self, name: str) -> MeasurementSession:
        old = self.dev.session
        if old is not None:
            old.request_stop()
            old.exit()
        session = session_class(name)(self.dev)
        if name != 'std':
            session.enter()
        self.dev.set_session(session)
        if not session.is_ready():
            raise SessionNotReady(name)
        return session


def make_session(dev, name: str) -> MeasurementSession:
    """Enter/switch a measurement session (automatically exits the old session and enters the new one)."""
    return SessionManager(dev).switch(name)


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
    'VsaSession',
    'SessionManager',
    'SessionNotReady',
    'StdSession',
    'make_session',
    'session_class',
]
