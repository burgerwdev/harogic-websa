"""SDK alias regression tests.

The 1.1.5 detector change shipped a missing `TraceDetector_TypeDef` re-export that only
failed when the worker started (`_profile()`), which the pure-logic tests never touched.
These tests exercise the alias table and the profile builder.
"""
import pytest

from web_sa.config import DeviceCapabilities
from web_sa.hardware import sdk_bindings as sb
from web_sa.hardware.device import HarogicDevice

# Every alias the business layer touches (keeping one entry per SDK type used).
REQUIRED_ALIASES = [
    'SWP_Profile_TypeDef', 'SWP_TraceInfo_TypeDef', 'SWP_FreqAssignment_TypeDef',
    'SweepTimeMode_TypeDef', 'TracePointsStrategy_TypeDef', 'TraceAlign_TypeDef',
    'SpurRejection_TypeDef', 'RBWMode_TypeDef', 'VBWMode_TypeDef', 'Window_TypeDef',
    'PreamplifierState_TypeDef', 'GainStrategy_TypeDef', 'ReferenceClockSource_TypeDef',
    'TraceDetector_TypeDef', 'TraceDetectMode_TypeDef',
    'GNSSInfo_TypeDef', 'MeasAuxInfo_TypeDef', 'DeviceInfo_TypeDef',
    'BootProfile_TypeDef', 'BootInfo_TypeDef',
]


def test_required_aliases_are_exported():
    missing = [name for name in REQUIRED_ALIASES if not hasattr(sb, name)]
    assert not missing, f'missing sdk_bindings aliases: {missing}'


@pytest.mark.parametrize(
    'detector,expected',
    [
        ('pos_peak', 'TraceDetector_PosPeak'),
        ('neg_peak', 'TraceDetector_NegPeak'),
        ('rms', 'TraceDetector_RMS'),
        ('sample', 'TraceDetector_Sample'),
        ('auto_peak', 'TraceDetector_AutoPeak'),
    ],
)
def test_profile_maps_every_detector(monkeypatch, detector, expected):
    monkeypatch.setattr(sb.dll, 'SWP_ProfileDeInit', lambda *a, **k: 0)
    dev = HarogicDevice()
    dev.state.caps = DeviceCapabilities.from_model(67)
    dev.state.detector = detector
    profile = dev._profile()
    assert int(profile.TraceDetector.value) == int(getattr(sb.TraceDetector_TypeDef, expected))
    assert int(profile.TraceDetectMode.value) == int(
        sb.TraceDetectMode_TypeDef.TraceDetectMode_Manual)


def test_profile_auto_detector_uses_automatic_mode(monkeypatch):
    monkeypatch.setattr(sb.dll, 'SWP_ProfileDeInit', lambda *a, **k: 0)
    dev = HarogicDevice()
    dev.state.caps = DeviceCapabilities.from_model(67)
    dev.state.detector = 'auto'
    profile = dev._profile()
    assert int(profile.TraceDetectMode.value) == int(
        sb.TraceDetectMode_TypeDef.TraceDetectMode_Auto)
