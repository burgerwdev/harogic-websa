"""Auto-reference control loop (web_sa/hardware/auto_reference.py).

These tests drive the controller directly with a stub device and a fake clock, so the
decisions are deterministic and need no hardware. The behaviour-level tests in
tests/test_device_state.py cover the same rules through HarogicDevice.
"""
from __future__ import annotations

import threading
from types import SimpleNamespace

import pytest

from web_sa.hardware.auto_reference import FLOOR_MIN_DBM, AutoReferenceController


class StubDevice:
    """Only what the controller touches: state, the lock, configure_swp and the session."""

    def __init__(self, **state):
        base = dict(
            ref_mode='auto', ref_level=0.0, rta_ref_mode='auto', rta_ref_level=0.0,
            atten=-1, ref_range_db=100.0, status_warning=0, mode='std',
            center_hz=1e9, span_hz=10e6, rbw_hz=1e5, vbw_hz=1e5, window=1,
            rta_center_hz=1e9, rta_span_hz=10e6, rta_rbw_hz=0.0, rta_vbw_hz=0.0,
        )
        base.update(state)
        self.state = SimpleNamespace(**base)
        self._hw = threading.RLock()
        self.session = None
        self.configured = 0

    def configure_swp(self):
        self.configured += 1
        return True, 'ok'


class Session:
    name = 'rta'

    def __init__(self):
        self.reconfigured = 0

    def _configure(self):
        self.reconfigured += 1


@pytest.fixture
def clock(monkeypatch):
    """A monotonic clock the test advances by hand."""
    now = [1000.0]
    monkeypatch.setattr('web_sa.hardware.auto_reference.time.monotonic', lambda: now[0])
    return now


def observe(dev, controller, peak, floor=None, mode='std'):
    controller.observe_peak(mode, peak, floor)


def test_anchors_the_noise_floor_below_the_top_edge(clock):
    dev = StubDevice(ref_level=-20.0)           # 25 dB of floor above the bottom: too high
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)  # first observation arms the candidate
    assert ctl.pending is None
    clock[0] += 0.2
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    # target = floor + window - 8 = -3, rounded up to the next 5 dB step
    assert ctl.pending == ('std', 0.0)


def test_raise_is_immediate_and_lowering_waits(clock):
    dev = StubDevice(ref_level=10.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-10.0, floor=-40.0)   # arms the candidate
    clock[0] += 0.2                              # a fresh tracker commits after 0.15 s
    observe(dev, ctl, peak=-10.0, floor=-40.0)
    assert ctl.pending == ('std', 30.0)          # clamped to the instrument ceiling

    dev2 = StubDevice(ref_level=10.0)
    ctl2 = AutoReferenceController(dev2)
    ctl2.reset('std')
    clock[0] += 0.3                              # past the reset guard
    observe(dev2, ctl2, peak=-40.0, floor=-95.0)  # target well below the current level
    assert ctl2.pending is None                   # lowering needs ~1.5 s of stability
    clock[0] += 1.6
    observe(dev2, ctl2, peak=-40.0, floor=-95.0)
    assert ctl2.pending == ('std', 0.0)


def test_holds_when_the_placement_is_already_good(clock):
    dev = StubDevice(ref_level=-20.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    # noise floor 8 dB above the bottom (current - window = -120), peak 20 dB below the top
    observe(dev, ctl, peak=-40.0, floor=-112.0)
    assert ctl.pending is None
    assert ctl.tracker('std')['candidate'] is None


def test_needs_a_signal_above_the_noise_floor(clock):
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-100.0, floor=-105.0)   # < 15 dB above the floor
    assert ctl.pending is None


def test_does_nothing_without_auto_or_with_manual_attenuation(clock):
    manual_ref = StubDevice(ref_mode='manual')
    ctl = AutoReferenceController(manual_ref)
    observe(manual_ref, ctl, peak=-30.0, floor=-95.0)
    assert ctl.pending is None

    manual_atten = StubDevice(atten=10)
    ctl2 = AutoReferenceController(manual_atten)
    observe(manual_atten, ctl2, peak=-30.0, floor=-95.0)
    assert ctl2.pending is None


def test_ignores_observations_during_the_settle_window(clock):
    dev = StubDevice(ref_level=10.0)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std', delay=0.75)
    observe(dev, ctl, peak=-10.0, floor=-40.0)
    assert ctl.pending is None
    clock[0] += 0.8
    observe(dev, ctl, peak=-10.0, floor=-40.0)   # arms after the settle window
    clock[0] += 0.2
    observe(dev, ctl, peak=-10.0, floor=-40.0)
    assert ctl.pending == ('std', 30.0)


def test_geometry_change_drops_the_learned_overflow_floor(clock):
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')
    ctl.tracker('std')['floor'] = 10.0          # learned by an overflow escape
    dev.state.span_hz = 20e6                     # geometry change
    ctl.begin_settle('std')
    assert ctl.tracker('std')['floor'] == FLOOR_MIN_DBM


def test_mode_private_trackers():
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0, mode='rta')
    assert ctl.tracker('rta')['last_peak'] == -30.0
    assert ctl.tracker('std')['last_peak'] is None


def test_overflow_nudge_raises_one_step_and_learns_the_floor(clock):
    dev = StubDevice(ref_level=-40.0, status_warning=-12)
    ctl = AutoReferenceController(dev)
    assert ctl.nudge_out_of_overflow() is True
    assert ctl.pending == ('std', -35.0)
    assert ctl.tracker('std')['floor'] == -35.0
    assert dev.state.status_warning == 0        # consumed, the device re-reports if needed

    dev.state.status_warning = -12
    assert ctl.nudge_out_of_overflow() is False  # rate limited to one step per second
    clock[0] += 1.1
    assert ctl.nudge_out_of_overflow() is True


def test_overflow_nudge_needs_auto_mode_and_a_supported_mode():
    dev = StubDevice(status_warning=-12, ref_mode='manual')
    assert AutoReferenceController(dev).nudge_out_of_overflow() is False
    dev2 = StubDevice(status_warning=-12, mode='sdr')
    assert AutoReferenceController(dev2).nudge_out_of_overflow() is False


def test_apply_pending_configures_the_swp_path():
    dev = StubDevice(mode='std')
    ctl = AutoReferenceController(dev)
    ctl.pending = ('std', -15.0)
    assert ctl.apply_pending() is True
    assert dev.state.ref_level == -15.0
    assert dev.configured == 1
    assert ctl.pending is None


def test_apply_pending_reconfigures_the_rta_session():
    dev = StubDevice(mode='rta')
    session = Session()
    dev.session = session
    ctl = AutoReferenceController(dev)
    ctl.pending = ('rta', 5.0)
    assert ctl.apply_pending() is True
    assert dev.state.rta_ref_level == 5.0
    assert session.reconfigured == 1


def test_apply_pending_ignores_a_pending_from_another_mode():
    dev = StubDevice(mode='std')
    ctl = AutoReferenceController(dev)
    ctl.pending = ('rta', 5.0)
    assert ctl.apply_pending() is False
    assert ctl.pending == ('rta', 5.0)


def test_apply_pending_reports_a_rejected_configuration():
    class Rejecting(StubDevice):
        def configure_swp(self):
            self.configured += 1
            return False, 'nope'

    dev = Rejecting()
    ctl = AutoReferenceController(dev)
    ctl.pending = ('std', -15.0)
    assert ctl.apply_pending() is False


def test_prepare_retune_clamps_lowered_refs_and_re_arms(clock):
    dev = StubDevice(ref_level=-25.0)
    ctl = AutoReferenceController(dev)
    assert ctl.prepare_retune('std') is True
    assert dev.state.ref_level == 0.0
    assert ctl.tracker('std')['fresh'] is True

    dev2 = StubDevice(ref_level=5.0)
    assert AutoReferenceController(dev2).prepare_retune('std') is False
    dev3 = StubDevice(ref_mode='manual')
    assert AutoReferenceController(dev3).prepare_retune('std') is False


def test_view_reports_the_diagnostics_for_status(clock):
    dev = StubDevice(ref_level=-20.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    view = ctl.view('std')
    assert view['last_peak'] == -30.0
    assert view['last_noise_floor'] == -95.0
    assert view['candidate'] == 0.0     # armed, not yet committed
    assert view['pending'] is None
    clock[0] += 0.2
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    assert ctl.view('std')['pending'] == 0.0
    assert ctl.view('rta')['last_peak'] is None
