"""Reference-level placement: one-shot fit plus an always-armed safety ranger.

`HarogicDevice` used to own this: ~200 lines of decision logic spread over seven methods,
sharing the device's mutable state and reaching into `configure_swp()` / the active session
(report finding P1-8). It is a control loop, not device I/O, so it lives here and the device
only forwards to it.

Two behaviours, deliberately separated (industry practice: a bench analyser's Ref "Auto" is a
one-shot action - Keysight Auto Scale, R&S Auto Level, Anritsu Auto Scale - while what runs
continuously is the overload protection):

1. `fit()` - the user's Auto Scale. Compute one target from the latest trace and apply it once.
   Changing the reference costs a full device reconfiguration (~0.3-1 s, measured 1.9 s
   end-to-end while the old continuous loop also waited for a settled frame), so this cannot be
   a live-tracking mode. It reuses the placement rules below, and does nothing at all when the
   trace is already placed well - clicking Auto Scale must not cost a pointless glitch.

2. `observe_peak()` -> safety ranger - armed always, never user-controlled:
   * IF overflow (-12) raises Ref one 5 dB step per second (that path has no frames at all, so
     the peak-based rule cannot act; it deadlocked before),
   * a trace that is entirely outside the display window (peak above the top edge, or noise
     floor below the bottom edge) is fitted once, rate-limited.
   Both used to require `ref_mode == 'auto'` AND auto attenuation, so selecting a manual
   attenuator silently disabled overload protection altogether (measured: with `atten != -1`
   the device kept reporting -12 and nothing raised the level).

Placement rules (unchanged, they took real debugging):

* anchor the NOISE FLOOR just above the bottom of the display window and lift Ref only as far
  as the peak needs (anchoring on the peak left weak signals 7 divisions high),
* accept the current placement while the floor is 4-12 dB above the bottom and the peak keeps
  >= 8 dB of headroom (a wobbling estimate must not trigger a reconfiguration),
* keep ~30 dB of headroom when the noise floor is high,
* never descend below a floor learned from an actual IF overflow, and drop that floor when the
  measurement geometry or the front-end changes.
"""
from __future__ import annotations

import math
import time

#: Default display window height (10 divisions x 10 dB/div) when the frontend has not
#: reported one; the window is what the fit anchors the noise floor to.
DEFAULT_WINDOW_DB = 100.0
#: Lowest Ref the loop will ever propose.
FLOOR_MIN_DBM = -50.0
#: Highest Ref the device accepts here.
CEILING_DBM = 30.0
#: Ref step used for the IF-overflow escape, and its rate limit.
OVERFLOW_STEP_DB = 5.0
OVERFLOW_INTERVAL_S = 1.0
#: A trace is "outside the window" once it misses an edge by this much.
OUT_OF_WINDOW_MARGIN_DB = 3.0
#: At most one safety fit per this many seconds (a fit that did not help must not hunt).
SAFETY_INTERVAL_S = 2.0
#: Ref changes smaller than this are not worth a reconfiguration.
MIN_CHANGE_DB = 5.0
#: Peak-to-noise ratio below which there is nothing worth anchoring to (inside the window).
MIN_SIGNAL_DB = 15.0


def new_tracker() -> dict:
    return {
        'last_peak': None, 'last_noise_floor': None,
        'last_target': None,          # level this loop last applied
        'last_change': 0.0,           # monotonic time of that application
        'ignore_until': 0.0,          # observations are stale until then (settle window)
        'floor': FLOOR_MIN_DBM,       # learned lower bound (IF saturation)
        'result': 'idle',             # last fit outcome, for the UI ('ok', 'no_signal', ...)
    }


class AutoReferenceController:
    """Per-mode reference placement for the swept ('std') and RTA paths."""

    MODES = ('std', 'rta')

    def __init__(self, dev) -> None:
        self.dev = dev                      # provides state, _hw lock, configure_swp, session
        self._trackers = {mode: new_tracker() for mode in self.MODES}
        self._pending: tuple[str, float] | None = None
        self._geometry_seen: dict[str, tuple | None] = {mode: None for mode in self.MODES}

    # ---------------- state access ----------------

    def tracker(self, mode: str) -> dict:
        return self._trackers[mode]

    @property
    def pending(self) -> tuple[str, float] | None:
        return self._pending

    @pending.setter
    def pending(self, value: tuple[str, float] | None) -> None:
        self._pending = value

    def ref_level(self, mode: str) -> float:
        state = self.dev.state
        return state.rta_ref_level if mode == 'rta' else state.ref_level

    def set_ref_level(self, mode: str, value: float) -> None:
        state = self.dev.state
        if mode == 'rta':
            state.rta_ref_level = value
        else:
            state.ref_level = value

    def window_db(self) -> float:
        return max(20.0, float(getattr(self.dev.state, 'ref_range_db', DEFAULT_WINDOW_DB)))

    def clear_learned_floor(self) -> None:
        """Forget the IF-overflow floor (front-end or geometry changed)."""
        with self.dev._hw:
            for tracker in self._trackers.values():
                tracker['floor'] = FLOOR_MIN_DBM

    # ---------------- observations ----------------

    def observe_peak(self, mode: str, peak_dbm: float, noise_floor_dbm: float | None = None) -> None:
        """Feed one trace observation: record it, then let the safety ranger act."""
        with self.dev._hw:
            self._observe_locked(mode, peak_dbm, noise_floor_dbm)

    def _observe_locked(self, mode, peak_dbm, noise_floor_dbm) -> None:
        if mode not in self.MODES or not math.isfinite(peak_dbm):
            return
        tracker = self._trackers[mode]
        # Recording is unconditional: the safety ranger and a user's Auto Scale both need the
        # newest trace, and gating this on a mode was how "click Auto after the warning" ended
        # up with no data to work from.
        tracker['last_peak'] = peak_dbm
        tracker['last_noise_floor'] = noise_floor_dbm
        now = time.monotonic()
        if now < tracker['ignore_until'] or self._pending is not None:
            return
        floor = noise_floor_dbm if (noise_floor_dbm is not None
                                    and math.isfinite(noise_floor_dbm)) else None
        current = self.ref_level(mode)
        kind, target = self._decide(peak_dbm, floor, current, self.window_db(), tracker)
        if kind != 'out_of_window':
            return
        if now - tracker['last_change'] < SAFETY_INTERVAL_S:
            return
        self._queue(mode, target, tracker, now, result='out_of_window')

    # ---------------- the user's Auto Scale ----------------

    def fit(self, mode: str) -> tuple[str, float | None]:
        """Place the reference once, from the newest trace. Returns (result, target).

        result: 'applied'    - one reconfiguration queued for `target`
                'ok'         - already placed well, nothing changed
                'no_signal'  - peak too close to the noise floor to anchor to
                'no_data'    - no trace observed yet (just switched mode / geometry)
        """
        with self.dev._hw:
            if mode not in self.MODES:
                return 'no_data', None
            tracker = self._trackers[mode]
            peak = tracker['last_peak']
            if peak is None:
                tracker['result'] = 'no_data'
                return 'no_data', None
            floor = tracker['last_noise_floor']
            floor = floor if (floor is not None and math.isfinite(floor)) else None
            current = self.ref_level(mode)
            kind, target = self._decide(peak, floor, current, self.window_db(), tracker)
            if kind == 'ok':
                tracker['result'] = 'ok'
                return 'ok', None
            if kind == 'no_signal':
                tracker['result'] = 'no_signal'
                return 'no_signal', None
            # An explicit user action overrides the settle window: the observation the user is
            # looking at is the one to fit, even if it arrived during a reconfiguration.
            self._queue(mode, target, tracker, time.monotonic(), result='applied')
            return 'applied', target

    # ---------------- decisions ----------------

    def _decide(self, peak: float, floor: float | None, current: float,
                window: float, tracker: dict) -> tuple[str, float]:
        """Classify the placement and compute the target that fixes it."""
        bottom = current - window
        if peak > current + OUT_OF_WINDOW_MARGIN_DB:
            kind = 'out_of_window'          # clipped at the top edge: always fix
        elif floor is not None and floor < bottom - OUT_OF_WINDOW_MARGIN_DB:
            kind = 'out_of_window'          # whole trace below the window: always fix
        else:
            kind = 'inside'

        if kind == 'inside':
            if floor is not None and peak - floor < MIN_SIGNAL_DB:
                # Nothing worth anchoring to: leave the level alone (chasing noise was worse).
                return 'no_signal', current
            if floor is not None:
                noise_above_bottom = floor - bottom
                headroom = current - peak
                if 4.0 <= noise_above_bottom <= 12.0 and headroom >= 8.0:
                    return 'ok', current     # already placed well: do not reconfigure

        # No floor estimate: keep the peak below the top edge (the best available rule).
        target = peak + 10.0 if floor is None else max(floor + window - 8.0, peak + 10.0)
        target = math.ceil(target / 5.0) * 5.0
        target = min(CEILING_DBM, max(FLOOR_MIN_DBM, target, tracker.get('floor', FLOOR_MIN_DBM)))
        if floor is not None and kind == 'inside':
            # A high noise floor needs room: 30 dB of headroom above it.
            target = min(CEILING_DBM, max(target, floor + 30.0))
        if abs(target - current) < MIN_CHANGE_DB:
            return 'ok', current
        return ('applied' if kind == 'inside' else 'out_of_window'), target

    def _queue(self, mode: str, target: float, tracker: dict, now: float,
               result: str) -> None:
        tracker['last_change'] = now
        tracker['last_target'] = target
        tracker['result'] = result
        self._pending = (mode, target)

    # ---------------- retune safety ----------------

    def prepare_retune(self, mode: str) -> bool:
        """Use a safe Ref before changing frequency after a fit had lowered it.

        A low Ref fitted for one band can saturate the IF on the next one; lifting it back to
        0 dBm before the retune is what the old continuous loop did for the same reason.
        """
        with self.dev._hw:
            tracker = self._trackers.get(mode)
            if tracker is None or tracker['last_target'] is None:
                return False
            current = self.ref_level(mode)
            if current >= 0.0:
                return False
            self.set_ref_level(mode, max(0.0, current))
            self.begin_settle(mode)
            return True

    # ---------------- housekeeping ----------------

    def geometry(self, mode: str) -> tuple:
        """Signature of the measurement geometry the learned floor belongs to.

        The IF saturates at a Ref that depends on the in-band power, i.e. on span / RBW /
        points / window - not only on the front-end. A floor learned at another geometry
        either blocks a legitimate low Ref or invites saturation probing, so it is dropped
        when this signature changes. Applying a new Ref does NOT change it (verified by the
        signature itself), which is what keeps the fit from clearing its own floor.
        """
        s = self.dev.state
        if mode == 'rta':
            # The RTA profile takes its window from the same user setting as SWP.
            return (s.rta_center_hz, s.rta_span_hz, s.rta_rbw_hz, s.rta_vbw_hz,
                    s.window, getattr(s, 'rta_decimate', 0))
        return (s.center_hz, s.span_hz, s.rbw_hz, s.vbw_hz, s.window, 0)

    def begin_settle(self, mode: str, delay: float = 0.75) -> None:
        """Discard stale observations after any acquisition reconfiguration."""
        with self.dev._hw:
            geometry = self.geometry(mode)
            if self._geometry_seen.get(mode) not in (None, geometry):
                # Span/RBW/points/window changed: the learned saturation floor no longer
                # describes this configuration.
                self._trackers[mode]['floor'] = FLOOR_MIN_DBM
            self._geometry_seen[mode] = geometry
            self._rearm(mode, delay)

    def reset(self, mode: str) -> None:
        """Forget observations after a mode switch or a preset (the trace is gone)."""
        with self.dev._hw:
            self._rearm(mode, 0.25)

    def _rearm(self, mode: str, delay: float) -> None:
        tracker = self._trackers[mode]
        tracker['last_peak'] = None
        tracker['last_noise_floor'] = None
        tracker['ignore_until'] = time.monotonic() + delay
        if tracker['result'] == 'applied':
            tracker['result'] = 'idle'
        self._drop_pending(mode)

    def _drop_pending(self, mode: str) -> None:
        if self._pending is not None and self._pending[0] == mode:
            self._pending = None

    # ---------------- IF overflow escape ----------------

    def nudge_out_of_overflow(self) -> bool:
        """Raise Ref one 5 dB step when the device reports IF overflow (-12).

        The vendor's remedy for -12 is to raise RefLevel_dBm. This cannot live in the normal
        peak-based path because an overflowing IF delivers no frames at all - so clicking Auto
        after the warning appeared did nothing (deadlock). Runs regardless of the attenuation
        setting: manual attenuation used to disable overload protection entirely.
        """
        with self.dev._hw:
            s = self.dev.state
            if s.status_warning != -12 or s.mode not in self.MODES:
                return False
            mode = s.mode
            tracker = self._trackers[mode]
            now = time.monotonic()
            if now - tracker['last_change'] < OVERFLOW_INTERVAL_S:
                return False
            current = self.ref_level(mode)
            target = min(CEILING_DBM, current + OVERFLOW_STEP_DB)
            if target <= current:
                return False
            # Learn the usable lower bound: the IF overflows at this Ref, so never propose one
            # this low again. Without it the peak-based rule keeps trying to go back down and
            # the two mechanisms fight, oscillating 5-10 dB (measured).
            tracker['floor'] = max(tracker.get('floor', FLOOR_MIN_DBM), target)
            # Cleared so the next tick does not queue another step before this one lands; the
            # device re-reports -12 on the following frame if it is still saturating.
            s.status_warning = 0
            self._queue(mode, target, tracker, now, result='overflow')
            return True

    def apply_pending(self) -> bool:
        """Apply one queued update in the active acquisition worker."""
        with self.dev._hw:
            pending = self._pending
            if pending is None or pending[0] != self.dev.state.mode:
                return False
            self._pending = None
            mode, target = pending
            session = self.dev.session
            if mode == 'rta' and session is not None and session.name == 'rta':
                self.dev.state.rta_ref_level = target
                session._configure()
            elif mode == 'std':
                self.dev.state.ref_level = target
                ok, _ = self.dev.configure_swp()
                if not ok:
                    return False
            else:
                return False
            return True

    def view(self, mode: str) -> dict:
        """Diagnostics for STATUS (the serializer must not read the tracker directly)."""
        tracker = self._trackers.get(mode, {})
        pending = self._pending
        return {
            'last_peak': tracker.get('last_peak'),
            'last_noise_floor': tracker.get('last_noise_floor'),
            'target': tracker.get('last_target'),
            'result': tracker.get('result', 'idle'),
            'pending': pending[1] if pending else None,
            # True while a change is queued or still settling: drives the button's busy glow.
            'adjusting': pending is not None or time.monotonic() < tracker.get('ignore_until', 0.0),
        }
