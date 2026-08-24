# Harogic SAN Series Web Spectrum Analyzer (SAN-45 / SAN-60 / SAN-90)

A browser-based control and measurement application for **Harogic SAN series spectrum analyzers** (SAN-45 / SAN-60 / SAN-90, 海得逻捷), built on the official SDK (`htra_api.py` + `libhtraapi.so`, USB connection).

![Main UI (dark)](screenshots/main_dark.png)

## Features

- **Spectrum display** — Clear Write / Max Hold / Min Hold / Average / View, 4 traces, frontend smoothing
- **Control panel** — center/span, RBW/VBW/points, FFT windows (FlatTop / B-Nuttall / LowSideLobe / Rectangle / Kaiser, matching official), attenuation / preamp / IF gain, reference clock (Int / Ext / ExtForce + output)
- **Marker & DSP engine**
  - Savitzky-Golay smoothing (2nd order + gradient-adaptive)
  - 3-stage peak engine: local extrema → excursion (≥6 dB both sides) → parabolic sub-bin fit
  - Valley merging (25-bin per depression, consistent with Valley positioning), frequency-direction traversal
  - Raw Anchor (real extremum in raw trace when unsmoothed)
- **Measurements** — amplitude (n-dB bandwidth), harmonic (H1–H5 server-side auto-tune), phase noise (6 offsets 100 Hz–10 MHz)
- **Normalization** — through-cal, adaptive absorption, display-layer transform

## Screenshots

| Dark theme | Chinese UI | Light theme |
|---|---|---|
| ![main](screenshots/main_dark.png) | ![zh](screenshots/main_zh.png) | ![light](screenshots/main_light.png) |

| Multi-marker peaks (MAX_HOLD + smooth) | Harmonic measurement | Phase noise |
|---|---|---|
| ![peaks](screenshots/marker_peaks.png) | ![harm](screenshots/measure_harmonic.png) | ![pnm](screenshots/measure_phasenoise.png) |
| Valley detection (DUT sweep, MAX_HOLD + smooth=5) | Amplitude measurement (N dB) | — |
| ![valley](screenshots/marker_valley.png) | ![amp](screenshots/measure_amplitude.png) | |

## Quick Start

### Prerequisites

- Python ≥ 3.10 (aiohttp)
- Node.js ≥ 18 (only needed to rebuild the frontend)
- Harogic SAN series analyzer + official SDK:
  - `htra_api.py` — included in the repo root (official Python wrapper, HAROGIC copyright)
  - `libhtraapi.so` — **proprietary binary, obtain it from HAROGIC**; place it somewhere `ctypes` can find it

### 1. Install dependencies

```bash
pip install -r requirements.txt          # aiohttp, pytest
cd frontend/modern && npm install && npm run build   # frontend build (dist included)
```

### 2. Run

```bash
./run.sh
```

Open http://localhost:8080

### 3. Test

```bash
./test.sh      # backend pytest + frontend vitest
```

## Directory Layout

```
harogic-websa/
├─ web_sa/               backend (aiohttp, single process, serialized device calls)
│  ├─ hardware/          sdk_bindings.py (only DLL contact point) / device.py
│  ├─ measurements/      Std/Harmonic/PhaseNoise sessions + framer (frame protocol)
│  └─ web/               ws.py / http_api.py / publisher.py
├─ frontend/
│  └─ modern/            TS frontend (Vite + TypeScript, i18n + themes)
│     └─ src/__tests__/  vitest tests (DSP engine, synthetic traces)
├─ htra_api.py           official SDK Python wrapper (HAROGIC copyright)
├─ docs/                 docs (en/ + zh-CN/): architecture / API / refactor log / known issues / FAQ
├─ tests/                backend pytest (protocol, config, device state, HTTP API)
├─ screenshots/          README screenshots
├─ run.sh / stop.sh / clean.sh / build.sh / test.sh / Makefile
├─ pyproject.toml / requirements.txt / LICENSE / .gitignore
```

## Scripts

| Script | Description |
|---|---|
| `./run.sh` | Start service (single-shot, no retry loop) |
| `./stop.sh` | Stop service |
| `./clean.sh` | Clean caches / logs / build artifacts |
| `./test.sh` | Backend pytest + frontend vitest |
| `make run/stop/clean/build/test` | Same via Makefile |

## Tests

- **Backend (14)**: frame protocol encode/decode + float32 dtype guard, config constants & SAN model capability derivation, `DeviceState` serialization & `build_status` payload, HTTP API endpoints (`/api/state`, `/api/config`) via aiohttp TestClient with a stubbed device — **no hardware required**
- **Frontend (13)**: DSP engine with synthetic traces — S-G smoothing (peak/edge preservation), parabola sub-bin fit, excursion filter, peak/valley detection & 25-bin depression merging, peak-preserving resample, normalization reference building — **no hardware required**

## Open-Source Notes

- Project code (backend + frontend): **MIT** licensed
- `htra_api.py`: HAROGIC official SDK Python wrapper, copyright HAROGIC; bundled for convenience with your own SDK
- `libhtraapi.so`: proprietary binary, **not included** — obtain from HAROGIC
- Screenshots captured with a real SAN-90 + TinySA sweep source


---

## Author

[好奇牛马 (Bilibili)](https://space.bilibili.com/28447213)
