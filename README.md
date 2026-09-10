# Harogic SAN Series Web Spectrum Analyzer (SAN-45 / SAN-60 / SAN-90)

**中文文档: [README.zh-CN.md](README.zh-CN.md)**

A browser-based control and measurement application for **Harogic SAN series spectrum analyzers** (SAN-45 / SAN-60 / SAN-90, 海得逻捷), built on the official SDK (`htra_api.py` + `libhtraapi.so`, USB connection).

![Main UI (dark)](screenshots/main_dark.png)

## Hardware

| SAN-90 | SAN-90 + WebSA |
|---|---|
| ![san90](screenshots/harogic_san-90.jpg) | ![websa](screenshots/harogic_san-90_websa.jpg) |

## Features

- **Spectrum display** — Clear Write / Max Hold / Min Hold / Average / View, 4 traces, frontend smoothing
- **Control panel** — atomic Center/Span and Start/Stop linking, custom/auto span step with `▼/▲` stepping and Full Span, mode-private SWP/RTA settings, RBW/VBW/points, FFT windows (FlatTop / B-Nuttall / LowSideLobe / Rectangle / Kaiser, matching official), attenuation / preamp / IF gain, Manual/Auto Ref Level, reference clock (Int / Ext / ExtForce + output)
- **Marker & DSP engine** — 4 markers, per-row On/Off toggles, independent Tracking toggles, ranked multi-marker assignment with continuous peak following, and peak/valley navigation

  - Savitzky-Golay smoothing (2nd order + gradient-adaptive)
  - 3-stage peak engine: local extrema → excursion (≥6 dB both sides) → parabolic sub-bin fit
  - Valley merging (25-bin per depression, consistent with Valley positioning), frequency-direction traversal
  - Raw Anchor (real extremum in raw trace when unsmoothed)
- **Real-time spectrum (RTA)** — FPGA engine, multi-trace (per-tab modes), probability-density background with fading traces, waterfall
- **GNSS detail popover** — click the indicator for full info (lock/sats/antenna/position/UTC time); states auto-refresh every 1 s
- **Measurements** — amplitude (n-dB bandwidth), harmonic (H1–H5 server-side auto-tune), phase noise (6 offsets 100 Hz–10 MHz)
- **Limit lines** - up to four breakpoints, dB tolerance, pass/fail with worst margin, violation CSV
- **Channel measurements** - channel power, occupied bandwidth (90/95/99%) and ACPR (upper/lower in dBc)
- **Exports** - PNG snapshot (acquisition header plus a bottom-right timestamp), peak-list CSV, trace CSV
- **Amplitude units and compensation** - dBm / dBmV / dBuV / dBV, with an external gain/loss offset that
  applies to the plot and to every readout
- **Trigger** - RTA device level trigger (threshold, edge, debounce, delay, pre-trigger, acquisition,
  re-trigger, trigger out, with POI and a status chip) and a swept-mode software level trigger (crossing
  between consecutive sweeps; live display while armed, frozen on a hit)
- **Jump rail** - one click to any control group, collapsible without consuming layout width
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
| Waterfall + RTA real-time spectrum |
| ![rta-wf](screenshots/waterfall_rta.png) |

## Quick Start

### Prerequisites

- Python ≥ 3.10 (aiohttp, NumPy; pyserial is optional for hardware smoke tests)
- Node.js ≥ 18 (only needed to rebuild the frontend)
- Harogic SAN series analyzer + official SDK:
  - `htra_api.py` — included in the repo root (official Python wrapper, HAROGIC copyright)
  - `libhtraapi.so` — **proprietary binary, obtain it from HAROGIC**; place it somewhere `ctypes` can find it

### 1. Install dependencies

```bash
pip install -r requirements.txt          # aiohttp, NumPy, pytest, pyserial
./build.sh                               # sync frontend dependencies and run the Vite build
```

### 2. Run

```bash
./run.sh
```

Open http://127.0.0.1:8080

The service listens on loopback by default. Remote control requires a token:

```bash
WEBSA_HOST=0.0.0.0 WEBSA_TOKEN='replace-with-a-long-random-token' ./run.sh
```

Then open `http://device-address:8080/?token=the-same-token`. See
[`docs/en/FAQ_NOTES.md`](docs/en/FAQ_NOTES.md) for all environment variables, remote deployment,
logging, and hardware tests. SWP/RTA parameter semantics are documented in
[`docs/en/MODE_STATE_FLOW.md`](docs/en/MODE_STATE_FLOW.md).

### 3. Test

```bash
./test.sh
python3 -m ruff check web_sa tests tools
cd frontend/modern && npm audit
```

## Directory Layout

```
harogic-websa/
├─ web_sa/               backend (supervisor + aiohttp worker, serialized device calls)
│  ├─ hardware/          sdk_bindings.py (only DLL contact point) / device.py
│  ├─ measurements/      Std/Harmonic/PhaseNoise sessions + framer (frame protocol)
│  └─ web/               ws.py / http_api.py / publisher.py
├─ frontend/
│  └─ modern/            TS frontend (Vite + TypeScript, i18n + themes)
│     └─ src/__tests__/  vitest tests (DSP engine, synthetic traces)
├─ htra_api.py           official SDK Python wrapper (HAROGIC copyright)
├─ docs/                 docs (en/ + zh-CN/): architecture / API / mode flow / known issues / FAQ / refactor log
├─ tests/                backend pytest (protocol, config, device state, HTTP API)
├─ screenshots/          README screenshots
├─ run.sh / stop.sh / clean.sh / build.sh / test.sh / Makefile
├─ pyproject.toml / requirements.txt / LICENSE / .gitignore
```

## Scripts

| Script | Description |
|---|---|
| `./run.sh` | Start supervisor + WebSA worker; restart after native SDK crash/fatal timeout |
| `./stop.sh` | Stop service |
| `./clean.sh` | Clean caches / logs / build artifacts |
| `./test.sh` | Backend pytest + Ruff + frontend Vitest; any failed stage returns non-zero |
| `make run/stop/clean/build/test` | Same via Makefile |

## Tests

- **Backend (63)**: protocol, configuration/security defaults, command validation, SWP/RTA state isolation, Auto Ref, RTA reference-clock and repeated-failure recovery, JSON sanitization, HTTP/WS authentication and path protection, bounded client streaming, acquisition watchdog, supervisor and TinySA safety rules — **normal tests require no hardware**
- **Frontend (79)**: synthetic-trace DSP, frequency unit commit, Span Step, SWP/RTA marker tracking, S-G smoothing, peak/valley detection, resampling, normalization and real-time percentile estimation — **no hardware required**

## Open-Source Notes

- Project code (backend + frontend): **MIT** licensed
- `htra_api.py`: HAROGIC official SDK Python wrapper, copyright HAROGIC; bundled for convenience with your own SDK
- `libhtraapi.so`: proprietary binary, **not included** — obtain from HAROGIC
- Screenshots captured with a real SAN-90 + TinySA sweep source


---

## Author

[好奇牛马 (Bilibili)](https://space.bilibili.com/28447213)
