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
- **Virtual keypad** - optional on-screen pad for the numeric fields (toggle in the top bar): unit keys
  for the frequency-like fields, plain-text entry (digits, commas, minus) for the n-dB threshold list,
  draggable, translucent, off by default
- **Normalization** — through-cal, adaptive absorption, display-layer transform
- **Device link monitor** — a run of bus errors on the swept path (an unplug) flips STATUS to
  `connected: false` (the canvas says DEVICE DISCONNECTED instead of showing a frozen trace), and the
  worker reopens the analyzer when it is plugged back in — no service restart. RTA/SDR recover through
  their own in-place reconfiguration

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

Unplugging the analyzer is detected and reported, and plugging it back in resumes the same mode
without a restart; `make status` shows the live link, and `make restart` restarts the service.

The service listens on loopback by default. Remote control requires a token:

```bash
WEBSA_HOST=0.0.0.0 WEBSA_TOKEN='replace-with-a-long-random-token' ./run.sh
```

Then open `http://device-address:8080/?token=the-same-token`. See
[`docs/en/FAQ_NOTES.md`](docs/en/FAQ_NOTES.md) for all environment variables, remote deployment,
logging, and hardware tests. SWP/RTA parameter semantics are documented in
[`docs/en/MODE_STATE_FLOW.md`](docs/en/MODE_STATE_FLOW.md). An independent architecture and code
review with prioritized refactor recommendations is in
[`docs/en/ARCH_REVIEW.md`](docs/en/ARCH_REVIEW.md); the development guide (layer map, state
ownership, feature checklists, testing/perf rules, guard rails, debugging playbook and a lesson
ledger) is [`docs/en/DEVELOPMENT.md`](docs/en/DEVELOPMENT.md).

### 3. Test

```bash
pip install -r requirements-dev.txt      # runtime + pytest/ruff/playwright/fonttools
make ci                                  # everything CI runs (no hardware needed)
python3 tools/bench.py --check tools/bench_baseline.json   # performance, service running
make hw-test                             # bench only: tinySA CW + UI state regression
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
│     ├─ src/ui/panels/  panel modules (frequency/resolution/rta/markers/refAmp/...)
│     └─ src/render/registry.ts  view renderer registry (views self-register)
│     └─ src/__tests__/  vitest tests (DSP engine, synthetic traces)
├─ htra_api.py           official SDK Python wrapper (HAROGIC copyright)
├─ docs/                 docs (en/ + zh-CN/): architecture / API / mode flow / known issues / FAQ /
│                        refactor log / arch review / development guide
├─ tests/                backend pytest (protocol, config, device state, HTTP API)
│  └─ fixtures/frames/   golden binary frames shared with the TS decoder test
├─ tools/                hardware smoke + e2e + bench + quality guards
│  └─ quality/           architecture_guard.py + baseline.json (fitness functions)
├─ screenshots/          README screenshots
├─ run.sh / stop.sh / status.sh / clean.sh / build.sh / test.sh / Makefile
├─ pyproject.toml / requirements.txt / LICENSE / .gitignore
```

## Scripts

| Script | Description |
|---|---|
| `./run.sh` | Start supervisor + WebSA worker; restart after native SDK crash/fatal timeout |
| `./stop.sh` | Stop service |
| `make restart` | Restart service (stop + start) |
| `make status` | Service status: state, PID/uptime, memory + CPU, log path/size, live device link |
| `./clean.sh` | Clean caches / logs / build artifacts (`--keep-deps` keeps `node_modules`) |
| `./test.sh` | Backend pytest + Ruff + frontend Vitest; any failed stage returns non-zero |
| `make run/stop/restart/status/clean/build/test` | Same via Makefile |
| `make dev` | Stop, clean (deps kept), rebuild the frontend, start with `WEBSA_TRACE=1` |
| `make ci` | Everything CI runs locally: test.sh + version check + frame fixtures + architecture guard + build |
| `make hw-test` | **Bench only** — tinySA CW through the SWP/RTA smoke test, then the Playwright UI state regression |
| `make bench` / `bench-record` | Compare against / re-record `tools/bench_baseline.json` |
| `python3 tools/sync_version.py --check` | Version drift check (pyproject → package.json + index.html) |
| `python3 tools/check_dom_ids.py` | DOM id contract: every id read from the TS sources exists in index.html |
| `python3 tools/check_docs_parity.py` | The en/ and zh-CN/ documents keep the same section structure |
| `GET /api/schema` | Machine-readable command/parameter schema (generated from the command table) |
| `python3 tools/command_sweep.py` | **Bench only** — executes every command in the table plus the guard rejections |
| `python3 tools/quality/architecture_guard.py` | Architecture fitness functions (cycles, god functions, mode branching, DLL access, limit literals) |

## Tests

- **Backend (147)**: protocol + golden frame fixtures, configuration/security defaults, command validation (including capability-driven limits), SWP/RTA state isolation, Auto Ref, RTA reference-clock and repeated-failure recovery, JSON sanitization, fatal-exit contract, HTTP/WS authentication and path protection, bounded client streaming, acquisition policy (session-owned watchdog/pacing), supervisor and TinySA safety rules
- **Frontend (141)**: synthetic-trace DSP, frequency unit commit, Span Step, SWP/RTA marker tracking, S-G smoothing, peak/valley detection, resampling, normalization, parameter slots, i18n key parity, the binary frame decoder against the Python-generated fixtures, and the STATUS → parameter-slot mapping
- **Hardware-free by default**: without `/opt/htraapi/lib/x86_64/libhtraapi.so` the seven vendor-dependent backend modules are skipped (`tests/conftest.py`), so CI and a plain checkout can run everything else. `make hw-test` and `tools/hardware_smoke.py` need the analyzer (and the tinySA for the CW source).

## Open-Source Notes

- Project code (backend + frontend): **MIT** licensed
- `htra_api.py`: HAROGIC official SDK Python wrapper, copyright HAROGIC; bundled for convenience with your own SDK
- `libhtraapi.so`: proprietary binary, **not included** — obtain from HAROGIC
- Screenshots captured with a real SAN-90 + TinySA sweep source


---

## Author

[好奇牛马 (Bilibili)](https://space.bilibili.com/28447213)
