# Development Guide (v1.8.1)

> **This is not a baseline standard but a working agreement that keeps improving.** It records
> what actually went wrong during the 2026-09 architecture review and refactor, and the
> practices that were pinned down afterwards. **How to improve it: every time a defect escapes
> the tests, add one "lesson + rule + guard (test/script)"** - see the lesson ledger in §12.
> A rule may be replaced by a better one, but **every rule should have a guard**; otherwise it
> is just a slogan.

---

## 1. How to use this guide

Look at it at three moments:

1. **Before coding**: find the layer in the map in §2 and decide where the code belongs; for a new
   extension point read §4.
2. **While coding**: state ownership (§3), the feature checklists (§5) and the performance rules (§7).
3. **Before committing**: how to assert in tests (§6), commits and versioning (§9), and the guard list
   in §10 (`make ci`).

Shortest path: `make ci` (green without hardware) + `make e2e-fake` (the browser e2e, also
hardware-free — CI runs it as its own job) -> `make hw-test` with the device attached ->
`make bench` whenever the data path was touched.

---

## 2. Architecture and code ownership

### 2.1 Backend (`web_sa/`)

| Module | Responsibility | Must not |
|---|---|---|
| `hardware/sdk_bindings.py` | **The only** place that touches `libhtraapi` (incl. the hand-declared PNM/ADM/IQStream structs) | Contain business logic; other modules may not `import htra_api` (guarded) |
| `hardware/state.py` | Device state: `Swp/Rta/Sdr/Trigger` parameter groups plus the shared front-end/meta fields; grouped fields also keep flat aliases during the transition | Business logic; new code should use the groups (`state.rta.span_hz`) |
| `hardware/device.py` | Device lifecycle, buffers, one `step()`, session host | Control-loop decisions, protocol encoding |
| `hardware/auto_reference.py` | Auto Ref control loop (pure decisions, unit-testable) | Call the DLL directly |
| `measurements/base.py` | Session interface: `enter/exit/step` + acquisition policy (`acquisition_timeout/pacing/dedupe_freq/reconfigure`) + `is_ready/request_stop` + `health` | Know specific mode names |
| `measurements/{harmonic,phase_noise,rta,sdr}.py` | Per-mode sessions: configure the device, produce frames | Handle HTTP/WS |
| `measurements/framer.py` | **The only** frame encoder (FREQ/POWR/RTAF/AUDF) | Depend on hardware |
| `measurements/results.py` | Pure result-payload builders (unit-testable) | Call the DLL |
| `web/commands.py` | Declarative command table: `CommandSpec` + `ParamSpec` + guard flags | Transport details |
| `web/ws.py` / `web/http_api.py` | Transport adapters (WebSocket / REST + STATUS serialisation) | Business branching |
| `web/publisher.py` | Acquisition scheduling: one step, backpressure, STATUS push | Branch on mode names (policy lives in the sessions) |
| `web/client_stream.py` | One writer per client, latest-wins, `FRAME_POLICY` | Parse frames |
| `web/recovery.py` / `supervisor.py` | `fatal()`/`EXIT_FATAL` and process-level restart | Business decisions |
| `web/jsonutil.py` | JSON boundary: NaN/Inf -> null, single serialisation | - |

### 2.2 Frontend (`frontend/src/`)

| Directory | Responsibility | Note |
|---|---|---|
| `core/model.ts` | Shared types (leaf) | Imports no application module |
| `core/params.ts` | Parameter slots (`confirmed/desired/epoch` + TTL + persistence) | The single-owner mechanism |
| `core/results.ts` | Measurement/display **results** with explicit setters | No desired/confirmed semantics |
| `core/store.ts` | Runtime state (connection, trigger runtime, current mode, ...) | Parameters and results moved out; re-exports only |
| `core/{ws,frames,wsSend}.ts` | Protocol: JSON commands + binary frame decoding (`frames.ts` is the only frame definition) | New frames: §5.4 |
| `core/{units,level,frequency,fmt,i18n,theme,markerCommon}.ts` | Pure helpers/data | Leaves |
| `dsp/` | Pure DSP (smoothing, peak finding, limits, level crossing, stats, ...) | **Must have unit tests** |
| `render/` | Drawing: `spectrum.ts` (swept/RTA view hub), `registry.ts` (view registry), `redraw.ts` (repaint seam), `plot.ts` (geometry), canvas layers | Draw only, never mutate state |
| `ui/` | Interaction and state: `*State.ts` (slots), `panels/*` (panel actions), `controls.ts` (binding/orchestration), `measure.ts` (measurement state machine), `measureRegistry.ts` (tab registry) | Keep it thin |
| `meas/` | Per-measurement: result handling + overlay drawing + tab registration | Register via `measureRegistry` |
| `audio/` | SDR audio: worker + worklet + resampler | Never block the main thread |

### 2.3 Dependency direction

Only "upper layer -> leaf" is allowed. `madge --circular` is enforced at **0** by the guard;
`render/registry.ts`, `render/redraw.ts`, `core/params.ts`, `core/model.ts` and `core/i18n/` are
leaves - **never** import application modules from them (that creates a cycle immediately).

---

## 3. State ownership (the most important rule)

| Kind | Where | Rule |
|---|---|---|
| **Parameters** (user-set + backend-confirmed) | Slots in `ui/*State.ts` | Only the STATUS handler calls `confirm()`; only a user action calls `set()`; readers call `get()` |
| Client-owned preferences the backend never reports (display unit, waterfall range, audio switch) | Slots with `authoritative: true` | Without it the value **reverts when the TTL expires** (this really happened) |
| **Results/data** (traces, density, peak tables, measurement results) | `core/results.ts` | Explicit setters, no desired/confirmed |
| Runtime state (connection, trigger armed/hit, current mode) | `core/store.ts` | Even single writers go through a setter |
| Persistence | The slot's `persistKey` | Read/write localStorage in exactly one place |

**Hot-path rule**: do not call a slot `get()` inside a loop that runs tens of thousands of times per
frame (it does a `Date.now()` plus pending checks). The density accumulation called it ~600k times per
frame and saturated the main thread -> STATUS fell behind -> the mode buttons toggled from a stale value.
**Read once, outside the loop.**

**The rule exists because six failure modes really happened** (from the parameter-state
investigation; all fixed and pinned since): (1) reading a value during the intent window and using
it while confirming (preset then immediately entering SDR handed over the stale centre); (2) a
private cache drifting from the rendered value and then refusing to correct itself because the
delta looked "already applied"; (3) an incomplete global reset (Preset did not invalidate the
"last action" memory, so an old intent came back); (4) several writers for one parameter
(`displayRef` had seven, with no ownership rule); (5) persistence that was not closed-loop
(a preference was read but never written, so a reload lost it); (6) sentinel values meaning
"unset" (0 Hz is also a legal frequency). Check new state against these six.

---

## 4. Extension points and registration

- **Registrations must be reachable**: a module-scope registration (`setRenderer` /
  `registerViewRenderer` / `registerMeasurementTab`) only happens if the module is imported. After
  breaking the cycles, "nothing imports it any more" is itself a failure (it produced a blank canvas
  with no error at all). The entry point `main.ts` pulls those modules in with a **side-effect import**,
  and `python3 tools/checks/check_registrations.py` enforces it in CI.
- **One owner per display slot**: the waterfall panel and the measurement result table compete for the
  same slot, so a measurement turns the waterfall off and disables it, restoring the user's choice when
  it ends.
- **Resolve cross-module string identifiers through one helper**: the unit group key is `rta_center`
  while the input id is `input-rta-center`; the mismatch silently killed both the unit buttons and the
  virtual keypad. Always go through `inputForField()` / `fieldForInput()`.
- **Mode policy belongs to the session**, not the scheduler: timeout/throttling/de-duplication/reconfigure
  are session methods, so a new mode does not edit `publisher.py`/`ws.py` (the guard counts `mode ==`
  branches).

---

## 5. Adding a feature (checklists)

### 5.1 A new command or parameter (backend)

1. `web/commands.py`: add a `_REGISTRY` row and declare each field with a `ParamSpec` in `PARAMS`
   (type/bounds/unit/default/conditional required); cross-field rules go to `EXTRA_VALIDATORS`;
2. Bounds: use the capability table when it applies (never literals - the guard counts `maximum=<number>`);
3. Mode/session restrictions are table flags (`SWP_OWNED` / `SWP_ONLY` / `NOT_IN_SDR`), not `if mode == ...`;
4. Tests: the table-consistency test plus `tools/bench/command_sweep.py` (hardware) covers the new command;
5. Frontend: parameters use slots (§3), commands use `send({cmd: ...})`, and `/api/schema` exposes the
   new field automatically.

### 5.2 A new device model / capability

Change one row in `DeviceCapabilities.from_model` (band + limits). Do **not** hard-code model-dependent
numbers in command validation. Special cases such as `pnm_supported` should become capability queries
(`supports('pnm')`).

### 5.3 A new measurement mode (session)

1. Backend: `measurements/<name>.py` extending `MeasurementSession` with `enter/exit/step` plus the
   policy methods it needs (`acquisition_timeout`, `pacing`, `dedupe_freq`, `reconfigure`, `health`,
   `request_stop/is_ready`); register it in `_SESSIONS` (lazy import) in `measurements/__init__.py`;
   put result payloads in a pure module like `results.py` so they are testable without hardware;
2. Frontend: register a view renderer in `render/registry.ts`; if it has a tab, `registerMeasurementTab`;
3. Tests: a stub-device unit test (no DLL) plus an e2e assertion that entering it really draws;
4. If it conflicts with other display elements (waterfall, limits, tables), decide the **single owner**
   first.

### 5.4 A new frame type

Add the encoder to `measurements/framer.py` -> add a retention policy row to `FRAME_POLICY` in
`web/client_stream.py` (unknown types default to latest-wins) -> add the decoder to `core/frames.ts` ->
extend `tools/fixtures/gen_frame_fixtures.py` to emit a golden fixture -> assert on both sides (Python asserts the
fixture matches its encoders, TS asserts the decode matches the manifest).

### 5.5 A new panel / piece of UI text

1. `ui/panels/<name>.ts` (panel actions) + a `data-action` binding; do not pile actions into `controls.ts`;
2. i18n: add the key to the right namespace in `core/i18n/dict.<domain>.ts` (**both en and zh** - the
   parity test enforces it);
3. New element ids: `tools/checks/check_dom_ids.py` checks "read by TS but absent from index.html";
4. User-visible behaviour needs an e2e assertion (§6).

### 5.6 A new DSP/SDR block

Pure functions go to `dsp/` (or backend `demod/`), **unit-test first, wire up second**; no heavy
computation inside a render loop; every DLL call runs under the device lock on a `to_thread` worker with
a watchdog (§7).

### 5.7 Interaction rule: "auto-like" buttons are one-shot actions, never silent

Anything whose result costs a device reconfiguration (Auto Scale, a fit, a calibration) is a momentary
action with visible feedback, not a tracking toggle:

1. the press produces exactly one decision - compute, apply once, done;
2. an already-good state must cost nothing (no reconfiguration, no visible jump);
3. the button shows that work is in flight (glow/busy class driven by the backend's `adjusting`, with a
   client-side fallback timer) and then names the outcome (`applied`/`ok`/`no_data`), so a refusal is never
   silent. A placement does not refuse for lack of a signal: it anchors on the noise floor, so
   "no signal" is a level to place, not a reason to do nothing;
4. nothing about the control is disabled while the action runs - a mode that locks the user out of the
   very field it is adjusting reads as a bug (that is what the old tracking `Auto` did);
5. background *safety* correction is separate, always armed, rate-limited and PROTECTIVE-only (the
   IF overload warning, which stops frames altogether) - never something the user has to switch on,
   and never a reversal of a control the user just used. A trace the user pushed above the top edge
   is feedback, not a fault: the automatic raise for a grossly clipped peak is gone, and "the trace
   no longer fits" is what the Auto button is for. A settings change (span/centre/RBW/window/decimation) - and a press -
   is the one exception that may move a level the loop itself placed: it arms a BOUNDED closed-loop
   placement for the new geometry (each settled frame re-measures and steps again, until it is good,
   the budget is used up, or a level the user typed appears), while a level the user typed stays
   theirs. An OPEN-loop placement is not enough here: the trace is not independent of Ref (the
   automatic attenuator re-picks with it, so the trace follows by ~half, in jumps), and a computed
   target that lands short used to leave the trace off the canvas with nothing to retry it;
6. a mode's USER SETTINGS are preferences: persist them (`persistKey`) and re-apply them when the
   mode is entered. A mode switch is not a reset - SDR re-derived its frequency, capture bandwidth
   and demodulator from the swept view on every entry, so the setup the user had left there was
   silently discarded. The deliberate gesture (Shift+click / band preset) is what hands something
   over, and only a first run derives defaults;
7. a DEVICE LIMIT is published once and read everywhere: the Ref range, the RTA span/points, the
   trigger range. Validation, the SDK profile, the auto-reference loop and the client all read the
   capability row (`caps` in STATUS / `config.ref_bounds`), never their own copy - this model was
   once clamped by three different literals (found while auditing hard-coded device values);
8. a REFUSAL message is withdrawn the moment its reason stops being true, not when a timer runs out:
   "waiting for a trace" ends with the first trace, and a notice that records the observation it came
   from (peak/floor) is withdrawn when that observation moves. Posting a statement about the
   measurement and then leaving it on screen after the measurement changed is how a message becomes
   noise (reported: the trace was already drawn while the message stayed for its full 6 s). The
   no-signal refusal no longer has a producer (see item 3), so that path now exists for the
   vocabulary rather than for a live decision.

---

## 6. Testing strategy: what to assert at which layer

| Layer | Tool | Assert | Counter-example (do not do this) |
|---|---|---|---|
| Pure logic | vitest / pytest | Algorithms, state machines, contracts (i18n parity, frame fixtures, schema, slot semantics) | - |
| Session/device boundary | pytest + stub device | Result assembly, policy, error paths (**no vendor library needed**) | Connecting to the real device just to test logic |
| Protocol | Golden fixtures on both sides | Byte layout | Testing only one side |
| **End to end (no hardware)** | `make e2e-fake`: `ui_smoke.py` (rendering and wiring, 29 checks) + `state_regression.py` (parameter state-machine contract, 72 checks) on one fake service; runs in CI | Canvas pixels, controls reaching the backend, mode switches/tabs/waterfall/i18n/keypad, the peak list off its threshold slot; slots/in-flight/hand-off/Preset/reload/rapid switching; a one-shot Auto Scale (glow -> one step -> `ok` with no reconfiguration); a settings change re-fits once and never undoes a manual level | Asserting only datasets/counters; **relaxing an assertion to make the fake pass** (it weakens the bench run too - use `require_device=True` for device-only checks instead) |
| **End to end (hardware)** | Playwright + the bench | **User-visible results**: canvas pixels, DOM text, device state after a real click | `dataset.rtaFrames` (it only says a frame was handed to the renderer, not that anything was drawn) |
| Performance | `tools/bench/bench.py` + baseline | **Comparable** frame-rate/latency/CPU numbers | Comparing while the device warns or leftover load runs |
| Hardware smoke | `tools/bench/hardware_smoke.py` + tinySA | Levels/frame integrity with a real signal | - |

Two hard rules:

1. **"A test that would not fail when the feature is dead is not a test"** - always ask "would this
   assertion fail if the code never ran?"
2. **Proxy assertions must be labelled**: if the environment forces a proxy (e.g. no hardware), say in
   the test what it proxies and where the real user-visible assertion lives.

---

## 7. Performance and concurrency rules

- **Measure before optimising**: `make bench` (fixed configuration, clean device state, one client) against
  `tools/bench/bench_baseline.json`. A single sample can produce a false alarm, so the bench re-measures once
  before declaring a regression.
- **Backend**: every DLL call is serialised under the device re-entrant lock; slow calls go through
  `asyncio.to_thread` + a watchdog; timeouts/fatal errors go through `fatal()` (process-level recovery is
  the supervisor's job) - never swallow them.
- **Do not block the event loop**: acquisition runs on a worker thread; HTTP/WS must stay responsive.
- **Backpressure**: one writer per client; high-rate frames are latest-wins; `FREQ` is retained separately;
  audio uses a FIFO and `seq=0` flushes stale data.
- **Frontend hot paths**: read slots outside loops, avoid per-frame allocations, throttle rendering
  (RTA ~30 fps, SWP ~30 fps).

---

## 8. Failure handling and observability

- `fatal(reason)` is the only fatal-exit path (`EXIT_FATAL` -> supervisor restart); the exit code is a
  cross-process contract.
- **STATUS is the only diagnostic surface**: anything the UI or a script needs goes into STATUS (through
  explicit view methods such as `health()`/`auto_reference_view()`), not into logs.
- The canvas status stack (top-right) is shared: every indicator `pushStatus`es into it; **a state change
  must trigger `requestRender()`** (an overflowing IF sends no frames, so relying on the frame loop alone
  means the warning is never drawn).
- Logging: use `logging`, `log.exception` at boundaries; no `print` (except the startup banner in `main.py`).

---

## 9. Commits, versioning and release

- **Commit messages**: English, `type(scope): summary` (feat/fix/refactor/test/docs/chore/perf), stating
  what was removed/changed and why, not which files were touched.
- **Every commit must pass the tests**: that is what makes bisecting possible. Run `make ci` before
  committing; for multi-commit work verify with
  `git worktree add /tmp/wt <commit> && (cd /tmp/wt && python3 -m pytest tests/ -q)`
  (this once found a "test committed before its implementation"; the history was rebuilt).
- **Single-source version**: edit `pyproject.toml` -> `python3 tools/checks/sync_version.py` (syncs
  `package.json` and `index.html`); `--check` runs in CI.
- **Release**: bump the version -> build the frontend (**the service serves `dist/`, so forgetting the
  build means testing the old bundle**) -> `make hw-test` -> `git merge --no-ff` into master -> tag -> push.
- **Branches**: `feature/*`, `refactor/*`, `analysis/*`; keep docs and code in separate commits for review.

---

## 10. Guard list (what CI enforces, and how to change a baseline)

| Guard | Command | Baseline file |
|---|---|---|
| Backend tests | `python3 -m pytest tests/ -q` | - |
| Static checks | `python3 -m ruff check web_sa tests tools` | - |
| Frontend types/tests | `npx tsc --noEmit` / `npm test` | - |
| Version in sync | `python3 tools/checks/sync_version.py --check` | - |
| Frame fixtures match the encoders | `python3 tools/fixtures/gen_frame_fixtures.py --check` | `tests/fixtures/frames/` |
| DOM id contract | `python3 tools/checks/check_dom_ids.py` | - |
| Bilingual docs share the structure | `python3 tools/checks/check_docs_parity.py` | - |
| Registration reachability | `python3 tools/checks/check_registrations.py` | - |
| Architecture metrics do not regress | `python3 tools/checks/architecture_guard.py` | `tools/checks/baseline.json` |
| Performance does not regress | `python3 tools/bench/bench.py --check tools/bench/bench_baseline.json` | `tools/bench/bench_baseline.json` |

**Baselines only go down**: after an improvement run `--update` to tighten them. If a baseline really has
to be relaxed, the commit message must say why (an upstream dependency change, for instance); otherwise
nobody can tell "deliberate" from "silent regression".

---

## 11. Debugging playbook (by symptom)

| Symptom | Check first | Usual cause |
|---|---|---|
| Service will not start / device busy | `pgrep -af web_sa`, `fuser -v /dev/ttyACM0`, `tail /tmp/websa.log` | A probe process never exited; "one process owns the device" |
| **Blank canvas, no error at all** | Canvas pixel count (the e2e's `painted_pixels`), `check_registrations.py` | The renderer was never registered (module not imported); `dist/` not rebuilt |
| Mode will not switch / needs two clicks | Is STATUS arriving (1 Hz push)? Is an intent pending? | Main thread saturated (hot-path `get()`); pending TTL not expired |
| Acquisition timeouts / worker restart loops | `tail -50 /tmp/websa.log` (supervisor restarts) and `/tmp/websa.err` (the `faulthandler` dump), supervisor exit codes | DLL hang or native crash; the watchdog scales with sweep time |
| UI value disagrees with the device | STATUS `req` (requested) vs `actual` | A slot `desired` was never confirmed (rejected/in flight); treat STATUS as truth |
| Unit buttons / virtual keypad do nothing | What does `fieldForInput(input)` return? | Field key and input id spellings diverge |
| A warning is visible only for an instant | Is there an independent repaint trigger? | Drawing depends on the frame loop, and that state has no frames |
| Frontend change has no effect | Was `npm run build` run? Was the browser reloaded? | The service serves `dist/`; without a rebuild it is the old bundle |

- Frontend **diagnostic keys** (read these for auto-ref/scaling problems instead of adding logs):
  `#spectrum.dataset.sdrRef` (applied value) and `dataset.sdrRefDbg` (the whole auto-ref state:
  `noise/peak/range/ref/applied/before/shown` - `before`/`shown` make "the decision is what the user
  sees" checkable from outside, and the record is written for every decision, so "nothing needed
  changing" is visible too); `auto_ref.{result,target,seq,adjusting}` in STATUS is the backend half of
  the same story (`seq` tells a new answer from a sticky old one). SDR audio state lives in `dataset.sdrAudio`.
- Backend: `WEBSA_TRACE=1 ./run.sh` -> `grep '\[trace\]' /tmp/websa.log`; a native crash prints the
  stack of every thread (`faulthandler`) into `/tmp/websa.err`, while the supervisor's exit
  codes/restarts land in `/tmp/websa.log`.

---

## 12. Lesson ledger (each entry has a guard)

| Symptom | Root cause | Rule | Guard |
|---|---|---|---|
| The review claimed "77 missing i18n keys"; the real gap was one | The regex behind the claim ignored keys containing `-` and truncated at nested braces | A script that supports a conclusion must be checked on a sample with a known answer | i18n parity test; report §8.1 correction |
| `controls.ts` at 1548 lines with 14 cycles | Directory layering is not extension points; a hub module was reverse-depended on by everyone | Invert with seams + registries (§4) | `madge` baseline, panel split |
| Parameters with "several writers, overwriting each other" | No single owner | Parameters use slots (`confirm/set/get`); results use a store | `params.test.ts`, `status.test.ts` |
| **Blank canvas** (the user reported "unusable") | After breaking cycles nothing imported the hub, so its registration never ran; tests asserted proxies only | A registration side effect needs an entry-point import; assert user-visible results | `check_registrations.py`, e2e pixel assertions, `entryWiring.test.ts` |
| RTA unit buttons/keypad dead | Field key `rta_center` vs id `input-rta-center` | Resolve cross-module identifiers through one helper | `units.test.ts`, e2e 6c |
| Waterfall kept the slot while measuring | Two owners for one display slot | Single owner + disable and restore | e2e 7b, `status.test` case |
| IF-overflow warning invisible | It is drawn only in the frame loop, and an overflow has no frames | Repaint on state transitions | `status.test` case |
| Bench reported false 2.5x/1.9x CPU regressions | Leftover device configuration/signal; single-sample noise | Fixed configuration, clean state, re-measure before judging | bench determinism + `device_warning` |
| A commit with "test before implementation" | Tests were not run from a clean tree before committing | Every commit must pass the suite | per-commit worktree verification |
| Auto announced a new Ref while the canvas and the Ref box kept the old value | A client-side "the user owns the scale now" flag blocked the automatic correction from moving the display, so only the device level changed | An automatic correction must reach everything the user sees (display + box + hint), or it must not be announced; protect a manual level with a *new-decision* test, not by refusing to follow corrections | e2e 9a2 (display + box), `refAutoScale.test.ts` (follows an automatic correction; a sticky target is not re-applied) |
| Ref box showed a stale value after Auto | In SDR the box was projected from the client display ref *after* the backend level had been written, and in dB (relative) mode it was pinned to `0` | One owner per displayed value: the box shows the reference level in dBm (what Set sends); the relative axis top is a rendering detail | e2e 9a2, `status.test.ts` |
| Auto Scale messages moved the Ref buttons sideways | The message was written into the row it describes; moving it to the group head only moved the problem | Transient feedback goes to the canvas status stack (no width limit, next to the condition it answers); a displayed value has one owner | e2e `ui_smoke` 2a (`dataset.notice`, unchanged row boxes), `refAutoScale.test.ts` (notice TTL/generation) |
| The Ref up arrow triggered Auto to pull the trace back down | The ranger corrected "noise floor below the bottom edge", which is a display choice, not a fault - it undid the button the user had just pressed. The same argument then applied to the other direction, so the automatic raise for a grossly clipped peak went too | An automatic correction acts only where the DEVICE is in trouble (the IF overflow warning, which stops frames) and never reverses a control the user just used; a placement that no longer fits is the Auto button's job, not the background's. An observation alone moves nothing | e2e `state_regression` 9c + `ui_smoke` 2a3 (a clipped trace held across frames), `test_auto_reference.py`, `test_device_state.py` |
| `AUTO_SCALE` was silently refused in SDR while the display showed -60 dBm | `current_ref` is a DISPLAY value but was validated against the device Ref range | Validate a value against the bounds of its owner, not of the device it eventually influences | `test_ws_commands.py::test_auto_scale_accepts_a_display_ref_outside_the_device_ref_range` |
| **"The trace is not in the canvas, or only a sliver is under the bottom edge"** with a small span and no external signal, and Auto answered `no_signal` without moving | The fit still carried the signal gate (`peak - floor < 15 dB -> no_signal`) from the time it anchored on the PEAK. Anchoring on the noise floor needs no signal, so the gate refused exactly the case the floor anchor handles - and the `inside` classification hid it (0-3 dB under the edge counted as inside) | An auto-like action must act on what it actually anchors to: with the floor as the anchor, a missing signal is a placement like any other (`ok` when nothing needs changing, otherwise a level). A classifier that says `inside` and then refuses to move is a contradiction | `test_auto_reference.py` (noise-only under/at the edge and all three reported settings end inside the window), e2e `state_regression` 9e |
| A settings change (span / capture bandwidth) left the trace under the bottom edge until Auto was pressed; and an SDR fit could not go below the device Ref range even though its window can | The placement only ever ran on demand, so a new geometry inherited the old level; and the SDR target is a DISPLAY level but was clamped to the DEVICE row | A settings change invalidates the placement: arm exactly ONE re-fit for the new geometry (never a tracking loop, rate-limited, disarmed by its first decision), while a level the user typed stays theirs; and clamp each value against the bounds of its OWNER - a display target against the display range, the IQS write against the device range | `test_auto_reference.py` (one re-fit per geometry change, a manual level survives it, the SDR fit follows the display range and the IQS write does not), e2e 9e, `refAutoScale.test.ts` (an unpressed SDR decision moves the display) |
| **Auto adjusted to -20 dBm and the trace was still invisible** (preset, centre 20 MHz / span 1 MHz, no source) | The one-shot placement was OPEN loop: it computed a target as if the trace were independent of Ref, but the automatic attenuator re-picks with Ref, so the trace follows Ref by ~half and in jumps. Measured (SAN-90, 100 dB window): Ref -10/-20/-30/-40/-50 dBm gave attenuation 12/15/6/0/0 dB and a noise floor 12.3/13.9/11.0/2.7/-7.2 dB below the bottom edge - only -50 dBm shows the trace, and the computed -20 dBm left it 14 dB under the canvas. Worse, the single shot was consumed, so nothing retried | A placement whose input depends on its own output must be CLOSED loop: re-measure after each step and keep stepping until the placement is good, the budget is spent, or the user takes the level over. The first good placement ends it (never a tracking mode), and "it computed a plausible number" is not evidence that the target was reached | `test_auto_reference.py::test_the_refit_keeps_going_when_a_step_undershoots` (models the measured curve) and `::test_the_refit_gives_up_after_its_budget`, bench run (0 -> -20 -> -40 -> -50 dBm in ~6 s, then 12 s of no movement; a manual Ref held for 8 s) |
| The Level offset displaced the trace but not the amplitude numbers on the plot | Two layers drew the same quantity: `getY()` shifted the trace, while the axis labels and the marker readout printed raw device dBm | A value that is drawn in more than one layer must be converted in ONE place (`fmtAxisLevel`/`fmtReadoutLevel`); an axis is part of the display domain, not of the device domain | `peakThr.test.ts` (readout rule), e2e `ui_smoke` 2a2 (`dataset.yLabels` follows the offset) |
| "No signal to fit" stayed on screen after the signal appeared | The refusal was posted with a fixed 6 s hold, and only a *new decision* could replace it | A refusal is a statement about the measurement: record the observation it was based on and withdraw it when that observation moves (or when the first trace arrives), instead of relying on timing | `refAutoScale.test.ts` (withdraw on trace change / first trace), bench probe |
| Ref 30 dBm "jumped" to 27 with no explanation; the notice that was added to explain it was then found to be worse than the silence, and was removed on request | The device clamps the level to its own maximum (which depends on the attenuation it picks) and echoes it. Saying so produced a canvas message on ANY press near the end of the range, next to arrows that had been greyed out at the same limit - feedback for a bound, not for an event, and the level on screen is the echoed one either way | A limit is not an event: the box shows the level the device reports, the arrows stay usable and a press past the end is a silent no-op. A message is for something that happened to the MEASUREMENT (a refusal, a placement, an overload), never for reaching a bound - and the bound such a message would name has to come from the DEVICE (a vendor row or a measured behaviour), never from this project's own guess (see the "ref must be <= 30" ledger entry) | `status.test.ts` (a clamped pair is followed silently; the arrows are never disabled), e2e `ui_smoke` 2a3 |
| The Ref range / RTA span lived as literals in three places | The capability row declared them, but `device.py`'s profile clamp, the Auto Ref loop and the client each kept their own copy | A device limit is published once (`caps` in STATUS, `config.ref_bounds`) and read everywhere; a test asserts the loop and the clamp follow the capability row | `test_config.py` (`ref_bounds`), `test_auto_reference.py` (target clamp follows caps), `test_device_state.py` (caps payload) |
| A test asserted a device value with one sample ("30 -> 27") | Test data was mistaken for the property under test: the message had to follow the reported value | Assert the RELATIONSHIP (several pairs, or a value that changes), never one recorded number; a hard-coded implementation must fail the test | `status.test.ts` (data-driven pairs + "follows the device when the limit moves") |
| SDR settings were re-derived from the swept view on every entry | The mode-entry path treated a mode switch as a fresh start (frequency from the sweep, demod from the band, decimate hard-coded), so the user's own tuning and listening setup were discarded | A mode's user settings are PREFERENCES: persist them and re-apply them on entry; the deliberate gesture (Shift+click / band preset) is what hands a frequency over, and only a first run derives defaults | e2e `state_regression` 5 (setup survives a round trip, Shift+click still hands off), `status.test.ts` (audio preference survives), FAQ |
| Shift+click into SDR landed on the wrong frequency | `listenAtFreq` set only the capture centre; the previous listen frequency stayed, and the backend re-centred the capture to chase it (measured: click at 216 MHz landed at 987 MHz) | "Listen here" means BOTH the capture centre and the listen frequency; a hand-off that sets half of a pair is a bug waiting for the other half | e2e `state_regression` 5 (`Shift+click hands that frequency to SDR`) |
| `./test.sh` failed on a clean checkout | Incomplete dependency declaration | Three files: runtime / dev / lock | CI installs in a clean environment |
| An unplugged analyzer froze the spectrum while STATUS still said `connected: true`, and replugging did not resume it | Disconnect was never detected: the acquisition path read a bus error as "no frame", and only a native crash/timeout reached the supervisor | A transport failure is a state the UI must see: a run of bus errors declares `connected=false` on the path that has no in-place recovery (swept), the scheduler stops stepping the dead handle, and a worker link loop reopens the device | `test_link_recovery.py` (link loop resumes the session; repeated -8 flips `connected`), `test_publisher.py` (no step while disconnected), `status.test.ts` (disconnect warning + repaint) |
| The bench RTA run answered a run of `-9` right after a `SET_FREQ` and the link watchdog closed the device | A swept-mode command issued `SWP_Configuration` while the RTA session owned the device, switching it out of RTA behind the live session (the same class Preset already had for SDR); the recoverable error run then looked like an unplug | A command may only reconfigure the mode that owns the device: in RTA the swept values are a stored preference (no `SWP_Configuration`), and a recoverable error run must not be escalated to a transport loss - link-loss detection lives where there is no in-place recovery | `test_ws_commands.py` (SET_FREQ/SET_DETECTOR in RTA never call `configure_swp`), bench `state_regression` + `hw-test` |
| **"The signal is inside the highlighted band and FT8 still decodes nothing"**, and the same session recorded 5 decodes out of 41 "attempts" with no way to tell what happened | The panadapter drew the IF passband symmetric about the listen frequency, but the decoder reads 100..3000 Hz **above** the dial. Measured (Pluto transmitting at 411.0015 MHz, dial parked on the tone): 0 decodes in 5 slots while the overlay still covered the signal - the tones sit at 0..44 Hz, under the decoder's 100 Hz floor. The overlay was also free to drift from the decoder it points at, and did: it said 200 Hz at the lower edge while the decoder said 100 | What the display claims a hidden stage reads must be derived from *that* stage, not from a neighbouring one (`demodBandHz`: the decoder's own band for a protocol decoder, the IF passband only for an analog demodulator), and the two must not be free to diverge (a test reads the Rust constants back). The same rule covers the counters: a reset is not a search, so report them apart - `dsp_attempts` counted both, which hid whether 41 attempts were 41 searches or 20 searches and 21 thrown-away windows | `sdrDemodBand.test.ts` (symmetric for analog, above-the-dial for FT8, constants must match `wasm/src/digital/ft8/mod.rs`), bench HIL: 5 consecutive slots decoded at the correct dial, 0 decodes with the dial 49.7 kHz off |
| **"Adjusting the frequency or the level means typing into a panel"** - the reported wish was to push the axis labels the way a modern analyzer and every SDR panadapter does | The canvas had exactly one gesture family (marker placement, inside the plot) and the window could only be changed from the panel inputs, where each change costs a device reconfiguration | The axis label bands are grab areas (the plot geometry already reserves them: the frequency row under the graticule, the level labels to its right): dragging the frequency row pans the centre and the wheel zooms the span around the pointer, dragging the level labels pans Ref and the wheel zooms dB/div. The content follows the finger | `axisDrag.test.ts` (hit test, direction, one request per gesture), e2e `ui_smoke` P |
| **The SDR IF-overflow escape asked for a raise and then dropped it** | `nudge_out_of_overflow` queues a step, and the SDR branch of `apply_pending` consumed EVERY queue entry without writing (that is what makes a fit display-only). So in SDR the escape was a no-op: the level stayed where the saturation started, while the code comment and the docs both claimed "the ADC is still protected" | A display-only rule needs its exceptions named. An IF overflow is the DEVICE in trouble (and that path delivers no frames at all), so it is the one queue entry that must reach the front end; a fit is not | `test_auto_reference.py::test_an_sdr_if_overflow_still_reaches_the_device` (the raise lands, one reconfiguration, `sdr_ref_set`), `test_overflow_nudge_*` |
| **A drag on the axis labels during a measurement produced an alert per command** | While a harmonic / PNM measurement owns the device the control rail is greyed out (`body.meas-mode`) and the command layer refuses SWP-owned commands - but the CANVAS was not gated, so the gesture sent them and the refusal arrived as a popup. The client's own measurement flag could not help: it only knows about a measurement it started itself, and `STATUS.mode` ('harmonic'/'pnm') was dropped by `isGraphMode` | Everything mode-gated must read the mode the DEVICE reports: `deviceMode` keeps `STATUS.mode` verbatim, and the gesture is disabled while a measurement owns the device (the same condition the rail and the command layer use). A client-local flag is not a mode | `scaleDevice ...` `ui_smoke` 5 (a band drag during an API-started measurement: no movement, no popup), `graphMode.test.ts` |
| **A first switch into SDR left the trace below the bottom edge, and coming back from RTA changed the Ref the user had set in SDR** | The SDR entry re-fit on every visit: it judged the placement against the display ref a previous mode had left behind, and its target then overwrote the level the user had set. The mode's own level existed (`sdr_ref_level`) but nothing told the client whether it did | An entry places a level ONLY when the mode has none (`STATUS.sdr.ref_set`), and that placement is committed through the manual path so the display scale and the IQS level move together (measured: letting the backend write it and a later decision move only the display left them 15 dB apart). It takes up to SDR_ENTRY_PLACEMENT_ROUNDS rounds, because one step lands short - the trace follows Ref by ~0.5-1.2 dB per dB until the attenuation bottoms out - and the rounds stop at the first `ok`; committing through the manual path IS a manual takeover, which disarms the backend's own loop by design, so the client drives them. Once a level exists, entering the mode restores it and fits nothing. Two follow-ups came out of the same report: the acceptance band's lower bound moved from 2 dB to half a division (`FLOOR_INSIDE_MIN_DB = 5`) - 2.5 dB above the bottom edge is on the canvas but reads as a flat line lying ON the axis - and a fit answering `idle` after a manual write was found to discard a REAL decision (the sequence number is the authority, not the result string: an `idle` with a NEW sequence must be consumed, or the next answer is mistaken for a repeat and the entry stalls) | `refAutoScale.test.ts` (one placement per entry, then the user's level; the display scale follows the stored level once per entry), `test_auto_reference.py` (the fit never writes the device), bench: first entry places once at -15 dBm, re-entry keeps -77 dBm with no new decision |
| **The Ref settings are not independent per mode** - a level set in one mode showed up in another, and the SDR level was forgotten between visits | The vendor has one profile per mode (`SWP_Configuration`, `RTA_Profile`, `IQS_Profile`) and each carries its own `RefLevel_dBm`, but the backend kept SWP's and SDR's in ONE field (`state.ref_level`): the IQS profile was built from it and the swept view showed it. RTA happened to be right only because it had `rta_ref_level` of its own, and the SDR session "restored" the swept level on exit, which hid the sharing for a round trip while losing the SDR level | One field per mode's level, because the device has one per mode: `ref_level` (SWP), `rta_ref_level`, `sdr_ref_level`, with `REF_FIELD` mapping the tracker/STATUS to the right one. A mode's level is one of its preferences, so it survives leaving and re-entering | `test_http_api.py` (each mode reports its own Ref), `test_auto_reference.py` (the SDR tracker reads and writes `sdr_ref_level`), bench: SWP -33 and RTA -11 both survive a round trip through SDR, and the SDR's -77 comes back on the next visit |
| **Auto left a 13.825 MHz / 1 MHz trace BELOW the bottom edge** (reported: "the auto ref adjustment runs but the spectrum is still not visible"), and the fix for it risked the opposite complaint - a background loop that keeps re-placing the level ("the spectrum keeps breathing") | Two separate defects. (1) The closed loop stopped after its FIRST step: the branch for "a frame arrived inside SAFETY_INTERVAL_S" was written as `else: gave up`, so the loop disarmed whenever a frame showed up too early to step - the case it was written to wait for. Measured: 0 -> -20 dBm, floor ended 13.7 dB BELOW the bottom edge, and three steps were needed to reach the first level where the trace is on the canvas. (2) The fit's floor was the capability row's -50 dBm, which is our own guess (the SDK documents no Ref range; -140 dBm is programmed exactly) | A bounded loop must distinguish "waiting" from "done": waiting keeps the loop armed, only a spent budget, a good placement or a manual level ends it. And a placement is allowed anywhere a user may set the level (the display domain), with the learned IF-overflow floor - not an invented row - as the protection. Anti-oscillation is part of the contract, not an accident: geometry-only arming (a Ref write by the loop itself must not re-arm it - the geometry signature excludes the level), a 10 dB acceptance band, a 5 dB minimum change, one placement per geometry change, and the first good placement disarms | `test_auto_reference.py` (a frame inside the interval does not disarm the fit; a settled placement is not disturbed by a wobbling trace; the floor may go below the capability row while the ceiling does not), bench: settles at Ref -10 dBm and holds for 20 s while the floor wobbles 8-12.6 dB above the bottom, a 40-step drag leaves the level alone, three rapid Auto presses produce three decisions and no movement |
| **"The Ref resets to 0 whenever I change frequency"** - reported while exercising the widened Ref range, and it happened with the panel's own Set too, in every mode | `prepare_retune` lifts a low Ref to 0 dBm before a retune so a level a fit chose for the old band cannot saturate the IF on the new one. Its guard was `last_target is not None` - the loop's memory of a placement, which a manual takeover never cleared - so after ANY fit had once run in a mode, every later frequency change silently undid the user's own level. Nothing announced it (by design), which is why it read as "the Ref has a mind of its own" | The retune lift belongs to the level the FIT placed: a level the user set is theirs and stays (the IF-overflow escape is what protects a manual level, on the device's own -12 warning). `user_level` is the flag that says whose level it is, and `last_target` must not be read as "the loop owns this" | `test_auto_reference.py::test_prepare_retune_leaves_a_level_the_user_set` (a fit then a manual takeover: no lift; a fit that places a level again: lift comes back), bench probe (Ref -40/-90 preserved across an axis drag and the panel Set, in SWP, RTA and SDR) |
| **"Ref must be <= 30 / >= -50" in a popup, in every mode**, while the vendor's own software only hints when the IF saturates | The Ref range was this project's own invention: `FALLBACK_REF_MIN/MAX_DBM = -50/30` is the dataclass DEFAULT of the capability row, and the command validator, the profile clamp and the client's arrows all read that row as if the device had reported it. The SDK documents no Ref range at all (`RefLevel_dBm` is a plain double) and its only Ref feedback is `APIRETVAL_WARNING_IFOverflow (-12)`. Measured on the bench after the report: Ref -60, -90 and **-140 dBm are accepted and echoed exactly**, +35/+40 come back as **+27** (the device's own maximum, which depends on the attenuation it picks) | A device limit is only a limit if the DEVICE reports it. `caps.ref_min/max` is now the AUTO-PLACEMENT row only; a user level is bounded by the display domain (-160..+40, the widest a client can show), the profile write is sanity-clamped to that, and the device's echo decides the rest. In SDR the display scale stays client-owned (a new `sdr` display-ref source, so no ack is armed for a level the device was never asked for) and the IQS level is the level itself (the same plain `double`; the bench accepts -140 dBm there too, with the IQS frames still arriving) | `test_config.py` (placement row and write clamp are separate concerns), `test_ws_commands.py` (a user Ref outside the device row validates; the display domain still bounds it), bench probes on the SAN-90 (SWP -60/-90/-140 exact, +35 -> +27; SDR -80/-140/+35 with no dialog and a steady frame rate) |
| **After a wheel zoom the canvas kept panning while the mouse only hovered**, and the next release did not place a marker | The wheel path created its gesture session with the pointer marked DOWN, and only a mouseup clears that flag - a wheel has no mouseup, so the flag stayed set for the preview's whole lifetime (6 s). Everything gated on it then behaved as if a button were held: hover-moves kept panning (and committing), and the release that belonged to a marker placement was swallowed | Pointer state may only come from a pointer event: anything that can start a gesture WITHOUT a pointer (a wheel) must not claim one. `down` (the pointer is held) and `live` (something is ongoing) are two different facts | `axisDrag.test.ts` (`does not leave a wheel gesture holding the pointer state`, `commits a wheel zoom once, with no pointer state involved`), e2e `ui_smoke` P |
| A drag cannot follow the pointer with real requests (0.3-1 s per frequency change, ~1.9 s per Ref change) - and refusing to preview reads as "the drag does nothing" | The two naive extremes are a laggy drag (one request per pointermove, each restarting the sweep) and no feedback at all | **Nothing is sent while the pointer moves**: the plot is redrawn locally through the same mapping every renderer already uses (`getX`/`getY`), and ONE request goes out when the gesture settles (release, or 250 ms of stillness - never closer than 400 ms). The preview is expressed against the window the DATA on screen was measured in, so the confirming STATUS turns it into the identity by itself, and it is dropped when the drawn window and the display ref match the request - that is what keeps the trace from snapping back to the old window while the device is still reconfiguring | `axisDrag.test.ts` (nothing while moving, the preview survives until the frame catches up and then drops, one command per gesture), e2e `ui_smoke` P |
| The density map had no way to be switched off, and its persistence control was filed in the RTA frequency block - invisible in SDR, where the same layer is drawn | The fade gears were a value list with no off state, and the row sat where the density is *drawn* rather than where the setting is *owned* | A display setting lives with the display (the Trace panel, kept usable by `trace-keep`, visible in every mode) and has an explicit off: Off stops the accumulation AND the drawing (`dsp/rtaDensity.ts` returns null), which also removes the bins x points work per frame. An off state is a VALUE, so it must survive the `parseFloat(v) || default` pattern that silently turned Off back into "about Medium" | `rtaDensity.test.ts` (Off accumulates nothing and round-trips through storage), e2e `ui_smoke` Q |
| SDR opened on Clear Write although an IQ panadapter's noise floor is what an average smooths; and the trace mode was one global setting, so a trip to RTA threw the choice away | The trace mode is per-trace, in-memory state with a single factory default for every view | Each display FAMILY remembers its own trace mode: SDR opens on Average at the panel's own default depth (16), the swept/RTA views keep Clear Write, and whatever the user picked inside a family is what that family shows when it is entered again. Applied once, on a CONFIRMED mode change - from the per-frame STATUS loop it would overwrite the change the user just made | `traceFamily.test.ts` (entry default, memory across a round trip, the STATUS wiring applies it once), e2e `ui_smoke` R |
| The level axis means two different things: the absolute (dBm) display shows a device reference, the relative (dB) display pins its top to 0, so "drag the level axis" cannot mean the same value in both | The first cut refused the gesture in the relative display, which made a feature the user asked for half-available (and took the dB/div wheel with it) | A gesture on an axis changes what that axis SHOWS: panning the level axis moves the device reference in the absolute display and the level OFFSET in the relative one - and the offset is a client-side display value, so it is applied as the pointer moves (nothing to wait for, nothing to commit) | `axisDrag.test.ts` (the dB display pans the offset, sends nothing, and needs no preview layer) |

---

## 13. Daily commands

```bash
make ci                      # all in-process hardware-free gates (tests/static/contracts/guards/build)
make build | make frontend   # full build (WASM cores when stale) / frontend only
make wasm | make wasm-dsp | make wasm-dfn   # both WASM cores / DSP only / DFN only
make clean | make clean-all  # clean keeping deps + WASM caches / full clean
make e2e-fake                # the browser e2e on the fake backend (CI runs it as its own job)
make run | make stop         # start/stop the service (supervisor + worker)
make restart | make status   # restart the service / pid, uptime, memory, CPU, log path+size, live link
make e2e-fake                # no hardware: ui_smoke (29 checks) + state_regression (72 checks) on the fake backend, same as CI
make hw-test                 # hardware: tinySA smoke + 24-command sweep + UI state regression (45 checks)
python3 tools/e2e/readme_shots.py      # re-capture the README screenshots (hardware + tinySA/Pluto sources; --help)
make bench                   # compare frame rate/switch latency/CPU against the baseline
python3 tools/bench/bench.py --write-baseline tools/bench/bench_baseline.json   # re-record (verify a clean state first)
python3 tools/bench/command_sweep.py        # command-layer contract only (hardware)
python3 tools/e2e/state_regression.py # UI state regression only (hardware)
python3 tools/checks/check_registrations.py  # registration reachability
make wasm | make wasm-check            # Rust/WASM DSP core: build+commit the artifact / verify it
python3 tools/checks/check_wasm_artifact.py  # artifact hash + exports (stdlib only, CI runs this)
python3 tools/checks/architecture_guard.py --baseline   # show the current architecture metrics
```

---

## 14. Known open work (pointer)

- **The STATUS stream stalls while SDR is being entered**: measured on the bench, the backend reports
  `mode=sdr` after ~4 s but the client's first `mode=sdr` STATUS arrives at ~9-12 s, with a 2.8 s hole
  in the frames before it (the SDR session's `enter()` configures the whole IQS/DDC chain while the
  publisher waits). The screen therefore keeps showing the previous mode (and its scale) for several
  seconds after the switch, and the entry placement only starts once the client knows. Fix: publish
  STATUS (or a "mode is switching" notice) from the entry path instead of blocking the loop.
- **Phase noise cannot be entered on the fake backend**: `session_class('pnm')` returns the real
  `PhaseNoiseSession`, whose `_configure` talks to the vendor SDK through `self.dev.dev` - which
  `FakeDevice` does not have. An AttributeError reaches the client as "Device: command failed"
  (measured by the e2e that now listens for dialogs; RTA and SDR have fake sessions, harmonic
  happens to work because it only uses device methods). Fix: a `FakePnmSession` next to
  `FakeRtaSession`/`FakeSdrSession`, or a capability check in the session factory.
- See `ARCH_REVIEW.md` §9.3: Firefox e2e coverage, per-command STATUS trimming (P2-9), the three
low-priority candidates recorded there, and ESLint (blocked by the upstream
`typescript-eslint` / TypeScript 7 incompatibility). The fake-backend e2e in CI, the
`DeviceState` per-mode split and the scratch-TODO archive listed here earlier are done
(§9.1 A1/B1/C2). **This guide evolves together with that list**: finishing an item updates
both places.

---

## 15. Rust/WASM DSP core (build and artifact policy)

The real-time SDR DSP (DDC, demodulators, audio enhancement) is the Rust crate in `wasm/`, running
in a Web Worker. It has **no crate dependencies** and exactly one toolchain requirement:

```bash
rustup target add wasm32-unknown-unknown
make wasm-dsp       # cargo test + release build + publish frontend/public/dsp.wasm
make wasm-check     # rebuild and fail when the committed artifact differs (release gate)
```

Rules that keep it buildable without Rust everywhere else:

- `frontend/public/dsp.wasm` is **committed**, and `wasm/dsp.artifact.json` records its
  sha256, size, toolchain and export list. `./build.sh` never calls cargo, so a machine (and CI)
  without Rust still builds and serves the app.
- `tools/checks/check_wasm_artifact.py` (part of `make ci`) verifies the recorded hash *and* parses the
  module's export section with the stdlib alone: a stale, truncated or renamed artifact fails
  without a Rust toolchain.
- The release profile pins `lto`, one codegen unit and `strip`, so the build is byte-reproducible;
  `make wasm-check` compares bytes, not behaviour.
- The ABI is a pointer plus a length into the module's linear memory — no wasm-bindgen, no
  wasm-pack. `frontend/src/sdr/wasm.ts` wraps it and `__tests__/wasm.test.ts` asserts the
  exported signatures, the version gate and the view rules against the committed bytes.
- The crate also builds natively, which is what `cargo test` runs the kernels against.
- A view over the module's memory must be created **after** the last allocation: growing the
  memory detaches existing views, and a detached view reads as length 0 instead of throwing.

---

## 16. Hardware-in-the-loop results

Measured on the bench this refactor was verified against: **SAN-90** (9 kHz–9 GHz) with a **tinySA
Ultra ZS407** as the signal source. IQ was captured from the analyzer's own stream
(`tools/bench/hil_audio_check.py`) and processed through the **committed** `dsp.wasm`
(`frontend/src/__tests__/hil.test.ts`), with the Python reference run over the same capture
(`tools/bench/hil_reference_check.py`) so a weak number can be attributed.

| Check | Result |
|---|---|
| `make hw-test` (tinySA CW smoke + 24-command sweep + UI state regression) | 66 PASS, 1 FAIL |
| `tools/e2e/state_regression.py` | 65 PASS, 1 FAIL — **identical on `master`** (same check, same numbers): pre-existing, not a regression |
| AM tone (1 kHz, 50 % depth) through the browser DSP | tone 993.1 Hz, SINAD **28.6 dB**, THD **−36.2 dB** (Python reference on the same capture: 26.1 dB / −36.2 dB) |
| NFM tone (1 kHz, 6 kHz deviation, 25 kHz IF) through the browser DSP | tone 996.8 Hz, SINAD **9.1 dB**, THD **−6.5 dB** (Python reference: 8.7 dB / −6.5 dB) |
| CW carrier through the browser DSP | sidetone 764 Hz (pitch + residual carrier offset), level −14.0 dBFS, THD −92 dB; SINAD is phase-noise limited, which is the worst case for an unmodulated carrier |

Two things about those numbers. The browser chain is at or slightly above the Python reference on real
hardware (AM +2.5 dB, NFM +0.4 dB, identical THD), which is what says the port did not cost quality;
the absolute SINAD is set by the combined phase noise of the TinySA and the analyzer synthesizer, and
the reference shows the same floor. And the tone-quality numbers are measured with the enhancement
chain **off**: its adaptive notch removes the strongest single tone in the channel, which is correct
for a voice channel and removes a lone bench tone (the chain's own behaviour is covered by
`cargo test`, and the RAW-path separation test).

The failing state-regression check is `Auto Scale puts the whole trace inside the window after the
change` (ref −20.0 dBm, window −120…−20, floor −132.8 dBm): the fit leaves the noise floor about
13 dB below the window. It reproduces on `master` with the same numbers, which is why it is recorded
here rather than “fixed” by this refactor.

With a −25 dBm tone connected, one further check (`a settings change does not undo a level the user
set`) fails because the device's documented IF-overflow safety raises the reference to 0 dBm against
the hot input; it passes with the generator's output off.

Reproduce (the tone-quality numbers are measured with the enhancement chain **off**, because its
adaptive notch removes a lone tone by design, and the AM case needs the 6 kHz IF the table lists):

```bash
make hw-test
python3 tools/bench/hil_audio_check.py --modulation am --mode auto --ifbw 6000 --seconds 3
WEBSA_HIL_IQ=/tmp/hil_iq.json WEBSA_HIL_NO_CHAIN=1 npx vitest run src/__tests__/hil.test.ts
python3 tools/bench/hil_reference_check.py /tmp/hil_iq.json
```

For NFM add `--modulation fm --ifbw 25000 --deviation 6000`, and for CW `--modulation cw --ifbw 500`.
