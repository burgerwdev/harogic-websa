"""Auto-reference control loop.

`HarogicDevice` used to own this: ~200 lines of decision logic spread over seven methods,
sharing the device's mutable state and reaching into `configure_swp()` / the active session
(report finding P1-8). It is a control loop, not device I/O, so it lives here and the device
only forwards to it.

What it decides: given an observed peak and noise floor, should the reference level move, and
to what? The rules are the industry ones for a spectrum analyser, with the measured device
quirks that took real debugging to find:

* anchor the NOISE FLOOR just above the bottom of the display window and lift Ref only as far
  as the peak needs (anchoring on the peak left weak signals 7 divisions high),
* accept the current placement while the floor is 4-12 dB above the bottom and the peak keeps
  >= 8 dB of headroom (a wobbling estimate must not trigger a reconfiguration),
* raise immediately (overload safety) but wait for a time-stable peak before lowering,
* drop the learned IF-overflow floor when the measurement geometry changes,
* raise one 5 dB step when the device reports IF overflow (-12) - that path has no frames at
  all, so the peak-based rule cannot act (it deadlocked before).
"""
from __future__ import annotations

import math
import time

#: Default display window height (10 divisions x 10 dB/div) when the frontend has not
#: reported one; the window is what Auto Ref anchors the noise floor to.
DEFAULT_WINDOW_DB = 100.0
#: Lowest Ref the loop will ever propose.
FLOOR_MIN_DBM = -50.0
#: Highest Ref the device accepts here.
CEILING_DBM = 30.0
#: Ref step used for the IF-overflow escape.
OVERFLOW_STEP_DB = 5.0


def new_tracker() -> dict:
    return {
        'candidate': None, 'candidate_since': 0.0, 'last_change': 0.0,
        'last_peak': None, 'last_noise_floor': None, 'ignore_until': 0.0,
        'floor': FLOOR_MIN_DBM,
    }


class AutoReferenceController:
    """Per-mode Auto Ref state machines for the swept ('std') and RTA paths."""

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

    def ref_mode(self, mode: str) -> str:
        state = self.dev.state
        return state.rta_ref_mode if mode == 'rta' else state.ref_mode

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

    # ---------------- the loop ----------------

    def observe_peak(self, mode: str, peak_dbm: float, noise_floor_dbm: float | None = None) -> None:
        """Feed one trace observation; queue a Ref change when the rules say so."""
        with self.dev._hw:
            self._observe_locked(mode, peak_dbm, noise_floor_dbm)

    def _observe_locked(self, mode, peak_dbm, noise_floor_dbm) -> None:
        if mode not in self.MODES or not math.isfinite(peak_dbm):
            return
        state = self.dev.state
        if self.ref_mode(mode) != 'auto' or state.atten != -1:
            return
        tracker = self._trackers[mode]
        now = time.monotonic()
        if now < tracker['ignore_until']:
            return
        tracker['last_peak'] = peak_dbm
        tracker['last_noise_floor'] = noise_floor_dbm
        have_floor = noise_floor_dbm is not None and math.isfinite(noise_floor_dbm)

        if have_floor and peak_dbm - noise_floor_dbm < 15.0:
            # No signal worth anchoring to.
            self._clear_candidate(tracker)
            self._drop_pending(mode)
            return

        current = self.ref_level(mode)
        window = self.window_db()
        if not have_floor:
            # No floor estimate available: fall back to keeping the peak below the top edge.
            target = peak_dbm + 10.0
        else:
            target = max(noise_floor_dbm + window - 8.0, peak_dbm + 10.0)
        target = math.ceil(target / 5.0) * 5.0
        target = min(CEILING_DBM, max(FLOOR_MIN_DBM, target, tracker.get('floor', FLOOR_MIN_DBM)))

        # Window criterion instead of a bare 5 dB dead-band: while the noise floor sits
        # between 4 and 12 dB above the bottom edge AND the peak keeps >= 8 dB of headroom,
        # the placement is already right, so a wobbling estimate (or a small RBW/point change
        # that moves the noise floor by a dB or two) must not trigger a reconfiguration -
        # each one costs a full device reconfigure and is visible as a jump.
        if have_floor:
            noise_above_bottom = noise_floor_dbm - (current - window)
            headroom = current - peak_dbm
            if 4.0 <= noise_above_bottom <= 12.0 and headroom >= 8.0:
                self._clear_candidate(tracker)
                return
            # When the noise floor is high, keep ~30 dB of headroom above it.
            target = max(target, noise_floor_dbm + 30.0)
        target = min(CEILING_DBM, target)
        if abs(target - current) < 5.0:
            self._clear_candidate(tracker)
            return

        if tracker['candidate'] != target:
            tracker['candidate'] = target
            tracker['candidate_since'] = now
        # Raise the reference immediately for overload safety. Lowering waits for a
        # time-stable peak so RTA settle/empty frames cannot collapse Ref. After a re-arm
        # (the user pressed Auto, or a setting changed) the first decision is taken quickly
        # in both directions: the previous observation is known to be stale.
        fresh = bool(tracker.pop('fresh', False))
        stable_for = 0.15 if (fresh or target > current) else 1.5
        if (now - tracker['candidate_since'] >= stable_for
                and now - tracker['last_change'] >= 1.0):
            tracker['last_change'] = now
            self._pending = (mode, target)

    def _clear_candidate(self, tracker: dict) -> None:
        tracker['candidate'] = None
        tracker['candidate_since'] = 0.0

    def _drop_pending(self, mode: str) -> None:
        if self._pending is not None and self._pending[0] == mode:
            self._pending = None

    def prepare_retune(self, mode: str) -> bool:
        """Use a safe Ref before changing frequency when Auto Ref had lowered it."""
        with self.dev._hw:
            if self.ref_mode(mode) != 'auto':
                return False
            current = self.ref_level(mode)
            self.set_ref_level(mode, max(0.0, current))
            self.begin_settle(mode)
            return current < 0.0

    def geometry(self, mode: str) -> tuple:
        """Signature of the measurement geometry the learned floor belongs to.

        The IF saturates at a Ref that depends on the in-band power, i.e. on span / RBW /
        points / window - not only on the front-end. A floor learned at another geometry
        either blocks a legitimate low Ref or invites saturation probing, so it is dropped
        when this signature changes. Applying a new Ref does NOT change it (verified by the
        signature itself), which is what keeps the auto-ref from clearing its own floor on
        every application.
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
        """Re-arm: drop stale observations AND force a fresh decision.

        Called when the user enables Auto and whenever a setting changes that moves the
        trace (a reconfiguration calls begin_settle instead, which also re-arms). This is the
        event that answers "when should Auto act again?": a setting change invalidates the
        level the previous decision was based on, even if the new target ends up within the
        5 dB dead-band.
        """
        with self.dev._hw:
            self._rearm(mode, 0.25)

    def _rearm(self, mode: str, delay: float) -> None:
        tracker = self._trackers[mode]
        tracker['candidate'] = None
        tracker['candidate_since'] = 0.0
        tracker['last_peak'] = None
        tracker['last_noise_floor'] = None
        tracker['ignore_until'] = time.monotonic() + delay
        tracker['fresh'] = True
        self._drop_pending(mode)

    def nudge_out_of_overflow(self) -> bool:
        """Raise Ref one step when the device reports IF overflow (-12).

        The vendor's remedy for -12 is to raise RefLevel_dBm. This cannot live in the normal
        auto-reference path because that path needs a measured peak, and an overflowing IF
        delivers no frames at all - so clicking Auto Ref after the warning appeared did
        nothing (deadlock). Queues one step per second at most.
        """
        with self.dev._hw:
            s = self.dev.state
            if s.status_warning != -12 or s.mode not in self.MODES:
                return False
            mode = s.mode
            if self.ref_mode(mode) != 'auto' or s.atten != -1:
                return False
            tracker = self._trackers[mode]
            now = time.monotonic()
            if now - tracker['last_change'] < 1.0:
                return False
            current = self.ref_level(mode)
            target = min(CEILING_DBM, current + OVERFLOW_STEP_DB)
            if target <= current:
                return False
            tracker['last_change'] = now
            self._clear_candidate(tracker)
            # Learn the usable lower bound: the IF overflows at this Ref, so never propose
            # one this low again. Without it the peak-based rule keeps trying to go back down
            # and the two mechanisms fight, oscillating 5-10 dB (measured).
            tracker['floor'] = max(tracker.get('floor', FLOOR_MIN_DBM), target)
            # Cleared so the next tick does not queue another step before this one lands;
            # the device re-reports -12 on the following frame if it is still saturating.
            s.status_warning = 0
            self._pending = (mode, target)
            return True

    def apply_pending(self) -> bool:
        """Apply one queued update in the active acquisition worker."""
        with self.dev._hw:
            pending = self._pending
            if pending is None or pending[0] != self.dev.state.mode:
                return False
            self._pending = None
            mode, target = pending
            self._clear_candidate(self._trackers[mode])
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
            'candidate': tracker.get('candidate'),
            'pending': pending[1] if pending else None,
        }
