# Development

[Documentation](INDEX.md) · [Architecture](ARCHITECTURE.md)

## Build from source

Requirements: Linux, Rust ≥1.95, C++17 compiler, pkg-config and FDK AAC ≥2.0.3.
Frontend development uses the committed npm lockfile and a Node version supported by the installed
test stack (22.22+, 24.15+ or ≥26). Basic Python tools require ≥3.10; the complete development
environment and locked dependency installation below require Python ≥3.12 (CI uses 3.12/3.13).
Tests and browser benches use newer asynchronous APIs, so the basic tool minimum does not define
the test environment. Hardware adds the proprietary
SDK; Soapy and external decoders are optional. See [dependencies and notices](#dependencies-and-notices) below.

```bash
make native                     # release Rust service, one build job
make frontend                   # type-check and Vite build; no WASM
make sdk-shim SDK_ROOT=/opt/htraapi
./run.sh                        # or ./run.sh --fake
```

Run heavyweight builds separately; do not use unrestricted `make -j`. Rust test/build commands
below use one build job and one test thread. Frontend Vitest is capped at two workers.
Install Python tooling in a virtual environment:

```bash
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements-dev.txt -c requirements-lock.txt
python -m playwright install chromium
```

`make wasm` rebuilds optional diagnostic artifacts only. `make soapy-driver` builds the pinned
patched driver into a user prefix. Provider/model setup is in the [tools guide](../../tools/README.md).

## Test by responsibility

| Command | Responsibility |
|---|---|
| `./test.sh` | Native service contracts, Python tools, Ruff and frontend behavior |
| `make wasm-test` | Native shared DSP kernel tests; does not run browser WASM |
| `make ci` | Above suites, fixture/version/document/architecture guards, Soapy patch and frontend build |
| `make e2e-fake` | Isolated fixture service: browser smoke, state regression and viewport geometry |
| `make hw-test` | SAN hardware plus tinySA and browser measurement checks |
| `make bench` | Compare a running service to the recorded performance baseline |

The hosted workflow is `.github/workflows/ci.yml`; it also runs focused workbench/recording
browser checks. A local `make ci` is not an assertion that every hosted or hardware job ran.
On small-memory hosts, serialize heavy builds, decoder/NR tests and performance measurements.
Independent lightweight browser tests may run with limited concurrency after checking memory headroom;
sustained real RF measurements run exclusively to avoid contaminating the results with test load.

For a focused native regression:

```bash
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib TEST_NAME -- --test-threads=1
```

Keep tests that verify distinct contracts even when inputs overlap: numerical correctness,
serialization, source clocks, state transitions, consumer cancellation and actual browser wiring
catch different failures. Remove a test only after identifying the replacement coverage. Avoid
source-string checks where an observable behavioral assertion is practical. Hardware tests and
synthetic probes must retain separate labels and prerequisites.

Browser asynchronous conditions must wait for their resolved result and verify that a persistently
false condition times out; a Promise object itself must not count as success. Mark manual cases
requiring additional captures as ignored, and fail explicit runs when their inputs are missing
instead of returning and counting them as passes.

## Extend the implementation

Follow [state ownership](reference/STATE.md) for controls and [API](reference/API.md) for wire changes.
Backend capabilities are authoritative; avoid hard-coded frontend limits. Source capacity lives
in `contracts/service-capacity.json`, and version metadata in `pyproject.toml`.

A new receiver mode needs a catalog descriptor, admission/rate constraints, processing ownership,
output/clock behavior and a failure path. Reuse shared DSP kernels; avoid a parallel browser
implementation. A UI control needs its existing intent/receipt path, bilingual labels, accessible
focus behavior and a regression at the appropriate boundary.

Keep comments for units, ownership, invariants, numerical derivations and non-obvious tradeoffs.
Delete obsolete migration narratives and comments that only repeat the code. Preserve third-party
source provenance and license files. Update both document languages in the same change.

Report manual reading, repository-wide static checks and behavioral regression coverage separately;
a file inventory or structural scan does not replace reading the implementation. Fault injection must
first fail on the original behavior, then verify rejection, cleanup and recovery after the fix.
Keep conclusions about the Python reference, Soapy and production SDK shim separate.

## Fixtures and evidence

Golden frames and DSP fixtures under `tests/fixtures/` connect Python reference generators, Rust
kernels and TypeScript decoders. Generator `--check` modes are CI contracts; regenerate intentionally
and review the changed values instead of updating fixtures merely to make a test pass.

For sixteen-channel synthetic coverage, use the existing probe with an unused persistent output
folder: `python tools/bench/eight_channel_bench.py --channels 16 --seconds 30 --warmup 5 --artifacts <new-directory>`.
The legacy filename does not limit channel count. Synthetic checks do not certify RF reception.

With controlled RF coupling between a SAN90 and PlutoSDR, build an SDK shim matching the current
source, stop services using the device, and run:

```bash
python tools/bench/multichannel_rf.py --shim <matching-sdk-shim.so> \
  --channels 16 --seconds 600 --gain -10 --artifacts <new-external-directory>
```

The tool owns its service and transmitter. It checks all 32 IQ/audio streams for sixteen
AM/NFM/USB/LSB receivers, exact sample continuity, identity, and known tones at the start and end.
Capture uses 62.5 MSps / 50 MHz; the stimulus occupies 434.85–435.15 MHz. This does not establish
RF quality across the entire 50 MHz or every decoder combination. Choose TX gain for the actual
coupling; the example does not guarantee a pass on every bench. On exit, the tool stops its service,
restores Pluto settings and deletes tone PCM after recording hashes. Pause heavy builds and browser
tests during this measurement.

Record binary revision/hash, device/firmware, capture geometry, host load, duration, expected signals,
continuity and output artifacts for hardware/performance evidence. Do not tune unrelated processes
to manufacture a benchmark pass. Keep reports in CI artifacts or outside the checkout, and remove owned bulk temporary captures.
The comparison tool attributes CPU only to the owner of the requested local loopback HTTP listener;
remote services or ambiguous listeners have no local CPU measurement.

## Release and cleanup

After building a service executable, restart the intended instance and verify `./status.sh` and
its process executable. An old process can keep its previous inode after a successful rebuild.
If restarting is deferred, say so explicitly in the handoff.

`make package-runtime` copies existing native/frontend/adapter outputs; it does not compile.
Build matching artifacts first. The package includes native NR dependencies/notices as supported
by the packaging helper; optional external providers remain separate. Verify the packaged launch
and runtime library requirements rather than assuming a development machine proves portability.

`make clean` removes caches/dist while retaining dependencies and Rust targets. `make clean-all`
also removes dependencies and targets; committed diagnostic WASM remains. Never remove user
recordings, logs needed for diagnosis or unrelated caches as incidental cleanup. Documentation
covers current usage, interfaces, build procedures and maintenance constraints. Update both languages,
check document links/structure, and remove superseded pages. Keep individual screenshots, logs and
measurement reports in CI artifacts, release attachments or test directories outside the checkout.
Turn data needed for regression into documented `tests/fixtures/` inputs. Use Git for old documents
and historical reports.

## Dependencies and notices

- Authorized project-owned code: **AGPL-3.0-only with the HAROGIC SDK / FDK AAC section 7 exception** (`LicenseRef-Harogic-WebSA`). See [scope](../../LICENSING.md), [third-party notices](../../THIRD_PARTY_NOTICES.md) and [source packaging / unresolved rights](LICENSING.md). Earlier MIT grants remain effective.
- Native listening NR includes DeepFilterNet3, GTCRN, UL_UNAS, and FastEnhancer. The latter three use official MIT-licensed ONNX weights and the native ONNX Runtime CPU library; PCM DSP and streaming are implemented in Rust. See [model versions, notices, rates, attenuation and delay](../../backend/models/nr/README.txt). Source builds can restore the pinned library with `bash tools/fetch-nr-runtime.sh` before `make package-runtime`.
- `htra_api.py`: HAROGIC official SDK Python wrapper, copyright HAROGIC; excluded from relicensing and generated source bundles pending confirmation of the vendor's redistribution/modification terms.
- `libhtraapi.so`: proprietary binary, **not included** — obtain from HAROGIC
- Official [SoapyHarogic](https://github.com/HAROGIC-Technologies/soapy-htra) revision `762dbfa1512c705e1d8b86e820b06e8a10e08a92` is **GNU LGPL v2.1 or later** and is downloaded at build time, not vendored. `tools/soapy-htra-stream-format.patch` is this project's compatibility patch and is applied only to a private build copy.
- Optional [multimon-ng](https://github.com/EliasOenal/multimon-ng) and [rtl_433](https://github.com/merbanan/rtl_433) use GPL v2; [OnAir](https://github.com/adswill/OnAir) uses GPL v3 with component-specific notices. They run as independently installed processes with the existing pipe/network integration. The setup script preserves pinned sources/licenses for source-built tools and OnAir's binary notices plus corresponding-source information; redistributors must retain the applicable upstream terms.
- `tools/companions/rtty/main.cpp` remains **GPL-3.0-or-later**, a separately built companion linked to OnAir's RTTY decoder and excluded from the project-owned license change; `backend/src/external/rtty.rs` communicates with that executable through pipes. See [RTTY setup and replay](../../tools/README.md#independent-rtty-companion).
- `tools/vendor/ggmorse/`: the CW (Morse) decoder is
  [ggmorse](https://github.com/ggerganov/ggmorse) by Georgi Gerganov, **MIT** licensed, vendored at
  commit `7b4822a8` (see `tools/vendor/ggmorse/LICENSE`). `backend/cw_build.rs` compiles it
  into the native service with the project's `backend/native/ggmorse_bridge.cpp` bridge.

## Maintenance priorities

| Priority | Work | Acceptance |
|---|---|---|
| P2 | Extend sixteen-channel real RF coverage across frequencies, modes and host loads | Retain binary/host/capture/source/duration/continuity and failure evidence; use bounded USB diagnostics to investigate intermittent transfer timeouts during resource pressure, without treating one successful run as a root-cause fix |
| P2 | Extend blanker real-audio and concurrent model-memory evidence | Preserve analytic cold-start/cross-block pulse/step references and record partition effects; distinguish process RSS from exclusive model cost |
| P2 | Replace remaining source-string tests with behavior where practical | Equivalent failure coverage, not merely fewer test files |
| P2 | Qualify Soapy geometry across real SDK/device combinations | Retain allocation/malformed-packet fault injection; add actual width/rate evidence without relaxing bounds |
| P2 | Qualify process exit and recovery when a real SDK call never returns | Retain owner retirement/exclusion regressions; separately verify the launcher deadline for vendor calls and static destructors |
| P2 | Re-audit older archive compatibility when retirement is proposed | Real retained-file compatibility evidence and explicit migration path |
