# Development Guide

This guide covers the native Rust service, shared DSP, browser controls and development tools.
See [Architecture](ARCHITECTURE.md) for component boundaries and [API](API.md) for wire contracts.

## 1. Environment, build and run

Release 1.9.4 uses native Rust PFB/DDC; Python and retained browser WASM artifacts are
offline tooling and optional diagnostics, not production backends. To run the committed
bundle, use `./run.sh` or `./run.sh --fake`; see the [release prerequisites](../../README.md#run-the-bundled-release).
The bundle contains `runtime/linux-x86_64/websad`, the project's SDK adapter and
`runtime/frontend/dist/`. It requires Linux x86_64 with glibc 2.44, GLIBCXX 3.4.30,
CXXABI 1.3.15 and dynamic FDK AAC (`libfdk-aac.so.2`). Hardware additionally requires
the user-provided HAROGIC SDK; neither that SDK nor its dependencies are bundled.
Rebuild for older distributions or other architectures; no cross-platform portability is claimed.

Rebuild and package release outputs with `make build sdk-shim && make package-runtime`.
`make package-runtime` alone copies and strips existing outputs without compiling. Keep the service,
adapter and frontend bundle from the same release. The launcher prefers local rebuilt outputs,
then the committed runtime; `WEBSA_BIN` and `WEBSA_STATIC` override those selections.
Synthetic digital-mode playback needs the checkout's tracked `tests/fixtures/` in addition to
`runtime/`. The root launcher sets the working directory; `WEBSA_FIXTURES` overrides the
fixture directory, which otherwise defaults to `tests/fixtures` relative to that directory.

To rebuild, use Linux, Rust stable, a C/C++ toolchain, CMake,
pkg-config and Node.js 22.22+, 24.15+ or ≥26 (the frontend test stack's floor; a plain Vite
build accepts 22.12+). Python 3.12 or 3.13 supports tests, fixtures and bench tools; it is
not a production backend. Native audio decoding requires FDK AAC ≥2.0.3 at build time and
its shared library at runtime; [the workflow](../../.github/workflows/ci.yml) records the
pinned source and installation. The user-provided vendor SDK is required for hardware operation.

Run these commands from the repository root, in sequence:

```sh
python3 -m pip install -r requirements-dev.txt -c requirements-lock.txt
make frontend
make native
./run.sh --fake --port 8099 --instance websad-dev
./status.sh --instance websad-dev
./stop.sh --instance websad-dev
```

`make frontend` runs `build.sh` and produces `frontend/dist/`. Rebuild after frontend changes:
the service serves that bundle. `make build` builds the frontend and native service;
`make wasm` is an explicit offline DSP artifact build, not a product frontend prerequisite.

For hardware, install the vendor SDK, then run `make sdk-shim` and `./run.sh`.
`SDK_ROOT` and `SDK_SHIM` configure the shim build; `WEBSA_RUST_IQS_SHIM` selects it at runtime.
The default instance is `websad` at `127.0.0.1:8080`. Use `WEBSA_INSTANCE` or `--instance`
to select it; status and stop read saved endpoint/ownership metadata. Keep one process as the device owner.
See [FAQ notes](FAQ_NOTES.md) and [the tools guide](../../tools/README.md) for service and hardware configuration.

**Memory-limited hosts: builds must be serial and isolated.** Run Rust builds, Rust tests and
browser suites sequentially; do not use parallel Make. Cargo `-j 1` limits only one invocation,
not other builds or browsers. Keep `--test-threads=1` for Rust test runs. Isolate builds from
the device process with the host's available resource controls and an explicit memory limit.
The launcher uses a detached background process and does not require a service manager.

## 2. Dependency direction and ownership

| Area | Responsibility |
|---|---|
| `backend/src/bin/websad.rs` | Service lifecycle, commands, STATUS and transport |
| `backend/sdk/san90_sdk.cpp` | Retained device session and serialized SDK calls |
| `backend/src/` | Native channelizer and receivers |
| `dsp/core/`, `dsp/deepfilter/` | Shared native/WASM DSP crates and explicit browser diagnostics |
| `tools/reference/` | Offline reference and fixture support |
| `frontend/src/core/` | Types, parameter slots, results, runtime state and protocol |
| `frontend/src/dsp/` | Pure frontend calculations |
| `frontend/src/render/` | Drawing and repaint seams |
| `frontend/src/ui/`, `frontend/src/meas/` | Interaction, measurement lifecycle and registration |
| `frontend/src/audio/` | Audio workers, worklet and resampling |

Dependencies point from orchestration toward leaves. Shared types, parameter slots, pure
helpers and registry seams must not import their consumers. Keep frontend module cycles at
zero. Renderers draw; they do not write application state. Keep acquisition and heavy DSP off
the HTTP/WS event loop and browser main thread. Production processing uses the native PFB
path; offline Python references must not become runtime fallbacks.

## 3. Parameters, results and display state

| Kind | Owner and rule |
|---|---|
| User parameters | Slots in `ui/*State.ts`: user actions call `set()`, STATUS calls `confirm()`, readers call `get()` |
| Client-only preferences | Slots with `authoritative: true`; backend confirmation is not expected |
| Traces and measurement results | `core/results.ts`, with explicit setters and no desired/confirmed semantics |
| Connection, trigger and mode runtime | `core/store.ts`, through explicit setters |
| Persistence | The slot's `persistKey`; avoid independent localStorage copies |

Treat backend STATUS as device truth. Keep requested and actual values distinct; do not
confirm from a pending user value or a private cache. Reset must invalidate stale intent.
Use explicit absence rather than a valid frequency or level as an unset sentinel. Read slots
once outside hot loops. Mode switches preserve preferences; explicit handover actions change
them deliberately.

Keep manual display Ref separate from the requested and effective hardware references. The local
viewport uses -200..+100 dBm plus Offset; wire requests clamp Ref minus Offset to -160..+40 dBm.
Matching raw reports must retain the chosen viewport. Repeated clamped targets must reuse pending
or accepted hardware intent. Pending edits survive stale STATUS and roll back on rejection or timeout. Explicit
Auto is a bounded operation; cancel stale decisions on manual, frequency, mode or Preset changes.
SDR entry fitting is display-only and must not configure IQS. See [FAQ notes](FAQ_NOTES.md) for
overflow recovery and placement rules. Apply amplitude offset once at the display boundary.
Read actual gain from the active SDK mode, preserving missing readbacks as null; preamp Auto/Off
describes the requested policy, not a physical on/off indication. Give each shared display slot
one owner; restore the user's prior choice when an exclusive measurement ends. Status changes
must request a repaint even when acquisition produces no frames. Waterfall mapping changes clear
history even while paused; reject non-finite or invalid level ranges before changing state.

## 4. Extending the implementation

- Commands and parameters: implement native validation, mode ownership and transitions; update
  the HTTP/WS contract and `tests/fixtures/contract/schema.json` together. Use frontend slots
  and the existing command sender. Publish device limits once through capabilities.
- Modes and measurements: define lifecycle, acquisition, recovery and result ownership. Register
  renderers and measurement tabs through their existing registries, and ensure the entry point
  imports registration modules. Assert that entering the mode produces its visible result.
- Frames: update the native encoder, retention policy, `core/frames.ts` decoder and shared golden
  fixtures together. Verify the layout on both consumers; `tools/reference/framer.py` is offline support.
- Panels: put actions in `ui/panels/`, use existing action bindings, and add text in both i18n
  languages. Resolve field/input identifiers with `inputForField()` and `fieldForInput()`.
- DSP: use the shared Rust kernels or pure frontend DSP modules. Preserve sample accounting,
  ordering and bounded queues. Keep SDK calls on the retained session thread, with stable
  handle storage; initialize thread-sensitive runtimes on their owning thread.

Update affected documentation with the implementation. Comments should explain invariants,
units, ownership or constraints, not sessions, attempts or review finding identifiers.

## 5. Tests, fixtures and CI

Choose assertions that fail when the behavior is broken: numerical relationships, boundary
conditions, transitions, protocol bytes and visible results. Avoid implementation copies and
counters that merely prove a renderer was called. Label proxies and their evidence limits.
Keep real captures, golden fixtures, models and vendor provenance when they have consumers;
regenerate only reproducible fixtures with their existing generators.

| Layer | Entry point and evidence |
|---|---|
| Native service and tools | `./test.sh`: release-mode backend library, binaries and integration tests, Python tool tests, Ruff and frontend Vitest |
| DSP kernels | `make wasm-test`: release-mode native Rust kernel tests against reference fixtures, including optimized FT8 paths |
| Local aggregate | `make ci`: kernel and service/tool/frontend tests, version and fixture checks, DOM/registration/doc/architecture guards, frontend build |
| Browser fixtures | `make e2e-fake`: native fixture service, UI smoke, state regression and viewport geometry gate |
| Hardware | `make hw-test`: tinySA-driven smoke, command sweep and UI state regression |
| Performance | `make bench`: comparison with `tools/bench/bench_baseline.json` |

Read DSP ABI text messages immediately after the push that reports them. Later pushes replace this result list; the lifetime decode count does not retain message payloads.

Install browser support with `python3 -m playwright install --with-deps chromium` and build
the frontend before browser runs. Fixture mode proves application behavior without the SDK;
it does not prove RF correctness, sustained capture or hardware recovery.

`make ci` and GitHub Actions are different entry points. The workflow has separate native,
Python tools (3.12 and 3.13), frontend, UI smoke and architecture jobs. It explicitly runs
`npx tsc --noEmit` and installs dependencies in clean environments. Its browser job runs
`ui_smoke.py` and `state_regression.py`; the local `make e2e-fake` also runs
`viewport_baseline.py`. Consult [Makefile](../../Makefile), [test.sh](../../test.sh) and
[ci.yml](../../.github/workflows/ci.yml) for the actual gates; neither a local aggregate nor
one CI job substitutes for hardware validation.

## 6. Hardware validation and performance isolation

Use a dedicated device session, fixed RF source and configuration, one intended client and
no competing builds, benchmarks or browser suites. Record configuration, duration, warnings,
packet/sample continuity and the exact command. Exercise the native service and actual browser
surface; frame counters alone do not prove the trace, audio or decoder result is correct.
Include the relevant mode transitions, reconfiguration and recovery paths.

Keep a hardware result's scope explicit. Synthetic throughput, short captures and headless
transport checks do not establish sustained RF or full-layer browser performance. Consult
[the tools guide](../../tools/README.md) and [Architecture](ARCHITECTURE.md)
for focused capture and receiver checks. Do not present old results as fresh verification.

Before comparing performance, establish a clean device state and remove unrelated host load.
Avoid per-frame allocation and repeated work; preserve transport backpressure and stream-specific
retention. `make bench` uses a retained baseline whose workload and protocol applicability must
be checked before interpreting it for the native service. `make bench-record` rewrites that
baseline; use it only with documented comparable measurements, not to hide a regression.
Architecture baselines likewise require an explained, reviewed change when relaxed.

## 7. Failures and releases

Native recovery owns device link failures; process failures require `make restart`. Preserve error
context in the instance log, and expose UI/script diagnostics through STATUS. Keep warnings distinct
from failed acquisition, including usable IQ returned with an SDK warning. Do not turn a
recoverable warning into silent data loss or claim a disconnected device is healthy.

Start diagnosis with `./status.sh` (select a named instance with `--instance`) and read its
reported log file. For a blank canvas, check the rebuilt bundle, registration reachability and
visible pixels. For a stale control, inspect STATUS, pending intent and main-thread load.
For a device-busy failure, inspect exclusive ownership before retrying.

Use English `type(scope): summary` commit messages that explain the change and its purpose.
Keep commits independently valid. Run applicable gates before release, including hardware
validation for device behavior, and state any unverified scope explicitly. Update the version
in `pyproject.toml`, then run `python3 tools/checks/sync_version.py`; `--check` validates the
frontend package, lockfile and HTML copies. Rebuild the frontend before validating a release.
Merge reviewed work and tag the validated version; keep source, documentation and release
artifacts consistent.
