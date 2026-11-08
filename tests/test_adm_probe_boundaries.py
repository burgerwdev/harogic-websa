"""Manual ADM probe's ctypes capture boundary, independent of the Rust SDK shim."""
import ctypes as C
import importlib
import sys
from types import SimpleNamespace

import pytest

from tools import reference


class DataFormat(C.c_int):
    Complex16bit = 0
    Complexfloat = 6


class Profile(C.Structure):
    _fields_ = [('CenterFreq_Hz', C.c_double), ('RefLevel_dBm', C.c_double),
                ('DecimateFactor', C.c_uint32), ('DataFormat', DataFormat),
                ('TriggerSource', C.c_uint32), ('TriggerMode', C.c_uint32),
                ('TriggerLength', C.c_uint64), ('BusTimeout_ms', C.c_uint32),
                ('DCCancelerMode', C.c_uint32), ('QDCMode', C.c_uint32)]


class Info(C.Structure):
    _fields_ = [('StreamSamples', C.c_uint64), ('PacketSamples', C.c_uint32),
                ('PacketCount', C.c_uint32), ('PacketDataSize', C.c_uint32),
                ('IQSampleRate', C.c_double)]


class Stream(C.Structure):
    _fields_ = [('IQS_StreamInfo', Info), ('IQS_Profile', Profile),
                ('AlternIQStream', C.c_void_p), ('IQS_ScaleToV', C.c_double)]


@pytest.fixture
def probe(monkeypatch):
    sdk = SimpleNamespace(dll=SimpleNamespace(), IQS_Profile_TypeDef=Profile,
                          IQS_StreamInfo_TypeDef=Info, IQStream_TypeDef=Stream,
                          DataFormat_TypeDef=DataFormat,
                          IQS_TriggerSource_TypeDef=SimpleNamespace(Bus=0),
                          TriggerMode_TypeDef=SimpleNamespace(FixedPoints=0),
                          DCCancelerMode_TypeDef=SimpleNamespace(DCCOff=0),
                          QDCMode_TypeDef=SimpleNamespace(QDCOff=0))
    monkeypatch.setitem(sys.modules, 'htra_api', sdk)
    monkeypatch.setitem(sys.modules, 'tinysa', SimpleNamespace(TinySA=None))
    monkeypatch.setitem(sys.modules, 'tools.reference.sdk_bindings', sdk)
    monkeypatch.setattr(reference, 'sdk_bindings', sdk, raising=False)
    module = importlib.import_module('tools.sdr_probe.probe_adm')
    monkeypatch.setattr(module, 'T', sdk)
    monkeypatch.setattr(module, 'sb', sdk)
    return module, sdk


def setup_capture(probe, monkeypatch, *, packet_fault=None, geometry_fault=None, fail_fetch=False):
    module, sdk = probe
    events = []
    raw = (C.c_int16 * 8)(1, -1, 2, -2, 3, -3, 4, -4)

    def configure(_dev, _requested, out, info):
        out._obj.DataFormat = 0
        info._obj.StreamSamples = 8
        info._obj.PacketSamples = 4
        info._obj.PacketCount = 2
        info._obj.PacketDataSize = 16
        info._obj.IQSampleRate = 48000
        if geometry_fault == 'rate':
            info._obj.IQSampleRate = float('nan')
        elif geometry_fault == 'count':
            info._obj.PacketCount = 3
        elif geometry_fault == 'format':
            out._obj.DataFormat = 3
        return 0

    def fetch(_dev, stream):
        events.append('fetch')
        if fail_fetch:
            return -9
        stream._obj.IQS_StreamInfo.PacketSamples = 4
        stream._obj.IQS_StreamInfo.PacketDataSize = 16
        stream._obj.IQS_Profile.DataFormat = 0
        stream._obj.IQS_ScaleToV = 0.5
        stream._obj.AlternIQStream = C.addressof(raw)
        if packet_fault == 'pointer':
            stream._obj.AlternIQStream = 0
        elif packet_fault == 'bytes':
            stream._obj.IQS_StreamInfo.PacketDataSize = 15
        elif packet_fault == 'format':
            stream._obj.IQS_Profile.DataFormat = 3
        elif packet_fault == 'samples':
            stream._obj.IQS_StreamInfo.PacketSamples = 5
        return 0

    sdk.dll.IQS_ProfileDeInit = lambda *_args: None
    sdk.dll.IQS_Configuration = configure
    sdk.dll.IQS_BusTriggerStart = lambda *_args: events.append('start') or 0
    sdk.dll.IQS_BusTriggerStop = lambda *_args: events.append('stop') or 0
    sdk.dll.IQS_GetIQStream_PM1 = fetch
    monkeypatch.setattr(module, 'memmove', lambda *_args: events.append('copy'))
    return module, events


@pytest.mark.parametrize('fault', ['pointer', 'bytes', 'format', 'samples'])
def test_adm_probe_rejects_bad_packets_before_memmove(probe, monkeypatch, fault):
    module, events = setup_capture(probe, monkeypatch, packet_fault=fault)
    with pytest.raises(RuntimeError, match='packet'):
        module.capture(C.c_void_p(1), 100e6, 32, 8)
    assert 'copy' not in events
    assert events[-1] == 'stop'


@pytest.mark.parametrize('fault', ['rate', 'count', 'format'])
def test_adm_probe_rejects_inconsistent_configuration_before_start(probe, monkeypatch, fault):
    module, events = setup_capture(probe, monkeypatch, geometry_fault=fault)
    with pytest.raises(RuntimeError, match='configuration'):
        module.capture(C.c_void_p(1), 100e6, 32, 8)
    assert events == []


def test_adm_probe_fetch_failure_always_stops_capture(probe, monkeypatch):
    module, events = setup_capture(probe, monkeypatch, fail_fetch=True)
    with pytest.raises(RuntimeError, match='GetIQStream'):
        module.capture(C.c_void_p(1), 100e6, 32, 8)
    assert events[-1] == 'stop'


def test_adm_probe_valid_capture_copies_and_can_start_again(probe, monkeypatch):
    module, events = setup_capture(probe, monkeypatch)
    def copy(*args):
        events.append('copy')
        C.memmove(*args)
    monkeypatch.setattr(module, 'memmove', copy)
    for _ in range(2):
        buf, info, scale = module.capture(C.c_void_p(1), 100e6, 32, 8)
        assert len(buf) == 16 and info.StreamSamples == 8 and scale == 0.5
        assert list(buf) == [1, -1, 2, -2, 3, -3, 4, -4] * 2
    assert events == ['start', 'fetch', 'copy', 'fetch', 'copy', 'stop'] * 2


def test_adm_probe_recovers_after_rejected_packet(probe, monkeypatch):
    module, failed = setup_capture(probe, monkeypatch, packet_fault='bytes')
    with pytest.raises(RuntimeError, match='packet'):
        module.capture(C.c_void_p(1), 100e6, 32, 8)
    assert failed[-1] == 'stop' and 'copy' not in failed
    module, recovered = setup_capture(probe, monkeypatch)
    module.capture(C.c_void_p(1), 100e6, 32, 8)
    assert recovered == ['start', 'fetch', 'copy', 'fetch', 'copy', 'stop']


@pytest.mark.parametrize('failure', ['Device', 'DSP', 'ADM'])
def test_adm_probe_open_failure_releases_partial_owners(probe, failure):
    module, sdk = probe
    events = []

    class BootProfile(C.Structure):
        _fields_ = [('DevicePowerSupply', C.c_int), ('PhysicalInterface', C.c_int)]

    sdk.BootProfile_TypeDef = BootProfile
    sdk.BootInfo_TypeDef = BootProfile
    sdk.DevicePowerSupply_TypeDef = SimpleNamespace(USBPortAndPowerPort=0)
    sdk.PhysicalInterface_TypeDef = SimpleNamespace(USB=0)

    def open_handle(name, value):
        def open_(handle, *_args):
            handle._obj.value = None if name == failure == 'ADM' else value
            events.append(('open', name))
            return -1 if name == failure else 0
        return open_

    for index, name in enumerate(['Device', 'DSP', 'ADM'], 1):
        setattr(sdk.dll, name + '_Open', open_handle(name, index))
        setattr(sdk.dll, name + '_Close', lambda handle: events.append(('close', handle._obj.value)))
    with pytest.raises(RuntimeError, match=failure + '_Open'):
        module.open_device()
    assert [event[1] for event in events if event[0] == 'close'] == ([1] if failure == 'Device' else [2, 1])


@pytest.mark.parametrize('fault', ['status', 'pointer', 'zero_count', 'excess_count'])
def test_adm_waveform_rejects_invalid_result_before_read(probe, monkeypatch, fault):
    module, _sdk = probe
    raw = (C.c_float * 8)(*range(8))
    params = module.AMDemodParam()
    params.DemodWaveform = C.cast(raw, C.POINTER(C.c_float))
    params.DemodWaveformSize = 8
    status = -9 if fault == 'status' else 0
    if fault == 'pointer':
        params.DemodWaveform = C.POINTER(C.c_float)()
    elif fault == 'zero_count':
        params.DemodWaveformSize = 0
    elif fault == 'excess_count':
        params.DemodWaveformSize = 9

    def no_foreign_read(*_args, **_kwargs):
        pytest.fail('invalid ADM metadata reached the foreign read')
    monkeypatch.setattr(module.np.ctypeslib, 'as_array', no_foreign_read)
    with pytest.raises(RuntimeError, match='ADM'):
        module.demod_waveform(params, 8, status)


def test_adm_waveform_uses_reported_count_and_owns_its_copy(probe):
    module, _sdk = probe
    raw = (C.c_float * 8)(*range(8))
    params = module.AMDemodParam()
    params.DemodWaveform = C.cast(raw, C.POINTER(C.c_float))
    params.DemodWaveformSize = 3
    result = module.demod_waveform(params, 8, 0)
    assert result.tolist() == [0, 1, 2]
    raw[1] = 99
    assert result.tolist() == [0, 1, 2]


def setup_ddc(probe, monkeypatch, fault=None):
    module, sdk = probe
    events = []
    raw = (C.c_float * 8)(1, -1, 2, -2, 3, -3, 4, -4)

    class DdcProfile(C.Structure):
        _fields_ = [('DDCOffsetFrequency', C.c_double), ('SampleRate', C.c_double),
                    ('DecimateFactor', C.c_double), ('SamplePoints', C.c_uint64)]

    class DdcStream(C.Structure):
        _fields_ = [('IQS_StreamInfo', Info), ('IQS_Profile', Profile),
                    ('AlternIQStream', C.POINTER(C.c_void_p)), ('IQS_ScaleToV', C.c_double)]

    sdk.DSP_DDC_TypeDef = DdcProfile
    sdk.IQStream_TypeDef = DdcStream

    def configure(_dsp, _din, dout):
        events.append('configure')
        dout._obj.SampleRate = float('nan') if fault == 'rate' else 960
        dout._obj.SamplePoints = 201 if fault == 'geometry_count' else 4
        return -9 if fault == 'configure' else 0

    def execute(_dsp, _ins, outs):
        events.append('execute')
        outs._obj.AlternIQStream = C.cast(raw, C.POINTER(C.c_void_p))
        outs._obj.IQS_Profile.DataFormat = 0 if fault == 'format' else 6
        outs._obj.IQS_StreamInfo.PacketSamples = 3 if fault == 'packet_count' else 4
        outs._obj.IQS_StreamInfo.PacketDataSize = 31 if fault == 'bytes' else 32
        outs._obj.IQS_StreamInfo.IQSampleRate = 960
        if fault == 'pointer':
            outs._obj.AlternIQStream = C.POINTER(C.c_void_p)()
        return -9 if fault == 'execute' else 0

    sdk.dll.DSP_DDC_DeInit = lambda *_args: -9 if fault == 'deinit' else 0
    sdk.dll.DSP_DDC_Configuration = configure
    sdk.dll.DSP_DDC_Reset = lambda *_args: -9 if fault == 'reset' else 0
    sdk.dll.DSP_DDC_Execute = execute
    return module, events, raw


@pytest.mark.parametrize('fault', ['deinit', 'configure', 'reset', 'execute', 'rate',
                                   'geometry_count', 'packet_count', 'bytes', 'format', 'pointer'])
def test_adm_ddc_rejects_failed_status_or_geometry_before_read(probe, monkeypatch, fault):
    module, _events, _raw = setup_ddc(probe, monkeypatch, fault)

    def no_foreign_read(*_args, **_kwargs):
        pytest.fail('invalid DDC metadata reached the foreign read')
    monkeypatch.setattr(module.np.ctypeslib, 'as_array', no_foreign_read)
    with pytest.raises(RuntimeError, match='DDC'):
        module.ddc_float_block(C.c_void_p(1), (C.c_int16 * 400)(), 200, 48000)


def test_adm_ddc_valid_result_owns_iq_and_recovers_after_rejection(probe, monkeypatch):
    module, _events, raw = setup_ddc(probe, monkeypatch)
    samples, rate = module.ddc_float_block(C.c_void_p(1), (C.c_int16 * 400)(), 200, 48000)
    assert rate == 960 and samples.tolist() == [1, -1, 2, -2, 3, -3, 4, -4]
    raw[0] = 99
    assert samples[0] == 1
    module, _events, _raw = setup_ddc(probe, monkeypatch, 'rate')
    with pytest.raises(RuntimeError, match='DDC'):
        module.ddc_float_block(C.c_void_p(1), (C.c_int16 * 400)(), 200, 48000)
    module, _events, _raw = setup_ddc(probe, monkeypatch)
    samples, _rate = module.ddc_float_block(C.c_void_p(1), (C.c_int16 * 400)(), 200, 48000)
    assert samples.tolist() == [1, -1, 2, -2, 3, -3, 4, -4]
