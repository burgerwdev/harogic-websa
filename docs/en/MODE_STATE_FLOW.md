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

1. `SET_REF {mode:auto}` enables mode-private Auto Ref.
2. A signal is recognized only when its peak is at least 10 dB above the estimated noise floor; without a signal, current Ref is held.
3. After detection, the backend targets about 5 dB above the peak and quantizes to 5 dB steps.
4. Every SWP/RTA reconfiguration clears stale candidates and pauses observation for 0.75 seconds.
5. Range is limited to -50 through +30 dBm.
6. Raising Ref requires about 150 ms of stability; lowering requires 1.5 seconds.
7. Adjustments are at least one second apart.
8. Auto remains selected but is suspended under manual Atten and resumes with Atten Auto.
9. `auto_ref.last_peak/last_noise_floor/candidate/pending` exposes diagnostics.

A Ref change invalidates RTA density tied to the previous amplitude grid. SWP and RTA Auto states are independent.

## 11. Marker Toggle and Tracking

- The Marker table's first column is an independent On/Off toggle. Enabling chooses the best unoccupied ranked peak; disabling retains position and Tracking state.
- Tracking runs on both SWP POWR and RTAF using the active Trace, including Hold/Average results.
- Multiple Tracking markers initially occupy distinct peaks by amplitude. Later frames prefer a nearby frequency-continuous peak to avoid jumping to a stronger distant spur.
- On SWP/RTA axis changes, markers are first relocated by `marker.freq`, then Tracking runs.

## 12. Hardware Validation Baseline

SAN-90 + TinySA Ultra+ ZS407 at 1 GHz / -25 dBm:

- SWP Auto Ref: peak near -26.8 dBm, Ref converges from 0 to -20 dBm.
- RTA Auto Ref: peak near -29.67 dBm, Ref converges from 0 to -20 dBm.
- SWP -> RTA: one configuration, default 50.78125 MHz / Auto RBW / Equal VBW / x4.
- RTA -> SWP: one configuration, with SWP Center/Span/RBW/VBW restored.
- Center/Span, Start/Stop, and RTA Center/Span each increment config version once per submission.
