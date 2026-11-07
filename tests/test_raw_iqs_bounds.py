"""Raw Python probe validates fake SDK metadata before reading foreign memory."""
import ctypes as C
import sys
from types import SimpleNamespace

import pytest

from tools import reference
from tools.bench import sdr_raw_iqs_check as probe


class Format(C.c_int):
    Complex8bit = 1
    Complex16bit = 0


class Profile(C.Structure):
    _fields_ = [('DataFormat', Format)]


class Info(C.Structure):
    _fields_ = [('PacketSamples', C.c_uint32), ('PacketDataSize', C.c_uint32)]


class Stream(C.Structure):
    _fields_ = [('AlternIQStream', C.c_void_p), ('IQS_Profile', Profile), ('IQS_StreamInfo', Info)]


@pytest.mark.parametrize(('count', 'size', 'format_value', 'pointer'), [
    (4, 16, 0, 0), (4, 15, 0, 1), (4, 17, 0, 1), (4, 16, 1, 1),
    (0, 0, 0, 1), (5, 20, 0, 1), (2**32 - 1, 16, 0, 1),
])
def test_raw_probe_rejects_bad_packet_before_copy_and_stops(monkeypatch, tmp_path,
        count, size, format_value, pointer):
    stopped, copied = [], []
    def fetch(_dev, stream):
        value = stream._obj
        value.AlternIQStream = pointer
        value.IQS_Profile.DataFormat = format_value
        value.IQS_StreamInfo.PacketSamples = count
        value.IQS_StreamInfo.PacketDataSize = size
        return 0
    vendor = SimpleNamespace(DataFormat_TypeDef=Format,
        TriggerMode_TypeDef=SimpleNamespace(Adaptive=0),
        dll=SimpleNamespace(IQS_GetIQStream_PM1=fetch, IQS_BusTriggerStart=lambda _: 0,
                            IQS_BusTriggerStop=lambda _: stopped.append(True)))
    bindings = SimpleNamespace(IQStream_TypeDef=Stream)
    monkeypatch.setitem(sys.modules, 'htra_api', vendor)
    monkeypatch.setitem(sys.modules, 'tools.reference.sdk_bindings', bindings)
    monkeypatch.setattr(reference, 'sdk_bindings', bindings, raising=False)
    monkeypatch.setitem(sys.modules, 'tools.sdr_probe.probe_stream', SimpleNamespace(
        config=lambda *a, **kw: (0, None, SimpleNamespace(PacketSamples=4, IQSampleRate=62500000))))
    monkeypatch.setattr(probe, 'string_at', lambda *a: copied.append(a) or b'unsafe')
    raw = tmp_path / 'raw.cs16'
    with pytest.raises(RuntimeError, match='invalid packet'):
        probe.measure(C.c_void_p(1), 1, .001, raw)
    assert not copied
    assert stopped == [True]
    assert raw.read_bytes() == b''
    count, size, format_value, pointer = 4, 16, 0, 1
    monkeypatch.setattr(probe, 'string_at', lambda *_: bytes(range(16)))
    recovered = probe.measure(C.c_void_p(1), 1, .001, raw)
    assert recovered['saved_packets'] > 0
    assert raw.read_bytes() == bytes(range(16)) * recovered['saved_packets']
    assert stopped == [True, True]


@pytest.mark.parametrize(('count', 'rate'), [
    (0, 62500000), (1, 62500000), (2**32 - 1, 62500000),
    (4, 0), (4, float('nan')), (4, float('inf')), (4, -1),
])
def test_raw_probe_rejects_invalid_configuration_before_start(monkeypatch, count, rate):
    started = []
    vendor = SimpleNamespace(DataFormat_TypeDef=Format,
        TriggerMode_TypeDef=SimpleNamespace(Adaptive=0),
        dll=SimpleNamespace(IQS_GetIQStream_PM1=lambda *_: -1,
            IQS_BusTriggerStart=lambda _: started.append(True) or 0,
            IQS_BusTriggerStop=lambda _: None))
    bindings = SimpleNamespace(IQStream_TypeDef=Stream)
    monkeypatch.setitem(sys.modules, 'htra_api', vendor)
    monkeypatch.setitem(sys.modules, 'tools.reference.sdk_bindings', bindings)
    monkeypatch.setattr(reference, 'sdk_bindings', bindings, raising=False)
    monkeypatch.setitem(sys.modules, 'tools.sdr_probe.probe_stream', SimpleNamespace(
        config=lambda *a, **kw: (0, None, SimpleNamespace(PacketSamples=count, IQSampleRate=rate))))
    with pytest.raises(RuntimeError, match='invalid configuration'):
        probe.measure(C.c_void_p(1), 1, .001)
    assert not started
