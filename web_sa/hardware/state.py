"""Device state.

Split by owner (report finding P1-8, step 2): each measurement mode keeps its own parameters
in a small dataclass, instead of one flat bag of ~80 attributes growing with every feature:

  SwpParams     swept-mode parameters + the values the device reports back for them
  RtaParams     RTA window / RBW / VBW / sweep / trigger-independent state
  SdrParams     IQS capture, channelizer and demod settings
  TriggerParams RTA acquisition trigger

The shared front end (attenuation, preamp, IF gain, reference clock) and the session/meta
fields stay on ``DeviceState`` itself, because they genuinely apply to every mode.

For the transition, every grouped field is also reachable under its old flat name
(``state.center_hz`` is ``state.swp.center_hz``). That keeps the ~520 existing call sites and
the whole test suite working while new code uses the groups; the alias table below is the
single place that describes the mapping, and the flat view can be deleted once the call sites
have moved (DEVELOPMENT.md §2.1). The WS STATUS shape does not depend on the grouping at all.
"""
from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Any

from ..config import (
    DEFAULT_RTA_CENTER_HZ,
    DEFAULT_RTA_RBW_MODE,
    DEFAULT_RTA_REF_DBM,
    DEFAULT_RTA_SPAN_HZ,
    DEFAULT_RTA_SWEEP_MODE,
    DEFAULT_RTA_VBW_MODE,
    DEFAULT_TRIGGER_ACQ_TIME_S,
    DEFAULT_TRIGGER_DELAY_S,
    DEFAULT_TRIGGER_EDGE,
    DEFAULT_TRIGGER_LEVEL_DBM,
    DEFAULT_TRIGGER_OUT,
    DEFAULT_TRIGGER_OUT_POLARITY,
    DEFAULT_TRIGGER_PRE_TIME_S,
    DEFAULT_TRIGGER_RETRIGGER_COUNT,
    DEFAULT_TRIGGER_RETRIGGER_PERIOD_S,
    DEFAULT_TRIGGER_SAFE_TIME_S,
    DEFAULT_TRIGGER_SOURCE,
    DeviceCapabilities,
)


@dataclass
class SwpParams:
    """Swept-mode window and detection settings, plus what the device reported back."""

    center_hz: float = 1e9
    span_hz: float = 100e6
    #: Ref of the SWEPT profile (SWP_Configuration). Each mode keeps its own: the RTA profile has
    #: `rta_ref_level` and the IQS stream has `sdr_ref_level`, because the vendor has one profile
    #: per mode and sharing a Python field between them made a level set in one mode appear in
    #: another (reported: "the Ref settings are not independent per mode").
    ref_level: float = 0.0
    ref_mode: str = 'manual'
    rbw_mode: str = 'manual'
    rbw_hz: float = 100e3
    vbw_mode: str = 'manual'
    vbw_hz: float = 100e3
    points_req: int = 1000
    window: int = 1
    spur_mode: str = 'bypass'
    detector: str = 'auto'
    #: 频点内帧间检波 (Detector_TypeDef, htra_api.h:511-525). 'device_default' leaves the
    #: profile field at its ProfileDeInit value (today's behavior); the other values map 1:1
    #: to the header enum. MaxPower/RawFrames are SWP-only per the header.
    frame_detector: str = 'device_default'
    #: 迹线对齐 (TraceAlign_TypeDef) 与迹线点数映射策略 (TracePointsStrategy_TypeDef).
    trace_align: str = 'start'
    points_strategy: str = 'speed'
    sweep_time_mode: int = 0     # 0=minSWT 1=x2 2=x4 3=x10 4=x20 5=x50 6=xN 7=Manual 8=minSMPxN
    sweep_time: float = 0.0      # Manual=absolute seconds; xN=multiplier; ignored otherwise
    #: Device-estimated sweep time (SWP_TraceInfo.EstimateMinSweepTime): carried in the
    #: FREQ frame header and shown as the SWT display. The publisher's measured loop
    #: interval is a SEPARATE field (sweep_ms_meas) — overwriting this one made the
    #: display flip between two unrelated quantities.
    sweep_ms: float = 0.0
    #: Publisher-measured acquisition interval (EMA of the step period, ms).
    sweep_ms_meas: float = 0.0
    # Height of the visible display window in dB (grid divisions x dB/div), pushed by the
    # frontend. Auto Ref anchors the noise floor just above the bottom of this window.
    ref_range_db: float = 100.0
    actual: dict = field(default_factory=dict)


@dataclass
class RtaParams:
    """Real-time spectrum window (mode-private, restored when the session exits)."""

    rta_center_hz: float = DEFAULT_RTA_CENTER_HZ
    rta_span_hz: float = DEFAULT_RTA_SPAN_HZ
    rta_ref_level: float = DEFAULT_RTA_REF_DBM
    rta_ref_mode: str = 'manual'
    rta_rbw_mode: str = DEFAULT_RTA_RBW_MODE
    rta_rbw_hz: float = 0.0
    rta_vbw_mode: str = DEFAULT_RTA_VBW_MODE
    rta_vbw_hz: float = 0.0
    rta_sweep_time_mode: int = DEFAULT_RTA_SWEEP_MODE
    rta_sweep_time: float = 0.0
    rta_actual: dict = field(default_factory=dict)


@dataclass
class SdrParams:
    """SDR receive chain: IQS capture, channelizer and demodulator (independent of SWP/RTA)."""

    sdr_center_hz: float = 1e9
    sdr_decimate: int = 16
    #: Ref of the IQS profile. Its own field, like the RTA profile's `rta_ref_level`: them all
    #: sharing `SwpParams.ref_level` is what made a level set in one mode turn up in another.
    sdr_ref_level: float = 0.0
    #: True once an SDR level exists (set by the user, or by the one automatic placement of a first
    #: entry). Until then entering SDR places the trace once; afterwards the level is the user's and
    #: nothing may move it (reported: "the Ref I set in SDR was changed when I came back from RTA").
    sdr_ref_set: bool = False
    sdr_actual: dict = field(default_factory=dict)
    sdr_listen_hz: float = 1e9
    sdr_demod: str = 'am'
    sdr_if_bw: float = 6000.0
    sdr_squelch: float = -110.0
    sdr_squelch_open: bool = False
    #: Squelch reference: 'power' gates on channel power (dBm); 'snr' gates on
    #: channel minus the off-channel noise floor (dB).
    sdr_squelch_mode: str = 'power'
    #: Off-channel noise floor (dBm, panadapter 25th percentile outside the signal
    #: window) — the SNR squelch's second input.
    sdr_noise_dbm: float | None = None
    sdr_volume: float = 0.8
    sdr_agc: bool = True
    sdr_pitch: float = 700.0
    # FM de-emphasis in microseconds: -1 = auto (50 us for WFM, none elsewhere), 0 = off,
    # 50/75/300 = explicit (regional pre-emphasis complement).
    sdr_deemph_us: float = -1.0
    sdr_level_dbfs: float = -120.0
    #: Channel power at the listen frequency (dBm, panadapter, pre-AGC): the squelch
    #: gate's reference level and the S-meter quantity that responds to signal strength.
    sdr_channel_dbm: float | None = None
    #: Capture-wide peak (IQStream_TypeDef.MaxPower_dBm, computed by the device per packet),
    #: peak-hold with decay — a free PEP-style meter over the whole capture, not just the
    #: demod channel.
    sdr_peak_dbm: float = -200.0
    #: Aux DDC channels (G1'): {int channel: {'enabled': bool, 'freq': Hz}}. Channels 2/3, each
    #: with its own DSP handle; blocks stream as IQAX to ?iqaux=1 sockets only.
    sdr_aux: dict = field(default_factory=dict)
    sdr_adm: dict = field(default_factory=dict)


@dataclass
class TriggerParams:
    """RTA acquisition trigger (the swept engine has no level trigger)."""

    trigger_source: str = DEFAULT_TRIGGER_SOURCE
    trigger_edge: str = DEFAULT_TRIGGER_EDGE
    trigger_level_dbm: float = DEFAULT_TRIGGER_LEVEL_DBM
    trigger_safe_time_s: float = DEFAULT_TRIGGER_SAFE_TIME_S
    trigger_delay_s: float = DEFAULT_TRIGGER_DELAY_S
    trigger_pre_time_s: float = DEFAULT_TRIGGER_PRE_TIME_S
    trigger_acq_time_s: float = DEFAULT_TRIGGER_ACQ_TIME_S
    trigger_retrigger_count: int = DEFAULT_TRIGGER_RETRIGGER_COUNT
    trigger_retrigger_period_s: float = DEFAULT_TRIGGER_RETRIGGER_PERIOD_S
    trigger_out: str = DEFAULT_TRIGGER_OUT
    trigger_out_polarity: str = DEFAULT_TRIGGER_OUT_POLARITY
    trigger_actual: dict = field(default_factory=dict)


#: group attribute -> (group name, field name). Also the flat alias table: every entry is
#: readable and writable as ``state.<field>``.
GROUPS: dict[str, tuple[str, ...]] = {
    'swp': tuple(SwpParams.__dataclass_fields__),
    'rta': tuple(RtaParams.__dataclass_fields__),
    'sdr': tuple(SdrParams.__dataclass_fields__),
    'trigger': tuple(TriggerParams.__dataclass_fields__),
}


def _install_flat_views(cls) -> None:
    """Expose every grouped field under its old flat name (transitional, see the module doc)."""
    for group, names in GROUPS.items():
        for name in names:
            def getter(self, _group=group, _name=name):
                return getattr(getattr(self, _group), _name)

            def setter(self, value, _group=group, _name=name):
                setattr(getattr(self, _group), _name, value)

            setattr(cls, name, property(getter, setter, doc=f'Alias for {group}.{name}'))


class DeviceState:
    """Device state, grouped by owner, serialized to WS STATUS.

    Construct with ``DeviceState()`` or with keyword overrides; both the grouped names
    (``swp=SwpParams(...)``) and the flat ones (``center_hz=1e9``) are accepted so existing
    call sites and tests keep working.
    """

    def __init__(self, **overrides: Any) -> None:
        self.connected: bool = False
        self.label: str = ''
        self.detail: str = ''
        self.device_detail: dict = {}
        self.caps: DeviceCapabilities | None = None          # model capabilities
        # ---- shared front end (applies to every mode) ----
        self.atten: int = -1
        self.preamplifier: int = 0
        self.ifgain: int = 2
        # IF AGC (device-specific; see _profile). Off by default because the official
        # Profile.xml ships EnableIFAGC=0. WEBSA_IFAGC=1 flips the default for A/B testing.
        self.ifagc: int = 1 if os.getenv('WEBSA_IFAGC', '0').lower() not in (
            '0', '', 'false', 'no', 'off') else 0
        self.ifagc_target: float = float(os.getenv('WEBSA_IFAGC_TARGET', '-9'))
        self.ifagc_gain: float = 0.0
        self.gain_strategy: int = 0
        self.amp_atten: int = -1
        self.preamplifier_actual: int | None = None
        self.ref_clock: str = 'internal'
        self.refclk_out: bool = False   # reference clock output enable
        self.refclk_ppm: float = 0.0
        self.last_cal_freq: float = 0.0
        self.calibrating: bool = False
        self.has_docxo: bool = False
        # ---- session / meta ----
        self.mode: str = 'std'
        self.pnm_supported: bool = False
        self.gnss: dict = {}
        self.freq_version: int = 0
        self.config_version: int = 0
        self.status_warning: int = 0
        self.last_error: str = ''
        self.harm_results: list = []
        self.pnm_last: dict | None = None
        # ---- per-mode groups ----
        self.swp = SwpParams()
        self.rta = RtaParams()
        self.sdr = SdrParams()
        self.trigger = TriggerParams()
        for key, value in overrides.items():
            if not hasattr(self, key):
                raise TypeError(f'DeviceState() got an unexpected keyword argument {key!r}')
            setattr(self, key, value)


_install_flat_views(DeviceState)
