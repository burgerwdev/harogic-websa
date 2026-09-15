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

2. `observe_peak()` - two things, neither of them a tracking loop:
   a) a ONE-SHOT re-fit armed by a settings change (`begin_settle` sees a new geometry). The
      reference that was right for the old span/RBW/decimation is not right for the new one, so
      the first frame of the new geometry places it again - once, then disarmed. A level the user
      set by hand is left alone (press Auto to re-fit it), and only the geometry that changed can
      arm it, so nothing moves on its own while the settings stand still.
   b) the safety ranger - armed always, never user-controlled, and only in the PROTECTIVE
      direction:
      * IF overflow (-12) raises Ref one 5 dB step per second (that path has no frames at all, so
        the floor-based rule cannot act; it deadlocked before),
      * a peak that is grossly clipped above the top edge (>= AUTO_CLIP_MARGIN_DB) is raised once,
        rate-limited. Lowering is never automatic: a level that pushes the noise floor under the
        bottom edge is a display choice, and correcting it would undo the button the user just
        pressed (press Auto to re-fit).
      Both used to require `ref_mode == 'auto'` AND auto attenuation, so selecting a manual
      attenuator silently disabled overload protection altogether (measured: with `atten != -1`
      the device kept reporting -12 and nothing raised the level).

Placement rules (they took real debugging):

* anchor the NOISE FLOOR just above the bottom of the display window and lift Ref only as far
  as the peak needs (anchoring on the peak left weak signals 7 divisions high). This is the
  industry Auto Scale convention (Keysight Auto Scale, R&S Auto Level, Anritsu Auto Scale all
  place the reference so the whole trace fits the graticule, noise floor included);
* accept the current placement while the floor is 2-12 dB above the bottom and the peak keeps
  >= 8 dB of headroom (a wobbling estimate must not trigger a reconfiguration). The lower bound
  is one dB above the resolution of the 5 dB Ref grid, so anything lower really is clipped;
* there is NO signal-level gate any more. Anchoring on a peak needed a signal; anchoring on the
  floor does not, and the old `peak - floor < MIN_SIGNAL_DB` refusal produced the reported dead
  zone: a noise-only trace a couple of dB under the bottom edge classified as `inside` and then
  refused to move (`no_signal`), leaving it off the canvas with no way to fix it except typing a
  level by hand (measured, SWP 20 MHz / 1 MHz, fake floor -101 dBm at Ref 0 / 100 dB window);
* keep ~30 dB of headroom when the noise floor is high,
* never descend below a floor learned from an actual IF overflow, and drop that floor when the
  measurement geometry or the front-end changes.

The display scale (dB/div) reaches the backend with the Auto Scale request, so a dB/div change is
still fitted on the next press - the frontend owns that gesture and says so (`setScale`).
"""
from __future__ import annotations

import math
import time

from ..config import (
    DISPLAY_REF_MAX_DBM,
    DISPLAY_REF_MIN_DBM,
    FALLBACK_REF_MAX_DBM,
    FALLBACK_REF_MIN_DBM,
    ref_bounds,
)

#: Default display window height (10 divisions x 10 dB/div) when the frontend has not
#: reported one; the window is what the fit anchors the noise floor to.
DEFAULT_WINDOW_DB = 100.0
#: Fallback bounds when the device does not report a capability row; the real range comes from
#: `config.ref_bounds(dev.state.caps)` so the loop, the validator and the profile agree.
FLOOR_MIN_DBM = FALLBACK_REF_MIN_DBM
CEILING_DBM = FALLBACK_REF_MAX_DBM
#: Ref step used for the IF-overflow escape, and its rate limit.
OVERFLOW_STEP_DB = 5.0
OVERFLOW_INTERVAL_S = 1.0
#: A trace is "outside the window" once it misses an edge by this much.
OUT_OF_WINDOW_MARGIN_DB = 3.0
#: How far ABOVE the top edge the peak has to be before the ranger raises Ref on its own.
#:
#: A peak over the top edge loses information (the trace is cut off), so raising the level is the
#: protective direction - but the user may have moved Ref there deliberately, so only a gross
#: clipping is corrected. The opposite direction (a noise floor pushed below the bottom edge by
#: RAISING Ref) is never corrected: it is a display choice, not a fault, and undoing it means
#: undoing the button the user just pressed (reported: "raising Ref with the up arrow triggers
#: Auto to pull the trace back down").
AUTO_CLIP_MARGIN_DB = 10.0
#: At most one safety fit per this many seconds (a fit that did not help must not hunt).
SAFETY_INTERVAL_S = 2.0
#: Ref changes smaller than this are not worth a reconfiguration.
MIN_CHANGE_DB = 5.0
#: Where the noise floor is anchored: this far ABOVE the bottom edge of the display window.
#: One division at the default 10 dB/div, i.e. the trace sits flush with the bottom graticule -
#: the industry Auto Scale convention. Quantised by the 5 dB Ref grid, so the floor actually
#: lands 3-8 dB above the bottom, which is as close as a 5 dB step allows.
FLOOR_ANCHOR_DB = 8.0
#: A floor between these offsets above the bottom edge counts as comfortably inside the window;
#: within that band a settled display is left alone (a 5 dB step would be a visible glitch for
#: nothing). The lower bound is what makes a trace that is *only just* inside get re-fitted: a
#: floor below it is not reliably on the canvas, and "partially off the bottom" is the reported
#: failure mode of the small-span / no-signal case.
FLOOR_INSIDE_MIN_DB = 2.0
FLOOR_INSIDE_MAX_DB = 12.0
#: SDR: how far the fitted level must be from the device level before it is worth reconfiguring
#: IQS (that write interrupts the audio, so a display-only fit stays free).
SDR_DEVICE_DEADBAND_DB = 3.0


def _floor_default(mode: str) -> float:
    """The lower bound a tracker starts from before any IF overflow has been learned.

    SWP/RTA want the device's own minimum (a lower Ref is a device error); SDR's fitted level is
    a display scale, and the client window goes lower than the device Ref range, so its floor is
    the display minimum (`FLOOR_MIN_DBM` is kept as the SWP/RTA value and the public name).
    """
    return DISPLAY_REF_MIN_DBM if mode == 'sdr' else FLOOR_MIN_DBM


def new_tracker(floor: float = FLOOR_MIN_DBM) -> dict:
    return {
        'last_peak': None, 'last_noise_floor': None,
        'last_target': None,          # level this loop last applied
        'last_change': 0.0,           # monotonic time of that application
        'ignore_until': 0.0,          # observations are stale until then (settle window)
        'floor': floor,               # learned lower bound (IF saturation)
        'result': 'idle',             # last fit outcome, for the UI ('ok', 'applied', ...)
        'seq': 0,                     # increments per DECISION, so the UI can tell a new answer
                                      # from the sticky remainder of the previous one
        'user_level': False,           # the level was set BY HAND for this geometry: the
                                      # automatic re-fit must not reverse it
        'refit_due': False,            # a settings change asked for ONE automatic re-fit
    }


class AutoReferenceController:
    """Per-mode reference placement for the swept ('std'), RTA and SDR paths.

    SDR is included so that the fit has ONE implementation: the rule is the same whether the
    reference is a swept profile level or an IQS level/display scale. The frontend still owns the
    SDR *display* mapping, so it applies the reported target to its own display reference; the
    device level is only written when it is actually off (that write reconfigures the capture and
    interrupts the audio).
    """

    MODES = ('std', 'rta', 'sdr')

    def __init__(self, dev) -> None:
        self.dev = dev                      # provides state, _hw lock, configure_swp, session
        # An SDR fit is a DISPLAY scale, not a device Ref, so its floor starts at the display
        # minimum (see placement_bounds): starting it at the device minimum would keep a
        # noise-only SDR trace under the bottom of a window that can show it.
        self._trackers = {mode: new_tracker(_floor_default(mode)) for mode in self.MODES}
        self._pending: tuple[str, float] | None = None
        # Incremented by reset(): a queued update from before a manual takeover/mode change must
        # not be applied afterwards (measured: a safety fit queued a moment earlier landed on top
        # of the level the user had just set).
        self._epoch = 0
        self._pending_epoch = 0
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
            # 'std' and 'sdr' both keep the level the IQS/SWP profile uses (the SDR session
            # saves and restores it with its snapshot).
            state.ref_level = value

    def device_ref_bounds(self) -> tuple[float, float]:
        """The range this DEVICE accepts, from its capability row (see config.ref_bounds)."""
        return ref_bounds(getattr(self.dev.state, 'caps', None))

    def placement_bounds(self, mode: str) -> tuple[float, float]:
        """The range the OWNER of this mode's level accepts.

        SWP/RTA: the fitted value IS the device Ref, so the capability row limits it.
        SDR: it is a DISPLAY level - the client owns that scale, the window can go to -160 dBm,
        and the IQS level is only written when the two are more than `SDR_DEVICE_DEADBAND_DB`
        apart. Clamping an SDR fit to the device row (-50..+30 dBm) left a low noise floor
        under the bottom edge of a window that could have shown it - the same mistake the
        command layer already fixed for `AUTO_SCALE.current_ref`.
        """
        if mode == 'sdr':
            return DISPLAY_REF_MIN_DBM, DISPLAY_REF_MAX_DBM
        return self.device_ref_bounds()

    def window_db(self) -> float:
        return max(20.0, float(getattr(self.dev.state, 'ref_range_db', DEFAULT_WINDOW_DB)))

    def clear_learned_floor(self) -> None:
        """Forget the IF-overflow floor (front-end or geometry changed)."""
        with self.dev._hw:
            for mode, tracker in self._trackers.items():
                tracker['floor'] = _floor_default(mode)

    # ---------------- observations ----------------

    def observe_peak(self, mode: str, peak_dbm: float, noise_floor_dbm: float | None = None) -> None:
        """Feed one trace observation: record it, then run the armed re-fit and/or the ranger."""
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
        kind, target = self._decide(mode, peak_dbm, floor, current, self.window_db(), tracker)
        if tracker['refit_due']:
            # The settings change that armed this is the user's own action, so one placement is
            # expected - and it is the only way an observation may move the reference by itself.
            # A manual level is still respected, and the rate limit keeps a burst of settings
            # changes from reconfiguring back to back (the request stays armed until it lands).
            if tracker['user_level'] or kind == 'ok':
                tracker['refit_due'] = False
            elif now - tracker['last_change'] >= SAFETY_INTERVAL_S:
                tracker['refit_due'] = False
                self._queue(mode, target, tracker, now, result='applied')
                return
        # Only the protective direction, and only when the loss is gross:
        #   * 'clipped'      - the peak is above the top edge: raising Ref brings it back.
        #   * 'below_window' - RAISING Ref pushed the noise floor under the bottom edge. That is a
        #     display choice the user just made; it is fixed by pressing Auto, not behind their back.
        if kind != 'clipped' or peak_dbm <= current + AUTO_CLIP_MARGIN_DB:
            return
        if target <= current:
            return                     # never move against the protective direction on our own
        if now - tracker['last_change'] < SAFETY_INTERVAL_S:
            return
        self._queue(mode, target, tracker, now, result='clipped')

    # ---------------- the user's Auto Scale ----------------

    def fit(self, mode: str, current: float | None = None) -> tuple[str, float | None]:
        """Place the reference once, from the newest trace. Returns (result, target).

        `current` is the level the user is looking at. In SDR the display scale belongs to the
        client, so the device level is not what the placement is judged against; passing the
        visible level keeps the decision (and the reported target) about what is on screen.

        result: 'applied'    - one reconfiguration queued for `target`
                'ok'         - already placed well, nothing changed
                'no_data'    - no trace observed yet (just switched mode / geometry)

        (The floor-anchored rule never refuses for lack of a signal, so `no_signal` is no longer
        produced; it stays in the STATUS vocabulary because the field's values are a contract.)
        """
        with self.dev._hw:
            if mode not in self.MODES:
                return 'no_data', None
            tracker = self._trackers[mode]
            peak = tracker['last_peak']
            if peak is None:
                tracker['result'] = 'no_data'
                tracker['seq'] += 1
                return 'no_data', None
            floor = tracker['last_noise_floor']
            floor = floor if (floor is not None and math.isfinite(floor)) else None
            if current is None or not math.isfinite(current):
                current = self.ref_level(mode)
            kind, target = self._decide(mode, peak, floor, current, self.window_db(), tracker)
            if kind == 'ok':
                tracker['result'] = 'ok'
                tracker['seq'] += 1
                return 'ok', None
            # An explicit user action overrides the settle window: the observation the user is
            # looking at is the one to fit, even if it arrived during a reconfiguration. It also
            # clears any queued automatic re-fit: the user asked, so only one answer may land.
            tracker['refit_due'] = False
            self._queue(mode, target, tracker, time.monotonic(), result='applied')
            return 'applied', target

    # ---------------- decisions ----------------

    def _decide(self, mode: str, peak: float, floor: float | None, current: float,
                window: float, tracker: dict) -> tuple[str, float]:
        """Classify the placement and compute the target that fixes it."""
        bottom = current - window
        if peak > current + OUT_OF_WINDOW_MARGIN_DB:
            kind = 'clipped'                # peak above the top edge: information is cut off
        elif floor is not None and floor < bottom - OUT_OF_WINDOW_MARGIN_DB:
            kind = 'below_window'           # whole trace below the bottom edge
        else:
            kind = 'inside'

        if kind == 'inside':
            if floor is not None:
                noise_above_bottom = floor - bottom
                headroom = current - peak
                if (FLOOR_INSIDE_MIN_DB <= noise_above_bottom <= FLOOR_INSIDE_MAX_DB
                        and headroom >= 8.0):
                    return 'ok', current     # already placed well: do not reconfigure

        # No floor estimate: keep the peak below the top edge (the best available rule).
        target = peak + 10.0 if floor is None else max(floor + window - FLOOR_ANCHOR_DB,
                                                       peak + 10.0)
        target = math.ceil(target / 5.0) * 5.0
        ref_lo, ref_hi = self.placement_bounds(mode)
        target = min(ref_hi, max(ref_lo, target, tracker.get('floor', ref_lo)))
        if floor is not None and kind == 'inside':
            # A high noise floor needs room: 30 dB of headroom above it.
            target = min(ref_hi, max(target, floor + 30.0))
        if abs(target - current) < MIN_CHANGE_DB:
            return 'ok', current
        return ('applied' if kind == 'inside' else kind), target

    def _queue(self, mode: str, target: float, tracker: dict, now: float,
               result: str) -> None:
        tracker['last_change'] = now
        tracker['last_target'] = target
        tracker['result'] = result
        tracker['seq'] += 1
        # The loop just placed the level, so it owns it: a later settings change may move it
        # again without being told to (see `reset(manual=True)` for the opposite).
        tracker['user_level'] = False
        self._pending = (mode, target)
        self._pending_epoch = self._epoch

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
        if mode == 'sdr':
            # The saturation point follows the captured bandwidth (decimation) and the centre.
            return (s.sdr_center_hz, getattr(s, 'sdr_decimate', 0), s.sdr_if_bw, 0, 0)
        return (s.center_hz, s.span_hz, s.rbw_hz, s.vbw_hz, s.window, 0)

    def begin_settle(self, mode: str, delay: float = 0.75) -> None:
        """Discard stale observations after any acquisition reconfiguration."""
        with self.dev._hw:
            geometry = self.geometry(mode)
            if self._geometry_seen.get(mode) not in (None, geometry):
                # Span/RBW/points/window/decimation changed: the learned saturation floor no
                # longer describes this configuration - and neither does the level that was
                # placed for the old one, so arm ONE automatic re-fit for the new geometry.
                # (The initial settle of a mode is not a change and does not arm anything, which
                # is what keeps connecting/preset-into-mode from moving the level on its own.)
                tracker = self._trackers[mode]
                tracker['floor'] = _floor_default(mode)
                tracker['refit_due'] = True
            self._geometry_seen[mode] = geometry
            self._rearm(mode, delay)

    def reset(self, mode: str, manual: bool = False) -> None:
        """Forget observations after a mode switch, a preset or a manual takeover.

        `manual` marks a takeover that came from a level the USER typed: the automatic re-fit
        after a later settings change must not reverse it (pressing Auto is what re-fits it).
        """
        with self.dev._hw:
            self._epoch += 1
            self._rearm(mode, 0.25)
            tracker = self._trackers[mode]
            # Nothing is pending and the last decision is no longer about this configuration.
            tracker['result'] = 'idle'
            tracker['user_level'] = bool(manual)
            tracker['refit_due'] = False

    def _rearm(self, mode: str, delay: float) -> None:
        tracker = self._trackers[mode]
        tracker['last_peak'] = None
        tracker['last_noise_floor'] = None
        tracker['ignore_until'] = time.monotonic() + delay
        # The result deliberately survives: applying a target reconfigures the device, and that
        # settle must not erase the very decision it is settling (measured: the UI reported
        # 'idle' for a fit it had just made, and the fallback glow outlived it).
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
            ref_lo, ref_hi = self.device_ref_bounds()
            target = min(ref_hi, current + OVERFLOW_STEP_DB)
            if target <= current:
                return False
            # Learn the usable lower bound: the IF overflows at this Ref, so never propose one
            # this low again. Without it the peak-based rule keeps trying to go back down and
            # the two mechanisms fight, oscillating 5-10 dB (measured).
            tracker['floor'] = max(tracker.get('floor', ref_lo), target)
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
            if self._pending_epoch != self._epoch:
                # Superseded by a manual takeover/mode change: dropping it is the whole point of
                # the epoch (the user's level wins).
                self._pending = None
                return False
            self._pending = None
            mode, target = pending
            session = self.dev.session
            if mode == 'rta' and session is not None and session.name == 'rta':
                self.dev.state.rta_ref_level = target
                session._configure()
            elif mode == 'sdr':
                if session is None or session.name != 'sdr':
                    return False
                # The display scale is client-side, so a fit within a few dB of the device level
                # needs no device traffic at all (reconfiguring IQS interrupts the audio). The
                # IQS level itself must stay a DEVICE value even when the display target went
                # below the device range (the display scale owns the placement).
                ref_lo, ref_hi = self.device_ref_bounds()
                level = min(ref_hi, max(ref_lo, target))
                if abs(level - self.dev.state.ref_level) < SDR_DEVICE_DEADBAND_DB:
                    return False
                self.dev.state.ref_level = level
                session.reconfigure()
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
            'seq': tracker.get('seq', 0),
            'pending': pending[1] if pending else None,
            # True while a change is queued or still settling: drives the button's busy glow.
            'adjusting': pending is not None or time.monotonic() < tracker.get('ignore_until', 0.0),
        }
