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
7. **Failure recovery**: SDK calls leave the asyncio thread; supervisor restarts after native crash/fatal timeout
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
- **Pk threshold**: auto = global peak −50 dB when unset; user edit locks (activeElement guard + oninput); Auto restores; no update when all markers off

## Display and Control Design
- **Reference Level**: Manual configures the active SWP/RTA Profile. Auto adjusts only when the peak is
  at least 15 dB above noise. Before Center/cross-mode retuning it temporarily raises a negative Ref to
  0 dBm, then uses 5 dB headroom, settling time, and hysteresis. Reconfiguration waits 0.75 seconds;
  Auto is suspended while Atten is manual.
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
