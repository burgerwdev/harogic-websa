"""Reference-level placement: the user's one-shot fit plus the IF-overflow protection.

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
   a) a BOUNDED CLOSED-LOOP placement armed by a settings change (`begin_settle` sees a new
      geometry) or by the user's Auto press. The reference that was right for the old
      span/RBW/decimation is not right for the new one, and one computation cannot land it: the
      trace is not independent of Ref (with the automatic attenuator the device re-picks
      attenuation as Ref moves, so the trace follows Ref by roughly half, in jumps). Each settled
      frame therefore re-measures and applies the next step until the placement is good, the
      budget (`REFIT_ATTEMPTS`) is used up, or a level the user set by hand appears - so it can
      never become a tracking mode, and nothing moves while the settings stand still.
   b) the IF-overflow protection - armed always, never user-controlled, and only in the
      PROTECTIVE direction: IF overflow (-12) raises Ref one 5 dB step per second (that path has
      no frames at all, so the floor-based rule cannot act; it deadlocked before).
      A peak that is grossly clipped above the top edge used to be raised here as well; that was
      removed on request. A level the user asked for is the user's:
      seeing a clipped trace is feedback, not a fault to be corrected behind their back, and the
      Auto Scale button is the deliberate way to re-fit. Lowering was never automatic either: a
      level that pushes the noise floor under the bottom edge is a display choice.
      The escape used to require `ref_mode == 'auto'` AND auto attenuation, so selecting a manual
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

import logging
import math
import time

from ..config import (
    DISPLAY_REF_MAX_DBM,
    DISPLAY_REF_MIN_DBM,
    FALLBACK_REF_MAX_DBM,
    ref_bounds,
)

log = logging.getLogger(__name__)

#: Default display window height (10 divisions x 10 dB/div) when the frontend has not
#: reported one; the window is what the fit anchors the noise floor to.
DEFAULT_WINDOW_DB = 100.0
#: Fallback bounds when the device does not report a capability row; the real range comes from
#: `config.ref_bounds(dev.state.caps)` so the loop, the validator and the profile agree.
FLOOR_MIN_DBM = DISPLAY_REF_MIN_DBM     # the display domain, NOT the -50..+30 capability guess
CEILING_DBM = FALLBACK_REF_MAX_DBM
#: Ref step used for the IF-overflow escape, and its rate limit.
OVERFLOW_STEP_DB = 5.0
OVERFLOW_INTERVAL_S = 1.0
#: A trace is "outside the window" once it misses an edge by this much.
OUT_OF_WINDOW_MARGIN_DB = 3.0
#: At most one automatic fit per this many seconds (a fit that did not help must not hunt).
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
#: nothing). The LOWER bound is half a division at the default 10 dB/div: closer to the edge than
#: that the trace reads as a flat line lying ON the axis - technically on the canvas, but reported
#: as "the trace is not displayed properly" (measured after a Preset: the SDR entry accepted a
#: placement with the floor 2.5 dB above the bottom and stopped there). The UPPER bound keeps the
#: noise floor near the bottom, which is the industry Auto Scale convention.
FLOOR_INSIDE_MIN_DB = 5.0
FLOOR_INSIDE_MAX_DB = 12.0
#: How many placements one arming may apply before it gives up (see `_observe_locked`).
#:
#: The placement is a CLOSED loop, not a single computation, because the trace is not independent
#: of the reference: with the automatic attenuator the device re-picks attenuation as Ref moves,
#: so the trace follows Ref by roughly half and in discrete jumps. Measured on the SAN-90 at
#: centre 20 MHz / span 1 MHz (auto Atten, 100 dB window, no signal): Ref -10/-20/-30/-40/-50 dBm
#: gave attenuation 12/15/6/0/0 dB and a noise floor 12.3/13.9/11.0/2.7/-7.2 dB BELOW the bottom
#: edge - i.e. only Ref -50 dBm actually shows the trace, and no single open-loop step reaches
#: it (a fit that computed -20 from a -115 dBm floor left the trace 14 dB under the canvas).
#: So each attempt re-measures; this bounds the loop, and it stops at the first good placement
#: (`ok`), so it can never become a tracking mode.
REFIT_ATTEMPTS = 4


def _floor_default(mode: str) -> float:
    """The lower bound a tracker starts from before any IF overflow has been learned.

    The display minimum, in every mode: a lower Ref is not a device error (the SDK documents no Ref
    range), and until the device actually reports IF saturation there is nothing to learn a floor
    from. `FLOOR_MIN_DBM` is kept as the historical name for it.
    """
    del mode                     # every mode starts from the same place now
    return FLOOR_MIN_DBM


def new_tracker(floor: float = FLOOR_MIN_DBM) -> dict:
    return {
        'last_peak': None, 'last_noise_floor': None,
        'last_target': None,          # level this loop last applied
        'last_change': 0.0,           # monotonic time of that application
        'ignore_until': 0.0,          # observations are stale until then (settle window)
        'floor': floor,               # learned lower bound (IF saturation)
        'result': 'idle',             # last fit outcome, for the UI ('ok', 'applied', ...)
        #: WHY the pending was queued ('applied' from a fit, 'overflow' from the escape). `result` is
        #: sticky (STATUS reports it), so deciding how to APPLY a pending from it is wrong: one past
        #: IF overflow made every later fit's pending look like an overflow, so the display-only fit
        #: wrote the IQS level after all - an SDR entry reconfigured the stream four times over seven
        #: seconds while the client sent ONE AUTO_SCALE and no SET_REF.
        'pending_reason': None,
        'seq': 0,                     # increments per DECISION, so the UI can tell a new answer
                                      # from the sticky remainder of the previous one
        'user_level': False,           # the level was set BY HAND for this geometry: the
                                      # automatic re-fit must not reverse it
        'refit_due': False,            # a settings change (or a press) asked for a placement
        'refit_left': 0,               # placements still allowed for that request
    }


#: Which piece of device state each mode's level lives in. One profile per mode in the vendor SDK,
#: so one field per mode here: SWP/RTA/IQS levels are independent (they used to share `ref_level`,
#: which made a level set in SDR show up in the swept view).
REF_FIELD = {'std': 'ref_level', 'rta': 'rta_ref_level', 'sdr': 'sdr_ref_level'}


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
        return getattr(self.dev.state, REF_FIELD[mode])

    def set_ref_level(self, mode: str, value: float) -> None:
        setattr(self.dev.state, REF_FIELD[mode], value)

    def device_ref_bounds(self) -> tuple[float, float]:
        """The range this DEVICE accepts, from its capability row (see config.ref_bounds)."""
        return ref_bounds(getattr(self.dev.state, 'caps', None))

    def placement_bounds(self, mode: str) -> tuple[float, float]:
        """The range a PLACEMENT may use for this mode's level.

        The FLOOR is the display domain in every mode: the capability row's -50 dBm is this
        project's own guess, not a device limit (the SDK documents no Ref range at all - measured
        on this bench, -140 dBm is programmed exactly). Clamping the fit's floor to that guess left
        a trace that needs a lower window unmovable; what protects the front end instead is the
        learned IF-overflow floor in `_decide`.

        The CEILING stays the device's own row for SWP/RTA: its maximum is real and measured (+35
        dBm asked, +27 programmed, depending on the attenuation the device picks). Asking for more
        would leave the closed loop comparing its target against an echo it can never reach, and
        re-stepping on every observation for nothing. In SDR the fitted value is a DISPLAY level
        the client owns, so its ceiling is the display maximum.
        """
        if mode == 'sdr':
            return DISPLAY_REF_MIN_DBM, DISPLAY_REF_MAX_DBM
        _, hi = self.device_ref_bounds()
        return DISPLAY_REF_MIN_DBM, hi

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
        # Recording is unconditional: a user's Auto Scale acts on the newest observation, and
        # gating this on a mode was how "click Auto after the warning" ended up with no data to
        # work from. Recording is also *all* an observation does by itself now.
        tracker['last_peak'] = peak_dbm
        tracker['last_noise_floor'] = noise_floor_dbm
        now = time.monotonic()
        if now < tracker['ignore_until'] or self._pending is not None:
            return
        if not tracker['refit_due']:
            # No automatic placement is armed, and nothing here moves the reference behind the
            # user's back: the gross-clip correction was removed on request (a clipped trace is
            # feedback, not a fault to be corrected), and the only protective path left is the
            # IF-overflow escape, which acts on a device warning and no trace at all.
            return
        floor = noise_floor_dbm if (noise_floor_dbm is not None
                                    and math.isfinite(noise_floor_dbm)) else None
        current = self.ref_level(mode)
        kind, target = self._decide(mode, peak_dbm, floor, current, self.window_db(), tracker)
        # A settings change (or an Auto press) asked for a placement. It is the only way an
        # observation may move the reference by itself, and it is a CLOSED loop: the trace
        # follows Ref by ~half and in jumps (the automatic attenuator re-picks with Ref), so
        # the first target usually lands short - each attempt re-measures, and the loop ends
        # at the first good placement (`ok`), when a manual level appears, or when the budget
        # is used up. That is what keeps it a bounded placement and not a tracking mode.
        if tracker['user_level'] or kind == 'ok':
            tracker['refit_due'] = False
            tracker['refit_left'] = 0
        elif tracker['refit_left'] <= 0:
            tracker['refit_due'] = False        # budget spent: give up, never hunt
        elif now - tracker['last_change'] >= SAFETY_INTERVAL_S:
            tracker['refit_left'] -= 1
            self._queue(mode, target, tracker, now, result='applied')
        # else: a frame arrived inside the settle interval - the loop stays ARMED and steps on the
        # next settled one. Treating "too soon" as "gave up" is what stopped a fit after a single
        # step (reported on the bench: Auto left the trace below the bottom edge at 13.825 MHz /
        # 1 MHz, where three steps were needed).

    # ---------------- the user's Auto Scale ----------------

    def fit(self, mode: str, current: float | None = None) -> tuple[str, float | None]:
        """Place the reference once, from the newest trace. Returns (result, target).

        `current` is the level the user is looking at. In SDR the display scale belongs to the
        client, so the device level is not what the placement is judged against; passing the
        visible level keeps the decision (and the reported target) about what is on screen.

        result: 'applied'    - the first step is queued for `target`, and (unless the trace is
                               then already in the window) the bounded loop keeps going on the
                               following settled frames
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
            # arms the same bounded loop, so a press that undershoots (the usual case when the
            # automatic attenuator moves with Ref) keeps going until the trace is on the canvas.
            tracker['refit_due'] = True
            tracker['refit_left'] = REFIT_ATTEMPTS - 1
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
        tracker['pending_reason'] = result
        tracker['seq'] += 1
        # The loop just placed the level, so it owns it: a later settings change may move it
        # again without being told to (see `reset(manual=True)` for the opposite).
        tracker['user_level'] = False
        self._pending = (mode, target)
        self._pending_epoch = self._epoch

    # ---------------- retune safety ----------------

    def prepare_retune(self, mode: str) -> bool:
        """Use a safe Ref before changing frequency after a FIT had lowered it.

        A low Ref fitted for one band can saturate the IF on the next one; lifting it back to
        0 dBm before the retune is what the old continuous loop did for the same reason.

        A level the USER set is never lifted. It is theirs, and the lift is invisible by design (no
        message, no busy state), so getting it wrong looks exactly like "the Ref resets itself":
        reported on the bench - after any fit had once placed a level in a mode, every later
        frequency change reset a manually set Ref to 0 dBm (`last_target` is kept as the loop's
        memory of a placement and a manual takeover did not clear it). What still protects a manual
        level is the IF-overflow escape, which acts on the device's own -12 warning.
        """
        with self.dev._hw:
            tracker = self._trackers.get(mode)
            if tracker is None or tracker['last_target'] is None or tracker['user_level']:
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
                tracker['refit_left'] = REFIT_ATTEMPTS
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
            tracker['refit_left'] = 0

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
            tracker = self._trackers[mode]
            pending_reason = tracker.get('pending_reason')
            tracker['pending_reason'] = None
            if mode == 'rta' and session is not None and session.name == 'rta':
                self.set_ref_level('rta', target)
                session._configure()
            elif mode == 'sdr':
                # SDR applies a *display* fit only: the client owns its display scale, and writing the
                # IQS level reconfigures the whole stream (the audio drops out for about half a second
                # and re-primes). Measured on the bench: writing it from the fit oscillated - each
                # write moved the trace the fit was reading, so it wrote again (0 -> -15 -> -20 -> -25
                # -> -20 dBm in three seconds, one full reconfiguration and one audio flush each). That
                # is an audible puff on every level step, and a digital mode cannot integrate a slot
                # across the holes it leaves.
                if session is None or session.name != 'sdr':
                    return False
                if pending_reason == 'overflow':
                    # ...with ONE exception, and it is the whole reason the escape exists: an IF
                    # overflow is the DEVICE in trouble (and that path delivers no frames at all), so
                    # the raise must reach the front end. It used to be swallowed by the display-only
                    # path below - the escape queued a step and then dropped it, so the level stayed
                    # where the saturation started until the user moved it.
                    self.set_ref_level('sdr', target)
                    self.dev.state.sdr_ref_set = True
                    self._rearm(mode, 0.75)
                    session.reconfigure()
                    return True
                # Otherwise: consume the pending and settle the tracker exactly as an applied fit does
                # (the fit target is the client's input; without this the next observation re-fits,
                # because nothing about the trace changed) - but write nothing to the device.
                log.info('SDR auto-reference: the fit target %.1f dBm goes to the client display; '
                         'the device level is left alone', target)
                # Not `_rearm`: that also clears the observations, and nothing moved (no device
                # write happened), so the diagnostics keep the trace the fit was made from.
                tracker['ignore_until'] = time.monotonic() + 0.75
                tracker['refit_due'] = False
                tracker['refit_left'] = 0
                return False
            elif mode == 'std':
                self.set_ref_level('std', target)
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
