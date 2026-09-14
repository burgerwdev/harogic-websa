"""Reference placement: the one-shot fit plus the always-armed safety ranger.

These tests drive the controller directly with a stub device and a fake clock, so the
decisions are deterministic and need no hardware. The behaviour-level tests in
tests/test_device_state.py cover the same rules through HarogicDevice.
"""
from __future__ import annotations

import threading
from types import SimpleNamespace

import pytest

from web_sa.hardware.auto_reference import (
    FLOOR_MIN_DBM,
    OVERFLOW_INTERVAL_S,
    SAFETY_INTERVAL_S,
    AutoReferenceController,
)


class StubDevice:
    """Only what the controller touches: state, the lock, configure_swp and the session."""

    def __init__(self, **state):
        base = dict(
            ref_mode='manual', ref_level=0.0, rta_ref_mode='manual', rta_ref_level=0.0,
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


# ---------------- the user's Auto Scale (one-shot) ----------------

def test_fit_anchors_the_noise_floor_and_applies_once(clock):
    dev = StubDevice(ref_level=-20.0)            # floor 25 dB above the bottom: too high
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    assert ctl.pending is None                   # observing alone never moves the reference
    # target = floor + window - 8 = -3, rounded up to the next 5 dB step
    assert ctl.fit('std') == ('applied', 0.0)
    assert ctl.pending == ('std', 0.0)
    assert ctl.view('std')['result'] == 'applied'


def test_fit_raises_to_the_instrument_ceiling(clock):
    dev = StubDevice(ref_level=10.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-10.0, floor=-40.0)
    assert ctl.fit('std') == ('applied', 30.0)


def test_fit_does_nothing_when_the_placement_is_already_good(clock):
    dev = StubDevice(ref_level=-20.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    # noise floor 8 dB above the bottom (current - window = -120), peak 20 dB below the top
    observe(dev, ctl, peak=-40.0, floor=-112.0)
    assert ctl.fit('std') == ('ok', None)
    assert ctl.pending is None                   # no pointless reconfiguration
    assert ctl.view('std')['result'] == 'ok'


def test_fit_needs_a_signal_above_the_noise_floor(clock):
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-100.0, floor=-95.0)    # 5 dB above the floor, inside the window
    assert ctl.fit('std') == ('no_signal', None)
    assert ctl.pending is None
    assert ctl.view('std')['result'] == 'no_signal'


def test_fit_reports_missing_data_before_the_first_trace(clock):
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    assert ctl.fit('std') == ('no_data', None)


def test_fit_uses_the_observation_recorded_during_a_settle_window(clock):
    """A user action overrides the settle guard: the newest frame is what they are looking at."""
    dev = StubDevice(ref_level=10.0)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std', delay=0.75)
    observe(dev, ctl, peak=-10.0, floor=-40.0)   # recorded, but the ranger stays quiet
    assert ctl.pending is None
    assert ctl.fit('std') == ('applied', 30.0)


def test_fit_is_mode_private(clock):
    dev = StubDevice(ref_level=-20.0, rta_ref_level=-20.0)
    dev.session = Session()
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0, mode='rta')
    assert ctl.fit('rta') == ('applied', 0.0)
    assert ctl.pending == ('rta', 0.0)
    assert ctl.view('std')['last_peak'] is None
    dev.state.mode = 'rta'                       # the worker only applies its own mode
    assert ctl.apply_pending() is True
    assert dev.state.rta_ref_level == 0.0


# ---------------- the safety ranger (always armed) ----------------

def test_safety_fit_fixes_a_trace_below_the_window(clock):
    """Measured with the SAN-90: Ref 0 dBm, 80 dB window, everything at -108 dBm.

    The old loop refused this because the peak was less than 15 dB above the floor, and left
    the display empty for as long as the signal stayed away.
    """
    dev = StubDevice(ref_level=0.0, ref_range_db=80.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-98.0, floor=-108.0)
    assert ctl.pending == ('std', -35.0)         # floor + window - 8 = -36 -> -35
    assert ctl.view('std')['result'] == 'out_of_window'


def test_safety_fit_raises_a_clipped_trace(clock):
    dev = StubDevice(ref_level=-40.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=0.0, floor=-95.0)     # peak above the top edge
    assert ctl.pending == ('std', 10.0)          # peak + 10 dB of headroom


def test_safety_ranger_leaves_a_good_placement_alone(clock):
    dev = StubDevice(ref_level=-20.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-40.0, floor=-112.0)
    observe(dev, ctl, peak=-41.0, floor=-111.0)
    assert ctl.pending is None


def test_safety_fit_is_rate_limited(clock):
    dev = StubDevice(ref_level=0.0, ref_range_db=80.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-98.0, floor=-108.0)
    assert ctl.pending is not None
    ctl.apply_pending()
    dev.state.ref_level = 0.0                   # pretend the device ignored it
    observe(dev, ctl, peak=-98.0, floor=-108.0)
    assert ctl.pending is None                  # inside the settle window
    clock[0] += 1.0
    observe(dev, ctl, peak=-98.0, floor=-108.0)
    assert ctl.pending is None                  # still inside SAFETY_INTERVAL_S
    clock[0] += SAFETY_INTERVAL_S
    observe(dev, ctl, peak=-98.0, floor=-108.0)
    assert ctl.pending == ('std', -35.0)


def test_safety_fit_needs_a_supported_mode(clock):
    """Harmonic/PNM sweeps own the device, so their observations are ignored."""
    dev = StubDevice(ref_level=0.0, ref_range_db=80.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-98.0, floor=-108.0, mode='harmonic')
    assert ctl.pending is None


def test_sdr_is_fitted_with_the_same_rule(clock):
    """SDR has its own tracker, driving the IQS level (the client applies the display scale)."""
    dev = StubDevice(ref_level=-20.0, ref_range_db=100.0, mode='sdr')
    dev.session = Session()
    dev.session.name = 'sdr'
    dev.session.reconfigure = dev.session._configure
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0, mode='sdr')
    assert ctl.pending is None                   # observing alone never moves the level
    assert ctl.fit('sdr') == ('applied', 0.0)
    assert ctl.view('std')['last_peak'] is None  # the SDR tracker is its own
    ctl.apply_pending()
    assert dev.state.ref_level == 0.0


# ---------------- IF overflow escape ----------------

def test_overflow_nudge_raises_one_step_and_learns_the_floor(clock):
    dev = StubDevice(ref_level=-40.0, status_warning=-12)
    ctl = AutoReferenceController(dev)
    assert ctl.nudge_out_of_overflow() is True
    assert ctl.pending == ('std', -35.0)
    assert ctl.tracker('std')['floor'] == -35.0
    assert dev.state.status_warning == 0        # consumed, the device re-reports if needed

    dev.state.status_warning = -12
    assert ctl.nudge_out_of_overflow() is False  # rate limited to one step per second
    clock[0] += OVERFLOW_INTERVAL_S + 0.1
    assert ctl.nudge_out_of_overflow() is True


def test_overflow_escape_works_with_manual_attenuation_and_manual_ref(clock):
    """Selecting a manual attenuator used to disable overload protection entirely."""
    dev = StubDevice(ref_level=-40.0, status_warning=-12, atten=10, ref_mode='manual')
    assert AutoReferenceController(dev).nudge_out_of_overflow() is True


def test_overflow_nudge_still_needs_a_supported_mode():
    dev = StubDevice(status_warning=-12, mode='harmonic')
    assert AutoReferenceController(dev).nudge_out_of_overflow() is False


# ---------------- housekeeping ----------------

def test_geometry_change_drops_the_learned_overflow_floor(clock):
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')
    ctl.tracker('std')['floor'] = 10.0          # learned by an overflow escape
    dev.state.span_hz = 20e6                     # geometry change
    ctl.begin_settle('std')
    assert ctl.tracker('std')['floor'] == FLOOR_MIN_DBM


def test_front_end_change_can_clear_the_learned_floor():
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    ctl.tracker('std')['floor'] = 5.0
    ctl.clear_learned_floor()
    assert ctl.tracker('std')['floor'] == FLOOR_MIN_DBM


def test_reset_forgets_observations_and_pending(clock):
    dev = StubDevice()
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    ctl.pending = ('std', 0.0)
    ctl.reset('std')
    assert ctl.pending is None
    assert ctl.fit('std') == ('no_data', None)


# ---------------- application in the worker ----------------

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


# ---------------- retune safety ----------------

def test_prepare_retune_clamps_a_ref_the_fit_had_lowered(clock):
    dev = StubDevice(ref_level=-25.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    ctl.fit('std')                               # the loop owns this level now
    ctl.tracker('std')['last_target'] = -25.0
    assert ctl.prepare_retune('std') is True
    assert dev.state.ref_level == 0.0
    assert ctl.prepare_retune('std') is False    # already safe
    dev2 = StubDevice(ref_level=-25.0)           # a level the user set: leave it alone
    assert AutoReferenceController(dev2).prepare_retune('std') is False


# ---------------- diagnostics for STATUS ----------------

def test_view_reports_the_diagnostics_for_status(clock):
    dev = StubDevice(ref_level=-20.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    view = ctl.view('std')
    assert view['last_peak'] == -30.0
    assert view['last_noise_floor'] == -95.0
    assert view['target'] is None
    assert view['result'] == 'idle'
    assert view['adjusting'] is False
    ctl.fit('std')
    view = ctl.view('std')
    assert view['target'] == 0.0
    assert view['result'] == 'applied'
    assert view['pending'] == 0.0
    assert view['adjusting'] is True             # the button glows while it is applied
    assert ctl.view('rta')['last_peak'] is None


def test_a_fit_result_survives_the_settle_it_causes(clock):
    """Applying a target reconfigures the device; that settle must not say "idle".

    Measured on the bench: the UI reported no decision at all for a fit that had just landed,
    because begin_settle() cleared the result the application had set.
    """
    dev = StubDevice(ref_level=-20.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    assert ctl.fit('std') == ('applied', 0.0)
    ctl.apply_pending()
    ctl.begin_settle('std')                      # what configure_swp() does next
    assert ctl.view('std')['result'] == 'applied'
    assert ctl.view('std')['target'] == 0.0
    ctl.reset('std')                             # a mode switch/preset is a real reset
    assert ctl.view('std')['result'] == 'idle'


def test_a_pending_fit_is_dropped_by_a_manual_takeover(clock):
    """Measured in SDR: a safety fit queued a moment earlier landed on the level the user set."""
    dev = StubDevice(ref_level=-20.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    ctl.fit('std')
    assert ctl.pending == ('std', 0.0)
    ctl.reset('std')                      # the manual SET_REF path does this
    assert ctl.apply_pending() is False   # the queued level must not be written
    assert dev.state.ref_level == -20.0
