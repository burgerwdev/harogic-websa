"""Hardware smoke-test safety rules."""
import pytest

from tools.hardware_smoke import TinySaSource, safe_tinysa_level
from web_sa.supervisor import should_restart


@pytest.mark.parametrize(
    ('frequency', 'expected'),
    [
        (1e9, -25.0),
        (6e9, -25.0),
        (6.1e9, -35.0),
        (7e9, -35.0),
        (7.1e9, -42.0),
        (9e9, -42.0),
    ],
)
def test_safe_tinysa_level(frequency, expected):
    assert safe_tinysa_level(frequency) == expected


def test_safe_tinysa_level_rejects_above_9ghz():
    with pytest.raises(ValueError):
        safe_tinysa_level(9.1e9)


def test_tinysa_configures_safely_before_enabling():
    source = object.__new__(TinySaSource)
    commands = []

    def command(value):
        commands.append(value)
        if value == 'sweep':
            return 'sweep\r\n7000000000 7000000000 450\r\nch> '
        if value == 'status':
            return 'status\r\nResumed\r\nch> '
        return f'{value}\r\nch> '

    source.command = command
    assert source.enable_cw(7e9, 'mixer') == -35.0
    assert commands[:4] == ['output off', 'level -42', 'mode output', 'output off']
    assert commands.index('sweep cw 7000000000') < commands.index('level -35')
    assert commands.index('level -35') < commands.index('output on')


def test_supervisor_only_restarts_native_or_timeout_failures():
    assert should_restart(-11)
    assert should_restart(70)
    assert not should_restart(0)
    assert not should_restart(1)
