"""Reference placement: the user's one-shot fit plus the IF-overflow protection.

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
    REFIT_ATTEMPTS,
    SAFETY_INTERVAL_S,
    AutoReferenceController,
)


class StubDevice:
    """Only what the controller touches: state, the lock, configure_swp and the session."""

    def __init__(self, **state):
        base = dict(
            # One Ref per mode, like the device state: SWP profile, RTA profile, IQS stream.
            ref_mode='manual', ref_level=0.0, rta_ref_mode='manual', rta_ref_level=0.0,
            sdr_ref_level=0.0, sdr_ref_set=False,
            atten=-1, ref_range_db=100.0, status_warning=0, mode='std',
            center_hz=1e9, span_hz=10e6, rbw_hz=1e5, vbw_hz=1e5, window=1,
            rta_center_hz=1e9, rta_span_hz=10e6, rta_rbw_hz=0.0, rta_vbw_hz=0.0,
            sdr_center_hz=1e9, sdr_decimate=16, sdr_if_bw=1e5,
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
    """A session stub for one mode: the controller only needs its name and one re-configure."""

    def __init__(self, name='rta'):
        self.name = name
        self.reconfigured = 0

    def _configure(self):
        self.reconfigured += 1

    def reconfigure(self):
        self._configure()


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


def test_fit_places_a_noise_only_trace_that_is_not_fully_inside(clock):
    """Reported: with no external signal and a small span the noise-only trace sat just under the
    bottom edge, and Auto refused with `no_signal` - so it stayed off the canvas.

    Measured on the fake backend at SWP 20 MHz / 1 MHz: floor -101 dBm at Ref 0 / 100 dB window
    (bottom edge -100 dBm). The peak is only 0.5 dB above the floor, i.e. there is no signal to
    anchor to - but the placement anchors on the FLOOR, so it is placed anyway:
    target = floor + window - 8 = -9 -> -5 on the 5 dB grid, i.e. 4 dB above the edge.
    """
    dev = StubDevice(ref_level=0.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-100.5, floor=-101.0)
    assert ctl.fit('std') == ('applied', -5.0)
    assert ctl.pending == ('std', -5.0)
    assert ctl.view('std')['result'] == 'applied'


def test_a_noise_only_trace_already_inside_reports_ok_not_a_refusal(clock):
    """No signal is not a reason to refuse: the noise floor is what gets anchored.

    "Inside" means the band [5, 12] dB above the bottom edge - half a division at the default
    10 dB/div is where a trace stops reading as a flat line lying on the axis (see
    `FLOOR_INSIDE_MIN_DB`). A floor comfortably in that band is left alone.
    """
    dev = StubDevice(ref_level=0.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-94.5, floor=-95.0)     # noise floor 5 dB above the bottom edge (-100)
    assert ctl.fit('std') == ('ok', None)
    assert ctl.pending is None
    assert ctl.view('std')['result'] == 'ok'


def test_a_trace_hugging_the_bottom_edge_is_re_fitted(clock):
    """Reported (after a Preset, entering SDR): the trace stopped 2.5 dB above the bottom edge and
    read as "not displayed". Two dB above the edge is on the canvas but not a usable placement, so
    the band's lower bound is half a division."""
    dev = StubDevice(ref_level=0.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-97.5, floor=-98.0)     # only 2 dB above the bottom edge
    assert ctl.fit('std') == ('applied', -5.0)     # the floor anchor (-8) on the 5 dB grid


def test_a_floor_only_just_inside_is_still_fitted(clock):
    """One dB above the bottom edge is not reliably on the canvas, and "partially off the bottom"
    is exactly the reported failure mode - so the band's lower bound re-fits it."""
    dev = StubDevice(ref_level=0.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-98.0, floor=-99.0)     # 1 dB above the bottom edge (-100)
    assert ctl.fit('std') == ('applied', -5.0)


def test_fit_may_go_below_the_capability_floor_but_not_above_the_ceiling(clock):
    """The capability row's -50 dBm floor is our guess; its maximum is the device's own.

    Measured on the bench: the SDK documents no Ref range and -140 dBm is programmed exactly, while
    +35 dBm comes back as +27 (the attenuation the device picks decides it). So a fit's FLOOR is the
    display domain - clamping it to the row left a low trace unmovable - and its CEILING is still
    the row, or the closed loop would chase an echo it can never reach.
    """
    dev = StubDevice(ref_level=-20.0)
    dev.state.caps = _caps(-30.0, 10.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-139.0, floor=-140.0)   # floor anchor wants -48
    result, target = ctl.fit('std')
    assert result == 'applied'
    assert target == -45.0                         # NOT clamped to the row's -30
    dev2 = StubDevice(ref_level=0.0)
    dev2.state.caps = _caps(-30.0, 10.0)
    ctl2 = AutoReferenceController(dev2)
    observe(dev2, ctl2, peak=-10.0, floor=-40.0)   # wants +55
    assert ctl2.fit('std') == ('applied', 10.0)    # clamped to the device maximum


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


# ---------------- observations never move the level on their own ----------------

def test_raising_ref_is_respected_and_not_undone(clock):
    """Reported: pressing the up arrow to raise Ref made Auto pull the trace back down.

    Raising Ref pushes the noise floor below the bottom edge. That is a display choice the user
    just made, not a fault, so nothing corrects it - pressing Auto re-fits it.
    """
    dev = StubDevice(ref_level=0.0, ref_range_db=80.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-98.0, floor=-108.0)   # floor below the bottom edge (-80)
    assert ctl.pending is None                   # nothing happens behind the user's back
    clock[0] += 5.0
    observe(dev, ctl, peak=-98.0, floor=-108.0)
    assert ctl.pending is None
    # ...but the explicit press still fits it (measured case: the old loop refused even that).
    assert ctl.fit('std') == ('applied', -35.0)  # floor + window - 8 = -36 -> -35
    assert ctl.view('std')['result'] == 'applied'


def test_a_grossly_clipped_trace_is_left_to_the_user(clock):
    """A clipped trace is feedback, not a fault to be corrected behind the user's back.

    The ranger used to raise Ref when the peak stood 10 dB or more above the top edge. Removed on
    request: the level someone typed or dragged is the level they get, and the warning on the
    canvas plus the Auto button are the deliberate ways to act on it.
    """
    dev = StubDevice(ref_level=-40.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=0.0, floor=-95.0)     # peak 40 dB above the top edge: gross clipping
    clock[0] += 10.0
    observe(dev, ctl, peak=0.0, floor=-95.0)     # however long it stands, and however often seen
    assert ctl.pending is None
    assert ctl.tracker('std')['result'] == 'idle'
    # ...while the explicit press still fixes it (peak + 10 dB of headroom, on the 5 dB grid).
    assert ctl.fit('std') == ('applied', 10.0)
    assert ctl.view('std')['result'] == 'applied'


def test_a_slightly_clipped_trace_is_left_alone_too(clock):
    """The direction of the old margin (only gross clipping was corrected) no longer matters:
    neither a slight nor a gross clip is touched by an observation."""
    dev = StubDevice(ref_level=-5.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-4.0, floor=-95.0)    # 1 dB over the top edge
    assert ctl.pending is None
    # The explicit press still tidies it up (the fit uses the tight margin).
    assert ctl.fit('std')[0] == 'applied'


def test_an_observation_alone_never_moves_an_unarmed_tracker(clock):
    dev = StubDevice(ref_level=-20.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-40.0, floor=-112.0)
    observe(dev, ctl, peak=-41.0, floor=-111.0)
    assert ctl.pending is None


def test_a_clipped_observation_never_queues_anything(clock):
    """The old rate limit existed because the ranger could act on its own. It cannot any more, so
    no number of clipped observations may produce a pending placement."""
    dev = StubDevice(ref_level=-40.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    for _ in range(5):
        observe(dev, ctl, peak=0.0, floor=-95.0)
        clock[0] += SAFETY_INTERVAL_S
        assert ctl.pending is None
    # The tracked observation is still recorded, so the next Auto press has data to work from.
    assert ctl.tracker('std')['last_peak'] == 0.0


def test_an_observation_needs_a_supported_mode(clock):
    """Harmonic/PNM sweeps own the device, so their observations are ignored."""
    dev = StubDevice(ref_level=-40.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=0.0, floor=-95.0, mode='harmonic')
    assert ctl.pending is None


def test_the_sdr_fit_follows_the_display_range_not_the_device_range(clock):
    """In SDR the fitted level is what the CLIENT displays, and that window goes to -160 dBm while
    the device accepts only -50..+30 dBm. Clamping the fit to the device row left a low noise
    floor under the bottom edge of a window that could have shown it; the IQS write is the half
    that must stay a device value.

    The write is *not* made from the fit: measured on the bench, writing it reconfigured the stream
    and moved the trace the fit was reading, so the fit wrote again (0 -> -15 -> -20 -> -25 -> -20 dBm
    in three seconds). Each write is an audio dropout and a level step, and a digital mode cannot
    integrate a slot across them, so SDR's fit is display-only and the device level is left alone (the
    overflow ranger still protects the ADC).
    """
    dev = StubDevice(sdr_ref_level=0.0, ref_range_db=100.0, mode='sdr')
    dev.state.sdr_ref_set = True               # the SDR already has a level of its own
    dev.session = Session('sdr')
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-179.5, floor=-180.0, mode='sdr')   # display target -88 -> -85 dBm
    assert ctl.fit('sdr') == ('applied', -85.0)
    # The fit is reported to the client (its display scale owns the placement) ...
    assert ctl.view('sdr')['target'] == -85.0
    # ... and no device traffic is generated, in either direction: writing the IQS level would
    # reconfigure the stream and move the trace the fit read (see the first-placement test).
    assert ctl.apply_pending() is False
    assert dev.state.sdr_ref_level == 0.0      # the IQS level, which is the SDR's own
    assert dev.session.reconfigured == 0
    assert ctl.tracker('sdr')['last_noise_floor'] >= -85.0 - ctl.window_db()


def test_an_sdr_if_overflow_still_reaches_the_device(clock):
    """The display-only rule has one exception: the IF-overflow escape is DEVICE protection.

    `nudge_out_of_overflow` queues a raise, and the SDR branch of `apply_pending` consumed every
    queue entry without writing - so in SDR the escape asked for a step and then dropped it, leaving
    the level where the saturation started, while the docs and the code comment both claimed the ADC
    was still protected.
    """
    dev = StubDevice(ref_range_db=100.0, mode='sdr', sdr_ref_level=-30.0)
    dev.state.status_warning = -12
    dev.session = Session()
    dev.session.name = 'sdr'
    ctl = AutoReferenceController(dev)
    assert ctl.nudge_out_of_overflow() is True
    assert ctl.pending == ('sdr', -25.0)
    assert ctl.apply_pending() is True                  # this one DOES reach the front end
    assert dev.state.sdr_ref_level == -25.0
    assert dev.session.reconfigured == 1
    assert dev.state.sdr_ref_set is True                # a real placement, not a display fit
    assert dev.state.status_warning == 0                # the device re-reports if still saturated


def test_the_sdr_fit_never_writes_the_device_not_even_for_a_first_placement(clock):
    """One writer for the SDR level: the client's manual path, and the fit only reports a target.

    Reported twice: a first switch into SDR showed the trace near/below the bottom edge, and coming
    back from RTA changed a Ref the user had set in SDR. The first placement therefore happens - but
    through the SAME path a manual Set takes (the client commits the reported target once, display and
    IQS level together), because the fit writing it and a later decision moving only the display left
    the two 15 dB apart (measured). The backend keeps reporting `sdr_ref_set` so the client knows
    whether a level exists at all.
    """
    dev = StubDevice(ref_range_db=100.0, mode='sdr', sdr_ref_level=0.0)
    dev.session = Session()
    dev.session.name = 'sdr'
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-95.0, floor=-108.0, mode='sdr')
    assert ctl.fit('sdr')[0] == 'applied'
    assert ctl.view('sdr')['target'] is not None       # the client commits this
    assert ctl.apply_pending() is False                # ...the fit writes nothing
    assert dev.state.sdr_ref_level == 0.0
    assert dev.state.sdr_ref_set is False              # only the manual path sets this
    assert dev.session.reconfigured == 0
    # A settings change later re-fits, and that stays display-only too.
    observe(dev, ctl, peak=-95.0, floor=-150.0, mode='sdr')
    assert ctl.fit('sdr') [0] == 'applied'
    assert ctl.apply_pending() is False
    assert dev.state.sdr_ref_level == 0.0


def test_sdr_is_fitted_with_the_same_rule(clock):
    """SDR has its own tracker, driving the IQS level (the client applies the display scale)."""
    dev = StubDevice(sdr_ref_level=-20.0, ref_range_db=100.0, mode='sdr')
    dev.state.sdr_ref_set = True
    dev.session = Session()
    dev.session.name = 'sdr'
    dev.session.reconfigure = dev.session._configure
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0, mode='sdr')
    assert ctl.pending is None                   # observing alone never moves the level
    assert ctl.fit('sdr') == ('applied', 0.0)
    assert ctl.view('std')['last_peak'] is None  # the SDR tracker is its own
    # The fit target is published for the client's display scale; the device level stays.
    assert ctl.apply_pending() is False
    assert ctl.view('sdr')['target'] == 0.0
    assert dev.state.sdr_ref_level == -20.0     # the SDR's own level, untouched by the fit


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


def test_a_settings_change_arms_one_automatic_refit(clock):
    """A span/centre/RBW change invalidates the level that was right for the old geometry: the
    first settled frame of the new one places it again."""
    dev = StubDevice(ref_level=0.0, span_hz=100e6)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')                       # the initial settle is not a change
    clock[0] += 1.0
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    assert ctl.pending is None                    # nothing moves while the settings stand still
    dev.state.span_hz = 1e6                       # the user changes span
    ctl.begin_settle('std')
    clock[0] += 1.0
    observe(dev, ctl, peak=-100.5, floor=-101.0)
    assert ctl.pending == ('std', -5.0)
    assert ctl.view('std')['result'] == 'applied'
    # The loop is closed: it keeps placing while the trace is still outside, and stops as soon as
    # the placement is good - so a device that lands the target ends the loop after one step.
    ctl.apply_pending()
    dev.state.ref_level = -5.0                    # the device landed it (the stub is inert)
    clock[0] += SAFETY_INTERVAL_S + 1.0
    observe(dev, ctl, peak=-100.5, floor=-101.0)
    assert ctl.pending is None


def test_a_frame_inside_the_step_interval_does_not_disarm_the_fit(clock):
    """The loop waits for the settle interval; "too soon" is not "gave up".

    Reported on the bench: at 13.825 MHz / 1 MHz Auto moved a visible trace to one 13.7 dB BELOW the
    bottom edge and stopped. The step itself was fine (the measured floor followed Ref: -116 dBm at
    Ref 0 -> -134 dBm at Ref -20); what stopped the loop is that the next frame arrived inside
    SAFETY_INTERVAL_S, and that branch disarmed the fit instead of waiting for a settled one. Three
    steps were needed to reach the first level where the trace is on the canvas.
    """
    dev = StubDevice(ref_level=0.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-104.0, floor=-116.0)
    assert ctl.fit('std') == ('applied', -20.0)          # the floor anchor, one step
    ctl.apply_pending()
    dev.state.ref_level = -20.0
    # The measured response: the noise floor followed Ref down, so the placement got worse.
    observe(dev, ctl, peak=-121.0, floor=-134.0)
    assert ctl.pending is None                           # too soon to step...
    assert ctl.tracker('std')['refit_due'] is True       # ...but the loop is still armed
    clock[0] += SAFETY_INTERVAL_S
    observe(dev, ctl, peak=-121.0, floor=-134.0)
    assert ctl.pending == ('std', -40.0)                 # so it steps again


def test_the_refit_keeps_going_when_a_step_undershoots(clock):
    """Measured on the SAN-90: the trace follows Ref by roughly half, so one step lands short.

    At centre 20 MHz / span 1 MHz with the automatic attenuator on, Ref -10/-20/-30/-40/-50 dBm
    put the noise floor 12.3/13.9/11.0/2.7/-7.2 dB below the bottom edge of a 100 dB window
    (attenuation 12/15/6/0/0 dB). An open-loop fit from the -115 dBm floor at Ref 0 computed
    -20 dBm and left the trace 14 dB under the canvas - the reported "Auto adjusted but the trace
    is still invisible". The loop re-measures after every step and reaches Ref -50 dBm, the first
    level where the trace is on the canvas.
    """
    # floor(ref) from the measured table; below -10 dBm the -10/-20 slope is extrapolated.
    measured = ((-10.0, -122.3), (-20.0, -133.9), (-30.0, -141.0), (-40.0, -142.7),
                (-50.0, -142.8))

    def floor_at(ref: float) -> float:
        pts = sorted(measured)
        if ref <= pts[0][0]:
            return pts[0][1]
        for (r0, f0), (r1, f1) in zip(pts, pts[1:], strict=False):
            if r0 <= ref <= r1:
                return f0 + (f1 - f0) * (ref - r0) / (r1 - r0)
        return pts[-1][1]

    def trace_at(ref: float):
        floor = floor_at(ref)
        return floor + 2.0, floor               # no signal: the peak is the noise

    dev = StubDevice(ref_level=0.0)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')                    # the first settle records the geometry
    dev.state.span_hz = 1e6                    # the user's settings change...
    ctl.begin_settle('std')                    # ...arms the closed loop

    def step():
        """One settled frame of the closed loop; returns the level applied, if any."""
        clock[0] += SAFETY_INTERVAL_S + 1.0
        peak, floor = trace_at(ctl.ref_level('std'))
        observe(dev, ctl, peak=peak, floor=floor)
        if ctl.pending is None:
            return None
        applied = ctl.pending[1]
        ctl.apply_pending()
        dev.state.ref_level = applied           # the device landed it
        return applied

    first = step()
    assert first is not None
    # The open-loop step does NOT fit the trace (this is the reported failure).
    floor_after = trace_at(first)[1]
    assert floor_after < first - ctl.window_db()
    applied = [first]
    while len(applied) <= REFIT_ATTEMPTS:
        nxt = step()
        if nxt is None:
            break
        applied.append(nxt)
    assert len(applied) >= 2                  # the loop was needed
    ref = ctl.ref_level('std')
    assert ctl.tracker('std')['last_noise_floor'] >= ref - ctl.window_db()   # finally inside
    assert ctl.pending is None
    clock[0] += 10 * SAFETY_INTERVAL_S
    peak, floor = trace_at(ref)
    observe(dev, ctl, peak=peak, floor=floor)
    assert ctl.pending is None                 # and it stopped there: no hunting


def test_a_settled_placement_is_not_disturbed_by_a_wobbling_trace(clock):
    """The anti-hunt rule: once the loop is satisfied, only a NEW geometry change re-arms it.

    Measured on the bench (13.825 MHz / 1 MHz, 100 dB window): after Auto settled at Ref -10 dBm the
    noise floor wobbled between 8.0 and 12.6 dB above the bottom edge for 20 s and the level never
    moved again (`auto_ref.seq` stayed put, `adjusting` false). A fit that re-decided on every frame
    would instead hunt around the acceptance band - "the spectrum keeps breathing" - so a settled
    tracker must not act on an observation at all, and only the geometry signature may wake it.
    """
    dev = StubDevice(ref_level=0.0, ref_range_db=100.0)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')                          # records the geometry the fit is judged against
    clock[0] += 1.0
    observe(dev, ctl, peak=-30.0, floor=-95.0)       # floor 5 dB above the bottom: already good
    assert ctl.fit('std') == ('ok', None)
    settled = ctl.tracker('std')['seq']
    for wobble in (-2.0, 2.0, -1.0, 3.0, -3.0, 2.5):
        observe(dev, ctl, peak=-30.0 - wobble, floor=-95.0 + wobble)
    assert ctl.pending is None
    assert ctl.tracker('std')['seq'] == settled      # not one new decision
    # A real settings change is what may move the level again - and exactly one placement per change.
    dev.state.span_hz = 2e6
    ctl.begin_settle('std')
    assert ctl.tracker('std')['refit_due'] is True
    assert ctl.tracker('std')['refit_left'] == 4


def test_the_refit_gives_up_after_its_budget(clock):
    """A trace that reacts MORE than Ref (so no level settles it) must stop, not hunt.

    The formula is a contraction while the trace follows Ref by less than 1:1 (the real device);
    with a 2:1 reaction each step asks for another 5 dB and only the budget ends it.
    """
    dev = StubDevice(ref_level=0.0)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')
    dev.state.span_hz = 1e6
    ctl.begin_settle('std')
    steps = 0
    for _ in range(REFIT_ATTEMPTS + 3):
        clock[0] += SAFETY_INTERVAL_S + 1.0
        floor = -100.0 + 2.0 * ctl.ref_level('std')      # runs away from the formula
        observe(dev, ctl, peak=floor + 2.0, floor=floor)
        if ctl.pending is None:
            break
        steps += 1
        dev.state.ref_level = ctl.pending[1]
        ctl.apply_pending()
    assert steps == REFIT_ATTEMPTS                     # bounded, then it stops
    assert ctl.pending is None
    assert ctl.tracker('std')['refit_due'] is False


def test_a_manual_ref_survives_a_later_settings_change(clock):
    """Reported twice: Auto pulled a level the user had set back down. A settings change must not
    become a loophole - only the explicit press re-fits a manual level."""
    dev = StubDevice(ref_level=0.0, span_hz=100e6)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle('std')
    clock[0] += 1.0
    ctl.reset('std', manual=True)                  # the SET_REF path
    dev.state.span_hz = 1e6
    ctl.begin_settle('std')
    clock[0] += 1.0
    observe(dev, ctl, peak=-100.5, floor=-101.0)   # the manual level left the trace below
    assert ctl.pending is None                     # left alone
    assert ctl.fit('std') == ('applied', -5.0)     # the press is what fixes it


def test_a_retune_lift_is_followed_by_one_refit_for_the_new_geometry(clock):
    """prepare_retune() lifts a fitted low Ref to 0 dBm before retuning (IF safety). The new
    geometry then has no valid placement, so its first frame places it again - once."""
    dev = StubDevice(ref_level=-25.0, span_hz=1e6)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-100.5, floor=-101.0)
    assert ctl.fit('std') == ('applied', -5.0)
    ctl.apply_pending()
    assert dev.state.ref_level == -5.0
    ctl.begin_settle('std')                        # configure_swp() does this after applying
    dev.state.center_hz = 20e6                     # the retune
    assert ctl.prepare_retune('std') is True       # ...and calls begin_settle again
    assert dev.state.ref_level == 0.0
    clock[0] += SAFETY_INTERVAL_S + 1.0
    observe(dev, ctl, peak=-100.5, floor=-101.0)
    assert ctl.pending == ('std', -5.0)


# ---------------- the reported scenario: no signal + a small span/bandwidth ----------------

#: The three settings from the report, with the observation that goes with them. A small span or
#: capture bandwidth lowers the in-band noise, so with no external signal the floor sits just
#: under the bottom edge of the default 100 dB window (Ref 0) - the reported "trace not on the
#: canvas / only a sliver under the bottom". The floor/peak pair is the noise-only estimate the
#: paths feed the loop (peak 0.5 dB over the floor: there is no signal to anchor to).
NO_SIGNAL_SCENARIOS = (
    ('std', {'center_hz': 20e6, 'span_hz': 1e6}),               # SWP center 20 MHz / span 1 MHz
    ('rta', {'rta_center_hz': 20e6, 'rta_span_hz': 1.59e6}),    # RTA center 20 MHz / 1.59 MHz
    ('sdr', {'sdr_center_hz': 20e6, 'sdr_decimate': 64}),       # SDR center 20 MHz
)


@pytest.mark.parametrize('mode, geometry', NO_SIGNAL_SCENARIOS)
def test_the_reported_no_signal_scenario_ends_inside_the_window(clock, mode, geometry):
    """SWP 20 MHz/1 MHz, RTA 20 MHz/1.59 MHz, SDR 20 MHz - no external signal.

    Changing the setting arms ONE re-fit; the first settled frame places the noise-only trace so
    that the whole of it is inside the window, and nothing moves again while the setting stands.
    """
    dev = StubDevice(ref_level=0.0, mode=mode)
    dev.state.sdr_ref_set = True                  # this scenario re-fits an existing level
    dev.session = Session(mode)
    ctl = AutoReferenceController(dev)
    ctl.begin_settle(mode)                        # the geometry before the user's change
    clock[0] += 1.0
    for field, value in geometry.items():
        setattr(dev.state, field, value)          # the user changes the setting
    ctl.begin_settle(mode)
    clock[0] += 1.0
    observe(dev, ctl, peak=-100.5, floor=-101.0, mode=mode)
    assert ctl.view(mode)['result'] == 'applied'
    # Once a level exists, SDR's fit is display-only (writing the IQS level reconfigures the stream
    # and moves the very trace the fit reads, which oscillates); the other modes own the device
    # level. This scenario is a RE-fit - the first SDR placement is the one that writes, and it has
    # its own test.
    if mode != 'sdr':
        assert ctl.apply_pending()
    else:
        assert ctl.apply_pending() is False
    # The whole trace is inside the window now: floor above the bottom edge, peak under the top.
    # SDR's reference is the fitted display level (the client applies it to its scale), not the device
    # level - which is exactly why the fit no longer writes the device.
    tracker, ref = ctl.tracker(mode), (ctl.view('sdr')['target'] if mode == 'sdr' else ctl.ref_level(mode))
    assert tracker['last_noise_floor'] >= ref - ctl.window_db()
    assert tracker['last_peak'] <= ref
    clock[0] += 3 * SAFETY_INTERVAL_S
    observe(dev, ctl, peak=-100.5, floor=-101.0, mode=mode)
    assert ctl.pending is None                    # one placement per settings change, not a loop


def test_the_reported_scenarios_through_the_fake_backend(clock, monkeypatch):
    """The whole path in one process: fake device + its sessions + the real control loop.

    The fake's synthetic carrier is replaced by the noise-only trace the bench shows with no
    source, which is the condition the report is about, so each of the three settings goes
    through frame -> observation -> settings change -> ONE re-fit -> apply and ends with the
    whole trace inside the window. The e2e fake keeps its carrier (its UI checks derive their
    levels from the measurement), so this test is where "no external signal" is exercised
    end-to-end.
    """
    import numpy as np

    from web_sa.hardware import fake_device as hardware_fake
    from web_sa.measurements import fake as measurement_fake

    # A noise-only trace a dB under the bottom edge: no signal, just the floor.
    monkeypatch.setattr(hardware_fake, 'NOISE_DBM', -101.0)
    monkeypatch.setattr(hardware_fake, 'PEAK_DBM', -100.5)

    def noise_only(self, center, span, points):
        freq = np.linspace(center - span / 2.0, center + span / 2.0, points)
        powers = -101.0 + self._rng.normal(0.0, 0.5, points)
        return freq, powers.astype(np.float32)

    monkeypatch.setattr(measurement_fake._FakeRtaBase, '_spectrum', noise_only)

    # (mode, how the user changes the setting, the state that restates it)
    scenarios = (
        ('std', None, {'center_hz': 20e6, 'span_hz': 1e6}),
        ('rta', lambda s: s.set_params(center=20e6, span=1.59e6),
         {'rta_center_hz': 20e6, 'rta_span_hz': 1.59e6}),
        ('sdr', lambda s: s.set_params(center=20e6, decimate=64),
         {'sdr_center_hz': 20e6, 'sdr_decimate': 64}),
    )

    for mode, change, geometry in scenarios:
        dev = hardware_fake.FakeDevice()
        dev.open()
        dev.state.ref_level = 0.0
        if mode == 'sdr':
            dev.state.sdr_ref_set = True          # this scenario re-fits an existing SDR level
        dev.state.ref_range_db = 100.0
        session = None
        if mode == 'std':
            dev.configure_swp()                    # the starting geometry is recorded here
            for field, value in geometry.items():
                setattr(dev.state, field, value)    # the user changes center/span
            dev.configure_swp()                    # ...which arms ONE re-fit
            clock[0] += 1.0
            dev.fetch_sweep()                      # the first settled frame of the new geometry
        else:
            session = (measurement_fake.FakeRtaSession(dev) if mode == 'rta'
                       else measurement_fake.FakeSdrSession(dev))
            dev.set_session(session)
            session.enter()                        # records the starting geometry
            change(session)
            clock[0] += 1.0
            dev.step()                             # the first settled frame of the new geometry
        assert dev.auto_ref.view(mode)['result'] == 'applied', mode
        # SDR's fit is display-only: writing the IQS level reconfigures the stream (and moves the trace
        # the fit reads), so nothing is applied for it - the client's display scale owns the placement.
        if mode == 'sdr':
            # A re-fit of a level that already exists stays display-only (the FIRST placement is the
            # one exception, and it is covered by its own test).
            assert dev.apply_pending_auto_reference() is False, mode
        else:
            assert dev.apply_pending_auto_reference(), mode
        # Applying reconfigures (and clears the observation), so read the placement from the next
        # settled frame - which must not queue anything: one re-fit per settings change.
        clock[0] += 3 * SAFETY_INTERVAL_S
        if mode == 'std':
            dev.fetch_sweep()
        else:
            dev.step()
        assert dev.auto_ref.pending is None, mode
        tracker = dev.auto_ref.tracker(mode)
        # SDR's reference is the fit's display target (the client applies it to its own scale); the
        # device level is deliberately left alone (see AutoReferenceController.apply_pending).
        ref = (dev.auto_ref.view('sdr')['target'] if mode == 'sdr'
               else dev.auto_ref.ref_level(mode))
        assert tracker['last_noise_floor'] >= ref - dev.auto_ref.window_db(), f'{mode} floor'
        assert tracker['last_peak'] <= ref, f'{mode} peak'


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
    dev2 = StubDevice(ref_level=-25.0)           # a fresh tracker: nothing was ever placed
    assert AutoReferenceController(dev2).prepare_retune('std') is False


def test_prepare_retune_leaves_a_level_the_user_set(clock):
    """Reported on the bench: a frequency change reset a manually set Ref to 0 dBm.

    The lift exists for a level the FIT lowered, and `last_target` is the loop's memory of a
    placement - it survives the user taking the level over, so without this guard every retune after
    any fit in that mode silently undid the operator's own setting (measured: Ref -40 -> 0 on a
    centre change, with no message at all).
    """
    dev = StubDevice(ref_level=-40.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-30.0, floor=-95.0)
    ctl.fit('std')                               # a placement happened in this mode
    assert ctl.tracker('std')['last_target'] is not None
    ctl.reset('std', manual=True)                # ...and then the user took the level over
    assert ctl.tracker('std')['user_level'] is True
    assert ctl.prepare_retune('std') is False
    assert dev.state.ref_level == -40.0          # the user's level is untouched
    # A fit that places a level again owns it, so the safety lift comes back for THAT level.
    ctl.tracker('std')['user_level'] = False
    assert ctl.prepare_retune('std') is True
    assert dev.state.ref_level == 0.0


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


def _caps(ref_min: float, ref_max: float):
    """A capability row for this bench's model with the given Ref range."""
    from web_sa.config import DeviceCapabilities
    caps = DeviceCapabilities.from_model(67)
    caps.ref_min_dbm, caps.ref_max_dbm = ref_min, ref_max
    return caps


def test_the_target_clamp_follows_the_device_capability(clock):
    """The loop's ceiling/floor come from the device's capability row, not from literals."""
    dev = StubDevice(ref_level=-20.0)
    dev.state.caps = _caps(-45.0, 10.0)
    ctl = AutoReferenceController(dev)
    observe(dev, ctl, peak=-25.0, floor=-40.0)   # would target ~30 dBm with the old literals
    result, target = ctl.fit('std')
    assert result == 'applied'
    assert target == 10.0                        # clamped to the reported maximum
    dev.state.ref_level = 6.0                    # and the overflow escape cannot exceed it
    dev.state.status_warning = -12
    ctl.tracker('std')['last_change'] = -10.0
    assert ctl.nudge_out_of_overflow() is True
    assert ctl.pending == ('std', 10.0)
