"""
config.py -- configuration dataclasses + environment variables + device capability
inference (full SAN series support)

Source: constants migrated from web_sa/server.py (v0.11.1) + SAN series model
capabilities (product manual V1.3)
"""
from __future__ import annotations

import ipaddress
import os
from dataclasses import dataclass

# ---- Global constants (for SWP configuration) ----
DEFAULT_CENTER_HZ = 1e9
DEFAULT_SPAN_HZ = 100e6
DEFAULT_POINTS = 1000
DEFAULT_REF_DBM = 0.0
DEFAULT_RBW_HZ = 100e3
DEFAULT_VBW_HZ = 100e3
PUBLISH_MIN_INTERVAL = 0.004   # ~250 fps max
GNSS_POLL_INTERVAL = 1.0   # GNSS polling + periodic STATUS push interval (1s)

# Spur rejection mapping (SWP_Profile.SpurRejection)
SPUR_BY_MODE = {
    'bypass': 'SpurRejection_TypeDef.Bypass',
    'standard': 'SpurRejection_TypeDef.Standard',
    'enhanced': 'SpurRejection_TypeDef.Enhanced',
}

# RBW/VBW mode mapping
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
    """Capabilities for the full SAN series -- derived from the device Model, never hard-code a single model."""
    model: int
    freq_min_hz: float
    freq_max_hz: float
    name: str
    pnm_supported: bool = True

    @classmethod
    def from_model(cls, model: int, pnm_supported: bool = True) -> DeviceCapabilities:
        # Product manual V1.3: SAN-45 9kHz-4.5GHz / SAN-60 9kHz-6GHz / SAN-90 9kHz-9GHz
        table = {
            45: ('SAN-45', 9e3, 4.5e9),
            60: ('SAN-60', 9e3, 6e9),
            67: ('SAN-90', 9e3, 9e9),   # this unit: Model 67 -> SAN-90
            90: ('SAN-90', 9e3, 9e9),
        }
        name, lo, hi = table.get(model, ('SAN-' + str(model), 9e3, 9e9))
        return cls(model=model, freq_min_hz=lo, freq_max_hz=hi,
                   name=name, pnm_supported=pnm_supported)


@dataclass
class AppConfig:
    """Application configuration: overridable via environment variables."""
    host: str = os.getenv('WEBSA_HOST', '127.0.0.1')
    port: int = int(os.getenv('WEBSA_PORT', '8080'))
    log_level: str = os.getenv('WEBSA_LOG', 'INFO')
    log_file: str = os.getenv('WEBSA_LOGFILE', '')
    static_dir: str = os.getenv('WEBSA_STATIC', '')
    auth_token: str = os.getenv('WEBSA_TOKEN', '')
    allowed_origins: str = os.getenv('WEBSA_ALLOWED_ORIGINS', '')
    allow_unauthenticated_remote: bool = os.getenv(
        'WEBSA_ALLOW_UNAUTHENTICATED_REMOTE', '').lower() in ('1', 'true', 'yes')
    # Measurement defaults
    harm_f0: float = 1e9
    harm_count: int = 5
    harm_span: float = 10e6
    pnm_center: float = 1e9
    pnm_threshold: float = -50.0      # official default
    pnm_rbwratio: float = 0.02
    pnm_start: float = 100.0
    pnm_stop: float = 10e6
    pnm_traceavg: int = 4

    def validate(self) -> None:
        """Reject accidentally exposing hardware control without authentication."""
        try:
            is_loopback = ipaddress.ip_address(self.host).is_loopback
        except ValueError:
            is_loopback = self.host.lower() == 'localhost'
        if not is_loopback and not self.auth_token and not self.allow_unauthenticated_remote:
            raise ValueError(
                'Remote WEBSA_HOST requires WEBSA_TOKEN; set '
                'WEBSA_ALLOW_UNAUTHENTICATED_REMOTE=1 only on a trusted network')

    @property
    def origin_allowlist(self) -> set[str]:
        return {x.strip().rstrip('/') for x in self.allowed_origins.split(',') if x.strip()}


def fit_span(center: float, span: float, cap: DeviceCapabilities) -> float:
    """Shrink span while keeping center (consistent with the frontend fitSpan)."""
    return max(100.0, min(float(span),
                          2 * min(center - cap.freq_min_hz, cap.freq_max_hz - center)))
