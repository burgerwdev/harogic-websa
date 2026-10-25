# Harogic SAN Series Web Spectrum Analyzer (SAN-45 / SAN-60 / SAN-90)

**中文文档: [README.zh-CN.md](README.zh-CN.md)**

A browser-based control and measurement application for **Harogic SAN series spectrum analyzers** (SAN-45 / SAN-60 / SAN-90, 海得逻捷), using the native Rust service and a C++ shim for the official `libhtraapi.so` SDK over USB.

Release **1.9.2** uses the native Rust PFB/DDC acquisition and receiver path. Python is reserved
for offline tools and tests; there is no Python production backend. Retained browser WASM
artifacts are optional offline diagnostics, not the production DSP path.

![Main UI (dark)](screenshots/main_dark.png)

## Hardware

| SAN-90 | SAN-90 + WebSA |
|---|---|
| ![san90](screenshots/harogic_san-90.jpg) | ![websa](screenshots/harogic_san-90_websa.jpg) |

## Features

- **Narrow SDR capture** — acquisition can cover a wider band while the panadapter shows the requested span; the channelizer chooses a suitable receiver rate.
- **Spectrum** — Clear Write / Max Hold / Min Hold / Average / View, four traces, smoothing,
  peak-preserving resampling
- **Display zoom** — enlarge the captured spectrum on screen. No retune, no new sweep. An
  overview shows the full window. The waterfall follows the same view.
- **SDR tuning step** — set the arrow-key step per capture bandwidth. Quick steps run from
  1 Hz to 100 MHz, and To Center puts the signal at the centre.
- **Controls** — atomic Center/Span and Start/Stop, span stepping and Full Span, RBW / VBW /
  points, FFT windows, attenuation / preamp / IF gain, manual or one-shot Auto Ref Level,
  reference clock and output
- **Markers** — four markers with independent tracking, ranked peak assignment, peak/valley
  navigation, Savitzky-Golay smoothing
- **Real-time spectrum** — FPGA engine, multiple traces, density background, waterfall
- **Measurements** — amplitude (n-dB bandwidth), harmonic (H1–H5), phase noise
- **Channel measurements** — channel power, occupied bandwidth (90/95/99%), ACPR
- **SDR receiver** — IQ streaming with native Rust DSP: AM/DSB/USB/LSB/CW/NFM/WFM/PM
  demodulators, noise reduction (Wiener, optional DeepFilterNet3), audio chain, panadapter and
  waterfall; the browser handles control, playback and display
- **SDR filter** — per-mode presets (compact k/M labels, full Hz on hover) with exact numeric entry and remembered confirmed values. The input offers Hz/kHz/MHz units shared by typed and virtual-keypad entry; custom values may exceed common presets when admitted. View highlights while active and a second press restores the previous window and zoom state, until a manual view/capture/mode/reset change; passband zoom changes only the display.
  The input tooltip shows the actual passband, confirmed state and available range; View sits beside the units.
- **Digital modes** — FT8 decoding, CW decoding, DRM/DRM+ reception and SSTV picture decoding (18 modes). The FT8 window supports QSO highlighting, regex filtering and column sorting. Current decoder and playback limits are documented in [KNOWN_ISSUES](docs/en/KNOWN_ISSUES.md).
- **FM stereo and RDS** — WFM recovers L/R from the 19 kHz pilot and 38 kHz difference signal.
  It decodes RDS PI, station name and RadioText from 57 kHz. The FM pane shows a pilot-based
  STEREO/MONO badge, deviation, audio level and L/R scope. The badge reports the pilot, not
  whether the current programme has different L/R content. Stereo is enabled by default; the native
  stream has two channels, while explicitly disabling stereo selects mono.
- **AM analysis** — envelope detection by default, optional synchronous detection with integral carrier tracking, recovered RF frequency and lock indication, smoothed modulation/input metrics, audio levels and a final-PCM waveform. The title badge reports detector lock and includes timing details on hover; the Synchronous toggle sits beside the carrier frequency.
- **Trigger** — device-side RTA level trigger plus a swept-mode software trigger; limit lines with
  pass/fail margins and normalization; PNG/CSV export. RTA capture completion is confirmed from
  device/frame metadata; fixture input does not simulate hardware trigger hits.
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

### Run the bundled release

The 1.9.2 checkout includes `runtime/linux-x86_64/websad`, the project SDK adapter
`runtime/linux-x86_64/libwebsa_san90_sdk_session.so`, and `runtime/frontend/dist/`.
Run `./run.sh` for hardware or `./run.sh --fake` for synthetic input, then open
http://127.0.0.1:8080. No Rust, Node.js or Python installation is needed to use the bundle.
Hardware still requires the user-provided HAROGIC SDK under `/opt/htraapi` (including
`libhtraapi.so.0`); the proprietary SDK is not bundled.

Synthetic digital-mode playback also needs this checkout's tracked `tests/fixtures/`;
copying only `runtime/` is insufficient. `run.sh` sets the working directory to the checkout;
`WEBSA_FIXTURES` can select another fixture directory.

These binaries target **Linux x86_64 with glibc** and were built on Arch Linux. They require
GLIBC 2.44, GLIBCXX 3.4.30 and CXXABI 1.3.15 across the service and adapter, plus the
system ELF loader, `libstdc++.so.6`, `libgcc_s.so.1`, `libm.so.6`, `libc.so.6` and
`libfdk-aac.so.2`. The SDK also needs its own dependencies (observed: libusb, liquid-dsp,
OpenMP, pthread and dl). This is not a portable or static binary distribution; rebuild on
older distributions or other architectures. See [Development](docs/en/DEVELOPMENT.md)
for rebuilding and packaging.

`run.sh` prefers rebuilt `backend/target/release/websad` and `frontend/dist/` when present,
otherwise it uses the bundled files. `WEBSA_BIN` overrides the service executable. For hardware,
an explicit `WEBSA_RUST_IQS_SHIM` wins, followed by the installed user shim and the bundled adapter.

The service itself needs GLIBCXX 3.4.29 and CXXABI 1.3.9; the bundled SDK adapter raises
those requirements to the versions above. The ELF loader path is `/lib64/ld-linux-x86-64.so.2`.
The adapter was linked against SDK 0.55.100 with library search path `/opt/htraapi/lib/x86_64`;
rebuild it with `SDK_ROOT` for a different SDK installation. CPU portability beyond Linux x86_64
has not been established.

### Prerequisites

- Linux with Bash, coreutils, `flock` (util-linux), `/proc` and `curl` for the launcher; native audio
  decoding requires the FDK AAC shared library (`libfdk-aac.so.2`, version ≥2.0.3).
- Rust, a C++17 compiler and pkg-config only for native rebuilds.
- Python ≥3.10 only for bench, e2e, reference and fixture tools; no production Python backend.
- Node.js — only for rebuilding the frontend and running its tests: a 22.22+, 24.15+ or ≥26
  release (the test stack's floor; a plain Vite build accepts 22.12+). Node 18/20/23 are not covered.
- Harogic SAN series analyzer + official SDK:
  - `htra_api.py` — vendor wrapper retained for offline tools (HAROGIC copyright)
  - `libhtraapi.so` — **proprietary binary, obtain it from HAROGIC**; install the SDK headers/libraries under `/opt/htraapi`

### 1. Rebuild from source

```bash
make native sdk-shim                     # native service and hardware SDK shim
./build.sh                               # frontend assets; make wasm rebuilds WASM explicitly
```

### 2. Run

```bash
./run.sh
```

Open http://127.0.0.1:8080

The default is real hardware; the launcher selects the installed user shim or bundled adapter as described above.
Use `WEBSA_FAKE=1 ./run.sh` explicitly for synthetic input. `WEBSA_INSTANCE`, `WEBSA_HOST`,
`WEBSA_PORT`, `WEBSA_BIN`, `WEBSA_RUST_IQS_SHIM` and `WEBSA_STATIC` configure the native launcher.
It starts an ordinary background process with `nohup` and, when available, `setsid`; no systemd
session is needed. `--instance NAME` or `WEBSA_INSTANCE` selects a checkout-scoped instance
(default `websad`), replacing the former `--unit` / `WEBSA_UNIT` interface. `./stop.sh` validates
the recorded PID, start time, executable and working directory before signalling it; repeated stops succeed.
`./status.sh` uses the recorded host/port and prints the PID, log path and HTTP state.
Private state and append-only logs live below `$XDG_RUNTIME_DIR/harogic-websa` when available,
otherwise `${XDG_STATE_HOME:-$HOME/.local/state}/harogic-websa`, separated by checkout and instance.
There is no automatic process restart; use `make restart` after a process failure.

Unplugging the analyzer is detected and reported, and plugging it back in resumes the same mode
without a restart; `make status` shows the live link, and `make restart` restarts the service.

The service listens on loopback by default. Local and LAN access need no token:

```bash
WEBSA_HOST=0.0.0.0 ./run.sh
```

Then open `http://device-address:8080/`. The [operations FAQ](docs/en/FAQ_NOTES.md) covers
environment variables, logging and recovery. Direct invocation of `backend/target/release/websad`
defaults to fixture input on port 8099 when no hardware shim is supplied; `run.sh` explicitly selects
hardware on port 8080 unless fake input is requested.

Current documentation: [architecture](docs/en/ARCHITECTURE.md), [HTTP/WS API](docs/en/API.md),
[mode and parameter flow](docs/en/MODE_STATE_FLOW.md), [development](docs/en/DEVELOPMENT.md),
[known limits](docs/en/KNOWN_ISSUES.md), and [tools](tools/README.md).

### 3. Test

```bash
pip install -r requirements-dev.txt      # optional development/bench/e2e tools
make ci                                  # local non-hardware gates
make e2e-fake                            # native fixture service + browser checks
make hw-test                             # bench only: tinySA CW + UI state regression
```

## Directory Layout

```
harogic-websa/
├─ backend/              Rust service, channelizer and C++ SDK shim
├─ dsp/
│  ├─ core/              shared native/WASM Rust DSP crate and browser WASM
│  └─ deepfilter/        shared native/WASM DeepFilterNet integration
├─ frontend/             TS frontend (Vite + TypeScript, i18n + themes)
│  ├─ src/ui/panels/     panel modules (frequency/resolution/rta/markers/refAmp/...)
│  ├─ src/render/registry.ts  view renderer registry (views self-register)
│  └─ src/__tests__/     vitest tests (DSP engine, synthetic traces)
├─ htra_api.py           vendor Python wrapper for offline tools (HAROGIC copyright)
├─ docs/                 bilingual architecture, API, operations, development and codec references
├─ tests/                reference/tool tests and shared protocol/DSP fixtures
│  └─ fixtures/frames/   golden binary frames shared with the TS decoder test
├─ tools/                hardware smoke + e2e + bench + quality guards
│  └─ checks/            architecture_guard.py + baseline.json (fitness functions)
│  └─ reference/         offline sdk_bindings/ddc/native_ddc/spectrum/demod/filters/framer/sdr_capture.py
├─ screenshots/          README screenshots
├─ run.sh / stop.sh / status.sh / clean.sh / build.sh / test.sh / Makefile
├─ pyproject.toml / requirements.txt / LICENSE / .gitignore
```

## Scripts

The day-to-day commands, plus what to run before committing:

| Script | Description |
|---|---|
| `./run.sh` / `./stop.sh` | Start / stop the owned native background process |
| `make restart` / `make status` | Restart / show PID, log path and live HTTP/device state |
| `make build` | Frontend assets + native Rust service (hardware shim built separately) |
| `make native` / `make sdk-shim` | Build the Rust service / hardware SDK shim |
| `make frontend` | Frontend only: npm install + Vite build (no WASM) |
| `make wasm` | Both WASM cores; `make wasm-dsp` / `make wasm-dfn` build one |
| `make clean` / `make clean-all` | Clean caches/dist while keeping dependencies and Rust targets / also remove dependencies and Rust targets; committed WASM stays |
| `./test.sh` | Native contracts + Python tool tests + Ruff + frontend Vitest |
| `make ci` | Local non-hardware gates; see the development guide for workflow differences |
| `make e2e-fake` | Browser checks against the native fixture service |
| `make hw-test` | **Bench only** — tinySA CW smoke test + UI state regression |
| `make bench` | Compare against the recorded performance baseline |

The [development guide](docs/en/DEVELOPMENT.md) and [tools guide](tools/README.md) cover fixture
regeneration, guards, document/version checks and hardware prerequisites.

## Tests

- **Native Rust**, **Python tool/reference** and **frontend Vitest** suites cover service contracts,
  protocol/DSP logic, parameter slots and i18n parity. Shared golden fixtures cover binary frames
  and DSP kernels; Python is not a production backend or a runtime fallback.
- **No hardware needed by default**: local gates and native fixture browser checks exercise deterministic service and DSP contracts. `make hw-test` needs the analyzer and a tinySA CW source. A fake source or passing unit suite does not establish sustained RF performance.
- Hardware results depend on device, firmware, host load and enabled browser layers. Existing screenshots and measurements are examples, not a fresh hardware qualification of every checkout. The retained performance baseline needs its configuration and provenance checked before native comparisons.

## Open-Source Notes

- Project code (backend + frontend): **MIT** licensed
- `htra_api.py`: HAROGIC official SDK Python wrapper, copyright HAROGIC; bundled for convenience with your own SDK
- `libhtraapi.so`: proprietary binary, **not included** — obtain from HAROGIC
- Screenshots taken on a real SAN-90 (the SDR digital-mode shots use a PlutoSDR as the signal source)
- `tools/vendor/ggmorse/`: the CW (Morse) decoder is
  [ggmorse](https://github.com/ggerganov/ggmorse) by Georgi Gerganov, **MIT** licensed, vendored at
  commit `7b4822a8` (see `tools/vendor/ggmorse/LICENSE`). `backend/cw_build.rs` compiles it
  into the native service with the project's `backend/native/ggmorse_bridge.cpp` bridge.


---

## Author

[好奇牛马 (Bilibili)](https://space.bilibili.com/28447213)
