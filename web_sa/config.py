"""
config.py —— 配置 dataclass + 环境变量 + 设备能力推导 (SAN 全系列支持)

来源: web_sa/server.py (v0.11.1) 常量迁移 + SAN 系列型号能力 (产品手册 V1.3)
"""
from __future__ import annotations

import os
from dataclasses import dataclass

# ---- 全局常量 (SWP 配置用) ----
DEFAULT_CENTER_HZ = 1e9
DEFAULT_SPAN_HZ = 100e6
DEFAULT_POINTS = 1000
DEFAULT_REF_DBM = 0.0
DEFAULT_RBW_HZ = 100e3
DEFAULT_VBW_HZ = 100e3
PUBLISH_MIN_INTERVAL = 0.004   # 最大 ~250fps
GNSS_POLL_INTERVAL = 2.0

# 杂散抑制映射 (SWP_Profile.SpurRejection)
SPUR_BY_MODE = {
    'bypass': 'SpurRejection_TypeDef.Bypass',
    'standard': 'SpurRejection_TypeDef.Standard',
    'enhanced': 'SpurRejection_TypeDef.Enhanced',
}

# RBW/VBW 模式映射
RBW_MODE = {'manual': 'RBWMode_TypeDef.Manual', 'auto': 'RBWMode_TypeDef.RBW_Auto'}
VBW_MODE = {
    'manual': 'VBWMode_TypeDef.Manual',
    'equal': 'VBWMode_TypeDef.VBW_EqualToRBW',
    'tenth': 'VBWMode_TypeDef.VBW_TenPercentRBW',
    'bypass': 'VBWMode_TypeDef.VBW_TenTimesRBW',
}
WINDOW_MAP = {0: 'FlatTop', 1: 'BlackmanNuttall', 2: 'Blackman', 3: 'Hamming', 4: 'Hanning'}


@dataclass
class DeviceCapabilities:
    """SAN 全系列能力 —— 由设备 Model 推导, 禁止硬编码单型号。"""
    model: int
    freq_min_hz: float
    freq_max_hz: float
    name: str
    pnm_supported: bool = True

    @classmethod
    def from_model(cls, model: int, pnm_supported: bool = True) -> DeviceCapabilities:
        # 产品手册 V1.3: SAN-45 9kHz-4.5GHz / SAN-60 9kHz-6GHz / SAN-90 9kHz-9GHz
        table = {
            45: ('SAN-45', 9e3, 4.5e9),
            60: ('SAN-60', 9e3, 6e9),
            67: ('SAN-90', 9e3, 9e9),   # 本机 Model 67 → SAN-90
            90: ('SAN-90', 9e3, 9e9),
        }
        name, lo, hi = table.get(model, ('SAN-' + str(model), 9e3, 9e9))
        return cls(model=model, freq_min_hz=lo, freq_max_hz=hi,
                   name=name, pnm_supported=pnm_supported)


@dataclass
class AppConfig:
    """应用配置: 环境变量可覆盖。"""
    host: str = os.getenv('WEBSA_HOST', '0.0.0.0')
    port: int = int(os.getenv('WEBSA_PORT', '8080'))
    log_level: str = os.getenv('WEBSA_LOG', 'INFO')
    log_file: str = os.getenv('WEBSA_LOGFILE', '')
    static_dir: str = os.getenv('WEBSA_STATIC', '')
    # 测量默认值
    harm_f0: float = 1e9
    harm_count: int = 5
    harm_span: float = 10e6
    pnm_center: float = 1e9
    pnm_threshold: float = -50.0      # 官方默认
    pnm_rbwratio: float = 0.02
    pnm_start: float = 100.0
    pnm_stop: float = 10e6
    pnm_traceavg: int = 4


def fit_span(center: float, span: float, cap: DeviceCapabilities) -> float:
    """保持 center 收缩 span (与前端 fitSpan 一致)。"""
    return max(100.0, min(float(span),
                          2 * min(center - cap.freq_min_hz, cap.freq_max_hz - center)))
