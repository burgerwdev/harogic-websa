"""Swept-profile capability exposure (S1/S1b/S2a/S4, findings/spectrum-display.md + rea-deep-2.md).

The frame detector (Detector_TypeDef), trace type (SWP_TraceType), RBW span-ratio modes
(RBW_OneThousandthSpan/OnePercentSpan), trace align and points strategy were all present
in the 0.55.100 header but unreachable; these tests pin the wire surface and the
profile mapping defaults.
"""
from __future__ import annotations

import pytest

from web_sa.config import DeviceCapabilities
from web_sa.hardware.device import DeviceState
from web_sa.web.ws import CommandError, _validate_command


class StubDevice:
    def __init__(self):
        self.state = DeviceState(
            connected=True,
            caps=DeviceCapabilities.from_model(67),
            pnm_supported=True,
        )


def test_rbw_accepts_the_span_ratio_modes():
    dev = StubDevice()
    for mode in ('one_thousandth_span', 'one_percent_span'):
        data = {'cmd': 'SET_RBW', 'mode': mode}
        _validate_command(dev, data['cmd'], data)


def test_rbw_still_rejects_unknown_modes():
    with pytest.raises(CommandError):
        _validate_command(StubDevice(), 'SET_RBW', {'cmd': 'SET_RBW', 'mode': 'warp'})


def test_tracetype_choices_are_the_header_enum_subset():
    dev = StubDevice()
    for mode in ('clear_write', 'max_hold', 'min_hold'):
        _validate_command(dev, 'SET_TRACETYPE', {'cmd': 'SET_TRACETYPE', 'mode': mode})
    with pytest.raises(CommandError):
        _validate_command(dev, 'SET_TRACETYPE', {'cmd': 'SET_TRACETYPE', 'mode': 'average'})


def test_framedet_accepts_device_default_and_header_values():
    dev = StubDevice()
    for mode in ('device_default', 'sample', 'pos_peak', 'average', 'neg_peak',
                 'max_power', 'raw_frames', 'rms', 'auto_peak'):
        _validate_command(dev, 'SET_FRAMEDET', {'cmd': 'SET_FRAMEDET', 'mode': mode})
    with pytest.raises(CommandError):
        _validate_command(dev, 'SET_FRAMEDET', {'cmd': 'SET_FRAMEDET', 'mode': 'max_hold'})


def test_tracegrid_accepts_align_and_strategy():
    dev = StubDevice()
    data = {'cmd': 'SET_TRACEGRID', 'trace_align': 'center', 'points_strategy': 'bin_size'}
    _validate_command(dev, data['cmd'], data)
    with pytest.raises(CommandError):
        _validate_command(dev, 'SET_TRACEGRID',
                          {'cmd': 'SET_TRACEGRID', 'trace_align': 'right'})


def test_swept_defaults_preserve_the_previous_device_behavior():
    s = DeviceState()
    # 'device_default' must be the default so an upgrade never silently changes the
    # frame detector a running setup already relies on.
    assert s.frame_detector == 'device_default'
    assert s.trace_type == 'clear_write'
    assert s.trace_align == 'start'
    assert s.points_strategy == 'speed'


def test_profile_maps_cover_every_wire_choice():
    from web_sa.hardware import sdk_bindings as sb

    frame_detectors = {
        'sample': sb.Detector_TypeDef.Detector_Sample,
        'pos_peak': sb.Detector_TypeDef.Detector_PosPeak,
        'average': sb.Detector_TypeDef.Detector_Average,
        'neg_peak': sb.Detector_TypeDef.Detector_NegPeak,
        'max_power': sb.Detector_TypeDef.Detector_MaxPower,
        'raw_frames': sb.Detector_TypeDef.Detector_RawFrames,
        'rms': sb.Detector_TypeDef.Detector_RMS,
        'auto_peak': sb.Detector_TypeDef.Detector_AutoPeak,
    }
    # The wire choices minus the sentinel must all map into the header enum.
    wire = {'device_default', 'sample', 'pos_peak', 'average', 'neg_peak',
            'max_power', 'raw_frames', 'rms', 'auto_peak'}
    assert wire - {'device_default'} == set(frame_detectors)
    # The enums are c_int subclasses (no .name); pin the header values instead.
    assert int(sb.SWP_TraceType_TypeDef.MaxHold) == 0x01
    assert int(sb.SWP_TraceType_TypeDef.MinHold) == 0x02
    assert int(sb.RBWMode_TypeDef.RBW_OneThousandthSpan) == 0x02
    assert int(sb.RBWMode_TypeDef.RBW_OnePercentSpan) == 0x03
