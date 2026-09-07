"""Command validation for atomic frequency and reference controls."""
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
async def test_set_rta_requires_rta_mode():
    dev = StubDevice()
    dev.state.mode = 'std'
    dev.session = None
    with pytest.raises(CommandError, match='requires RTA mode'):
        await _dispatch(dev, 'SET_RTA', {'cmd': 'SET_RTA', 'center': 1e9})
