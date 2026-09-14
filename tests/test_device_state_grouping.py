"""DeviceState is grouped by owner, with flat aliases for the transition (finding P1-8 step 2)."""
from __future__ import annotations

import dataclasses
import threading

import pytest

from web_sa.config import DeviceCapabilities
from web_sa.hardware.device import DeviceState, HarogicDevice
from web_sa.hardware.state import RtaParams, SdrParams, SwpParams, TriggerParams

GROUPS = (('swp', SwpParams), ('rta', RtaParams), ('sdr', SdrParams), ('trigger', TriggerParams))


def test_every_grouped_field_is_reachable_flat_and_through_its_group():
    state = DeviceState()
    for group, cls in GROUPS:
        for f in dataclasses.fields(cls):
            assert getattr(state, f.name) == getattr(getattr(state, group), f.name), (group, f.name)
            setattr(state, f.name, 'sentinel')
            assert getattr(getattr(state, group), f.name) == 'sentinel'
            setattr(getattr(state, group), f.name, 'other')
            assert getattr(state, f.name) == 'other'


def test_construction_accepts_flat_and_grouped_keys():
    flat = DeviceState(mode='rta', center_hz=2e9, sweep_time=60.0, sdr_demod='wfm')
    assert flat.mode == 'rta'
    assert flat.swp.center_hz == 2e9 and flat.swp.sweep_time == 60.0
    assert flat.sdr.sdr_demod == 'wfm'

    grouped = DeviceState(mode='std', swp=SwpParams(center_hz=1.5e9), rta=RtaParams(rta_span_hz=1e6))
    assert grouped.center_hz == 1.5e9 and grouped.rta_span_hz == 1e6

    with pytest.raises(TypeError, match='unexpected keyword'):
        DeviceState(no_such_field=1)


def test_resets_replace_the_group_with_fresh_defaults():
    dev = HarogicDevice.__new__(HarogicDevice)        # no DLL contact for a pure state test
    dev.state = DeviceState()
    dev._hw = threading.RLock()
    dev.state.rta_center_hz = 123e6
    dev.state.trigger_level_dbm = 5.0
    dev.state.sdr_demod = 'wfm'
    from web_sa.hardware.auto_reference import AutoReferenceController
    dev.auto_ref = AutoReferenceController(dev)       # reset_auto_reference delegates to it

    dev.reset_rta_state()
    assert dev.state.rta == RtaParams()
    assert dev.state.trigger == TriggerParams()
    assert dev.state.rta_center_hz == RtaParams().rta_center_hz

    dev.reset_sdr_state()
    assert dev.state.sdr == SdrParams()
    assert dev.state.sdr_demod == 'am'


def test_status_shape_is_independent_of_the_grouping():
    """build_status still reads the flat names; the grouping must not change the payload."""
    from web_sa.web.http_api import build_status

    class Stub:
        def __init__(self):
            self.state = DeviceState(connected=True, caps=DeviceCapabilities.from_model(67))
            self.preset_defaults = None
            self.session = None

        def auto_reference_view(self):
            return {'last_peak': None, 'last_noise_floor': None, 'candidate': None, 'pending': None}

        def session_health(self):
            return {}

    status = build_status(Stub())
    assert status['center'] == 1e9 and status['req']['rta_center'] == 1e9
    assert set(status['req']) >= {'swp', 'rta', 'sdr', 'center', 'span', 'rbw', 'ref'}
    assert status['req']['rta']['trigger_source'] == 'bus'
    assert status['req']['rta']['trigger_level'] == -40.0   # grouped fields still serialized
