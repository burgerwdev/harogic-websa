# Harogic SAN Series Web Spectrum Analyzer (SAN-45 / SAN-60 / SAN-90)

**中文文档: [README.zh-CN.md](README.zh-CN.md)**

A browser-based control and measurement application for **Harogic SAN series spectrum analyzers** (SAN-45 / SAN-60 / SAN-90, 海得逻捷), built on the official SDK (`htra_api.py` + `libhtraapi.so`, USB connection).

![Main UI (dark)](screenshots/main_dark.png)

## Hardware

| SAN-90 | SAN-90 + WebSA |
|---|---|
| ![san90](screenshots/harogic_san-90.jpg) | ![websa](screenshots/harogic_san-90_websa.jpg) |

## Features

- **Spectrum** — Clear Write / Max Hold / Min Hold / Average / View, four traces, smoothing,
  peak-preserving resampling
- **Controls** — atomic Center/Span and Start/Stop, span stepping and Full Span, RBW / VBW /
  points, FFT windows, attenuation / preamp / IF gain, manual or one-shot Auto Ref Level,
  reference clock and output
- **Markers** — four markers with independent tracking, ranked peak assignment, peak/valley
  navigation, Savitzky-Golay smoothing
- **Real-time spectrum** — FPGA engine, multiple traces, density background, waterfall
- **Measurements** — amplitude (n-dB bandwidth), harmonic (H1–H5), phase noise
- **Channel measurements** — channel power, occupied bandwidth (90/95/99%), ACPR
- **SDR receiver** — IQ streaming with the DSP in the browser: AM/DSB/USB/LSB/CW/NFM/WFM/PM
  demodulators, noise reduction (Wiener, optional DeepFilterNet3), audio chain, panadapter and
  waterfall
- **Digital modes** — FT8 decoding (multi-pass with OSD fallback), CW decoding, and DRM30 (SW broadcast) decoding
- **Trigger** — device-side RTA level trigger plus a swept-mode software trigger; limit lines with
  pass/fail margins and normalization; PNG/CSV export
- **UI** — jump rail, optional virtual keypad, amplitude units with external gain/loss
  compensation, dark/light theme, English/Chinese
- **Link monitor** — an unplug is reported on the canvas instead of a frozen trace, and reconnecting
  resumes the same mode without a service restart

## Screenshots

| Dark theme | Chinese UI | Light theme |
|---|---|---|
| ![main](screenshots/main_dark.png) | ![zh](screenshots/main_zh.png) | ![light](screenshots/main_light.png) |

| Multi-marker peaks | Harmonic measurement | Phase noise |
|---|---|---|
| ![peaks](screenshots/marker_peaks.png) | ![harm](screenshots/measure_harmonic.png) | ![pnm](screenshots/measure_phasenoise.png) |
| Amplitude measurement | RTA + waterfall | |
| ![amp](screenshots/measure_amplitude.png) | ![rta-wf](screenshots/waterfall_rta.png) | |
| FT8 decoding (SDR) | CW decoding (SDR) | |
| ![ft8](screenshots/sdr_ft8.png) | ![cw](screenshots/sdr_cw.png) | |

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
├─ frontend/             TS frontend (Vite + TypeScript, i18n + themes)
│  ├─ src/ui/panels/     panel modules (frequency/resolution/rta/markers/refAmp/...)
│  ├─ src/render/registry.ts  view renderer registry (views self-register)
│  └─ src/__tests__/     vitest tests (DSP engine, synthetic traces)
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

The day-to-day commands, plus what to run before committing:

| Script | Description |
|---|---|
| `./run.sh` / `./stop.sh` | Start / stop the service (supervisor + worker) |
| `make restart` / `make status` | Restart / show state, PID, memory, CPU, log, device link |
| `make build` | Full build: WASM cores (only when stale + Rust) + frontend |
| `make frontend` | Frontend only: npm install + Vite build (no WASM) |
| `make wasm` | Both WASM cores; `make wasm-dsp` / `make wasm-dfn` build one |
| `make clean` / `make clean-all` | Clean caches/logs/dist (keep deps + WASM caches) / full clean incl. node_modules + WASM target |
| `./test.sh` | Backend pytest + Ruff + frontend Vitest |
| `make ci` | Every in-process gate CI runs (no hardware needed) |
| `make e2e-fake` | Browser end-to-end on the fake backend (no hardware) |
| `make hw-test` | **Bench only** — tinySA CW smoke test + UI state regression |
| `make bench` | Compare against the recorded performance baseline |

The full list (fixture regeneration, guards, doc/version checks, the bench probes) is in
[`docs/en/DEVELOPMENT.md`](docs/en/DEVELOPMENT.md) §13.

## Tests

- **Backend** (pytest) and **frontend** (Vitest) suites cover the protocol, configuration, command
  validation, marker/DSP logic, parameter slots and i18n parity. Golden fixtures lock the binary
  frame codec and the DSP kernels against the Rust/WASM implementation.
- **No hardware needed by default**: without `libhtraapi.so` the vendor-dependent tests are skipped,
  so `make ci` runs everything else on any machine. `make e2e-fake` adds the browser end-to-end
  tests against a fake backend; `make hw-test` needs the analyzer (and a tinySA for the CW source).

## Open-Source Notes

- Project code (backend + frontend): **MIT** licensed
- `htra_api.py`: HAROGIC official SDK Python wrapper, copyright HAROGIC; bundled for convenience with your own SDK
- `libhtraapi.so`: proprietary binary, **not included** — obtain from HAROGIC
- Screenshots taken on a real SAN-90 (the SDR digital-mode shots use a PlutoSDR as the signal source)
- `frontend/src/dsp/ggmorse/` and `tools/ggmorse/`: the CW (Morse) decoder is
  [ggmorse](https://github.com/ggerganov/ggmorse) by Georgi Gerganov, **MIT** licensed, vendored at
  commit `7b4822a8` (see `tools/ggmorse/LICENSE`) and compiled to a single-file wasm module by
  `tools/build_ggmorse_wasm.sh`. `tools/ggmorse/ggmorse_wasm.cpp` is ours.


---

## Author

[好奇牛马 (Bilibili)](https://space.bilibili.com/28447213)
