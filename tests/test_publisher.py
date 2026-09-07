"""Publisher watchdog timeout policy tests."""
from web_sa.hardware.device import DeviceState
from web_sa.web.publisher import _acquisition_timeout


class StubDevice:
    def __init__(self, state):
        self.state = state


def test_rta_has_short_native_call_watchdog():
    assert _acquisition_timeout(StubDevice(DeviceState(mode='rta'))) == 5.0


def test_manual_sweep_timeout_scales_with_requested_time():
    state = DeviceState(mode='std', sweep_time_mode=7, sweep_time=60.0)
    assert _acquisition_timeout(StubDevice(state)) == 95.0


def test_sweep_watchdog_is_bounded():
    state = DeviceState(mode='std', actual={'est_min': 1000.0})
    assert _acquisition_timeout(StubDevice(state)) == 180.0
