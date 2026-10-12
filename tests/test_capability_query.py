"""Capability query bindings (G2, findings/ddc.md + rea-raw decompile evidence).

Device_GetDecimateFactorList / Device_GetBusBandwidth arrived in SDK 0.55.100 and are
absent from htra_api.py; without argtypes the ctypes pointer truncates (the same trap
the ref-clock binding documents). The fake's capability rows mirror the shape so the
fake-backend e2e exercises the STATUS surface.
"""
from __future__ import annotations

from web_sa.hardware import fake_device, sdk_bindings as sb
from web_sa.hardware.fake_device import FakeDevice


def test_capability_bindings_declare_complete_argtypes():
    # Partial argtypes truncate pointers on 64-bit (see the CalibrateRefClock note).
    assert len(sb.dll.Device_GetDecimateFactorList.argtypes) == 4
    assert len(sb.dll.Device_GetBusBandwidth.argtypes) == 2
    assert sb.DECIMATE_FACTOR_LIST_MAX >= 64


def test_fake_reports_capability_rows_and_caps_track_the_device_list():
    dev = FakeDevice()
    ok, _err = dev.open()
    assert ok
    dd = dev.state.device_detail
    factors = dd['decimate_factors']
    assert factors[0] == 1 and 2048 in factors
    assert all(f & (f - 1) == 0 for f in factors)   # IQS decimate is power-of-two only
    assert dd['bus_bandwidth'] > 0
    # The fake's own list max is the largest supported power of two.
    assert dev.state.caps.decimate_max == max(factors)


def test_fake_module_keeps_capability_keys_in_sync_with_device_shape():
    # The real open() fills these keys only when the library answers; the fake always
    # answers, so the e2e STATUS surface stays stable (same contract as pmu/agu).
    real_keys = {'pmu', 'pmu_str', 'agu', 'agu_str', 'decimate_factors', 'bus_bandwidth'}
    dev = FakeDevice()
    dev.open()
    assert real_keys <= set(dev.state.device_detail)
    assert hasattr(fake_device, 'FAKE_MODEL')
