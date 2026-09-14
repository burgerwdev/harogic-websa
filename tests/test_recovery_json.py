"""The fatal-exit contract and the JSON boundary helpers (report findings P1-10/P1-11)."""
from __future__ import annotations

import json
import math

import pytest

from web_sa.supervisor import should_restart
from web_sa.web import recovery
from web_sa.web.jsonutil import dumps_json, finite_json, is_periodic_status


def test_fatal_exits_with_the_supervisor_restart_code(monkeypatch):
    seen = {}

    def fake_exit(code):
        seen['code'] = code
        raise SystemExit(code)

    monkeypatch.setattr(recovery.os, '_exit', fake_exit)
    with pytest.raises(SystemExit):
        recovery.fatal('boom')
    assert seen['code'] == recovery.EXIT_FATAL


def test_supervisor_restarts_on_the_fatal_code_and_signals():
    assert should_restart(recovery.EXIT_FATAL)
    assert should_restart(-11)          # killed by a signal (native crash)
    assert not should_restart(0)        # clean exit
    assert not should_restart(1)        # configuration error: do not loop


@pytest.mark.parametrize('value,expected', [
    (1.5, 1.5),
    (float('nan'), None),
    (float('inf'), None),
    (float('-inf'), None),
])
def test_finite_json_replaces_non_finite_floats(value, expected):
    assert finite_json(value) == expected


def test_finite_json_walks_containers():
    payload = {'a': [1.0, float('nan')], 'b': {'c': float('inf')}, 'd': 'x', 'e': 2}
    assert finite_json(payload) == {'a': [1.0, None], 'b': {'c': None}, 'd': 'x', 'e': 2}


def test_dumps_json_is_strict_and_compact():
    text = dumps_json({'sdr': {'level_dbfs': float('nan')}, 'n': 1})
    assert json.loads(text) == {'sdr': {'level_dbfs': None}, 'n': 1}
    assert ' ' not in text                 # separators=(',', ':')
    assert math.isfinite(1.0)


def test_only_the_periodic_status_is_coalescable():
    assert is_periodic_status({'cmd': 'STATUS'})
    assert is_periodic_status({'cmd': 'STATUS', 'response_to': None})   # unset == periodic
    assert not is_periodic_status({'cmd': 'STATUS', 'response_to': 'SET_FREQ'})
