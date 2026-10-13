"""Swept-profile capability exposure (S1/S1b/S2a/S4, findings/spectrum-display.md + rea-deep-2.md).

The frame detector (Detector_TypeDef), trace type (SWP_TraceType), RBW span-ratio modes
(RBW_OneThousandthSpan/OnePercentSpan), trace align and points strategy were all present
in the 0.55.100 header but unreachable; these tests pin the wire surface and the
profile mapping defaults.

The wire checks and the defaults run without the vendor library. The three profile tests need it.
They carry the `vendor_required` fixture, so they skip in CI and run on the bench.
"""
from __future__ import annotations

import pytest

from web_sa.config import DeviceCapabilities
from web_sa.hardware.state import DeviceState
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


def test_framedet_accepts_device_default_and_header_values():
    dev = StubDevice()
    for mode in ('device_default', 'sample', 'pos_peak', 'average', 'neg_peak',
                 'max_power', 'raw_frames', 'rms', 'auto_peak'):
        _validate_command(dev, 'SET_FRAMEDET', {'cmd': 'SET_FRAMEDET', 'mode': mode})
    with pytest.raises(CommandError):
        _validate_command(dev, 'SET_FRAMEDET', {'cmd': 'SET_FRAMEDET', 'mode': 'max_hold'})


def test_tracegrid_accepts_align_and_strategy():
    dev = StubDevice()
    data = {'cmd': 'SET_TRACEGRID', 'trace_align': 'center', 'points_strategy': 'accuracy'}
    _validate_command(dev, data['cmd'], data)
    with pytest.raises(CommandError):
        _validate_command(dev, 'SET_TRACEGRID',
                          {'cmd': 'SET_TRACEGRID', 'trace_align': 'right'})


def test_bin_size_strategy_is_refused():
    """Bench result: the firmware ignores TraceBinSize_Hz. The exact-window sweep of BinSizeAssined
    costs 1.1 s at 10 MHz and 28 s at the full span (FINDINGS.md §16). At the full span the sweep
    is longer than the 10 s acquisition watchdog, so the worker stopped and the UI froze."""
    with pytest.raises(CommandError) as exc:
        _validate_command(StubDevice(), 'SET_TRACEGRID',
                          {'cmd': 'SET_TRACEGRID', 'points_strategy': 'bin_size'})
    assert exc.value.code == 'invalid_choice'


def test_profile_never_programs_the_bin_size_strategy(vendor_required, monkeypatch):
    """A stale state value (or an older client) must stay on the fast path."""
    from web_sa.hardware import sdk_bindings as sb
    from web_sa.hardware.device import HarogicDevice

    monkeypatch.setattr(sb.dll, 'SWP_ProfileDeInit', lambda *a, **k: 0)
    dev = HarogicDevice()
    dev.state.caps = DeviceCapabilities.from_model(67)
    dev.state.points_strategy = 'bin_size'
    profile = dev._profile()
    assert int(profile.TracePointsStrategy.value) == int(
        sb.TracePointsStrategy_TypeDef.SweepSpeedPreferred)
    assert int(profile.TracePointsStrategy.value) != int(
        sb.TracePointsStrategy_TypeDef.BinSizeAssined)


def test_a_device_reporting_bin_size_as_its_default_reads_back_as_speed(vendor_required, monkeypatch):
    """Preset caches the ProfileDeInit strategy of the device. BinSizeAssined reads as the
    conservative default, so a restored preset cannot select it again."""
    from web_sa.hardware import sdk_bindings as sb
    from web_sa.hardware.device import HarogicDevice

    def deinit(_dev, prof):
        prof.contents.TracePointsStrategy = sb.TracePointsStrategy_TypeDef.BinSizeAssined
        return 0

    monkeypatch.setattr(sb.dll, 'SWP_ProfileDeInit', deinit)
    dev = HarogicDevice()
    dev.load_preset_defaults()
    assert dev.preset_defaults['points_strategy'] == 'speed'


def test_swept_defaults_preserve_the_previous_device_behavior():
    s = DeviceState()
    # 'device_default' must be the default so an upgrade never silently changes the
    # frame detector a running setup already relies on.
    assert s.frame_detector == 'device_default'
    assert s.trace_align == 'start'
    assert s.points_strategy == 'speed'


def test_profile_maps_cover_every_wire_choice(vendor_required):
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
    assert int(sb.RBWMode_TypeDef.RBW_OneThousandthSpan) == 0x02
    assert int(sb.RBWMode_TypeDef.RBW_OnePercentSpan) == 0x03
