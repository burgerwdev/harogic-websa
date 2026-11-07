"""Geometry rejection at the Python reference ABI; no SDK or large allocations."""
import ctypes as C
import importlib.util
import sys
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import pytest


class Geometry(C.Structure):
    _fields_ = [('DDCOffsetFrequency', C.c_double), ('SampleRate', C.c_double),
                ('DecimateFactor', C.c_double), ('SamplePoints', C.c_uint32)]


@pytest.fixture
def adapters(monkeypatch):
    calls = []
    output = dict(rate=48000., points=4, delay=0, offset=0., factor=8.)
    def configure(_dsp, _input, result):
        calls.append('configure')
        value = result.contents
        value.SampleRate, value.SamplePoints = output['rate'], output['points']
        value.DDCOffsetFrequency, value.DecimateFactor = output['offset'], output['factor']
        return 0
    def delay(_dsp, result):
        result.contents.value = output['delay']
        return 0
    bindings = SimpleNamespace(DSP_DDC_TypeDef=Geometry, pointer=C.pointer, c_uint32=C.c_uint32,
        dll=SimpleNamespace(DSP_DDC_DeInit=lambda _: None, DSP_DDC_Configuration=configure,
            DSP_DDC_GetDelay=delay, DSP_DDC_Reset=lambda _: 0))
    from tools import reference
    monkeypatch.setattr(reference, 'sdk_bindings', bindings, raising=False)
    base = Path(__file__).resolve().parents[1] / 'tools/reference'
    modules = []
    for name in ('ddc', 'native_ddc'):
        spec = importlib.util.spec_from_file_location(f'tools.reference.{name}', base / f'{name}.py')
        module = importlib.util.module_from_spec(spec)
        monkeypatch.setitem(sys.modules, spec.name, module)
        spec.loader.exec_module(module)
        modules.append(module)
    return *modules, output, calls


@pytest.mark.parametrize(('key', 'value'), [
    ('rate', float('nan')), ('rate', float('inf')), ('points', 2**32 - 1),
    ('delay', 2**32 - 1), ('offset', float('inf')), ('factor', float('nan')),
])
def test_sdk_ddc_bad_reconfigure_cannot_leave_old_ready_state(adapters, key, value):
    module, _, output, _calls = adapters
    channel = module.DdcChannel(C.c_void_p(1))
    channel.configure(384000., 0., 8, 32)
    assert channel._ready
    output[key] = value
    with pytest.raises(RuntimeError, match='invalid output'):
        channel.configure(384000., 0., 8, 32)
    assert not channel._ready
    output.update(rate=48000., points=4, delay=0, offset=0., factor=8.)
    channel.configure(384000., 0., 8, 32)
    assert channel._ready and channel.fs_out == 48000.


@pytest.mark.parametrize(('rate', 'delay'), [
    (float('nan'), 0), (float('inf'), 0), (48000., float('nan')), (48000., float('inf')),
])
def test_native_ddc_bad_geometry_closes_handle_before_allocation(adapters, monkeypatch, rate, delay):
    module, native, _output, _calls = adapters
    closed, allocations = [], []
    channel = native.NativeDdcChannel.__new__(native.NativeDdcChannel)
    module.DdcChannel.__init__(channel, None)
    channel.handle = None
    channel.lib = SimpleNamespace(channel_open=lambda *_: 1,
        channel_output_rate=lambda _: rate, channel_delay=lambda _: delay,
        channel_close=lambda handle: closed.append(handle))
    monkeypatch.setattr(np, 'empty', lambda *a: allocations.append(a))
    with pytest.raises(RuntimeError, match='invalid geometry'):
        channel.configure(62500000., 0., 1280, 16240)
    assert closed == [1] and channel.handle is None and not channel._ready
    assert not allocations
