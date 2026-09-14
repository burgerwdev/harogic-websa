# Architecture

## Overview
```
Browser (frontend/modern: TS + Vite)
   │  bounded latest-wins binary frames + WS JSON
   ▼
Supervisor → web_sa worker (aiohttp + serialized SDK access; restart on native crash/timeout)
   │
   ▼
htra_api.py → libhtraapi.so → USB → SAN series analyzer
```

## Backend Layers
- `hardware/sdk_bindings.py`: all ctypes bindings (only DLL contact point), incl. PNM structs & calibration functions
- `hardware/device.py`: device abstraction (open/configure/fetch/query) + DeviceState + model capability derivation
- `measurements/`: measurement session objects (Std/RTA/Harmonic/PhaseNoise) + framer (frame encode/decode)
- `web/`: ws.py (command table) + http_api.py (STATUS/REST) + publisher.py (mode-aware scheduling)

## Key Design
1. **Session objects**: std/rta/harmonic/pnm share one interface; enter/exit restores configuration snapshots
2. **Model capability derivation**: DeviceCapabilities (SAN-45/60/90 frequency ranges), no hardcoding
3. **Frontend peak-preserving resampling**: backend sends device-native traces; frontend `resampleTrace` handles points (peak-preserving, no interpolation artifacts)
4. **Frame protocol**: 16-byte header (magic+ver+points+sweep_ms) + data; POWR forced to float32
5. **Client backpressure**: one sender per WS; retain FREQ/JSON and use latest-wins for POWR/RTAF
6. **Secure default**: loopback listener, token required remotely, static files confined to the build root
7. **Failure recovery**: SDK calls leave the asyncio thread; supervisor restarts after native crash/fatal timeout. The swept acquisition also declares `connected=false` on a run of bus errors (unplug) so the page never shows a frozen trace as connected; RTA/SDR keep their own in-place reconfiguration recovery. A worker link loop reopens a disconnected device and re-enters the active mode
8. **Mode-private settings**: SWP/RTA independently retain Center/Span/Ref/RBW/VBW/Sweep and actual values; mode changes issue one SET_MODE command. See [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md).

## Frame Protocol
| Type | Header | Data |
|---|---|---|
| FREQ | FREQ + ver(4) + points(4) + sweep_ms(f4) | float64 frequency axis |
| POWR | POWR + ... | float32 power dBm |

## WS Commands
CONNECT/STATUS/SET_PRESET/CAL_REFCLK/SET_FREQ/SET_REF/SET_RBW/SET_VBW/SET_SWEEP/
SET_POINTS/SET_SPUR/SET_WINDOW/SET_AMP/SET_REFCK/SET_REFCKOUT/SET_MODE/SET_RTA/SET_HARM/SET_PNM

## RTA Real-Time Spectrum (SWP/RTA modes)
- **Session** (`web_sa/measurements/rta.py`, `RtaSession`): built on the official SDK path
  `RTA_Configuration` → `BusTriggerStart` → `GetRealTimeSpectrum`; `acq=0.005`.
  Sustained SAN-90 push rate is about 105-129 fps, with a typical mode-to-first-frame delay near 0.57 s.
- **Push and display**: backend acquisition follows device Get rate; per-client latest-wins fan-out isolates slow
  connections; frontend RTA processing is throttled to about 33 Hz and SWP rendering to about 30 Hz.
- **RTAF frame** (`measurements/rta.py`): magic `RTAF` + `freq` (f8) + `spec` (f4) + `wfRow` (u2)
  + `stopHz`; each dtype is packed separately. The current trace uses the first spectrum in PacketFrame.
- **Multi-trace (T1-T4)**: per-trace accumulators (`rtaDisplays[4]`) with their own modes, rendered
  overlaid (own color + glow); **Freeze/View** is a standalone toggle button next to the trace-mode
  dropdown (VIEW = freeze accumulation; unfreeze restores the previous mode); Clear resets the active trace
- **Probability-density background**: default `frequency × 128` amplitude bins (64/96/128/192
  selectable), accumulated along the signal path. Persistence and grain are configurable; an O(n)
  histogram estimates noise floor and updates at about 33 Hz.
- **Waterfall**: SWP derives rows from the current trace at about 10 rows/s; RTA derives rows from live
  spec. The container replaces the Marker table slot and displays newest rows at the top.
- **Recovery**: after eight consecutive Trigger/Get failures, RTA reconfigures in place from retained
  mode-private settings up to two times. Persistent failure raises a fatal hardware error and the supervisor
  restarts the worker. STATUS exposes `rta_health`.
- **Known quirk**: `renderRta` wraps density+traces in an outer `save/clip(plotRect)` that must be
  `restore`d before drawing the bottom frequency row — otherwise the row (outside the plot) is clipped
  away and disappears after switching to RTA (fixed)

## Reachability of registration points (import side effects)

Some modules register themselves when imported (`render/spectrum.ts` registers the renderer,
the measurement modules register their tabs, the i18n namespaces merge). **After breaking the
cycles, "nothing imports it any more" is itself a failure**: the registration never runs and
the feature dies silently (this happened: no renderer -> `requestRender()` was a no-op -> a
blank canvas, with no exception and no console error).

Therefore:

- the entry point `main.ts` must pull those modules in with a **side-effect import**
  (`import './render/spectrum';`);
- `tools/check_registrations.py` computes import reachability from `main.ts`; a module that
  registers but is unreachable fails `make ci`;
- tests must assert the **user-visible result** (canvas pixels, DOM text), not an upstream
  counter or dataset: `dataset.rtaFrames` only says a frame was handed to the renderer, not
  that anything was drawn. The e2e now counts non-transparent canvas pixels after load and
  after switching to RTA.

## State ownership (parameters use slots, results use a store)

Frontend state falls into two kinds, and putting one in the wrong place is a bug class this
project hit repeatedly:

- **Parameters** (user-set, backend-confirmed): use the slots in `core/params.ts`; each
  parameter has exactly one owner. Only the STATUS handler calls `confirm()`, only a user
  action calls `set()`, readers use `get()`. `desired`/`confirmed`/`epoch` plus a TTL mean a
  just-set value is never reverted by an in-flight reply and a rejected command expires
  instead of sticking. The groups live in `ui/{freqState,refState,swpState,sdrState,triggerState,waterfallState,displayState,graphMode,displayRef}.ts`.
  A client-owned preference the backend never reports (display unit/offset, waterfall range,
  audio switch, ...) must be declared `authoritative: true` or it reverts when the TTL ends.
- **Results** (data produced by measurements/display): use `core/results.ts` - plain data with
  an explicit setter and no desired/confirmed semantics.

`core/model.ts` holds the types both sides share (a leaf module). `core/store.ts` keeps the
runtime state (connection, trigger runtime, current mode, ...) and re-exports the two groups,
so existing `S.x` / `S.setX()` call sites keep working.

In a hot path (loops running tens of thousands of times per frame) do not call a slot `get()`
inside the loop body: it performs a `Date.now()` plus pending checks, and the density
accumulation calling it hundreds of thousands of times per frame saturated the main thread
(1 Hz STATUS fell behind and the mode buttons toggled from a stale value).

## Frontend DSP Engine (marker peak/valley)
Implemented after modern analyzer architecture (Keysight/R&S style), all in the TS frontend:
- **S-G smoothing** `sgSmooth(src,w,adaptive)`: 2nd-order Savitzky-Golay + gradient-adaptive
  (|dY/df| > 90th percentile → shrink window to 3 to preserve edges); active when smoothBins > 1 (MAX/MIN/MEDIAN keep original logic)
- **3-stage peak engine** `findExtremesOrdered(dir,isPeak)`:
  1. Local extrema scan (±1 bin)
  2. Excursion both-side tracing ≥6 dB (up to 100 bins) to filter noise ripples
  3. Parabolic sub-bin fit `parabolaFit` (Δk = -0.5(y2-y0)/denom → sub-bin frequency/amplitude)
- **Raw Anchor** (unsmoothed): smooth locate → real extremum in raw trace neighborhood (deep notches not shallowed)
- **Dedup**: peaks 3 bins (narrow peaks kept); valleys 25 bins (merge same depression, rebuilt as depression-wide disp minimum; valley list matches Valley positioning)
- **Traversal semantics**: peaks/valleys both frequency-direction (left=lower, right=higher); pos match takes nearest list item (±3 bins); if not in list (e.g. Valley global minimum is not a local extremum) jump to nearest independent valley, skipping same-depression
- **Valley**: locates global minimum of display data + parabolic sub-bin
- **Marker Tracking**: per-row On/Off toggles; initial assignment uses ranked unoccupied peaks, while later
  updates follow a nearby `marker.freq`. The same strategy runs in SWP and RTA, with relocation after axis changes.
- **smooth data source**: when enabled, fully based on smoothed curve (position/amplitude smoothed, matches display); when disabled, raw + Raw Anchor
- **Pk threshold**: value lives in a slot (`meas` group); readers no longer parse the input element. Auto = **one decision per measurement geometry**: fitted (median of a few frames of the trace's 99.5th percentile, −50 dB) when span/RBW/Ref/dB-per-div/points/centre change, latched otherwise so a signal swing cannot move it or reshuffle the peak table/marker search; a manual edit locks it until Auto is pressed again; no update when all markers are off

## Display and Control Design
- **Reference Level**: Manual configures the active SWP/RTA Profile. **Auto Scale is a one-shot action**
  (`AUTO_SCALE`), not a tracking mode: it computes one target from the newest trace (noise floor just
  above the bottom of the display window, >=10 dB of headroom for the peak, 5 dB quantisation, never
  below a learned IF-saturation floor) and applies it once; an already-good placement costs nothing.
  A **safety ranger runs always**, independently of any user setting, but only in the protective
  direction: IF overflow (-12) raises Ref one 5 dB step per second, and a peak grossly clipped above the
  top edge (>= 10 dB over) is raised once, rate-limited. Lowering is never automatic: a level that pushes
  the noise floor below the bottom edge is a display choice and is left alone (press Auto to re-fit),
  because undoing it would undo the button the user just pressed (`clipped` vs `below_window`). The 15 dB peak-above-noise test only
  applies *inside* the window, so a weak signal is reported as `no_signal` instead of silently doing
  nothing. Before Center/cross-mode retuning a Ref that a fit had lowered is raised to 0 dBm. Each
  reconfiguration waits 0.75 s before observations are trusted again.
  **SDR runs the same rule** (own tracker, fed by the panadapter frames): the backend fits, the client
  applies the reported target to its display scale, and the IQS level is only written when it is more
  than 3 dB off (that write interrupts the audio). The command carries the level on screen
  (`current_ref`), because in SDR the device level is not what the user sees. A manual Ref therefore
  holds: raising it is never undone, and only a grossly clipped peak or an IF overflow is corrected on the
  device's own initiative. When a correction does happen, the display follows it (the SDR client applies
  the reported target to its scale).
- **Periodic STATUS push (1 s)**: the publisher pushes full STATUS every second (aligned with the GNSS poll),
  keeping GNSS lock/time, refclk_out, calibration state fresh without a page refresh
- **GNSS detail popover**: click the GNSS indicator for full info (lock/sats/docxo/antenna/lat/lon/alt/UTC time)


## Trigger architecture (RTA device / SWP software)

- **RTA (device)**: the trigger is written into `RTA_Profile_TypeDef` (source/edge/mode, threshold,
  debounce, delay, pre-trigger, acquisition, re-trigger, trigger out) by `web_sa/measurements/rta.py` on
  every configuration. The fetch loop (`RTA_BusTriggerStart` + `RTA_GetRealTimeSpectrum`) yields no
  packets while armed and delivers one frame on a hit. `ui/trigger.ts` owns the panel, the canvas status
  chip (`FREE/WAIT/TRIG`) and the threshold line; `ui/triggerEvents.ts` decides in the **frame path** that
  an arriving packet *is* the capture (with a 500 ms settle window so the in-flight frames sent right
  after arming are not mistaken for it).
- **SWP (frontend)**: the swept engine has no level trigger. `dsp/levelCross.ts` (pure, unit tested)
  compares the same bin across two consecutive sweeps to produce a crossing event; `ui/swpTrigger.ts`
  arms and evaluates every trace; on a hit the frame path stops updating the canvas, which is what
  freezes the picture. The decision never depends on the display scale, so changing Ref cannot break it.
- One set of buttons (`Capture` / `Free Run` / `Esc`) dispatches to either implementation by mode.

- **The canvas status area is shared**: the top-right indicators are drawn by
  `render/statusStack.ts` - each feature pushes a status block during a pass (trigger chip, limit
  verdict) and the blocks are stacked, right-aligned, so a new indicator can never overlap an
  existing one.
- **Limits work in both modes**: the evaluation core (`dsp/limits.ts`) is display independent, and the
  RTA branch now calls the same `renderLimits()` as the swept path. The canvas reports `LIMIT PASS` or
  `LIMIT FAIL n` plus `worst +x dB @ f`, matching the pass/fail row in the panel.


## Virtual keypad (v1.3.4)

An optional on-screen pad for the numeric fields, disabled by default and remembered in
`localStorage['websa-keypad']`. `ui/keypad.ts` builds it once and fills it per field:

- a field with a unit button group (`UNIT_OPTIONS` in `core/units.ts`) also gets unit keys; a unit
  key writes the value with `dataset.edited = '1'` and calls the panel's own `setUnit()`, so the
  command is identical to clicking the panel button
- a field marked `data-keypad="text"` (the n-dB threshold list) is edited as plain text: its display
  line is a real input, so the mouse places the caret, backspace deletes one character there and
  `C` clears the entry; its `.` key becomes the `,` separator
- every other field commits through a `change` event, except the staged ones (`ref`, `points`,
  `rbw`, `vbw`, `harm`, `pnm`, `ampdbs`) whose own Set/Meas button is clicked instead
