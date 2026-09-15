# SWP / RTA Parameter State and Mode Transitions

This document defines parameter ownership, defaults, command semantics, and frontend/backend state transitions for swept spectrum (SWP) and real-time spectrum (RTA).

## 1. Parameter Ownership

SWP and RTA acquisition settings are independent. RTA configuration must never overwrite SWP fields.

| Parameter | SWP state | RTA state | Shared |
|---|---|---|---|
| Center / Span | `center_hz` / `span_hz` | `rta_center_hz` / `rta_span_hz` | No |
| Ref / Ref Mode | `ref_level` / `ref_mode` | `rta_ref_level` / `rta_ref_mode` | No |
| RBW | `rbw_mode` / `rbw_hz` | `rta_rbw_mode` / `rta_rbw_hz` | No |
| VBW | `vbw_mode` / `vbw_hz` | `rta_vbw_mode` / `rta_vbw_hz` | No |
| Sweep | `sweep_time_mode` / `sweep_time` | `rta_sweep_time_mode` / `rta_sweep_time` | No |
| SDK actual values | `actual` | `rta_actual` | No |
| Points / Window / Spur | SWP only | Not applicable | No |
| Atten / Preamp / IF Gain | Device RF frontend | Device RF frontend | Yes |
| Reference Clock | Device-level | Device-level | Yes |

## 2. Application Startup Defaults

These are deterministic WebSA startup settings and are not the same as the device `SWP_ProfileDeInit` Preset.

### SWP

| Parameter | Default |
|---|---:|
| Center | 1 GHz |
| Span | 100 MHz |
| Ref | 0 dBm, Manual |
| RBW | 100 kHz, Manual |
| VBW | 100 kHz, Manual |
| Points | 1000 requested; SDK selects actual count |
| Window | Blackman-Nuttall (`1`) |
| Spur | Bypass |
| Sweep | minSWT (`0`) |
| Atten | Auto (`-1`) |
| Preamp | Auto |
| IF Gain | 2 |
| Gain Strategy | Low Noise |

### RTA

| Parameter | Default |
|---|---:|
| Center | 1 GHz |
| Span | 50.78125 MHz (Decimate 1) |
| Ref | 0 dBm, Manual |
| RBW | Auto; effective value returned by SDK/FFT size |
| VBW | Equal to RBW |
| Sweep | minSWT x4 (`2`) |
| Display Points | 1001 |

On the tested SAN-90, default RTA span returns effective RBW/VBW near 30.153 kHz. A 12.6953125 MHz span returns about 7.538 kHz. The UI must not replace these SDK actual values with `span/2000`.

## 3. Device Preset Defaults

`SET_PRESET` restores cached SWP defaults from `SWP_ProfileDeInit` and restores RTA to the fixed defaults above. A device SWP full span can include guard bands; WebSA normalizes it to the active model's `caps.fmin/fmax`.

Preset restores SWP Center/Span, Ref, RBW/VBW and modes, Points, Window, Spur, Sweep, Atten, Preamp, IF Gain, and Gain Strategy. Ref Mode returns to Manual.

## 4. Three STATUS Layers

- Top-level `center/span/ref/rbw/vbw/sweep_*`: effective values for the active mode, used directly by the UI.
- `req.swp` / `req.rta`: independently retained requested settings.
- `swp_actual` / `rta_actual`: latest successful SDK effective settings for each mode.
- `actual`: compatibility alias for the active mode's actual object.
- `config_version`: increments after each successful hardware reconfiguration.
- `response_to`: identifies a command-response STATUS; periodic STATUS messages omit it.

The frontend refreshes the active mode from top-level effective values while the backend retains both mode-private configurations.

## 5. SWP Frequency Field Linking

### Center / Span

1. The first focus selects the entire input; later clicks while focused can place the caret normally.
2. Editing Center or Span marks the complete SWP frequency editor dirty.
3. Periodic STATUS updates state but cannot overwrite dirty fields.
4. Unit buttons have safe dual behavior: after editing, clicking Hz/kHz/MHz/GHz commits using that unit; without editing, the button only converts the displayed value and does not configure hardware.
5. Set, Enter, or an edited field's unit button sends one `SET_FREQ {center, span}`.
6. The backend preserves the user-entered center and shrinks span to the largest symmetric range available around it. Only Full Span explicitly moves center to the full-band midpoint.
7. A successful SDK call returns STATUS with `response_to=SET_FREQ`.
8. The frontend clears dirty state and fills Center/Span/Start/Stop from SDK actual values.

### Start / Stop

1. Start and Stop are edited as one group; blur does not submit either field separately.
2. Set or Enter sends one `SET_FREQ {start, stop}`.
3. The backend rejects Stop <= Start and spans below 100 Hz.
4. The backend derives a unique Center/Span and fills all four fields after SDK success.

A command cannot mix `center/span` with `start/stop`.

### Span Step

- The `▼ / Full Span / ▲` row aligns with the frequency input column.
- Auto Step selects the nearest 1/2/5 value around one tenth of current SWP span, for example 100 MHz→10 MHz and 200 MHz→20 MHz.
- Editing Step selects custom mode; Auto restores span-linked behavior.
- Step is stored as absolute Hz. Changing Span units converts only its display.
- Narrower/wider actions send one atomic `SET_FREQ {center, span}` and respect device span limits.

## 6. SWP -> RTA

1. Clicking RTA disables the mode button and enters pending state; the UI does not switch optimistically.
2. The frontend sends only `SET_MODE {mode:rta}` and does not replay RTA/RBW/VBW/Sweep from localStorage.
3. The backend stops the old session, loads retained RTA-private settings, and performs one `RTA_Configuration`.
4. Success increments `config_version` once and switches top-level STATUS to RTA actual values.
5. After mode=rta STATUS, the frontend changes views, clears pending, and shows RTA Center/Span and actual RBW/VBW.
6. The complete SWP configuration remains in `req.swp` throughout.

Re-entering RTA uses the last backend RTA settings. Service restart or Preset restores RTA defaults.

## 7. RTA Parameter Changes

- Center/Span: one `SET_RTA {center, span}`; span maps to `50.78125 MHz / 2^n`.
- RBW: `SET_RBW` changes only RTA-private RBW; Auto displays SDK actual.
- VBW: `SET_VBW` changes only RTA-private VBW; default is Equal.
- Sweep: `SET_SWEEP` changes only RTA-private sweep; default is x4.
- Ref: `SET_REF` changes only RTA-private Ref/Ref Mode.

Each command performs at most one RTA reconfiguration and returns one STATUS with `response_to`.

## 8. Device-Level Settings in RTA

Reference Clock, Reference Clock Output, Atten, Preamp, IF Gain, and Gain Strategy are device-level shared settings. While RTA is active, these commands must update the RTA Profile and perform one `RTA_Configuration`; they must not call `SWP_Configuration`.

On the tested hardware, Internal/External and Clock Output On/Off commands respond in about 105-145 ms, with RTAF resuming after about 360 ms. Mode remains RTA. Do not automatically test `external_forced` without a valid external reference.

## 9. RTA -> SWP

1. Clicking RTA to exit sends only `SET_MODE {mode:std}`.
2. The backend stops the RTA trigger, restores retained SWP-private settings, and performs one `SWP_Configuration`.
3. Success increments `config_version` once and switches top-level STATUS to SWP actual values.
4. RTA settings remain in `req.rta` for the next entry.

## 10. Reference Level

### Manual

`SET_REF {mode:manual, ref}` configures the active mode's SDK RefLevel. The frontend displays SDK actual. If manual Atten causes an SDK-adjusted Ref, the requested value stays in `req` while top-level STATUS shows the effective value.

### Auto

Entering SDR restores the user's own SDR setup (tuning, capture bandwidth, demodulator, IF bandwidth,
de-emphasis, volume, squelch, AGC): it is persisted and re-applied, because a mode switch is not a
reset. The swept-centre hand-off belongs to the explicit gesture (Shift+click / a peak "listen
here"), which sets BOTH the capture centre and the listen frequency, and to a first run (nothing
stored yet), which also derives the demodulator/IF bandwidth from the band.

**Industry convention.** A bench analyser's reference "Auto" is a one-shot action, not a tracking
mode, in all three families this project is measured against: Keysight's *Auto Scale*, R&S's *Auto
Level* and Anritsu's *Auto Scale*. Their manuals differ in what else they touch (R&S also optimises
the RF attenuation; Keysight may also change scale/div), but the shared placement rule is the same
and is what this project implements: compute one reference level from the trace in front of the
user - with headroom for the peak - so that the whole trace fits the graticule, and let the noise
floor sit just above the bottom edge. What is continuously armed on such an analyser is overload
protection, not scaling; the same split is used here (rules 7-8 below).

The numbers below are this project's constants (`hardware/auto_reference.py`); no vendor figure is
quoted, because the vendors do not publish the same quantities.

1. `AUTO_SCALE` (or the legacy `SET_REF {mode:auto}`) asks for ONE placement from the newest trace; there is no tracking mode to latch, so `ref_mode` stays `manual`. Applying a target is one step of a **bounded closed loop** (rule 5): a press may therefore step the level two or three times before reporting `ok`, and the button glows while it does.
2. The anchor is the **NOISE FLOOR, not the peak**: the target puts the floor `FLOOR_ANCHOR_DB` (8 dB, i.e. ~1 division at the default 10 dB/div) above the bottom edge and lifts Ref only as far as the peak needs (>= 10 dB of headroom, ~30 dB when the noise floor is high). Quantised to the 5 dB Ref grid, the floor actually lands 3-8 dB above the bottom - as flush as a 5 dB step allows.
3. **There is no signal-level gate.** Anchoring on a peak needed a signal; anchoring on the floor does not. The old `peak - floor < 15 dB -> no_signal` refusal was a dead zone (table below): a noise-only trace a couple of dB under the bottom edge classified as `inside` and then refused to move, so Auto answered `no_signal`, held the level, and the trace stayed partly off the canvas (measured: SWP 20 MHz / 1 MHz, floor -101 dBm at Ref 0 with the 100 dB default window). `no_signal` stays in the STATUS vocabulary - the field's values are a contract - but the placement no longer produces it.
4. The fit applies nothing when the placement is already good: floor 2-12 dB above the bottom edge and >= 8 dB of headroom for the peak. The lower bound is one dB above the 5 dB grid's resolution, so anything lower is genuinely not reliably on the canvas and is fitted; the upper bound keeps a wobbling estimate from triggering a reconfiguration.
5. **A settings change arms a BOUNDED CLOSED-LOOP placement** (span, centre/start-stop, RBW, VBW, window function or SDR decimation - anything in `geometry()`), and so does a press. The loop is necessary because the trace is NOT independent of Ref: with the automatic attenuator the device re-picks attenuation as Ref moves, so the trace follows Ref by roughly half, in discrete jumps (measured curve below). One open-loop computation therefore lands short - measured: a fit that computed -20 dBm from a -115 dBm floor left the trace 14 dB under the canvas. Each settled frame re-measures and applies the next step until the placement is good (`ok`), the budget `REFIT_ATTEMPTS` (4) is used up, or a level the user typed appears; steps are rate-limited by `SAFETY_INTERVAL_S` (2 s). The *initial* settle of a mode arms nothing, so connecting or entering a mode never moves the level on its own, and the loop stops at the first good placement - it is not a tracking mode.
6. **A level the user typed is never reversed** - not by the per-frame logic and not by the settings-change loop (`user_level` is set by the manual `SET_REF` paths and cleared by an applied fit). Pressing Auto Scale places it. A dB/div change is display-only: the window height reaches the backend with the Auto Scale request, so it is fitted on the next press (unchanged).
7. A **safety ranger runs always**, independent of Atten and of whether Auto was ever pressed, but only in the PROTECTIVE direction: IF overflow (-12) raises Ref one 5 dB step per second, and a peak that is grossly clipped above the top edge (>= 10 dB) is raised once, with a 2 s rate limit. Lowering is never automatic - a level that pushes the noise floor below the bottom edge is a display choice (`below_window`), and the ranger leaves it to the user (press Auto to re-fit).
8. After a fit has lowered Ref, a Center change or cross-mode return raises it to 0 dBm before retuning (`prepare_retune`); the new geometry then gets its own bounded placement from rule 5.
9. Every SWP/RTA/SDR reconfiguration clears stale observations and pauses them for 0.75 seconds.
10. `auto_ref.last_peak/last_noise_floor/target/result/seq/pending/adjusting` exposes diagnostics;
   `adjusting` also drives the button's busy indication, `result` names the outcome
   (`applied`/`ok`/`no_signal`/`no_data`/`clipped`/`below_window`/`overflow`), and `seq` increments per decision
   so the UI can tell a new answer from the sticky remainder of the previous one. A re-fit that
   finds the placement already good reports nothing new: no level moved, so there is nothing to
   announce.
11. SDR runs the same fit, but its fitted value is a **DISPLAY** level: the client owns that scale and applies the reported target, the window goes to -160 dBm, and the IQS level is only written when it is more than 3 dB off. So the SDR target is clamped to the display range (not the device's -50..+30 dBm), while the IQS level written to the device is clamped to the device range - the same "validate against the owner" rule the command layer uses for `AUTO_SCALE.current_ref`.

A Ref change invalidates RTA density tied to the previous amplitude grid. SWP and RTA Auto states are independent.

### Where the old rules went wrong

Two separate mistakes produced one symptom ("small span, no external signal: the trace is not on
the canvas, or only a sliver under the bottom edge").

*First*, a dead zone in the *classification*:

| peak - floor | floor vs the bottom edge | old `_decide` | new `_decide` |
|---|---|---|---|
| >= 15 dB | inside the band | `ok` / fitted | unchanged |
| < 15 dB | inside the band | `no_signal`, level held | `ok` (nothing to change) |
| < 15 dB | 0-3 dB under the edge | `inside` then `no_signal`, level held (**the report**) | fitted: `applied`, target puts the floor 3-8 dB inside |
| any | > 3 dB under the edge | fitted (`below_window`) | unchanged |

The gate existed because the fit once anchored on the peak. Once the anchor is the floor, there is
always something to place, so a missing signal is a placement like any other.

*Second*, and only visible once the first was fixed: **the placement was open loop.** The trace is
not independent of Ref - the automatic attenuator re-picks with it - so the computed target is a
*prediction*, and it routinely lands short. Measured on the SAN-90 at centre 20 MHz / span 1 MHz
(auto Atten, 100 dB window, no signal):

| Ref | atten_actual | trace max | noise floor | floor vs the bottom edge |
|---|---|---|---|---|
| -10 dBm | 12 dB | -110.2 | -122.3 | **-12.3 dB** |
| -20 dBm | 15 dB | -121.9 | -133.9 | -13.9 dB |
| -30 dBm | 6 dB | -129.9 | -141.0 | -11.0 dB |
| -40 dBm | 0 dB | -129.8 | -142.7 | -2.7 dB |
| -50 dBm | 0 dB | -130.6 | -142.8 | **+7.2 dB (visible)** |

A 20 dB Ref change moved the trace ~10 dB, and the attenuation jumps (12 -> 15 -> 6 -> 0) make the
relationship non-monotonic. So a fit from the -115 dBm floor at Ref 0 computed -20 dBm, the floor
ended up 14 dB *under* the bottom edge, and because that one shot was consumed nothing retried -
the reported "Auto adjusted to -20 dBm and the trace is still invisible". The loop now re-measures
after every step, and on this device converges 0 -> -20 -> -40 -> -50 dBm in ~6 s.

### Verification of the three reported settings

No external signal; the default 100 dB window at Ref 0 dBm has its bottom edge at -100 dBm. On the
*fake* backend the trace is independent of Ref, so one step lands it:

| Mode / setting | floor observed | trace at Ref 0 | after the settings change |
|---|---|---|---|
| SWP centre 20 MHz / span 1 MHz | -101.3 dBm | floor 1.3 dB under the bottom edge | one re-fit -> Ref -5 dBm, floor 3.7 dB inside |
| RTA centre 20 MHz / 1.59 MHz bandwidth | -101.3 dBm | ditto | one re-fit -> Ref -5 dBm, inside |
| SDR centre 20 MHz (narrow capture) | -101.2 dBm | ditto | one re-fit -> Ref -5 dBm, inside (display scale) |

Evidence: `tests/test_auto_reference.py::test_the_reported_scenarios_through_the_fake_backend`
drives the fake device and its sessions with a noise-only trace through the real loop (frame ->
observation -> settings change -> placement -> apply); the same scenarios are checked at the
decision level (`test_the_reported_no_signal_scenario_ends_inside_the_window`). The e2e fake keeps
its synthetic carrier, so `state_regression` 9e covers the wiring instead: a settings change does
not undo a manual level, and Auto Scale then puts the whole trace inside the window.

On the **bench** (SAN-90, no source, `WEBSA` service + a browser; the publisher only steps the
device while a client is connected) the reported flow was reproduced end to end:

| Step | Ref | window | trace max | noise floor | on the canvas? |
|---|---|---|---|---|---|
| preset -> centre 20 MHz / span 1 MHz, first placement | -20 dBm | [-120, -20] | -122.4 | -133.7 | no (14 dB short) |
| second step | -40 dBm | [-140, -40] | -130.1 | -142.7 | no (2.7 dB short) |
| third step (`ok`) | -50 dBm | [-150, -50] | -130.5 | -142.5 | **yes** |

The loop stopped there (12 s of further watching moved neither `ref` nor `seq`), and a manual Ref
of 0 dBm - which pushes the trace off the canvas - was held for 8 s without being reversed; a
press of Auto Scale then converged the same way. Unit tests model the measured curve
(`test_the_refit_keeps_going_when_a_step_undershoots`) and the budget
(`test_the_refit_gives_up_after_its_budget`).


## 11. Marker Toggle and Tracking

- The Marker table's first column is an independent On/Off toggle. Enabling chooses the best unoccupied ranked peak; disabling retains position and Tracking state.
- Tracking runs on both SWP POWR and RTAF using the active Trace, including Hold/Average results.
- Multiple Tracking markers initially occupy distinct peaks by amplitude. Later frames prefer a nearby frequency-continuous peak to avoid jumping to a stronger distant spur.
- On SWP/RTA axis changes, markers are first relocated by `marker.freq`, then Tracking runs.

## 12. Hardware Validation Baseline

SAN-90 + TinySA Ultra+ ZS407 at 1 GHz / -25 dBm:

- SWP Auto Scale: a -18.5 dBm carrier, Ref parked 6 dB high -> one step to the fitted level in
  0.11-0.12 s (three trials); a second press reports `ok` and leaves `config_version` unchanged.
- RTA Auto Scale: same rule through the RTA profile (`session._configure`).
- SWP -> RTA: one configuration, default 50.78125 MHz / Auto RBW / Equal VBW / x4.
- RTA -> SWP: one configuration, with SWP Center/Span/RBW/VBW restored.
- Center/Span, Start/Stop, and RTA Center/Span each increment config version once per submission.


## 13. Trigger state (RTA device / SWP software)

Both implementations share one set of buttons but keep separate state machines:

| Mode | State | Display | Button |
|---|---|---|---|
| RTA | free | live | `Capture` |
| RTA | waiting (device waits for a crossing) | **cleared** (the device sends no packets at all) | `Stop` (highlighted) |
| RTA | hit | frozen on the captured frame, chip `TRIG hh:mm:ss` | `Capture again` |
| SWP | free | live | `Capture` |
| SWP | waiting (software waits for a crossing) | **stays live** | `Stop` (highlighted) |
| SWP | hit | frozen, chip `TRIG hh:mm:ss` plus the crossing frequency/level | `Capture again` |

- `Free Run` or `Esc` releases either one; a hit does not release itself, so the capture stays visible.
- Entering RTA resets the trigger source to `bus` (an armed trigger from an earlier session would come up
  with an empty plot); an Auto Scale press does not touch the trigger (see KNOWN_ISSUES 20).
- The chip only appears while armed or holding a capture; a free-running canvas shows no trigger text.

## 14. Average depth is per mode

SWP and RTA keep independent average depths (`avgTarget` / `avgTargetRta`, 2/4/.../256/inf) and never
overwrite each other on a mode switch. Finite N is exponential averaging (alpha = 2/(N+1), never freezes);
`inf` is the cumulative mean.
