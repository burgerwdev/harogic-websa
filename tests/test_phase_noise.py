"""Phase-noise session: deferred parameters must survive the cycle they arrived in.

`SET_PNM` mid-measurement is deferred ("reconfigure after the current cycle") by design —
but the `done` branch used to clear `dirty` unconditionally, so the next cycle started the
OLD profile again while the command had already been answered with success.
"""
import threading
from types import SimpleNamespace

import pytest

from web_sa.hardware import sdk_bindings as sb
from web_sa.measurements.phase_noise import PhaseNoiseSession

N_POINTS = 4
N_UPDATES = 1


def make_session():
    session = PhaseNoiseSession.__new__(PhaseNoiseSession)
    session.traceavg = 4
    session.dev = SimpleNamespace(
        _hw=threading.RLock(),
        dev=sb.c_void_p(),
        state=SimpleNamespace(pnm_last=None, last_error=''),
    )
    session._ready = True
    session._started = False
    session._i = 0
    session._stall = 0
    session._info = SimpleNamespace(TracePoints=N_POINTS, Segments=1,
                                    PartialUpdateCounts=N_UPDATES)
    session._cf = sb.c_double(); session._cf.value = 1e9
    session._cp = sb.c_float(); session._cp.value = -20.0
    session._rf = sb.c_float(); session._rf.value = -20.0
    session._freq = (sb.c_double * N_POINTS)()
    session._pn = (sb.c_float * N_POINTS)()
    return session


@pytest.fixture
def stubbed_dll(monkeypatch):
    monkeypatch.setattr(sb.dll, 'PNM_StartMeasure',
                        lambda handle: 0, raising=False)
    monkeypatch.setattr(sb.dll, 'PNM_StopMeasure',
                        lambda handle: 0, raising=False)

    def get_trace(handle, cf, cp, freq, pn, upd, aux, rf):
        cf.value = 1e9
        cp.value = -20.0
        rf.value = -20.0
        return 0

    monkeypatch.setattr(sb.dll, 'PNM_GetPartialUpdatedFullTrace',
                        get_trace, raising=False)


def test_midcycle_change_applies_before_the_next_cycle(stubbed_dll):
    session = make_session()
    configured = []
    session._configure = lambda: configured.append(1)   # the real one re-runs PNM_Configuration

    session.dirty = True            # changed while the previous cycle was still running
    (_, msgs), = [session.step()]   # starts the cycle, one update → done
    assert msgs[0]['done'] is True
    assert session.dirty is True    # the done branch must NOT drop the pending change

    session.step()                  # next cycle: the pending change configures FIRST again
    # The stubbed _configure cannot clear `dirty` (the real one does, after a successful
    # PNM_Configuration), so each new cycle start re-configures: the point is that the
    # pending change is applied BEFORE the cycle, not dropped at the previous `done`.
    assert configured == [1, 1]


def test_failed_reconfiguration_keeps_the_request_pending(stubbed_dll):
    session = make_session()
    attempts = []

    def failing_configure():
        attempts.append(1)
        raise RuntimeError('PNM_Configuration status=-9')

    session._configure = failing_configure
    session.dirty = True

    session.step()
    assert attempts == [1]
    assert session.dirty is True
    assert 'PNM reconfiguration failed' in session.dev.state.last_error
