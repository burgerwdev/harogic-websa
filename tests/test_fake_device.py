"""The fake backend used by the CI UI smoke must keep behaving like the real device.

If the fake rots, the CI smoke fails with a confusing UI error; these tests fail first, at the
level of the contract the application relies on (finding A1).
"""
from __future__ import annotations

import numpy as np
import pytest

from web_sa.hardware.fake_device import FakeDevice, install_fake_sessions
from web_sa.measurements import _SESSIONS, make_session
from web_sa.measurements.fake import FakeRtaSession, FakeSdrSession


@pytest.fixture
def device():
    dev = FakeDevice()
    ok, err = dev.open()
    assert ok, err
    return dev


@pytest.fixture
def fake_sessions():
    """Install the fake sessions for one test and restore the factory afterwards."""
    original = dict(_SESSIONS)
    install_fake_sessions()
    try:
        yield
    finally:
        _SESSIONS.clear()
        _SESSIONS.update(original)


def test_open_reports_a_san90_with_capabilities(device):
    assert device.state.connected
    assert device.state.caps.name == 'SAN-90'
    assert device.state.caps.freq_min_hz < 1e4
    assert device.preset_defaults['points'] == 1000
    assert device.state.has_docxo


def test_fetch_sweep_returns_the_requested_points_and_a_carrier(device):
    device.state.points_req = 501
    device.state.center_hz = 433e6
    device.state.span_hz = 5e6
    ok, _ = device.configure_swp()
    assert ok
    freq, powers = device.fetch_sweep()
    assert len(freq) == len(powers) == 501
    assert freq[0] == pytest.approx(430.5e6) and freq[-1] == pytest.approx(435.5e6)
    peak = int(np.argmax(powers))
    assert abs(peak - 250) <= 3                       # the carrier sits at the centre
    assert -30 < float(powers[peak]) < -20             # ~-25 dBm as configured
    assert float(np.median(powers)) < -90              # noise floor
    assert device.state.actual['points'] == 501


def test_the_application_can_drive_it_through_the_session_factory(device, fake_sessions):
    std = make_session(device, 'std')
    assert std.name == 'std'
    _frames, _msgs = std.step()                        # uses dev.fetch_sweep
    assert device.last_freq is not None

    rta = make_session(device, 'rta')
    assert isinstance(rta, FakeRtaSession) and rta.is_ready()
    frames, _msgs = rta.step()
    assert frames and frames[0][:4] == b'RTAF'
    assert device.state.mode == 'rta'

    sdr = make_session(device, 'sdr')
    assert isinstance(sdr, FakeSdrSession)
    frames, _msgs = sdr.step()
    magics = {frame[:4] for frame in frames}
    assert magics == {b'RTAF', b'AUDF'}                # panadapter + audio
    assert device.state.sdr_actual['pan_points'] > 0


def test_preset_and_resets_write_through_the_flat_aliases(device):
    device.state.center_hz = 2e9
    device.state.points_req = 4000
    device.preset_state()
    assert device.state.center_hz == 1e9 and device.state.points_req == 1000
    device.state.rta_center_hz = 5e9
    device.state.trigger_level_dbm = 0.0
    device.reset_rta_state()
    assert device.state.rta_center_hz == 1e9
    assert device.state.trigger_level_dbm == -40.0


def test_it_runs_without_any_session_or_vendor_call(device):
    """The fake never imports the binding layer, so it works where libhtraapi does not exist."""
    assert device.step() == ([], [])                                  # no session yet
    assert device.query_gnss()['lock'] == 1
    assert device.calibrate_ref_clock(3) == (True, 100e6)
    assert device.nudge_reference_out_of_overflow() is False
    assert device.apply_pending_auto_reference() is False
    assert device.auto_reference_view()['pending'] is None
    device.close()
    assert not device.state.connected
