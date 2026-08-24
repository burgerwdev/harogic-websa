# Architecture

## Overview
```
Browser (frontend/modern: TS + Vite)
   │  WS binary frames (FREQ/POWR) + WS JSON (STATUS/commands/HARM/PNM)
   ▼
web_sa/ (Python package, single-process aiohttp, serialized device calls)
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

## Frame Protocol
| Type | Header | Data |
|---|---|---|
| FREQ | FREQ + ver(4) + points(4) + sweep_ms(f4) | float64 frequency axis |
| POWR | POWR + ... | float32 power dBm |

## WS Commands
CONNECT/STATUS/SET_FREQ/SET_REF/SET_RBW/SET_VBW/SET_POINTS/SET_SPUR/SET_WINDOW/
SET_AMP/SET_REFCK/SET_REFCKOUT/SET_MODE/SET_HARM/SET_PNM

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
