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
- `measurements/`: measurement session objects (Std/Harmonic/PhaseNoise) + framer (frame encode/decode)
- `web/`: ws.py (command table) + http_api.py (STATUS/REST) + publisher.py (mode-aware scheduling)

## Key Design
1. **Session objects**: std/harmonic/pnm share one interface; enter/exit restores configuration snapshots
2. **Model capability derivation**: DeviceCapabilities (SAN-45/60/90 frequency ranges), no hardcoding
3. **Frontend peak-preserving resampling**: backend sends device-native traces; frontend `resampleTrace` handles points (peak-preserving, no interpolation artifacts)
4. **Frame protocol**: 16-byte header (magic+ver+points+sweep_ms) + data; POWR forced to float32
5. **Client backpressure**: one sender per WS; retain FREQ/JSON and use latest-wins for POWR/RTAF
6. **Secure default**: loopback listener, token required remotely, static files confined to the build root
7. **Failure recovery**: SDK calls leave the asyncio thread; supervisor restarts after native crash/fatal timeout
8. **Mode-private settings**: SWP/RTA independently retain Center/Span/Ref/RBW/VBW/Sweep and actual values; mode changes issue one SET_MODE command

## Frame Protocol
| Type | Header | Data |
|---|---|---|
| FREQ | FREQ + ver(4) + points(4) + sweep_ms(f4) | float64 frequency axis |
| POWR | POWR + ... | float32 power dBm |

## WS Commands
CONNECT/STATUS/SET_FREQ/SET_REF/SET_RBW/SET_VBW/SET_POINTS/SET_SPUR/SET_WINDOW/
SET_AMP/SET_REFCK/SET_REFCKOUT/SET_MODE/SET_HARM/SET_PNM

## RTA Real-Time Spectrum (SWP/RTA modes)
- **Session** (`web_sa/measurements/rta.py`, `RtaSession`): built on the official SDK path
  `RTA_Configuration` → `BusTriggerStart` → `GetRealTimeSpectrum`; official packet-drain loop
  (after trigger, `Get` repeatedly for `PacketCount` packets), switching latency ~1.5 s, `acq=0.005`
  (~150 fps device capability)
- **Push rate**: backend step waits only `acq+0.002` (~7 ms) → ~130 fps; frontend redraw throttle
  16 ms → ~60 fps display (SWP mode keeps 33 ms)
- **RTAF frame** (`framer/`): magic `RTAF` + `freq` (f8) + `spec` (f4) + `wfRow` (u2) + `stopHz`;
  each dtype is `tobytes`-packed separately (a single `np.concatenate` would upcast to f8 and corrupt
  the frontend parse); wfRow comes before density byte for 2-byte alignment; first frame drops the
  19-frame mirror image
- **Multi-trace (T1-T4)**: per-trace accumulators (`rtaDisplays[4]`) with their own modes, rendered
  overlaid (own color + glow); **Freeze/View** is a standalone toggle button next to the trace-mode
  dropdown (VIEW = freeze accumulation; unfreeze restores the previous mode); Clear resets the active trace
- **Probability-density background**: 2D accumulator `rtaDensity2d` (freq × 50 amp bins), 1 px dots
  drawn along the trace path with `fillRect` (source-over, grid stays visible); signal gating
  (noise floor +15 dB), peak ±1 bin (center +3 / flanks +1.5); decay ×0.97, new point +3,
  density display threshold 0.01 (keeps the low-power signal bottom visible), min brightness 90,
  full brightness at dens/8; offscreen persistence was rolled back → fluorescent glow trace + density dots
- **Waterfall**: SWP mode throttles POWR rows to 10 rows/s; RTA pushes `wfRow` from the backend;
  the container replaces the marker-table slot (same 135 px height → spectrum canvas does not jump);
  top-down growth (newest at top); RTA waterfall uses the spec row (bitmap rows are all-zero/unreliable)
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
- **smooth data source**: when enabled, fully based on smoothed curve (position/amplitude smoothed, matches display); when disabled, raw + Raw Anchor
- **Pk threshold**: auto = global peak −50 dB when unset; user edit locks (activeElement guard + oninput); Auto restores; no update when all markers off

## Display-Layer Design
- **ref level is display-only**: the device already returns port-referenced power (atten-compensated), so the
  frontend treats ref level purely as the Y-axis top (displayRef); it is never sent to the device —
  this avoids the device's ref-atten coupling (manual atten forces ref=atten-10) and Auto-atten
  re-configuration stalls on broadband sources
- **Periodic STATUS push (1 s)**: the publisher pushes full STATUS every second (aligned with the GNSS poll),
  keeping GNSS lock/time, refclk_out, calibration state fresh without a page refresh
- **GNSS detail popover**: click the GNSS indicator for full info (lock/sats/docxo/antenna/lat/lon/alt/UTC time)
