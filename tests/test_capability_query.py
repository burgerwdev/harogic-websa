"""Capability query bindings (G2, findings/ddc.md + rea-raw decompile evidence).

Device_GetDecimateFactorList / Device_GetBusBandwidth arrived in SDK 0.55.100 and are
absent from htra_api.py; without argtypes the ctypes pointer truncates (the same trap
the ref-clock binding documents). These checks cover the independent SDK binding
used by bench tools.
"""
from __future__ import annotations

from tools.reference import sdk_bindings as sb


def test_capability_bindings_declare_complete_argtypes():
    # Partial argtypes truncate pointers on 64-bit (see the CalibrateRefClock note).
    assert len(sb.dll.Device_GetDecimateFactorList.argtypes) == 4
    assert len(sb.dll.Device_GetBusBandwidth.argtypes) == 2
    assert sb.DECIMATE_FACTOR_LIST_MAX >= 64
