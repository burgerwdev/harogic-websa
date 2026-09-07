"""RTA settings must never overwrite standard-sweep settings."""
import threading
from types import SimpleNamespace

import pytest

from web_sa.config import DeviceCapabilities
from web_sa.hardware.device import DeviceError, DeviceState
from web_sa.measurements.base import MeasurementSession
from web_sa.measurements.rta import RtaSession


def make_session():
    state = DeviceState(
        caps=DeviceCapabilities.from_model(67),
        rbw_mode='manual',
        rbw_hz=200e3,
        vbw_mode='manual',
        vbw_hz=50e3,
        ref_level=-10,
        ref_mode='manual',
    )
    dev = SimpleNamespace(
        state=state,
        _hw=threading.RLock(),
        reset_auto_reference=lambda _mode: None,
    )
    session = RtaSession(dev)
    session._configure = lambda: None
    return state, session


def test_rta_bandwidth_and_sweep_are_mode_private():
    state, session = make_session()
    session.set_rbw('auto')
    session.set_vbw('equal')
    session.set_sweep(2, 0)
    session.set_params(center=1e9, span=12_695_312)

    assert state.rta_span_hz == 12_695_312.5
    assert state.rta_rbw_mode == 'auto'
    assert state.rta_vbw_mode == 'equal'
    assert state.rta_sweep_time_mode == 2
    assert state.rbw_mode == 'manual' and state.rbw_hz == 200e3
    assert state.vbw_mode == 'manual' and state.vbw_hz == 50e3


def test_rta_reference_is_mode_private():
    state, session = make_session()
    session.set_reference('manual', -20)
    assert state.rta_ref_level == -20
    assert state.rta_ref_mode == 'manual'
    assert state.ref_level == -10


def test_preset_can_replace_rta_swp_restore_snapshot():
    state = DeviceState(center_hz=1e9, span_hz=100e6, rbw_hz=100e3)
    configured = []
    dev = SimpleNamespace(state=state, configure_swp=lambda: configured.append(True))
    session = MeasurementSession(dev)
    session.enter()

    state.center_hz = 4e9
    state.span_hz = 500e6
    state.rbw_hz = 250e3
    session.snapshot_current()
    state.center_hz = 2e9  # simulate temporary measurement mutation
    session.exit()

    assert state.center_hz == 4e9
    assert state.span_hz == 500e6
    assert state.rbw_hz == 250e3
    assert configured == [True]


def test_rta_repeated_errors_reconfigure_then_escalate():
    _state, session = make_session()
    recoveries = []

    def recover(recovery=False):
        recoveries.append(recovery)
        session._error_streak = 0

    session._configure_locked = recover
    for _ in range(8):
        session._step_failed_locked('get', -1)
    assert recoveries == [True]

    session._recovery_attempts = 2
    session._error_streak = 7
    session._last_recovery -= 2
    with pytest.raises(DeviceError):
        session._step_failed_locked('get', -1)
