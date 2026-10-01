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
- `measurements/`: measurement session objects (Std/RTA/Harmonic/PhaseNoise) + framer (frame encode/decode)
- `web/`: ws.py (command table) + http_api.py (STATUS/REST) + publisher.py (mode-aware scheduling)

## Key Design
1. **Session objects**: std/rta/harmonic/pnm share one interface; enter/exit restores configuration snapshots
2. **Model capability derivation**: DeviceCapabilities (SAN-45/60/90 frequency ranges), no hardcoding
3. **Frontend peak-preserving resampling**: backend sends device-native traces; frontend `resampleTrace` handles points (peak-preserving, no interpolation artifacts)
4. **Frame protocol**: 16-byte header (magic+ver+points+sweep_ms) + data; POWR forced to float32
5. **Client backpressure**: one sender per WS; retain FREQ/JSON and use latest-wins for POWR/RTAF
6. **Secure default**: loopback listener, token required remotely, static files confined to the build root
7. **Failure recovery**: SDK calls leave the asyncio thread; supervisor restarts after native crash/fatal timeout. The swept acquisition also declares `connected=false` on a run of bus errors (unplug) so the page never shows a frozen trace as connected; RTA/SDR keep their own in-place reconfiguration recovery. A worker link loop reopens a disconnected device and re-enters the active mode
8. **Mode-private settings**: SWP/RTA independently retain Center/Span/Ref/RBW/VBW/Sweep and actual values; mode changes issue one SET_MODE command. See [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md).

## Frame Protocol
| Type | Header | Data |
|---|---|---|
| FREQ | FREQ + ver(4) + points(4) + sweep_ms(f4) | float64 frequency axis |
| POWR | POWR + ... | float32 power dBm |
| IQBF | IQBF + ver(4) + seq(4) + samples(4) + rate(f8) + center_hz(f8) | channelized interleaved complex float32 baseband (the DDC's output); seq=0 flushes |

## WS Commands
CONNECT/STATUS/SET_PRESET/CAL_REFCLK/SET_FREQ/SET_REF/SET_RBW/SET_VBW/SET_SWEEP/
SET_POINTS/SET_SPUR/SET_WINDOW/SET_AMP/SET_REFCK/SET_REFCKOUT/SET_MODE/SET_RTA/SET_HARM/SET_PNM

## RTA Real-Time Spectrum (SWP/RTA modes)
- **Session** (`web_sa/measurements/rta.py`, `RtaSession`): built on the official SDK path
  `RTA_Configuration` → `BusTriggerStart` → `GetRealTimeSpectrum`; `acq=0.005`.
  Sustained SAN-90 push rate is about 105-129 fps, with a typical mode-to-first-frame delay near 0.57 s.
- **Push and display**: backend acquisition follows device Get rate; per-client latest-wins fan-out isolates slow
  connections; frontend RTA processing is throttled to about 33 Hz and SWP rendering to about 30 Hz.
- **RTAF frame** (`measurements/rta.py`): magic `RTAF` + `freq` (f8) + `spec` (f4) + `wfRow` (u2)
  + `stopHz`; each dtype is packed separately. The current trace uses the first spectrum in PacketFrame.
- **Multi-trace (T1-T4)**: per-trace accumulators (`rtaDisplays[4]`) with their own modes, rendered
  overlaid (own color + glow); **Freeze/View** is a standalone toggle button next to the trace-mode
  dropdown (VIEW = freeze accumulation; unfreeze restores the previous mode); Clear resets the active trace
- **Probability-density background**: default `frequency × 128` amplitude bins (64/96/128/192
  selectable), accumulated along the signal path. Persistence and grain are configurable; an O(n)
  histogram estimates noise floor and updates at about 33 Hz.
- **Waterfall**: SWP derives rows from the current trace at about 10 rows/s; RTA derives rows from live
  spec. The container replaces the Marker table slot and displays newest rows at the top.
- **Recovery**: after eight consecutive Trigger/Get failures, RTA reconfigures in place from retained
  mode-private settings up to two times. Persistent failure raises a fatal hardware error and the supervisor
  restarts the worker. STATUS exposes `rta_health`.
- **Known quirk**: `renderRta` wraps density+traces in an outer `save/clip(plotRect)` that must be
  `restore`d before drawing the bottom frequency row — otherwise the row (outside the plot) is clipped
  away and disappears after switching to RTA (fixed)

## SDR DSP Pipeline (branch `refactor/wasm-dsp`)

The demodulator and the audio chain run in a Rust/WASM module inside a Web Worker. The
channelizer does **not**: the coarse decimation and the tuning stay where the hardware already
does them — the analyzer's own DDC plus a software NCO in the backend — because moving them to
the browser did not fit the analyzer: the raw IQS stream is 2-31 MSps (measured: the transport
topped out around 2.3 MSps and the browser's own 129-tap FIR could not run the 3.9 MSps the
default settings ask for), so the AudioWorklet ring starved and the audio was a fixed periodic
puff that no tuning could change. What the browser receives now is the channelized baseband
(~48-63 kHz, about 0.4 MB/s), which is exactly the signal the Python demodulator reads.

The Python DSP is **not deleted**: it is the no-WASM fallback and the numerical reference, and it
only runs while a client subscribes to its audio (with the browser owning playback the backend
skips that chain entirely).

```
SAN-90 ──IQ──▶ Python backend ──┬─ channelized baseband (IQBF) ──▶ Web Worker ──WASM──▶ AnalogDemod ─▶ Audio DSP ─▶ AudioWorklet ─▶ speaker
  (IQS)        DDC + tuning NCO  └─ AUDF audio (fallback only) ──▶ (same worklet when the DSP cannot run)
```

### Stage map

Every stage of the diagram is a named module. The table is the contract: a stage that has no
path here does not exist yet.

| Stage | Module |
|---|---|
| SAN-90 IQ source | `web_sa/measurements/sdr.py` (IQS stream), `web_sa/hardware/sdk_bindings.py` |
| Python device / control | `web_sa/hardware/`, `web_sa/measurements/`, `web_sa/web/` |
| Channelizer (DDC + tuning) | `web_sa/demod/ddc.py` (vendor `DSP_DDC`), `web_sa/measurements/sdr.py` (`_chain_coarse`, `_mix`) |
| Baseband over WebSocket | `web_sa/measurements/framer.py` (`IQBF`), `web_sa/web/client_stream.py` |
| Baseband ingress | `frontend/modern/src/sdr/iqWorker.ts` |
| Worker DSP orchestration | `frontend/modern/src/sdr/iqStream.ts`, `frontend/modern/src/sdr/wasmPipeline.ts` |
| WASM boundary (raw ABI) | `frontend/modern/src/sdr/wasm.ts` ↔ `wasm/src/pipeline_abi.rs`, `wasm/src/abi.rs` |
| Analog demodulators | `wasm/src/analog/mod.rs` (the mode table and the detectors) |
| Digital demodulator (FT8) | `wasm/src/digital/ft8/mod.rs`, `wasm/src/digital/ft8/tables.rs` |
| Audio DSP (analog PCM only) | `wasm/src/audio/stages.rs`, `wasm/src/audio/dc_block.rs`, `wasm/src/audio/wiener.rs`, `wasm/src/audio/notch.rs`, `wasm/src/audio/blanker.rs`, `wasm/src/fft.rs` |
| Noise reduction (DeepFilterNet3, streamed) | `frontend/modern/src/sdr/dfnWorker.ts`, `frontend/modern/src/sdr/dfnWasm.ts`, `wasm-dfn/src/wasm.rs` |
| DFN artifact and model | `wasm-dfn/build.sh` → `frontend/modern/public/dfn/df_bg.wasm` (committed), `frontend/modern/public/models/dfn/DeepFilterNet3_onnx.tar.gz` (fetched at runtime, never embedded) |
| Path assembly and the separation rule | `wasm/src/pipeline.rs` |
| DDC kernels (kept, and the reference chain) | `wasm/src/ddc/nco.rs`, `wasm/src/ddc/fir.rs`, `wasm/src/ddc/resampler.rs`, `wasm/src/ddc/agc.rs`, `wasm/src/ddc/mod.rs` |
| Plugin registry (single source of truth) | `wasm/src/plugin.rs`, `wasm/src/plugin_abi.rs`, `frontend/modern/src/sdr/registry.ts` |
| Audio PCM output and the jitter buffer | `frontend/modern/src/audio/sdrAudioWorklet.js`, `frontend/modern/src/audio/sdrAudio.ts`, `frontend/modern/src/audio/sdrAudioWorker.ts` |
| Python fallback and reference | `web_sa/demod/` (unchanged), `tools/dsp_parity.py`, `tools/gen_dsp_fixtures.py` |
| WASM artifact build | `wasm/build.sh` → `frontend/modern/public/dsp.wasm` (committed) |

`tools/check_doc_paths.py` verifies every path in this table exists, so the map cannot describe a
module that was renamed or never written.

### DSP paths and the separation rule

DDC is the shared base layer: every analog and digital mode receives the same mixed,
filtered, resampled and levelled stream. Above it the two paths are deliberately separate:

- **Analog path** — `AnalogDemod` → Audio DSP → PCM. The audio chain (DC block, LPF, AGC,
squelch, STFT Wiener, adaptive notch, IF noise blanker) is tuned for the human ear.
- **Digital path / RAW** — `DigitalDemod` → decoder and visualization, with **no audio
enhancement at all**. A voice denoiser on an FT8 tone would destroy the information the
decoder needs, so the separation is enforced by an automated test (RAW output must be
bit-identical with the audio stage enabled and disabled), not by convention.

Adding a mode is adding a plugin to a registry: an analog mode implements the demodulator
interface (already: am, dsb, usb, lsb, cw, nfm, wfm, pm), a digital protocol implements the
decoder interface (FT8 first). The UI mode list is read from the registry so a new mode
cannot be forgotten in the panel.

### WASM boundary and build

- The crate has **no dependencies** (no wasm-bindgen, no wasm-pack); `cargo build --target
wasm32-unknown-unknown` is the whole toolchain.
- Arrays cross the boundary as pointers into the module's linear memory, block-wise; the hot
path allocates nothing per block.
- The built `.wasm` is committed so CI needs no Rust toolchain. `wasm/build.sh` rebuilds it
and fails if the committed artifact differs from the fresh build.
- `wasm/` also builds natively, which is what `cargo test` runs the kernels against.

### DDC numerics (the port must not change the audio)

The kernels are ports, not rewrites: `web_sa/demod/filters.py` (`design_lowpass`, `StreamFilter`,
`LinearResampler`, `Agc`) and `sdr.py::_mix` are the reference, and `tests/fixtures/dsp/` holds
their output for a committed IQ block, stage by stage (`mix`, `fir`, `dec`, `res`, `agc`).
`wasm/tests/ddc_reference.rs` compares the Rust kernels against those bytes within the tolerance
recorded in the fixture manifest (1e-5 absolute on an f32 full scale of 1.0, about -100 dBFS).
Measured worst case: mixer, FIR and decimation exactly 0; resampler and AGC about 3e-8.

`tools/gen_dsp_fixtures.py --check` is a CI gate (the reference cannot drift silently), and the
Rust side runs with `make wasm-test`. `wasm/tests/ddc_bench.rs` records the per-block cost —
native release: about 1.2 ms per 4096-sample block, 290 ns per complex sample, ~0.3 of real time
at 1 MSps — and fails if the chain becomes quadratic or starts allocating per block.

The vendor `DSP_DDC` is a hardware call that cannot be reproduced in software, so the Rust DDC
*takes its place*; the numeric reference is the software chain Python ran around it. The
channelizer now runs entirely on the backend, so the Rust DDC kernels are kept as the reference
chain (`wasm/tests/ddc_reference.rs` compares them with the same Python fixtures) rather than as
the browser's front end.

### Reference projects and licences

The DSP here is written against published technique, not copied from a project. The reference list
the architecture was designed from, and what was (and was not) taken from each:

| Project | What it informed | Code taken |
|---|---|---|
| [liquid-dsp](https://github.com/jgaeddert/liquid-dsp) (MIT) | Algorithm shapes: windowed-sinc design, NCO/phased-lock structure, RMS AGC, resampler | none — the kernels are written to match this repo's own Python reference (`demod/filters.py`), and are compared against it byte-for-byte |
| [tpt-dsp](https://github.com/tpt-solutions/tpt-dsp) | The Rust DSP-core layout: no-allocation block processing, state carried in `struct`s, `f32`/`f64` split between kernels and PCM | none |
| [sdr-web](https://github.com/kwakasa/sdr-web) | The browser dataflow: worker owns the socket, worklet owns playback | none |
| [Radioband](https://github.com/hightemp/radioband) | Project structure: one module per stage, a registry per plugin family | none |
| [pffft.wasm](https://github.com/JorenSix/pffft.wasm) | STFT performance expectations (why the FFT plans are cached per size) | none |
| [ft8_lib](https://github.com/kgoba/ft8_lib) (MIT) | The FT8 protocol: Costas pattern, Gray map, CRC-14 polynomial, LDPC(174,91) matrices | **The constant tables**, generated into `wasm/src/digital/ft8/tables.rs` by `tools/port_ft8_tables.py` (which records the source and licence); the decoder and the fixture encoder are written against the specification, and the encoder is verified tone-for-tone against it |
| [BrowSDR](https://github.com/jLynx/BrowSDR) (AGPL-3.0) | Architecture reading only | **none** — AGPL, deliberately avoided as a source |

Attribution rule: an algorithm may be re-derived from a permissive source, but nothing is copied
from an incompatible licence, and the numeric reference for every kernel is this repository's own
Python implementation. That is also why the parity tests compare against Python rather than against
another SDR application's output.

### Audio playback, NR and the fallback split

The last stage before the speaker is `frontend/modern/src/audio/sdrAudioWorklet.js`, and it is not a
plain ring buffer: PCM arrives in ~20 ms blocks paced by the analyzer's packets while the processor
runs on the sound card's clock, and the two differ by a fraction of a percent that nobody controls.
It therefore holds a steered jitter buffer (target 250 ms, ceiling 500 ms): the read pointer advances
by a ratio driven by the fill, so a persistent mismatch is absorbed by resampling (which keeps the
pitch right) instead of draining or filling the buffer, and past the ceiling the oldest samples are
dropped so a producer that outruns the clock cannot become growing latency. A retune flushes it, so
the previous station never plays on. `frontend/modern/src/__tests__/sdrAudioWorklet.test.ts` drives
the ring directly (a matched producer, ±2% drift, a flood, a reset).

Noise reduction is a panel control whose algorithm select picks between STFT Wiener (off/on,
light/medium/strong, reaching the WASM audio chain over `websa_dsp_demod_set_nr`) and
DeepFilterNet3 — an ML denoiser streamed through a dedicated worker (`dfnWorker.ts` → `dfnWasm.ts`
→ the vendored `wasm-dfn` runtime, one 480-sample hop per frame, model fetched at runtime and
handed to `df_create`) that has no strength knob, so the strength select hides while it is active.
The algorithm is a client-owned preference (`sdr.nrAlgo`, persisted with the other SDR
preferences) because the Python fallback path carries no NR of its own; the squelch slider reaches
the audio chain over `websa_dsp_demod_set_squelch`. The
default policy runs **no** enhancement stage, which is what the Python reference produces — the
stages that used to run by default included an adaptive notch that removed the signal itself when the
signal was a tone (measured: -33 dB on an AM test tone), and a `std`/browser A/B could not be
compared while it did.

The Python audio path is the fallback and the reference only: the frontend does not open `?audio=1`
while the browser DSP owns playback, and `web_sa/measurements/sdr.py` skips its demodulator when no
client subscribes to AUDF. The S-meter reading comes from the browser's own PCM in that case
(`dspLevelDbfs`).

### What is not wired yet

- FT8 is implemented end to end: decode only (the device has no transmitter), standard message
  types 1/2 only (no hashed callsigns, no free text, no contest/telemetry types), and the sync
  search covers the slot edge (about +/- 0.128 s) because FT8 is slot-synchronised — a wideband
  skimmer would need a full-slot search at roughly 15x the cost. FT4 and the other digital
  protocols are registry seams, not implementations.
- The Python DSP path is the audio source when the browser module is unavailable, and while any
  client subscribes to its audio; it is kept as the fallback and as the numeric reference.
- Four audio stages are installed but no policy enables them (`dc_block`, `lpf`, `agc`, `notch`):
  the demodulator already blocks DC, low-passes and AGCs, and the notch is a tone *rejection* tool
  rather than noise reduction — with a panel control missing, it is off. They are candidates for
  deletion rather than for wiring.

## Reachability of registration points (import side effects)

Some modules register themselves when imported (`render/spectrum.ts` registers the renderer,
the measurement modules register their tabs, the i18n namespaces merge). **After breaking the
cycles, "nothing imports it any more" is itself a failure**: the registration never runs and
the feature dies silently (this happened: no renderer -> `requestRender()` was a no-op -> a
blank canvas, with no exception and no console error).

Therefore:

- the entry point `main.ts` must pull those modules in with a **side-effect import**
  (`import './render/spectrum';`);
- `tools/check_registrations.py` computes import reachability from `main.ts`; a module that
  registers but is unreachable fails `make ci`;
- tests must assert the **user-visible result** (canvas pixels, DOM text), not an upstream
  counter or dataset: `dataset.rtaFrames` only says a frame was handed to the renderer, not
  that anything was drawn. The e2e now counts non-transparent canvas pixels after load and
  after switching to RTA.

## State ownership (parameters use slots, results use a store)

Frontend state falls into two kinds, and putting one in the wrong place is a bug class this
project hit repeatedly:

- **Parameters** (user-set, backend-confirmed): use the slots in `core/params.ts`; each
  parameter has exactly one owner. Only the STATUS handler calls `confirm()`, only a user
  action calls `set()`, readers use `get()`. `desired`/`confirmed`/`epoch` plus a TTL mean a
  just-set value is never reverted by an in-flight reply and a rejected command expires
  instead of sticking. The groups live in `ui/{freqState,refState,swpState,sdrState,triggerState,waterfallState,displayState,graphMode,displayRef}.ts`.
  A client-owned preference the backend never reports (display unit/offset, waterfall range,
  audio switch, ...) must be declared `authoritative: true` or it reverts when the TTL ends.
- **Results** (data produced by measurements/display): use `core/results.ts` - plain data with
  an explicit setter and no desired/confirmed semantics.

`core/model.ts` holds the types both sides share (a leaf module). `core/store.ts` keeps the
runtime state (connection, trigger runtime, current mode, ...) and re-exports the two groups,
so existing `S.x` / `S.setX()` call sites keep working.

In a hot path (loops running tens of thousands of times per frame) do not call a slot `get()`
inside the loop body: it performs a `Date.now()` plus pending checks, and the density
accumulation calling it hundreds of thousands of times per frame saturated the main thread
(1 Hz STATUS fell behind and the mode buttons toggled from a stale value).

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
- **Marker Tracking**: per-row On/Off toggles; initial assignment uses ranked unoccupied peaks, while later
  updates follow a nearby `marker.freq`. The same strategy runs in SWP and RTA, with relocation after axis changes.
- **smooth data source**: when enabled, fully based on smoothed curve (position/amplitude smoothed, matches display); when disabled, raw + Raw Anchor
- **Pk threshold**: value lives in a slot (`meas` group); readers no longer parse the input element. Auto = **one decision per measurement geometry**: fitted (median of a few frames of the trace's 99.5th percentile, −50 dB) when span/RBW/Ref/dB-per-div/points/centre change, latched otherwise so a signal swing cannot move it or reshuffle the peak table/marker search; a manual edit locks it until Auto is pressed again; no update when all markers are off

## Display and Control Design
- **Reference Level**: Manual configures the active SWP/RTA Profile. **Auto Scale is a one-shot action**
  (`AUTO_SCALE`), not a tracking mode: it computes one target from the newest trace (noise floor just
  above the bottom of the display window, >=10 dB of headroom for the peak, 5 dB quantisation, never
  below a learned IF-saturation floor) and applies it; applying is one step of a bounded closed loop,
  because the trace is not independent of Ref (the automatic attenuator re-picks with it, so the trace
  follows by ~half) - each settled frame re-measures and steps until the placement is good or the
  budget is spent; an already-good placement costs nothing.
  A **safety ranger runs always**, independently of any user setting, but only in the protective
  direction: IF overflow (-12) raises Ref one 5 dB step per second, and a peak grossly clipped above the
  top edge (>= 10 dB over) is raised once, rate-limited. Lowering is never automatic: a level that pushes
  the noise floor below the bottom edge is a display choice and is left alone (press Auto to re-fit),
  because undoing it would undo the button the user just pressed (`clipped` vs `below_window`). There is
  no signal-level gate: the anchor is the noise floor, so a noise-only trace is placed like any other
  (a weak signal used to be reported as `no_signal` instead of being placed). A settings change
  (span/centre/RBW/VBW/window/decimation) additionally arms the same bounded placement for the new
  geometry, stopped by its first good decision; a level the user typed is never reversed by it. Before Center/cross-mode retuning a Ref that a fit had lowered is raised to 0 dBm. Each
  reconfiguration waits 0.75 s before observations are trusted again.
  **SDR runs the same rule** (own tracker, fed by the panadapter frames): the backend fits, the client
  applies the reported target to its display scale, and the IQS level is only written when it is more
  than 3 dB off (that write interrupts the audio). The command carries the level on screen
  (`current_ref`), because in SDR the device level is not what the user sees. A manual Ref therefore
  holds: raising it is never undone, and only a grossly clipped peak or an IF overflow is corrected on the
  device's own initiative. When a correction does happen, the display follows it (the SDR client applies
  the reported target to its scale).
- **Periodic STATUS push (1 s)**: the publisher pushes full STATUS every second (aligned with the GNSS poll),
  keeping GNSS lock/time, refclk_out, calibration state fresh without a page refresh
- **GNSS detail popover**: click the GNSS indicator for full info (lock/sats/docxo/antenna/lat/lon/alt/UTC time)


## Trigger architecture (RTA device / SWP software)

- **RTA (device)**: the trigger is written into `RTA_Profile_TypeDef` (source/edge/mode, threshold,
  debounce, delay, pre-trigger, acquisition, re-trigger, trigger out) by `web_sa/measurements/rta.py` on
  every configuration. The fetch loop (`RTA_BusTriggerStart` + `RTA_GetRealTimeSpectrum`) yields no
  packets while armed and delivers one frame on a hit. `ui/trigger.ts` owns the panel, the canvas status
  chip (`FREE/WAIT/TRIG`) and the threshold line; `ui/triggerEvents.ts` decides in the **frame path** that
  an arriving packet *is* the capture (with a 500 ms settle window so the in-flight frames sent right
  after arming are not mistaken for it).
- **SWP (frontend)**: the swept engine has no level trigger. `dsp/levelCross.ts` (pure, unit tested)
  compares the same bin across two consecutive sweeps to produce a crossing event; `ui/swpTrigger.ts`
  arms and evaluates every trace; on a hit the frame path stops updating the canvas, which is what
  freezes the picture. The decision never depends on the display scale, so changing Ref cannot break it.
- One set of buttons (`Capture` / `Free Run` / `Esc`) dispatches to either implementation by mode.

- **The canvas status area is shared**: the top-right indicators are drawn by
  `render/statusStack.ts` - each feature pushes a status block during a pass (trigger chip, limit
  verdict) and the blocks are stacked, right-aligned, so a new indicator can never overlap an
  existing one.
- **Limits work in both modes**: the evaluation core (`dsp/limits.ts`) is display independent, and the
  RTA branch now calls the same `renderLimits()` as the swept path. The canvas reports `LIMIT PASS` or
  `LIMIT FAIL n` plus `worst +x dB @ f`, matching the pass/fail row in the panel.


## Virtual keypad (v1.3.4)

An optional on-screen pad for the numeric fields, disabled by default and remembered in
`localStorage['websa-keypad']`. `ui/keypad.ts` builds it once and fills it per field:

- a field with a unit button group (`UNIT_OPTIONS` in `core/units.ts`) also gets unit keys; a unit
  key writes the value with `dataset.edited = '1'` and calls the panel's own `setUnit()`, so the
  command is identical to clicking the panel button
- a field marked `data-keypad="text"` (the n-dB threshold list) is edited as plain text: its display
  line is a real input, so the mouse places the caret, backspace deletes one character there and
  `C` clears the entry; its `.` key becomes the `,` separator
- every other field commits through a `change` event, except the staged ones (`ref`, `points`,
  `rbw`, `vbw`, `harm`, `pnm`, `ampdbs`) whose own Set/Meas button is clicked instead
