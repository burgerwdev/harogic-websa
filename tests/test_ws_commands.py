"""Command validation for atomic frequency and reference controls."""
from types import SimpleNamespace

import pytest

from web_sa.config import DeviceCapabilities
from web_sa.hardware.device import DeviceState
from web_sa.web.ws import CommandError, _dispatch, _validate_command


class StubDevice:
    def __init__(self):
        self.state = DeviceState(
            connected=True,
            caps=DeviceCapabilities.from_model(67),
            pnm_supported=True,
        )


def test_frequency_accepts_atomic_start_stop():
    data = {'cmd': 'SET_FREQ', 'start': 950e6, 'stop': 1050e6}
    _validate_command(StubDevice(), data['cmd'], data)
    assert data['stop'] - data['start'] == 100e6


def test_frequency_rejects_mixed_or_incomplete_assignments():
    dev = StubDevice()
    with pytest.raises(CommandError):
        data = {'cmd': 'SET_FREQ', 'center': 1e9, 'start': 950e6, 'stop': 1050e6}
        _validate_command(dev, data['cmd'], data)
    with pytest.raises(CommandError):
        data = {'cmd': 'SET_FREQ', 'start': 950e6}
        _validate_command(dev, data['cmd'], data)


def test_reference_auto_needs_no_value_but_manual_does():
    dev = StubDevice()
    auto = {'cmd': 'SET_REF', 'mode': 'auto'}
    _validate_command(dev, auto['cmd'], auto)
    with pytest.raises(CommandError):
        manual = {'cmd': 'SET_REF', 'mode': 'manual'}
        _validate_command(dev, manual['cmd'], manual)


@pytest.mark.asyncio
async def test_device_level_settings_reconfigure_active_rta_not_swp():
    dev = StubDevice()
    dev.state.mode = 'rta'

    class RtaSession:
        name = 'rta'
        reconfigures = 0
        sweep = None

        def reconfigure(self):
            self.reconfigures += 1

        def set_sweep(self, mode, time):
            self.sweep = (mode, time)

    dev.session = RtaSession()
    dev.configure_swp = lambda: (_ for _ in ()).throw(AssertionError('SWP configured'))

    assert await _dispatch(dev, 'SET_REFCK', {'cmd': 'SET_REFCK', 'mode': 'external'})
    assert dev.state.ref_clock == 'external'
    assert dev.session.reconfigures == 1

    assert await _dispatch(dev, 'SET_REFCKOUT', {'cmd': 'SET_REFCKOUT', 'on': True})
    assert dev.state.refclk_out is True
    assert dev.session.reconfigures == 2

    assert await _dispatch(dev, 'SET_AMP', {'cmd': 'SET_AMP', 'atten': 10})
    assert dev.state.atten == 10
    assert dev.session.reconfigures == 3

    assert await _dispatch(dev, 'SET_SWEEP', {'cmd': 'SET_SWEEP', 'time': 0.5})
    assert dev.session.sweep == (2, 0.5)


@pytest.mark.asyncio
async def test_swp_commands_are_rejected_during_measurement_sessions():
    dev = StubDevice()
    dev.state.mode = 'pnm'
    dev.session = SimpleNamespace(name='pnm')
    for cmd, payload in (
        ('SET_FREQ', {'cmd': 'SET_FREQ', 'center': 1e9}),
        ('SET_RBW', {'cmd': 'SET_RBW', 'mode': 'auto'}),
        ('SET_AMP', {'cmd': 'SET_AMP', 'atten': 10}),
    ):
        with pytest.raises(CommandError, match='measurement is active'):
            await _dispatch(dev, cmd, payload)


@pytest.mark.asyncio
async def test_swp_only_commands_are_rejected_in_rta_mode():
    dev = StubDevice()
    dev.state.mode = 'rta'
    dev.session = SimpleNamespace(name='rta')
    with pytest.raises(CommandError, match='only available in SWP mode'):
        await _dispatch(dev, 'SET_WINDOW', {'cmd': 'SET_WINDOW', 'window': 1})


@pytest.mark.asyncio
async def test_set_rta_requires_rta_mode():
    dev = StubDevice()
    dev.state.mode = 'std'
    dev.session = None
    with pytest.raises(CommandError, match='requires RTA mode'):
        await _dispatch(dev, 'SET_RTA', {'cmd': 'SET_RTA', 'center': 1e9})


def test_detector_choice_is_validated():
    dev = StubDevice()
    good = {'cmd': 'SET_DETECTOR', 'mode': 'rms'}
    _validate_command(dev, good['cmd'], good)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_DETECTOR', 'mode': 'nope'}
        _validate_command(dev, bad['cmd'], bad)


def test_model_limits_come_from_capabilities():
    """A new SAN model must be a DeviceCapabilities row, not an edit to the validation chain.

    Tightening the capability fields below must immediately change what the command layer
    accepts; if a hard-coded bound is ever reintroduced, one of these asserts fails
    (report finding E-2, guard rail in tools/quality/architecture_guard.py).
    """
    caps = DeviceCapabilities.from_model(67)
    caps.rbw_max_hz = 1e6
    caps.vbw_max_hz = 1e6
    caps.points_max = 1001
    caps.atten_max = 20
    caps.ifgain_max = 1
    caps.decimate_max = 64
    caps.rta_span_max_hz = 10e6
    caps.ref_max_dbm = 0.0
    dev = StubDevice()
    dev.state.caps = caps

    ok = {'cmd': 'SET_RBW', 'mode': 'manual', 'rbw': 1e6}
    _validate_command(dev, ok['cmd'], ok)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_RBW', 'mode': 'manual', 'rbw': 2e6}
        _validate_command(dev, bad['cmd'], bad)

    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_POINTS', 'points': 1002}
        _validate_command(dev, bad['cmd'], bad)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_AMP', 'atten': 21}
        _validate_command(dev, bad['cmd'], bad)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_AMP', 'ifgain': 2}
        _validate_command(dev, bad['cmd'], bad)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_SDR', 'decimate': 65}
        _validate_command(dev, bad['cmd'], bad)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_RTA', 'span': 11e6}
        _validate_command(dev, bad['cmd'], bad)
    with pytest.raises(CommandError):
        bad = {'cmd': 'SET_REF', 'mode': 'manual', 'ref': 1.0}
        _validate_command(dev, bad['cmd'], bad)


class ScaleDevice(StubDevice):
    """Device state plus the real reference controller, for the Auto Scale command."""

    def __init__(self):
        super().__init__()
        import threading

        from web_sa.hardware.auto_reference import AutoReferenceController

        self.state.ref_level = -20.0
        self.state.ref_range_db = 100.0
        self._hw = threading.RLock()
        self.session = None
        self.configured = 0
        self.auto_ref = AutoReferenceController(self)

    def configure_swp(self):
        self.configured += 1
        return True, 'ok'

    def auto_scale(self, mode, current=None):
        return self.auto_ref.fit(mode, current)

    def apply_pending_auto_reference(self):
        return self.auto_ref.apply_pending()

    def auto_reference_scope(self):
        return getattr(self.session, 'auto_ref_scope', 'std')

    def observe_reference_peak(self, mode, peak, floor=None):
        self.auto_ref.observe_peak(mode, peak, floor)


@pytest.mark.asyncio
async def test_auto_scale_places_the_reference_once():
    dev = ScaleDevice()
    dev.observe_reference_peak('std', -30.0, -95.0)   # floor 25 dB above the bottom: too high
    assert await _dispatch(dev, 'AUTO_SCALE', {'cmd': 'AUTO_SCALE', 'range_db': 100.0})
    assert dev.auto_ref.pending == ('std', 0.0)

    # Applied, then a second press on a well-placed trace must not re-enter the device. The
    # command still reports (a STATUS push carries `result: ok` to the UI), but nothing moves.
    assert dev.apply_pending_auto_reference()
    assert dev.state.ref_level == 0.0
    assert dev.configured == 1
    dev.observe_reference_peak('std', -25.0, -95.0)
    await _dispatch(dev, 'AUTO_SCALE', {'cmd': 'AUTO_SCALE', 'range_db': 100.0})
    assert dev.auto_ref.pending is None
    assert dev.auto_ref.view('std')['result'] == 'ok'
    assert dev.configured == 1                    # no second reconfiguration


@pytest.mark.asyncio
async def test_legacy_set_ref_auto_runs_one_fit_and_latches_no_mode():
    dev = ScaleDevice()
    dev.observe_reference_peak('std', -30.0, -95.0)
    assert await _dispatch(dev, 'SET_REF', {'cmd': 'SET_REF', 'mode': 'auto', 'range_db': 100.0})
    assert dev.auto_ref.pending == ('std', 0.0)
    assert dev.state.ref_mode == 'manual'      # there is no tracking mode to latch any more


@pytest.mark.asyncio
async def test_auto_scale_is_rejected_while_a_measurement_owns_the_device():
    dev = ScaleDevice()
    dev.state.mode = 'pnm'
    dev.session = SimpleNamespace(name='pnm')
    with pytest.raises(CommandError, match='measurement is active'):
        await _dispatch(dev, 'AUTO_SCALE', {'cmd': 'AUTO_SCALE'})


@pytest.mark.asyncio
async def test_auto_scale_judges_the_level_the_client_shows():
    """In SDR the display scale is client-side, so the fit must use the visible level.

    Measured on the fake backend: the device level was well placed while the display was clipped,
    so judging by the device level answered 'ok' and pressing Auto appeared to do nothing.
    """
    dev = ScaleDevice()
    dev.observe_reference_peak('std', -30.0, -95.0)
    dev.state.ref_level = 0.0
    # The device level is fine, but the client shows -40 dBm (the trace is clipped at the top).
    assert await _dispatch(dev, 'AUTO_SCALE',
                           {'cmd': 'AUTO_SCALE', 'range_db': 100.0, 'current_ref': -40.0})
    assert dev.auto_ref.view('std')['result'] == 'applied'
    assert dev.auto_ref.view('std')['target'] == 0.0


@pytest.mark.asyncio
async def test_auto_scale_rejects_a_current_ref_outside_the_device_range():
    dev = ScaleDevice()
    with pytest.raises(CommandError):
        await _dispatch(dev, 'AUTO_SCALE', {'cmd': 'AUTO_SCALE', 'current_ref': -120.0})


@pytest.mark.asyncio
async def test_auto_scale_works_in_sdr_through_the_same_command():
    """The SDR fit goes through AUTO_SCALE like the other modes (one implementation).

    The client applies the reported target to its display scale; the device level is only
    written when it is off by more than the dead band.
    """
    dev = ScaleDevice()
    dev.state.mode = 'sdr'

    class SdrSession:
        name = 'sdr'
        auto_ref_scope = 'sdr'
        reconfigures = 0

        def reconfigure(self):
            self.reconfigures += 1

    dev.session = SdrSession()
    assert not await _dispatch(dev, 'AUTO_SCALE', {'cmd': 'AUTO_SCALE'})   # no trace yet
    assert dev.auto_ref.view('sdr')['result'] == 'no_data'

    dev.observe_reference_peak('sdr', -30.0, -95.0)
    assert await _dispatch(dev, 'AUTO_SCALE', {'cmd': 'AUTO_SCALE', 'range_db': 100.0})
    assert dev.auto_ref.pending == ('sdr', 0.0)
    assert dev.state.ref_level == -20.0                    # not applied yet
    assert dev.apply_pending_auto_reference()
    assert dev.state.ref_level == 0.0
    assert dev.session.reconfigures == 1

    # A fitted level within the dead band of the device level must not touch IQS at all
    # (that write interrupts the audio), while the target is still reported.
    dev.auto_ref.pending = ('sdr', 2.0)
    assert not dev.apply_pending_auto_reference()
    assert dev.session.reconfigures == 1
