"""Fake analyzer: run the whole application with no vendor library and no hardware.

Purpose (report finding A1): the Playwright UI regression is the only test that asserts
*user-visible* results - canvas pixels, DOM text, real clicks - and it used to require the
bench. This device synthesises SWP traces, RTAF frames and SDR audio so the same UI checks can
run in CI.

It deliberately does not import `hardware.device`/`sdk_bindings` (the vendor library loads at
import time there), so nothing on this path needs libhtraapi.
"""
from __future__ import annotations

import threading

import numpy as np

from ..config import DEFAULT_SPAN_HZ, DeviceCapabilities
from .auto_reference import AutoReferenceController
from .state import DeviceState, RtaParams, SdrParams, TriggerParams

#: Model the fake reports; SAN-90 so the frontend enables every control.
FAKE_MODEL = 67
#: Synthetic trace: noise floor plus one carrier, in dBm.
NOISE_DBM = -95.0
PEAK_DBM = -25.0


class FakeDevice:
    """Drop-in replacement for `HarogicDevice` backed by synthetic data."""

    def __init__(self) -> None:
        self.state = DeviceState()
        self.session = None
        self.command_lock = None
        self.preset_defaults: dict | None = None
        self.last_freq = None
        self.last_freq_ver = 0
        self._hw = threading.RLock()
        self._rng = np.random.default_rng(20260914)   # deterministic frames
        self._sweep_count = 0
        self._window: tuple | None = None
        # The real control loop, not a stub: the fake feeds it the same observations the
        # device does, so the CI UI checks exercise the actual placement rules.
        self.auto_ref = AutoReferenceController(self)

    # ---------------- lifecycle ----------------

    def open(self) -> tuple[bool, str]:
        caps = DeviceCapabilities.from_model(FAKE_MODEL)
        self.state.connected = True
        self.state.caps = caps
        self.state.label = f'FAKE Model:{FAKE_MODEL}'
        self.state.device_detail = dict(uid='FAKE0000000000', model=FAKE_MODEL, hw=1,
                                        mfw=1, ffw=1, bus_speed=3, bus_ver=1, api_ver=1,
                                        warnings=0, errors=0)
        self.state.has_docxo = True
        self.state.pnm_supported = True
        self.state.last_error = ''
        self.load_preset_defaults()
        return True, 'ok (fake device)'

    def close(self) -> None:
        self.state.connected = False

    def reopen(self) -> tuple[bool, str]:
        """Parity with HarogicDevice: the link loop calls this to recover the device."""
        self.close()
        return self.open()

    def note_link_status(self, status: int, where: str) -> bool:
        """The fake never loses its link (no vendor transport to fail)."""
        return False

    def mark_link_lost(self, reason: str) -> None:
        self.state.connected = False
        self.state.last_error = reason

    def load_preset_defaults(self) -> None:
        self.preset_defaults = dict(
            center=1e9, span=DEFAULT_SPAN_HZ, fmin=1e9 - DEFAULT_SPAN_HZ / 2,
            fmax=1e9 + DEFAULT_SPAN_HZ / 2, ref=0.0, rbw=100e3, vbw=100e3, points=1000,
            atten=-1, window=1, rbw_mode='auto', vbw_mode='bypass', spur='bypass',
            detector='auto', preamp=0, ifgain=2, gain_strategy=0,
            sweep_time_mode=0, sweep_time=0.0,
        )

    def apply_caps_from_defaults(self) -> None:
        """Nothing to widen: the fake's capabilities already match its preset defaults."""

    # ---------------- configuration ----------------

    def configure_swp(self) -> tuple[bool, str]:
        window = (self.state.center_hz, self.state.span_hz, self.state.points_req,
                  self.state.rbw_hz)
        if window != self._window:
            self._window = window
            self.state.freq_version += 1
        self.state.config_version += 1
        return True, 'ok'

    def preset_state(self) -> dict:
        d = self.preset_defaults or {}
        s = self.state
        s.center_hz = float(d.get('center', 1e9))
        s.span_hz = float(d.get('span', DEFAULT_SPAN_HZ))
        s.ref_level = float(d.get('ref', 0.0))
        s.ref_mode = 'manual'
        s.rbw_mode = str(d.get('rbw_mode', 'auto'))
        s.rbw_hz = float(d.get('rbw', 100e3))
        s.vbw_mode = str(d.get('vbw_mode', 'bypass'))
        s.vbw_hz = float(d.get('vbw', 100e3))
        s.points_req = int(d.get('points', 1000))
        s.window = int(d.get('window', 1))
        s.spur_mode = str(d.get('spur', 'bypass'))
        s.detector = str(d.get('detector', 'auto'))
        s.atten = int(d.get('atten', -1))
        s.preamplifier = int(d.get('preamp', 0))
        s.ifgain = int(d.get('ifgain', 2))
        s.gain_strategy = int(d.get('gain_strategy', 0))
        s.sweep_time_mode = int(d.get('sweep_time_mode', 0))
        s.sweep_time = float(d.get('sweep_time', 0.0))
        self._window = None
        return dict(d)

    def reset_rta_state(self) -> None:
        self.state.rta = RtaParams()
        self.state.trigger = TriggerParams()

    def reset_sdr_state(self) -> None:
        self.state.sdr = SdrParams()

    def reset_common_state(self) -> None:
        d = DeviceState()
        s = self.state
        s.ref_clock = d.ref_clock
        s.refclk_out = d.refclk_out
        s.refclk_ppm = 0.0
        s.last_cal_freq = 0.0

    # ---------------- acquisition ----------------

    def fetch_sweep(self):
        """One synthetic SWP trace: noise floor plus a carrier at the centre."""
        s = self.state
        points = max(51, min(4000, int(s.points_req or 1000)))
        center, span = float(s.center_hz), float(s.span_hz)
        freq = np.linspace(center - span / 2, center + span / 2, points)
        floor = NOISE_DBM + self._rng.normal(0.0, 0.6, points)
        peak = PEAK_DBM + self._rng.normal(0.0, 0.2)
        width = max(1.0, points / 200.0)
        powers = floor + (peak - NOISE_DBM) * np.exp(
            -0.5 * ((np.arange(points) - points / 2.0) / width) ** 2)
        # Same observation the real device hands the reference loop: peak plus the 30th
        # percentile as the noise floor.
        finite = np.sort(powers[np.isfinite(powers)])
        if finite.size:
            self.observe_reference_peak(s.mode, float(finite[-1]),
                                        float(finite[int((finite.size - 1) * 0.3)]))
        s.sweep_ms = 5.0
        s.actual = {
            'center': center, 'span': span, 'start': center - span / 2,
            'stop': center + span / 2, 'ref': float(s.ref_level),
            'rbw': float(s.rbw_hz), 'vbw': float(s.vbw_hz), 'points': points,
            'est_min': 0.0, 'refclk': 100e6, 'refclk_src': 0,
        }
        self._sweep_count += 1
        return freq, powers.astype(np.float32)

    def step(self):
        """Publisher step: forward to the active session (std uses fetch_sweep)."""
        return self.session.step() if self.session else ([], [])

    def measure_sweep(self, dt: float) -> None:
        """No sweep-time measurement to do for synthetic frames."""

    def set_session(self, session) -> None:
        with self._hw:
            self.session = session
            self.state.mode = session.name if session else 'std'

    def session_health(self) -> dict:
        return self.session.health() if self.session is not None else {}

    # ---------------- queries the app uses ----------------

    def query_gnss(self) -> dict:
        return dict(lock=1, sats=12, docxo=1, docxo_mode=1, antenna=0, latitude=31.23,
                    longitude=121.47, altitude=4, year=2026, month=9, day=14, hour=10,
                    minute=30, second=0, time='2026-09-14 10:30:00')

    def calibrate_ref_clock(self, count: int = 10) -> tuple[bool, float]:
        return True, 100e6

    # ---------------- auto reference (the real control loop) ----------------

    def observe_reference_peak(self, mode: str, peak_dbm: float,
                               noise_floor_dbm: float | None = None) -> None:
        self.auto_ref.observe_peak(mode, peak_dbm, noise_floor_dbm)

    def prepare_auto_reference_retune(self, mode: str) -> bool:
        return self.auto_ref.prepare_retune(mode)

    def begin_auto_reference_settle(self, mode: str, delay: float = 0.75) -> None:
        self.auto_ref.begin_settle(mode, delay)

    def reset_auto_reference(self, mode: str) -> None:
        self.auto_ref.reset(mode)

    def nudge_reference_out_of_overflow(self) -> bool:
        return self.auto_ref.nudge_out_of_overflow()

    def apply_pending_auto_reference(self) -> bool:
        return self.auto_ref.apply_pending()

    def auto_scale(self, mode: str, current: float | None = None) -> tuple[str, float | None]:
        return self.auto_ref.fit(mode, current)

    def auto_reference_scope(self) -> str:
        return getattr(self.session, 'auto_ref_scope', 'std')

    def auto_reference_view(self) -> dict:
        return self.auto_ref.view(self.auto_reference_scope())


def create_device() -> FakeDevice:
    return FakeDevice()


def install_fake_sessions() -> None:
    """Point the session factory at the fake RTA/SDR sessions (std works as it is)."""
    from ..measurements import _SESSIONS

    _SESSIONS['rta'] = ('fake', 'FakeRtaSession')
    _SESSIONS['sdr'] = ('fake', 'FakeSdrSession')
