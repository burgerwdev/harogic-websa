"""
measurements/base.py -- measurement session base class + standard sweep session

Session objectification: std/harmonic/pnm share a uniform start/step/stop interface
plus enter/exit configuration snapshots.
Source: migrated from set_mode/_snap_std/_restore_std of web_sa/server.py (v0.11.1).
"""
from __future__ import annotations

from dataclasses import dataclass

from ..config import PUBLISH_MIN_INTERVAL


def _sweep_timeout(state) -> float:
    """Watchdog for one swept acquisition, scaled to the requested sweep time."""
    estimated = float(state.actual.get('est_min', 0.0) or 0.0)
    configured = state.sweep_time if state.sweep_time_mode == 7 else 0.0
    return max(10.0, min(180.0, max(estimated, configured) * 1.5 + 5.0))


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
    ref_mode: str
    atten: int
    preamp: int
    ifgain: int
    gain_strategy: int
    window: int
    spur: str


class MeasurementSession:
    """Measurement session base class. name is one of {std, harmonic, pnm}."""

    name = 'std'
    #: Which auto-reference tracker this session drives (device.auto_reference_view()).
    #: Declared by the session so the device never has to branch on the mode name.
    auto_ref_scope = 'std'
    #: Rate limit for the publisher (250 fps max); SDR overrides it because the IQS
    #: stream paces itself.
    publish_min_interval = PUBLISH_MIN_INTERVAL

    def __init__(self, dev):
        self.dev = dev
        self.snapshot: ConfigSnapshot | None = None

    def snapshot_current(self) -> None:
        """Replace the restore point with the current SWP configuration."""
        s = self.dev.state
        self.snapshot = ConfigSnapshot(
            center=s.center_hz, span=s.span_hz, points=s.points_req,
            rbw_mode=s.rbw_mode, rbw_hz=s.rbw_hz, vbw_mode=s.vbw_mode, vbw_hz=s.vbw_hz,
            ref=s.ref_level, ref_mode=s.ref_mode, atten=s.atten,
            preamp=s.preamplifier, ifgain=s.ifgain,
            gain_strategy=s.gain_strategy, window=s.window, spur=s.spur_mode)

    def enter(self) -> None:
        """Enter the session: snapshot the standard config."""
        self.snapshot_current()

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
        s.ref_level, s.ref_mode = snap.ref, snap.ref_mode
        s.atten, s.preamplifier = snap.atten, snap.preamp
        s.ifgain, s.gain_strategy = snap.ifgain, snap.gain_strategy
        s.window, s.spur_mode = snap.window, snap.spur
        self.snapshot = None
        prepare = getattr(self.dev, 'prepare_auto_reference_retune', None)
        if prepare is not None:
            prepare('std')
        self.dev.configure_swp()

    def step(self):
        """Run a single step (called by the publisher), returns (frames, json_msgs)."""
        return [], []

    # ---------------- Acquisition policy ----------------
    # The publisher asks the session instead of branching on the mode name; a new mode
    # therefore cannot require editing the scheduler (report finding E-3).

    #: True when repeated frequency axes should be sent only once (the swept path
    #: re-sends the same axis every sweep; RTA/SDR frames carry their own grid).
    dedupe_freq = False

    def acquisition_timeout(self) -> float:
        """Watchdog for one step. A hung DLL call must not be waited on for ever."""
        return _sweep_timeout(self.dev.state)

    def pacing(self, dt: float, produced: bool) -> float:
        """Seconds to sleep after a step that took ``dt`` and produced frames (or not)."""
        return max(0.002, self.publish_min_interval - dt)

    def reconfigure(self) -> None:
        """Re-apply the current device configuration for this session."""
        self.dev.configure_swp()

    def health(self) -> dict:
        """Session diagnostics surfaced in STATUS.

        Sessions that recover from transient failures override this instead of the
        serializer reaching into their private counters (report finding P1-7).
        """
        return {}

    # ---------------- Lifecycle protocol (report finding P1-9) ----------------
    # The command layer used to poke `session._ready` directly before and after a mode
    # switch. These two methods are that contract: stop the acquisition loop, then report
    # whether the session actually became usable.

    def request_stop(self) -> None:
        """Ask the session to stop its acquisition loop before it is exited."""
        return None

    def is_ready(self) -> bool:
        """True when the session finished configuring and can produce data."""
        return True


class StdSession(MeasurementSession):
    """Standard sweep session: continuously push FREQ/POWR frames via fetch_sweep."""

    name = 'std'
    #: The swept path re-sends an identical frequency axis on every sweep; send it once.
    dedupe_freq = True

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
