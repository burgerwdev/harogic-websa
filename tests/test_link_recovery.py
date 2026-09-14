"""Device link recovery: unplug detection, auto-reopen and session resume.

Two layers, matching the testing strategy in docs/*/DEVELOPMENT.md §6:

* the worker link loop is exercised with a stub device (no vendor library, runs in CI);
* the transport watchdog on the real device class is vendor-gated (``vendor_required``).

The bug these pin (reported on the bench): after a physical unplug the spectrum froze but
STATUS still said ``connected: true``, and plugging the analyzer back in did not resume it.
"""
from __future__ import annotations

import asyncio

import pytest

from web_sa.config import LINK_POLL_INTERVAL
from web_sa.hardware.state import DeviceState
from web_sa.main import _link_loop
from web_sa.web.app_keys import COMMAND_LOCK


class LinkStub:
    """Just enough device for the link loop: state, one reopen and the session host."""

    def __init__(self, reconnect: bool = True):
        self.state = DeviceState()
        self.state.connected = False
        self.session = None
        self.reopen_calls = 0
        self._reconnect = reconnect

    def reopen(self):
        self.reopen_calls += 1
        if not self._reconnect:
            return False, 'Device_Open status=-1'
        self.state.connected = True
        self.state.label = 'STUB'
        return True, 'ok'

    def set_session(self, session):
        self.session = session
        self.state.mode = session.name if session else 'std'


def _run_link_loop(dev, seconds: float) -> None:
    async def drive():
        app = {COMMAND_LOCK: asyncio.Lock()}
        task = asyncio.create_task(_link_loop(app, dev))
        await asyncio.sleep(seconds)
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)

    asyncio.run(drive())


@pytest.fixture
def fast_poll(monkeypatch):
    monkeypatch.setattr('web_sa.config.LINK_POLL_INTERVAL', 0.01)
    assert LINK_POLL_INTERVAL > 0.01        # the override really changed the module attribute


def test_link_loop_reopens_and_resumes_the_session(fast_poll):
    dev = LinkStub()
    _run_link_loop(dev, 0.08)
    assert dev.reopen_calls == 1
    assert dev.state.connected is True
    assert dev.session is not None and dev.session.name == 'std'


def test_link_loop_keeps_retrying_a_device_that_is_still_gone(fast_poll):
    dev = LinkStub(reconnect=False)
    _run_link_loop(dev, 0.08)
    assert dev.reopen_calls >= 2
    assert dev.state.connected is False
    assert dev.session is None


def test_link_loop_does_nothing_while_connected(fast_poll):
    dev = LinkStub()
    dev.state.connected = True
    _run_link_loop(dev, 0.05)
    assert dev.reopen_calls == 0


# ---------------- transport watchdog (vendor library) ----------------

def test_consecutive_bus_errors_declare_the_link_lost(vendor_required):
    from web_sa.hardware.device import _LINK_ERROR_LIMIT, HarogicDevice

    dev = HarogicDevice()
    dev.state.connected = True
    for i in range(_LINK_ERROR_LIMIT):
        declared = dev.note_link_status(-8, 'SWP_GetFullSweep')
        # Connected until the run is long enough to be a real disconnect, not a glitch.
        assert dev.state.connected is (i < _LINK_ERROR_LIMIT - 1)
        assert declared is (i == _LINK_ERROR_LIMIT - 1)
    assert dev.state.connected is False
    assert 'SWP_GetFullSweep' in dev.state.last_error


def test_a_good_frame_clears_the_bus_error_run(vendor_required):
    from web_sa.hardware.device import _LINK_ERROR_LIMIT, HarogicDevice

    dev = HarogicDevice()
    dev.state.connected = True
    for _ in range(_LINK_ERROR_LIMIT - 1):
        dev.note_link_status(-9, 'SWP_GetFullSweep')
    dev.note_link_status(0, 'SWP_GetFullSweep')       # one good frame
    for _ in range(_LINK_ERROR_LIMIT - 1):
        dev.note_link_status(-9, 'SWP_GetFullSweep')
    assert dev.state.connected is True


def test_vendor_warnings_never_declare_the_link_lost(vendor_required):
    from web_sa.hardware.device import _LINK_ERROR_LIMIT, HarogicDevice

    dev = HarogicDevice()
    dev.state.connected = True
    for _ in range(_LINK_ERROR_LIMIT * 3):
        assert dev.note_link_status(-12, 'SWP_GetFullSweep') is False
    assert dev.state.connected is True


def test_fetch_sweep_reports_a_disconnect_in_connected(vendor_required, monkeypatch):
    """The reported bug: a frozen trace kept ``connected: true``."""
    from web_sa.hardware import sdk_bindings as sb
    from web_sa.hardware.device import _LINK_ERROR_LIMIT, HarogicDevice

    dev = HarogicDevice()
    dev.state.connected = True
    dev._trace_points = 100
    monkeypatch.setattr(sb.dll, 'SWP_GetFullSweep', lambda *args: -8)
    for _ in range(_LINK_ERROR_LIMIT):
        assert dev.fetch_sweep() is None
    assert dev.state.connected is False


def test_reopen_closes_the_dead_handle_before_opening(vendor_required, monkeypatch):
    from web_sa.hardware.device import HarogicDevice

    dev = HarogicDevice()
    calls = []
    monkeypatch.setattr(dev, 'close', lambda: calls.append('close'))
    monkeypatch.setattr(dev, 'open', lambda: (calls.append('open'), (False, 'no device'))[1])
    assert dev.reopen() == (False, 'no device')
    assert calls == ['close', 'open']
