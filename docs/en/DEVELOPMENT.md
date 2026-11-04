# Development Guide

For RF preparation use `python tools/bench/sdr_capture_observe.py --origin http://127.0.0.1:8080 --expect-service-id <current-service-id> --seconds 300 --artifacts <new-directory>`. Read the identity with GET `/api/sdr/receivers` first. This tool only issues GET, refuses identity changes and existing artifact directories, records source clocks/sample progress/warnings/recordings/load, and never opens a WebSocket or arms capture. Keep an authorized browser/recording consumer running separately. The report is observation evidence; neither device connectivity nor an advancing counter proves RF reception or physical audio.

Prepare the new release build before the device window. Existing processes keep their old binary until explicitly restarted. Confirm the target instance, release of the N90 owner and RF source before replacing a hardware process; another port does not isolate the physical device. Recover one real signal first, observe at least five minutes and check spectrum/tuning, actual input rate, source epoch/positions and physical listening. Add channels within the same actual capture passband including transitions, then combine IQ/published-audio/results recordings for another five minutes. Stop and independently inspect saved files, UID/rate/source boundaries and accepted/persisted bytes. Mark unavailable real protocols and physical listening as untested. Synthetic `native_multichannel*.py` gates create/reconfigure their own service and must never target an existing hardware instance.


Finished audio tasks expose **Play audio** when `audio_archive_read` is available. Choose a source segment and start/length in sample frames, Load audio, use the native play/seek/volume controls, or download the same Float32 WAV/provenance snapshot. A stereo frame includes both channels; select at most 1,048,576 frames / 60 seconds. Original shared PCM processing is preserved, and live monitor controls are independent. Hiding or editing releases file playback. The verifier currently scans the complete task before loading, so longer tasks take longer without accumulating their samples. Gate: `python tools/e2e/native_multichannel_audio_playback.py --artifacts /tmp/websa-audio-playback`. It uses independent CRC/Zstd and SciPy WAV readers, two-volume stereo/mono fixtures with exact clocks, actual AM/FT8 network recordings, real native-media play/seek, 420px Chinese/light Pop out/Dock, held mouse/keyboard downloads, shared-reader cancellation/stalled TCP/corruption, healthy recording and SIGKILL prefixes. Build frontend/native first. Continuous long-file playback and digital file decoding remain separate work.

File tuning: expand Demodulate file, enter Offset / Hz relative to the recorded centre, select mode/filter and Render audio. The frequency readout is the original centre plus that offset. The whole selected sideband must be retained in the original IQ. `python tools/e2e/native_multichannel_tuning.py --artifacts /tmp/websa-file-tuning --sigmf-python /path/to/sigmf-venv/bin/python` verifies signed fractional offsets in all four modes, two-volume phase continuity, unchanged precise original clocks through the official SigMF reader, WAV pitch, boundary rejection, cancellation/backpressure and the real 420px player/download UI. Sub-Hz precision and block partition independence also have native regressions. Old-service controls remain hidden.

Recorded IQ → Waveform / SigMF → Demodulate file renders a selected range as AM, USB, LSB or NFM, offers native audio controls and downloads the same Float32 WAV with exact IQ provenance. Set a range of at most 60 seconds and a filter within the recorded passband; USB/LSB width is one sideband. The 48 kHz mono render starts fresh native filters without AGC/NR/squelch. The source file and live reception remain independent. Gate: `python tools/e2e/native_multichannel_demod.py --artifacts /tmp/websa-file-demod`; uses independent CRC/WBIQ and SciPy WAV readers, synthetic known tones at a noninteger IQ rate, actual native AM recordings, real keyboard/mouse media controls, 420px Pop out/Dock and live recording isolation. Build frontend/native before running. This analogue slice does not implement digital file decoding or continuous long-file playback.

Finished audio tasks offer **FLAC · 24 bit** beside Download audio when `audio_flac_export` is present. This exports the existing published audio at its actual rate as explicitly quantized signed-24 PCM; Float32 WAV remains the exact original-sample option. Fixed scaling, saturation counts and unchanged per-frame provenance are in the tar manifest/timeline. The pure Rust `flacenc` dependency adds no runtime executable or dynamic audio library. Gate: `python tools/e2e/native_multichannel_flac.py --artifacts /tmp/websa-flac`; install the reference `flac` CLI for this development check (1.5.0 tested). It independently decodes real AM/FT8 published samples and synthetic stereo/mono quantization boundaries, checks MD5/sample clocks, narrow-window native downloads, cancellation, corruption, stalled TCP and recovered prefixes. Source semantics and exact conversion are in [API](API.md).

Published-audio recording: workbench → Recordings → Record audio, then Download audio after stopping. The backend saves actual AUD2 Float32 PCM and continues after browser exit. Actual audio capability enables it, including digital companions. Downloads retain Float32 WAV configuration/format segments and per-frame clocks/original headers/configurations. Browser volume, pan and master mute do not change files. This is shared published PCM, not unenhanced protocol input. Opus and digital file decoding remain future work. `recording_audio.rs` reuses the bounded CRC/Zstd codec in `recording_journal.rs`; `receiver_workers.rs` carries actual packet configuration with PCM so queued old frames cannot acquire new settings. Gate: `python tools/e2e/native_multichannel_audio.py --artifacts /tmp/websa-audio`, using independent SciPy WAV/system-libzstd/CRC readers, actual network bytes, real browser controls and recovery/failure checks. See [API](API.md) for bounds and semantics.

Persistent results: in Channels → Recordings, use Record results for a processor with actual text/table/image/status output. IQ and results can run independently on the same UID. After stop, Download results streams a tar with task metadata and original published events as JSONL, including incremental image pixels. It captures from record start, so an image already in progress may remain partial. It continues after browser exit. Regression: `python tools/e2e/native_multichannel_results.py --artifacts /tmp/websa-results`; uses real native FT8/SSTV replay, an independent CRC/Zstd reader and JSONL/tar reader, real 420px browser gestures/downloads and SIGKILL recovery. This does not qualify hardware RF or backend audio recording.

This guide covers the native Rust service, shared DSP, browser controls and development tools.
See [Architecture](ARCHITECTURE.md) for component boundaries and [API](API.md) for wire contracts.

Single-channel presets: Channel workbench → Signal settings → **Presets**. Save confirmed frequency, IQ and processing settings on the server, then select a saved entry to preview and explicitly apply. Replacement and deletion require confirmation. Drafts, reception enabled state, monitoring and recording tasks are excluded. Presets survive browser and service restarts without being applied automatically. Gate: `python tools/e2e/native_multichannel_presets.py --artifacts /tmp/websa-presets`, covering persistence, conflicts/stale previews, recording isolation and 420px Pop out/Dock. Storage semantics are in [API](API.md). Atomic multi-channel scenes remain unimplemented.

Mode memory: each channel recalls its last confirmed IQ bandwidth/transition/minimum rate, demodulation bandwidth, shared PCM gain, AGC and squelch for each mode, including IQ only. A previously unused mode starts with its audio defaults. Recall changes the form only and keeps frequency/name/reception state; review and apply explicitly. Drafts and rejected changes are excluded. Memory survives reload in the same browser tab and is shared by its workbench windows, including Pop out/Dock; it is cleared for retired UIDs or a new service generation. It is not server persistence or cross-tab synchronization. Editing after a preview removes its Apply button and requires a fresh review.

All channel frequencies, full passbands and transitions must fit inside the shared capture. Workbench range rejections and previews show the current capture bounds and the requested channel bounds, directing the user to adjust the shared centre or capture bandwidth in the main SDR panel before retrying; retry alone does not widen capture. For example, a 101.7 MHz centre and 3.125 MHz usable bandwidth cover only 100.1375–103.2625 MHz and cannot receive 105 MHz FM. An explicit 103.35 MHz centre with 6.25 MHz usable bandwidth covers both frequencies. Shared capture changes affect every channel, and the main-panel centre entry also tunes C1; for a capture-only change, use `SET_SDR` without `listen` and verify each UID's frequency.

Three-channel workflow gate: `python tools/e2e/native_multichannel_workflow.py --artifacts /tmp/websa-daily-workflow`. It combines AM audio recording, FT8 result recording, healthy IQ recording, local listening, confirmed mode recall/reload, preset reuse, stop/start, copying and window/page closure, then checks the actual saved files with independent CRC/Zstd and WAV readers. Sources are synthetic/replay; RF, physical audio and sustained operation remain separate acceptance work.

## 1. Environment, build and run

The service uses native Rust PFB/DDC; Python and retained browser WASM artifacts are
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

Optional external receivers are installed independently with
`tools/setup-sdr-decoders.sh --prefix "$HOME/.local/share/websa/decoders"`.
The script pins and verifies multimon-ng, rtl_433 and OnAir, builds the first two serially,
preserves sources/licenses, and leaves system packages and running services alone.
Source the generated `env.sh` before launching your WebSA instance; it exports
`WEBSA_MULTIMON_PATH`, `WEBSA_RTL433_PATH` and `WEBSA_ONAIR_SERVER`.
Provider discovery is cached for the service lifetime; restart that instance after changing dependencies.
The main runtime package does not bundle these GPL tools. OnAir's portable build is Linux x86_64;
the setup script records its corresponding-source URL and checksum with the binary notices.
For fixture verification, place normalized interleaved f32le IQ in
`$WEBSA_FIXTURES/external/<mode>.f32` at the mode catalog's exact input rate.
Missing files reject the configuration. FT4/WSPR require UTC-aligned live input and reject
unaligned file/looping-fixture replay; pacing a recording alone does not establish slot alignment.

GPS L1 C/A uses `tools/setup-gnss-decoder.sh` for the separately licensed GNSS-SDR
executable and `cargo build -j 1 --release --manifest-path backend/Cargo.toml --bin websa-gnss`
for the native process bridge. Install that bridge into the decoder prefix's `bin`
directory and source its `gnss-env.sh`. The installer supports an isolated dependency
prefix and a build-only Python/Mako virtual environment via `--dependency-prefix`
and `--python`; the runtime bridge is Rust. See [GPS setup and real-recording provenance](../../tools/README.md#gps-l1-ca-from-sdr-iq).

GPS requires at least 2.6 MSps before adaptation and exactly 4 MSps at the provider
FIFO; a centered 3.90625 MSps capture can be adapted while preserving the configured
band and filter transitions. The CTTC real-recording replay is verified. A cold-start
435 MHz Pluto/SAN-90 run at gain −30 dB and 3.90625 MSps capture produced a valid
four-satellite PVT solution after 63.41 seconds, with 3.653 m horizontal error
against the synthetic target and no receiver error. This verifies the tested
translated GPS bench path; passive sky reception remains unverified.
Normal and reverse RTTY are implemented by the independent OnAir companion:
run `tools/setup-rtty-decoder.sh`, then restart the service. The default decoder prefix
is discovered automatically; a custom `--prefix` needs its `rtty-env.sh` or `WEBSA_DECODERS_PREFIX`.
Both 45.45-baud/170-Hz modes decoded known text in offline fixtures and over the
435 MHz Pluto/SAN-90 bench. See [RTTY setup and replay](../../tools/README.md#independent-rtty-companion).

To rebuild, use Linux, Rust stable ≥1.95, a C/C++ toolchain, CMake,
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

The native release profile uses optimization level 3, 16 codegen units and incremental
compilation without LTO. Keep `backend/target/` between builds. A profile or toolchain
change rebuilds dependencies; release binaries and test harnesses have separate panic strategies.

Keep `make build` warning-free with the default compiler and Vite diagnostics enabled.
Font URLs in CSS use `/fonts/` from `frontend/public/`; Vite applies the `/static/dist/`
base. Translations are emitted as a separate, eagerly loaded `i18n` chunk to keep the
main bundle below Vite's default chunk-size limit without changing language initialization.

For hardware, install the vendor SDK, then run `make sdk-shim` and `./run.sh`.
`SDK_ROOT` and `SDK_SHIM` configure the shim build; `WEBSA_RUST_IQS_SHIM` selects it at runtime.
The default instance is `websad` at `127.0.0.1:8080`. Use `WEBSA_INSTANCE` or `--instance`
to select it; status and stop read saved endpoint/ownership metadata. Keep one process as the device owner.
See [FAQ notes](FAQ_NOTES.md) and [the tools guide](../../tools/README.md) for service and hardware configuration.

The Pluto bench dispatcher supports `radiofax` and `wefax` through the same streaming transmitter. Use `python3 tools/pluto_tx.py radiofax --dry-run` without hardware, or add `--image chart.png --ioc 576 --lpm 120` for a page. Image height keeps its aspect ratio unless `--lines` is set. `--modulation am`, `--polarity inverted`, framing durations, offset, repeat and pause follow the mode's help. Default FM uses APT, 30 s phasing, 1500–2300 Hz image tones and APT stop. 180 LPM needs manual receiver settings. Full pages stream at the radio's accepted rate in fixed chunks; dry-run builds only a short head. See the [tools guide](../../tools/README.md) for examples.

`tools/pluto_tx.py` also dispatches DTMF/APRS/AIS/FT4/WSPR and synthetic GPS.
Digital and GPS generators use the shared `pluto_radio.Radio` streaming APIs
`open_stream`, `prime_stream`, `push_chunk` and `drain_stream` with four queued
buffers. Install the GPS source with `tools/setup-gps-simulator.sh`.
The shared radio layer enables a powered-down TX LO before transmission and
restores its original power state after destroying the TX buffer on exit.
The GPS CLI permits only a requested 435 MHz carrier; LO readback must be within
100 Hz. Before waveform generation, it negotiates a sample-rate readback within
20 Hz of the request and divisible by ten, then generates at that exact rate.
Its default TX rate is 2.6 MSps, independently of the exact 4 MSps receiver-provider
input. GPS gain defaults to −70 dB; the selected bench setting is explicit
`--gain -30`. A completed transmission alone does not establish GPS RF decoding.
See the [transmit guide](../../tools/README.md#rf-transmit-and-capture) for the
synthetic scenario, receive opt-in and continuity limits.

**Memory-limited hosts: builds must be serial and isolated.** Run Rust builds, Rust tests and
browser suites sequentially; do not use parallel Make. Cargo `-j 1` limits only one invocation,
not other builds or browsers. Keep `--test-threads=1` for Rust test runs. Isolate builds from
the device process with the host's available resource controls and an explicit memory limit.
The launcher uses a detached background process and does not require a service manager.

Start the multichannel development prototype with `npm --prefix frontend run dev:workbench`, then open
`http://127.0.0.1:5186/static/dist/workbench.html`. This standalone Vite page uses a fixture adapter,
connects to no native service, starts no audio and writes no recordings. The production build excludes it.
See [MULTICHANNEL_DRAFT](MULTICHANNEL_DRAFT.md) for the state contract and native offline experiments.
Browser regression: `python tools/e2e/multichannel_workbench.py` with a Playwright-enabled Python.

In the prototype, right-click the spectrum/waterfall to listen at that frequency, or use “＋ Listen to frequency” with MHz/kHz/Hz input.
Create a separate listening receiver when capacity permits, or explicitly choose an existing listening receiver.
The preview identifies recording boundaries; local routing starts after confirmation and preserves the editing target.
An already received frequency reuses its stream. This fixture entry produces no real audio.

Hardware-free native experiment (0.05 seconds of synthetic input by default; not capacity acceptance):

```sh
cargo run --offline --release -j 1 --manifest-path backend/Cargo.toml --features iq-experiments --bin multichannel_experiment -- /tmp/websa-multichannel-experiment 0.05
```

Produces `report.json` and experimental `.wbiq` files, retaining CF32 bits and actual rates.
`--offline` requires cached Zstd dependencies.

Production SDR → **Channels** opens the native receiver workbench. Build frontend/native, then use `./run.sh --fake --port 18139 --instance p3-dev` for an isolated synthetic service. Stop only that instance with `./stop.sh --instance p3-dev`. `python tools/e2e/native_multichannel.py` starts and cleans up its own independent service and checks actual IQ transport plus window lifecycle; it requires Playwright and websockets. Hardware RF validation remains separate.

The main Listen label shows the selected channel. Arrows beside Channels switch receiving channels; tuning and demodulation controls follow that channel without retuning on selection. The `＋` shortcut copies the current frequency and demodulation settings into an independent channel; at capacity it first asks which channel to replace. Spectrum and waterfall show other channels with colored passbands and share a faint dashed hover preview that clears on leaving the plots. Gate: `python tools/e2e/sdr_channel_shortcuts.py --artifacts /tmp/websa-channel-shortcuts`.

Entering SDR opens the waterfall with an equal spectrum/waterfall split; Preset restores this layout and selects C1. Waterfall clicks tune the selected receiver, and Zoom drags select the shared display window. The workbench groups reception on/off, duplicate and delete in one row, with duplicate/delete shortcuts in each channel’s More actions. Identical frequency, receiver audio settings, local volume and pan play through one route; moving the scratch receiver restores both independent routes automatically. This local rule leaves reception and recording tasks independent.

P4 monitoring/decoder gate: `python tools/e2e/native_multichannel_processing.py --artifacts /tmp/websa-p4-processing`.

Recovery and compact layout gate: `python tools/e2e/native_multichannel_recovery.py --artifacts /tmp/websa-p4-recovery`.
Chromium cuts and reconnects a real UID audio socket, checks resumed output PCM, and operates forms, compact popouts, Dock, retirement and slot reuse.
The production workbench enables IQ recording from actual capabilities and hides unavailable preset/scene actions and folds IQ and shared audio settings into disclosures. Compact windows retain editing controls. Direct Listen during follow switches to manual mixing while preserving the editor target.
The main Channels entry focuses the workbench while preserving independent popouts; Recall all windows and Dock remain explicit layout actions.
It owns a synthetic/replay service and checks actual browser PCM gain/pan, Solo/follow, FT8 output, SSTV pixels and
window lifecycles. The production Signal settings tab uses actual mode/provider capabilities. Local speakers controls
leave shared PCM unchanged; Shared audio gain explicitly configures backend output. See [MULTICHANNEL_DRAFT](MULTICHANNEL_DRAFT.md)
for semantics and the remaining shared-sample-block migration.

The workbench has three settings tabs: Signal settings, Recordings and Live outputs. A single mode selector includes None (IQ only); listening and IQ recording are independent actions. Selecting a channel edits it in place, while Separate window opens fixed settings. Signal info/Results opens an already running processor’s readings. Buttons carry action tooltips. Live outputs lists actual WebSocket connections; the folded IQ preview counts local delivery and does not save files.

Live workbench/Decode interaction gate: `python tools/e2e/native_multichannel_ui.py --artifacts /tmp/websa-multichannel-ui`.
It owns an isolated synthetic/replay service and recording directory. Chromium holds real mouse and Space presses across updates,
checks that live controls are never detached, retains drafts/search selection/scroll, reads actual FT8 messages and SSTV pixels,
and operates a 420px Pop out/Dock while backend IQ recording continues. It also checks actual AM waveforms, same-UID listening + IQ recording + filtered CSV/JSONL downloads and a decoded PNG downloaded from the popup. It does not qualify RF hardware.

P5 IQ recording gate: `python tools/e2e/native_multichannel_recording.py --artifacts /tmp/websa-p5-recording`. It isolates storage/service and uses an independent CRC32/libzstd reader to compare every archived block against actual network IQ bytes. It checks rate/tuning boundaries, drained stop, real mouse clicks across polling, 420px Pop out, Dock, recording after browser exit and recovery after SIGKILL. This validates synthetic input; RF and sustained disk throughput require separate acceptance.

`WEBSA_RECORDING_DIR` selects backend storage for the instance; the default is `$XDG_DATA_HOME/harogic-websa/recordings/<port>` (falling back to `~/.local/share`). An exclusive file lock prevents shared ownership; separate instances need separate directories. Startup streams checked prefixes and rebuilds JSONL indexes for interrupted/failed tasks without automatic resumption. The workbench exposes IQ start/stop, state and size with collapsible compression/clock details. Completed/failed IQ tasks expose Waveform / SigMF when the service advertises `iq_archive_read`; old-service/retired-UID tasks are accessible under Earlier recording tasks. Select a segment and sample range to inspect I/Q, seek, play the waveform at its actual rate or download standard SigMF. Each range is limited to 1,048,576 samples, 4096 blocks and 4 MiB provenance. Published-audio recording and bounded analogue file playback are available as described above; digital file decoding remains future work. The results window can download retained messages as filtered CSV or JSONL (all pages) and the selected picture as PNG. These are bounded browser-session snapshots; continuous recording remains a backend task. JSONL keeps receipt time separate from protocol UTC and preserves source timing strings. Unfinished pictures carry a partial filename; PNG export bounds RGBA memory to 20 MiB. Text tools appear only for text/table modes or retained text; audio-only modes show signal information without message filters or clear actions. AM/WFM reuse the original audio-scope renderer; CW shows native speed, tone and tuning offset; DRM shows station/programme and lock information.

Monitor sliders control the selected channel in this browser. Enable its Listen first; volume shows a percentage and pan a left/centre/right position. Pan needs stereo output. Editing another channel does not retarget the audible mix. Inactive, failed or stopped channels disable the sliders, and master/Solo mute remains explicit. Common instructions have moved out of the main workbench; action help is available on hover and source details are collapsed.

For large recorded IQ downloads, select Entire segment or enter a range and Download SigMF. Services advertising `iq_archive_export` use native browser downloads, with CF32 data files below the POSIX tar 8 GiB bound; waveform preview stays limited to 1,048,576 samples. Older services retain their original small-download capability. Regression: `python tools/e2e/native_multichannel_stream_export.py --artifacts /tmp/websa-stream-export --sigmf-python /path/to/sigmf-venv/bin/python` (`sigmf==1.6.1`). It checks an explicitly repeated offline 32 MiB fixture, real speaker gestures/PCM, cancellation, corruption, stalled TCP consumers, recording isolation and independent SigMF parsing. It does not qualify RF capture or the sound device.

Finished IQ tasks with multiple segments expose All segments when `iq_archive_collection` is available. This downloads a standard SigMF Collection archive while keeping each segment's rate, tuning and source clocks separate. Regression: `python tools/e2e/native_multichannel_collection.py --artifacts /tmp/websa-collection --sigmf-python /path/to/sigmf-venv/bin/python`. It checks actual native recordings against network IQ, a two-rate 32 MiB raw/Zstd fixture, direct official `SigMFCollection` loading after extraction, SHA-512 metadata references, per-record schema/sample validation, cancellation, later-member corruption, healthy recording, 420px downloads and two-segment SIGKILL recovery. The official 1.2.6 collection JSON Schema still admits only legacy tuples, whereas the specification recommends name/hash objects and sigmf-python 1.6.1 reads those objects. The gate records this upstream schema discrepancy without modifying the reader/schema; it validates each Recording with the official schema and directly verifies the Collection through the official reader.

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

The channel workbench uses service snapshots as confirmed state. Its session serializes receiver mutations,
coalesces repeated deletion, and scopes queued edits and late replies to a service generation. Deletion blocks
later starts for that receiver; restart or disposal invalidates old work. Drafts remain local to each view and
receiver. Main-panel listening recovery requires the matching command acknowledgment and a healthy snapshot;
a later explicit monitor choice takes precedence. `tools/e2e/multichannel_audio_settings.py` gates these controls,
burst deletion, compact layouts and playback recovery on an isolated fixture service.

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

Lifecycle tests require a locally built `backend/target/release/websad`. The Python CI job and `./test.sh` build it before pytest; the bundled runtime has its own host ABI requirements and is not the CI executable.

| Layer | Entry point and evidence |
|---|---|
| Native service and tools | `./test.sh`: release-mode backend library and binaries, standard-profile integration tests, Python tool tests, Ruff and frontend Vitest |
| DSP kernels | `make wasm-test`: release-mode native Rust kernel tests against reference fixtures, including optimized FT8 paths |
| Soapy patch | `make soapy-check-patch`: helper shell syntax and pinned patch applicability; requires curl + patch, no SDK |
| Soapy stream | `make soapy-check-stream SDK_ROOT=/opt/htraapi`: actual patched driver with real vendor headers, SoapySDR development files and stub HTRA; no vendor library or device |
| Local aggregate | `make ci`: kernel and service/tool/frontend tests, SDK-free Soapy patch check, version and fixture checks, DOM/registration/doc/architecture guards, frontend build |
| Browser fixtures | `make e2e-fake`: native fixture service, UI smoke, state regression and viewport geometry gate |
| Hardware | `make hw-test`: tinySA-driven smoke, command sweep and UI state regression |
| Performance | `make bench`: comparison with `tools/bench/bench_baseline.json` |

Read DSP ABI text messages immediately after the push that reports them. Later pushes replace this result list; the lifetime decode count does not retain message payloads.
Run backend library, binary and integration targets in separate Cargo invocations. Combining them can collide with this project's shared-library output artifacts. Integration tests use the standard test profile to match the harness's unwind strategy; production release artifacts use abort.

Install browser support with `python3 -m playwright install --with-deps chromium` and build
the frontend before browser runs. Fixture mode proves application behavior without the SDK;
it does not prove RF correctness, sustained capture or hardware recovery.

The density smoke scenario opens Trace → More before selecting Persist. It checks that
Off removes the density layer, saves `rta-halflife`, and survives mode changes; persistence
values are half-lives in seconds.

`make ci` and GitHub Actions are different entry points. The workflow has separate native,
Python tools (3.12 and 3.13), frontend, UI smoke and architecture jobs. It explicitly runs
`npx tsc --noEmit` and installs dependencies in clean environments. Its browser job runs
`ui_smoke.py` and `state_regression.py`; the local `make e2e-fake` also runs
`viewport_baseline.py`. Consult [Makefile](../../Makefile), [test.sh](../../test.sh) and
[ci.yml](../../.github/workflows/ci.yml) for the actual gates; neither a local aggregate nor
one CI job substitutes for hardware validation.

Both `make ci` and hosted CI run `soapy-check-patch`, including `bash -n` for the helper
and patch applicability against the pinned upstream sources. It requires `curl`, `patch`
and standard shell utilities. The separate `soapy-check-stream` gate also requires
`pkg-config`, `realpath`, a C++ compiler, SoapySDR development files and real vendor SDK
headers under `SDK_ROOT`. It links only SoapySDR and uses stub HTRA functions; it checks
the stream contract without qualifying RF acquisition or client applications.

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
