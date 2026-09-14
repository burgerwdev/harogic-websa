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
DEFAULT_RTA_CENTER_HZ = 1e9
DEFAULT_RTA_SPAN_HZ = 50.78125e6
DEFAULT_RTA_REF_DBM = 0.0
DEFAULT_RTA_RBW_MODE = 'auto'
DEFAULT_RTA_VBW_MODE = 'equal'
DEFAULT_RTA_SWEEP_MODE = 2

# RTA acquisition trigger. Defaults reproduce the previous behaviour exactly
# (bus-triggered free-running frames); level triggering is opt-in.
DEFAULT_TRIGGER_SOURCE = 'bus'            # bus | freerun | level | external | timer
DEFAULT_TRIGGER_EDGE = 'rising'           # rising | falling | double
DEFAULT_TRIGGER_LEVEL_DBM = -40.0
DEFAULT_TRIGGER_SAFE_TIME_S = 0.0         # debounce
DEFAULT_TRIGGER_DELAY_S = 0.0
DEFAULT_TRIGGER_PRE_TIME_S = 0.0          # pre-trigger capture
DEFAULT_TRIGGER_ACQ_TIME_S = 0.005        # post-trigger capture (FixedPoints)
DEFAULT_TRIGGER_RETRIGGER_COUNT = 0       # 0 = disabled
DEFAULT_TRIGGER_RETRIGGER_PERIOD_S = 0.0
DEFAULT_TRIGGER_OUT = 'none'              # none | per_hop | per_sweep | per_profile
DEFAULT_TRIGGER_OUT_POLARITY = 'positive'
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

# ---- Protocol / UI bounds (not model dependent, so they live here once instead of as
# literals inside the command validation chain) ----
REF_RANGE_DB_MIN, REF_RANGE_DB_MAX = 10.0, 200.0     # visible window height, dB
# The level the CLIENT displays (SDR owns its display scale, so it may sit outside the device's
# Ref range; the placement rules clamp the target to the device bounds themselves).
DISPLAY_REF_MIN_DBM, DISPLAY_REF_MAX_DBM = -160.0, 40.0
PNM_CARRIER_MIN_HZ, PNM_CARRIER_MAX_HZ = 1.0, 9e6    # offset sweep for phase noise
PNM_OFFSET_MAX_HZ = 10e6
HARM_COUNT_MAX = 10
HARM_SPAN_MAX_HZ = 100e6
SDR_IFBW_MIN_HZ, SDR_IFBW_MAX_HZ = 100.0, 500000.0
SDR_VOLUME_MIN, SDR_VOLUME_MAX = 0.0, 2.0
SDR_PITCH_MIN_HZ, SDR_PITCH_MAX_HZ = 200.0, 2000.0
SDR_DEEMPH_MIN_US, SDR_DEEMPH_MAX_US = -1.0, 1000.0
SQUELCH_MIN_DBFS, SQUELCH_MAX_DBFS = -150.0, 0.0
TRIGGER_LEVEL_MIN_DBM, TRIGGER_LEVEL_MAX_DBM = -150.0, 30.0
TRIGGER_TIME_MAX_S = 10.0
TRIGGER_ACQ_MIN_S, TRIGGER_ACQ_MAX_S = 0.0005, 60.0
TRIGGER_RETRIGGER_MAX = 65535
TRIGGER_RETRIGGER_PERIOD_MAX_S = 3600.0
REFCLK_CAL_COUNT_MIN, REFCLK_CAL_COUNT_MAX = 3, 120


@dataclass
class DeviceCapabilities:
    """Capabilities for the full SAN series -- derived from the device Model, never hard-code a single model."""
    model: int
    freq_min_hz: float
    freq_max_hz: float
    name: str
    pnm_supported: bool = True
    #: Hardware limits used by command validation. One row per model in `from_model`:
    #: a new SAN model must not require editing the validation chain (report finding E-2).
    rbw_max_hz: float = 10e6
    vbw_max_hz: float = 10e6
    points_max: int = 4000
    atten_max: int = 33
    ifgain_max: int = 3
    decimate_max: int = 2048
    rta_span_max_hz: float = DEFAULT_RTA_SPAN_HZ
    ref_min_dbm: float = -50.0
    ref_max_dbm: float = 30.0
    trigger_level_min_dbm: float = TRIGGER_LEVEL_MIN_DBM
    trigger_level_max_dbm: float = TRIGGER_LEVEL_MAX_DBM

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


def fit_center_span(
    center: float, span: float, cap: DeviceCapabilities, minimum_span: float = 100.0
) -> tuple[float, float]:
    """Clamp center and shrink span to preserve a symmetric window around center."""
    fitted_center = max(
        cap.freq_min_hz + minimum_span / 2,
        min(cap.freq_max_hz - minimum_span / 2, float(center)),
    )
    symmetric_limit = 2 * min(
        fitted_center - cap.freq_min_hz,
        cap.freq_max_hz - fitted_center,
    )
    fitted_span = max(minimum_span, min(float(span), symmetric_limit))
    return fitted_center, fitted_span


def fit_start_stop(
    start: float, stop: float, cap: DeviceCapabilities, minimum_span: float = 100.0
) -> tuple[float, float]:
    """Clamp start/stop and return their canonical center/span representation."""
    fitted_start = max(cap.freq_min_hz, min(cap.freq_max_hz, float(start)))
    fitted_stop = max(cap.freq_min_hz, min(cap.freq_max_hz, float(stop)))
    if fitted_stop <= fitted_start:
        fitted_stop = min(cap.freq_max_hz, fitted_start + minimum_span)
        if fitted_stop <= fitted_start:
            fitted_start = max(cap.freq_min_hz, fitted_stop - minimum_span)
    return (fitted_start + fitted_stop) / 2, fitted_stop - fitted_start


def fit_span(center: float, span: float, cap: DeviceCapabilities) -> float:
    """Shrink a span around a fixed centre (used by the harmonic session)."""
    symmetric_limit = 2 * min(
        float(center) - cap.freq_min_hz,
        cap.freq_max_hz - float(center),
    )
    return max(100.0, min(float(span), symmetric_limit))
