"""
demod -- SDR-mode DSP building blocks (numpy + vendor DSP_DDC).

Public pieces:
  * DdcChannel  -- DSP_DDC channelizer (complex-float output; needs the vendor library)
  * AnalogDemod -- AM/FM/NFM/WFM/USB/LSB/CW demodulators (pure numpy)
  * Panadapter  -- wideband FFT for the spectrum + waterfall (pure numpy)

The names are imported lazily (PEP 562), mirroring ``measurements/__init__.py``: only
``DdcChannel`` pulls in ``hardware.sdk_bindings`` and therefore ``libhtraapi``. Keeping it
lazy means ``import web_sa.demod.demod`` - and ``tools/gen_dsp_fixtures.py``, which checks
the pure-numpy reference kernels against their committed fixtures - works on a machine
without the vendor library, so that contract stays gated in CI instead of erroring at
import time (the same finding G-3 the measurement sessions already solved).
"""
from __future__ import annotations

from typing import Any

_LAZY = {
    'ANALOG_MODES': '.demod',
    'AnalogDemod': '.demod',
    'DdcChannel': '.ddc',
    'Panadapter': '.spectrum',
}


def __getattr__(name: str) -> Any:
    module = _LAZY.get(name)
    if module is None:
        raise AttributeError(f'module {__name__!r} has no attribute {name!r}')
    import importlib

    value = getattr(importlib.import_module(module, __name__), name)
    globals()[name] = value            # cache: later accesses skip the import machinery
    return value


def __dir__() -> list[str]:
    return sorted([*globals(), *_LAZY])
