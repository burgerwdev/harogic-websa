"""config 配置常量与映射单测(无设备依赖)"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from web_sa import config


def test_default_constants():
    assert config.DEFAULT_CENTER_HZ == 1e9
    assert config.DEFAULT_SPAN_HZ == 100e6
    assert config.DEFAULT_POINTS == 1000
    assert config.DEFAULT_REF_DBM == 0.0


def test_spur_mapping():
    assert config.SPUR_BY_MODE['bypass'].endswith('Bypass')
    assert config.SPUR_BY_MODE['standard'].endswith('Standard')
    assert config.SPUR_BY_MODE['enhanced'].endswith('Enhanced')


def test_rbw_vbw_mapping():
    assert config.RBW_MODE['auto'].endswith('RBW_Auto')
    assert config.RBW_MODE['manual'].endswith('Manual')
    assert config.VBW_MODE['bypass'].endswith('TenTimesRBW')
    assert config.VBW_MODE['equal'].endswith('EqualToRBW')
    assert config.VBW_MODE['tenth'].endswith('TenPercentRBW')


def test_device_caps_derivation():
    """SAN 型号能力推导: 频率范围/名称"""
    c90 = config.DeviceCapabilities.from_model(67)
    assert c90.name == 'SAN-90'
    assert c90.freq_min_hz == 9e3
    assert c90.freq_max_hz == 9e9
    c45 = config.DeviceCapabilities.from_model(45)
    assert c45.name == 'SAN-45' and c45.freq_max_hz == 4.5e9
    # 未知型号 fallback 不崩溃
    c_unk = config.DeviceCapabilities.from_model(999)
    assert c_unk.freq_max_hz == 9e9


def test_fit_span():
    c = config.DeviceCapabilities.from_model(67)
    # span 收缩到设备范围内(保持 center)
    s = config.fit_span(1e9, 20e9, c)
    assert s <= 2 * min(1e9 - c.freq_min_hz, c.freq_max_hz - 1e9)
    # 最小 span 下限
    assert config.fit_span(1e9, 10, c) >= 100.0
