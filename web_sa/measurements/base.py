"""
measurements/base.py -- measurement session base class + standard sweep session

Session objectification: std/harmonic/pnm share a uniform start/step/stop interface
plus enter/exit configuration snapshots.
Source: migrated from set_mode/_snap_std/_restore_std of web_sa/server.py (v0.11.1).
"""
from __future__ import annotations

from dataclasses import dataclass


@dataclass
class ConfigSnapshot:
    """Snapshot of the device config taken before entering a measurement session (restored on exit)."""
    center: float
    span: float
    points: int
    rbw_mode: str
    rbw_hz: float
    vbw_mode: str
    vbw_hz: float
    ref: float
    atten: int
    preamp: int
    ifgain: int
    gain_strategy: int
    window: int
    spur: str


class MeasurementSession:
    """Measurement session base class. name is one of {std, harmonic, pnm}."""

    name = 'std'

    def __init__(self, dev):
        self.dev = dev
        self.snapshot: ConfigSnapshot | None = None

    def enter(self) -> None:
        """Enter the session: snapshot the standard config."""
        s = self.dev.state
        self.snapshot = ConfigSnapshot(
            center=s.center_hz, span=s.span_hz, points=s.points_req,
            rbw_mode=s.rbw_mode, rbw_hz=s.rbw_hz, vbw_mode=s.vbw_mode, vbw_hz=s.vbw_hz,
            ref=s.ref_level, atten=s.atten, preamp=s.preamplifier, ifgain=s.ifgain,
            gain_strategy=s.gain_strategy, window=s.window, spur=s.spur_mode)

    def exit(self) -> None:
        """Exit the session: restore the snapshot and reconfigure the device."""
        snap = self.snapshot
        if snap is None:
            return
        s = self.dev.state
        s.center_hz, s.span_hz = snap.center, snap.span
        s.points_req = snap.points
        s.rbw_mode, s.rbw_hz = snap.rbw_mode, snap.rbw_hz
        s.vbw_mode, s.vbw_hz = snap.vbw_mode, snap.vbw_hz
        s.ref_level = snap.ref
        s.atten, s.preamplifier = snap.atten, snap.preamp
        s.ifgain, s.gain_strategy = snap.ifgain, snap.gain_strategy
        s.window, s.spur_mode = snap.window, snap.spur
        self.snapshot = None
        self.dev.configure_swp()

    def step(self):
        """Run a single step (called by the publisher), returns (frames, json_msgs)."""
        return [], []


class StdSession(MeasurementSession):
    """Standard sweep session: continuously push FREQ/POWR frames via fetch_sweep."""

    name = 'std'

    def step(self):
        r = self.dev.fetch_sweep()
        if r is None:
            return [], []
        f, p = r
        frames = []
        fv = self.dev.state.freq_version
        self.dev.last_freq = f          # for pushing to new clients on connect
        self.dev.last_freq_ver = fv
        from .framer import encode_freq, encode_powr
        frames.append(encode_freq(fv, f, self.dev.state.sweep_ms))
        frames.append(encode_powr(fv, p, self.dev.state.sweep_ms))
        return frames, []
