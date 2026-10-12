"""Device model / firmware display decoding (findings: version-display.md).

The rules are the vendor's own: the Model->name table is the DeviceClass_TypeDef enum
(htra_api.h:977-1010 of SDK 0.55.100), the u32 firmware format is the vendor example's
print, and the PMU/AGU format matches the official About screenshot
(/home/hui/Downloads/sa_ver.png: PMU 0.1.15(271), AGU 0.1.5(261)).
"""
from __future__ import annotations

from web_sa.hardware.fake_device import FakeDevice
from web_sa.hardware.version_info import fmt_fw3, fmt_pmu_agu, model_name


def test_model_name_matches_the_deviceclass_enum():
    # 67 = the bench unit (SAStudio shows "N90 R0" in its About panel)
    assert model_name(67) == 'N90 R0'
    assert model_name(12) == 'E90 R2'
    assert model_name(94) == 'N400 R2V2'
    # Header oddity kept verbatim: W200 R3 = 140 sorts before R2 = 141.
    assert model_name(140) == 'W200 R3'
    assert model_name(141) == 'W200 R2'
    assert model_name(68) == 'PN90 R0NDS'


def test_model_name_falls_back_for_unknown_models():
    assert model_name(999) == 'Model 999'
    assert model_name(None) == 'Model ?'


def test_fw3_decode_is_the_vendor_example_rule():
    # (hi << 16) | (mid << 8) | lo, printed hi.mid.lo (Device_GetDeviceInfo.py).
    assert fmt_fw3((0 << 16) | (55 << 8) | 109) == '0.55.109'
    assert fmt_fw3((0 << 16) | (55 << 8) | 104) == '0.55.104'
    assert fmt_fw3(0) == '0.0.0'
    assert fmt_fw3(-1) == '?'
    assert fmt_fw3(1 << 32) == '?'


def test_pmu_agu_decode_matches_the_about_screenshot():
    assert fmt_pmu_agu(271) == '0.1.15(271)'
    assert fmt_pmu_agu(261) == '0.1.5(261)'
    assert fmt_pmu_agu(0x10005) == '?'


def test_fake_device_reports_the_decoded_detail_shape():
    dev = FakeDevice()
    ok, _err = dev.open()
    assert ok
    dd = dev.state.device_detail
    assert dd['model_name'] == 'N90 R0'
    assert dd['mfw_str'] == '0.55.109'
    assert dd['ffw_str'] == '0.55.104'
    assert dd['pmu_str'] == '0.1.15(271)'
    assert dd['agu_str'] == '0.1.5(261)'
    # The status label is human readable now, not raw integers.
    assert dev.state.label.startswith('N90 R0')
