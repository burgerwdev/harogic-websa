"""The command table is the single source of truth for the command layer.

These checks are structural on purpose: they fail when a command is added to one place and
forgotten in another, when a guard references a command that does not exist, or when a
handler is not awaitable. The behavioural side is covered by tests/test_ws_commands.py, the
HTTP/WS tests and (on the bench) tools/command_sweep.py.
"""
from __future__ import annotations

import inspect
from types import SimpleNamespace

import pytest

from web_sa.config import DeviceCapabilities
from web_sa.web import commands as cmd
from web_sa.web import ws as ws_module
from web_sa.web.commands import (
    COMMANDS,
    NOT_IN_SDR,
    SWP_ONLY,
    SWP_OWNED,
    CommandContext,
    CommandError,
    command_names,
    spec_for,
    validate,
)

EXPECTED = {
    'STATUS', 'CONNECT', 'SET_PRESET', 'CAL_REFCLK', 'SET_FREQ', 'SET_REF', 'SET_RBW',
    'SET_VBW', 'SET_SWEEP', 'SET_POINTS', 'SET_SPUR', 'SET_WINDOW', 'SET_DETECTOR',
    'SET_AMP', 'SET_REFCK', 'SET_REFCKOUT', 'SET_MODE', 'SET_SDR', 'SET_SDR_TUNE',
    'SET_SDR_DEMOD', 'SET_RTA', 'SET_TRIGGER', 'SET_HARM', 'SET_PNM',
}


def make_state(mode: str = 'std') -> SimpleNamespace:
    """Only the fields the command layer touches (keeps this test hardware-free)."""
    return SimpleNamespace(connected=True, mode=mode, caps=DeviceCapabilities.from_model(67),
                           pnm_supported=True, sweep_time_mode=0, rta_sweep_time_mode=2,
                           last_error='')


class StubDevice:
    def __init__(self, mode: str = 'std', session_name: str | None = None):
        self.state = make_state(mode)
        self.session = None if session_name is None else type('S', (), {'name': session_name})()


def test_the_table_covers_exactly_the_documented_commands():
    assert command_names() == EXPECTED
    assert ws_module._COMMANDS == EXPECTED        # the transport-facing set is derived


def test_every_handler_is_awaitable_and_every_spec_is_consistent():
    for name, spec in COMMANDS.items():
        assert spec.name == name
        assert inspect.iscoroutinefunction(spec.run), f'{name} handler must be async'
        if spec.validate is not None:
            assert callable(spec.validate)


def test_only_status_and_connect_work_without_a_device():
    no_device = {name for name, spec in COMMANDS.items() if not spec.needs_device}
    assert no_device == {'STATUS', 'CONNECT'}


@pytest.mark.parametrize('group', [SWP_OWNED, SWP_ONLY, NOT_IN_SDR])
def test_guard_groups_only_reference_real_commands(group):
    unknown = set(group) - command_names()
    assert unknown == set(), f'guard references unknown commands: {sorted(unknown)}'


def test_guard_groups_are_wired_into_validation():
    # SWP-owned commands are rejected while a measurement session owns the device
    with pytest.raises(CommandError) as excinfo:
        validate(StubDevice(session_name='harmonic'), 'SET_RBW', {'cmd': 'SET_RBW'})
    assert excinfo.value.code == 'cmd_unavailable_measurement'

    # SWP-only parameters do not exist in an RTA profile
    with pytest.raises(CommandError) as excinfo:
        validate(StubDevice(mode='rta'), 'SET_POINTS', {'cmd': 'SET_POINTS', 'points': 1000})
    assert excinfo.value.code == 'swp_only'

    # the SDR demod chain owns the configuration in SDR mode
    with pytest.raises(CommandError) as excinfo:
        validate(StubDevice(mode='sdr'), 'SET_SWEEP', {'cmd': 'SET_SWEEP'})
    assert excinfo.value.code == 'sdr_unsupported'


def test_unknown_and_malformed_commands_are_rejected_before_any_handler():
    device = StubDevice()
    with pytest.raises(CommandError) as excinfo:
        validate(device, 'NO_SUCH_COMMAND', {'cmd': 'NO_SUCH_COMMAND'})
    assert excinfo.value.code == 'unknown_command'

    with pytest.raises(CommandError) as excinfo:
        validate(device, 'SET_RBW', ['not', 'an', 'object'])
    assert excinfo.value.code == 'json_object_required'

    with pytest.raises(CommandError) as excinfo:
        validate(device, None, {})
    assert excinfo.value.code == 'unknown_command'


def test_spec_for_returns_the_registered_spec():
    assert spec_for('SET_PNM') is COMMANDS['SET_PNM']
    with pytest.raises(CommandError):
        spec_for('nope')


def test_command_context_routes_configure_calls_through_the_hardware_wrapper():
    """configure_swp/configure_active must never call the device synchronously."""
    calls: list[str] = []

    class Dev:
        state = make_state()
        session = None

        def configure_swp(self):
            calls.append('swp')
            return True, 'ok'

    async def hw_call(fn, *a, **kw):
        return fn(*a, **kw)

    import asyncio

    ctx = CommandContext(dev=Dev(), hw_call=hw_call)
    asyncio.run(ctx.configure_swp())          # a rejected config must not raise here
    assert calls == ['swp']

    class Rejecting(Dev):
        def configure_swp(self):
            return False, 'bad window'

    ctx = CommandContext(dev=Rejecting(), hw_call=hw_call)
    with pytest.raises(CommandError) as excinfo:
        asyncio.run(ctx.configure_swp())
    assert excinfo.value.code == 'hardware_config'
    assert 'bad window' in excinfo.value.params['detail']
    assert cmd.HW_CALL_TIMEOUT_S > 0
