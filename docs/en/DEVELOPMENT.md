# Development Guide (v1.7.3)

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

Shortest path: `make ci` (green without hardware) -> `make hw-test` with the device attached ->
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
| `measurements/framer.py` | **The only** frame encoder (FREQ/POWR/RTAF/VSAD/AUDF) | Depend on hardware |
| `measurements/results.py` | Pure result-payload builders (unit-testable) | Call the DLL |
| `web/commands.py` | Declarative command table: `CommandSpec` + `ParamSpec` + guard flags | Transport details |
| `web/ws.py` / `web/http_api.py` | Transport adapters (WebSocket / REST + STATUS serialisation) | Business branching |
| `web/publisher.py` | Acquisition scheduling: one step, backpressure, STATUS push | Branch on mode names (policy lives in the sessions) |
| `web/client_stream.py` | One writer per client, latest-wins, `FRAME_POLICY` | Parse frames |
| `web/recovery.py` / `supervisor.py` | `fatal()`/`EXIT_FATAL` and process-level restart | Business decisions |
| `web/jsonutil.py` | JSON boundary: NaN/Inf -> null, single serialisation | - |

### 2.2 Frontend (`frontend/modern/src/`)

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
  and `python3 tools/check_registrations.py` enforces it in CI.
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
4. Tests: the table-consistency test plus `tools/command_sweep.py` (hardware) covers the new command;
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

Add the encoder to `measurements/framer.py` (and its decoder to `frontend/modern/src/core/frames.ts`,
with a fixture from `tools/gen_frame_fixtures.py`) -> add a retention policy row to `FRAME_POLICY` in
`web/client_stream.py` (unknown types default to latest-wins) -> add the decoder to `core/frames.ts` ->
extend `tools/gen_frame_fixtures.py` to emit a golden fixture -> assert on both sides (Python asserts the
fixture matches its encoders, TS asserts the decode matches the manifest).

### 5.5 A new panel / piece of UI text

1. `ui/panels/<name>.ts` (panel actions) + a `data-action` binding; do not pile actions into `controls.ts`;
2. i18n: add the key to the right namespace in `core/i18n/dict.<domain>.ts` (**both en and zh** - the
   parity test enforces it);
3. New element ids: `tools/check_dom_ids.py` checks "read by TS but absent from index.html";
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
5. background *safety* correction is separate, always armed, rate-limited and PROTECTIVE-only (IF
   overload, gross clipping) - never something the user has to switch on, and never a reversal of a
   control the user just used. A settings change (span/centre/RBW/window/decimation) - and a press -
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
| Performance | `tools/bench.py` + baseline | **Comparable** frame-rate/latency/CPU numbers | Comparing while the device warns or leftover load runs |
| Hardware smoke | `tools/hardware_smoke.py` + tinySA | Levels/frame integrity with a real signal | - |

Two hard rules:

1. **"A test that would not fail when the feature is dead is not a test"** - always ask "would this
   assertion fail if the code never ran?"
2. **Proxy assertions must be labelled**: if the environment forces a proxy (e.g. no hardware), say in
   the test what it proxies and where the real user-visible assertion lives.

---

## 7. Performance and concurrency rules

- **Measure before optimising**: `make bench` (fixed configuration, clean device state, one client) against
  `tools/bench_baseline.json`. A single sample can produce a false alarm, so the bench re-measures once
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
- **Single-source version**: edit `pyproject.toml` -> `python3 tools/sync_version.py` (syncs
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
| Version in sync | `python3 tools/sync_version.py --check` | - |
| Frame fixtures match the encoders | `python3 tools/gen_frame_fixtures.py --check` | `tests/fixtures/frames/` |
| DOM id contract | `python3 tools/check_dom_ids.py` | - |
| Bilingual docs share the structure | `python3 tools/check_docs_parity.py` | - |
| Registration reachability | `python3 tools/check_registrations.py` | - |
| Architecture metrics do not regress | `python3 tools/quality/architecture_guard.py` | `tools/quality/baseline.json` |
| Performance does not regress | `python3 tools/bench.py --check tools/bench_baseline.json` | `tools/bench_baseline.json` |

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
| The Ref up arrow triggered Auto to pull the trace back down | The ranger corrected "noise floor below the bottom edge", which is a display choice, not a fault - it undid the button the user had just pressed | An automatic correction acts only in the PROTECTIVE direction (device overload, gross clipping) and never reverses a control the user just used; otherwise report and leave the explicit action to fix it | e2e `state_regression` 9c, `test_auto_reference.py`, `test_device_state.py` |
| `AUTO_SCALE` was silently refused in SDR while the display showed -60 dBm | `current_ref` is a DISPLAY value but was validated against the device Ref range | Validate a value against the bounds of its owner, not of the device it eventually influences | `test_ws_commands.py::test_auto_scale_accepts_a_display_ref_outside_the_device_ref_range` |
| **"The trace is not in the canvas, or only a sliver is under the bottom edge"** with a small span and no external signal, and Auto answered `no_signal` without moving | The fit still carried the signal gate (`peak - floor < 15 dB -> no_signal`) from the time it anchored on the PEAK. Anchoring on the noise floor needs no signal, so the gate refused exactly the case the floor anchor handles - and the `inside` classification hid it (0-3 dB under the edge counted as inside) | An auto-like action must act on what it actually anchors to: with the floor as the anchor, a missing signal is a placement like any other (`ok` when nothing needs changing, otherwise a level). A classifier that says `inside` and then refuses to move is a contradiction | `test_auto_reference.py` (noise-only under/at the edge and all three reported settings end inside the window), e2e `state_regression` 9e |
| A settings change (span / capture bandwidth) left the trace under the bottom edge until Auto was pressed; and an SDR fit could not go below the device Ref range even though its window can | The placement only ever ran on demand, so a new geometry inherited the old level; and the SDR target is a DISPLAY level but was clamped to the DEVICE row | A settings change invalidates the placement: arm exactly ONE re-fit for the new geometry (never a tracking loop, rate-limited, disarmed by its first decision), while a level the user typed stays theirs; and clamp each value against the bounds of its OWNER - a display target against the display range, the IQS write against the device range | `test_auto_reference.py` (one re-fit per geometry change, a manual level survives it, the SDR fit follows the display range and the IQS write does not), e2e 9e, `refAutoScale.test.ts` (an unpressed SDR decision moves the display) |
| **Auto adjusted to -20 dBm and the trace was still invisible** (preset, centre 20 MHz / span 1 MHz, no source) | The one-shot placement was OPEN loop: it computed a target as if the trace were independent of Ref, but the automatic attenuator re-picks with Ref, so the trace follows Ref by ~half and in jumps. Measured (SAN-90, 100 dB window): Ref -10/-20/-30/-40/-50 dBm gave attenuation 12/15/6/0/0 dB and a noise floor 12.3/13.9/11.0/2.7/-7.2 dB below the bottom edge - only -50 dBm shows the trace, and the computed -20 dBm left it 14 dB under the canvas. Worse, the single shot was consumed, so nothing retried | A placement whose input depends on its own output must be CLOSED loop: re-measure after each step and keep stepping until the placement is good, the budget is spent, or the user takes the level over. The first good placement ends it (never a tracking mode), and "it computed a plausible number" is not evidence that the target was reached | `test_auto_reference.py::test_the_refit_keeps_going_when_a_step_undershoots` (models the measured curve) and `::test_the_refit_gives_up_after_its_budget`, bench run (0 -> -20 -> -40 -> -50 dBm in ~6 s, then 12 s of no movement; a manual Ref held for 8 s) |
| The Level offset displaced the trace but not the amplitude numbers on the plot | Two layers drew the same quantity: `getY()` shifted the trace, while the axis labels and the marker readout printed raw device dBm | A value that is drawn in more than one layer must be converted in ONE place (`fmtAxisLevel`/`fmtReadoutLevel`); an axis is part of the display domain, not of the device domain | `peakThr.test.ts` (readout rule), e2e `ui_smoke` 2a2 (`dataset.yLabels` follows the offset) |
| "No signal to fit" stayed on screen after the signal appeared | The refusal was posted with a fixed 6 s hold, and only a *new decision* could replace it | A refusal is a statement about the measurement: record the observation it was based on and withdraw it when that observation moves (or when the first trace arrives), instead of relying on timing | `refAutoScale.test.ts` (withdraw on trace change / first trace), bench probe |
| Ref 30 dBm "jumped" to 27 with no explanation | The device clamps the level to its own maximum (which depends on the attenuation it picks) and echoes it; the UI followed the echo silently | When a device echo differs from the request, say so (`req` vs `actual`) - the same rule as announcing a refusal | `status.test.ts` (clamp notice, once per distinct pair), FAQ |
| The Ref range / RTA span lived as literals in three places | The capability row declared them, but `device.py`'s profile clamp, the Auto Ref loop and the client each kept their own copy | A device limit is published once (`caps` in STATUS, `config.ref_bounds`) and read everywhere; a test asserts the loop and the clamp follow the capability row | `test_config.py` (`ref_bounds`), `test_auto_reference.py` (target clamp follows caps), `test_device_state.py` (caps payload) |
| A test asserted a device value with one sample ("30 -> 27") | Test data was mistaken for the property under test: the message had to follow the reported value | Assert the RELATIONSHIP (several pairs, or a value that changes), never one recorded number; a hard-coded implementation must fail the test | `status.test.ts` (data-driven pairs + "follows the device when the limit moves") |
| SDR settings were re-derived from the swept view on every entry | The mode-entry path treated a mode switch as a fresh start (frequency from the sweep, demod from the band, decimate hard-coded), so the user's own tuning and listening setup were discarded | A mode's user settings are PREFERENCES: persist them and re-apply them on entry; the deliberate gesture (Shift+click / band preset) is what hands a frequency over, and only a first run derives defaults | e2e `state_regression` 5 (setup survives a round trip, Shift+click still hands off), `status.test.ts` (audio preference survives), FAQ |
| Shift+click into SDR landed on the wrong frequency | `listenAtFreq` set only the capture centre; the previous listen frequency stayed, and the backend re-centred the capture to chase it (measured: click at 216 MHz landed at 987 MHz) | "Listen here" means BOTH the capture centre and the listen frequency; a hand-off that sets half of a pair is a bug waiting for the other half | e2e `state_regression` 5 (`Shift+click hands that frequency to SDR`) |
| `./test.sh` failed on a clean checkout | Incomplete dependency declaration | Three files: runtime / dev / lock | CI installs in a clean environment |
| An unplugged analyzer froze the spectrum while STATUS still said `connected: true`, and replugging did not resume it | Disconnect was never detected: the acquisition path read a bus error as "no frame", and only a native crash/timeout reached the supervisor | A transport failure is a state the UI must see: a run of bus errors declares `connected=false` on the path that has no in-place recovery (swept), the scheduler stops stepping the dead handle, and a worker link loop reopens the device | `test_link_recovery.py` (link loop resumes the session; repeated -8 flips `connected`), `test_publisher.py` (no step while disconnected), `status.test.ts` (disconnect warning + repaint) |
| The bench RTA run answered a run of `-9` right after a `SET_FREQ` and the link watchdog closed the device | A swept-mode command issued `SWP_Configuration` while the RTA session owned the device, switching it out of RTA behind the live session (the same class Preset already had for SDR); the recoverable error run then looked like an unplug | A command may only reconfigure the mode that owns the device: in RTA the swept values are a stored preference (no `SWP_Configuration`), and a recoverable error run must not be escalated to a transport loss - link-loss detection lives where there is no in-place recovery | `test_ws_commands.py` (SET_FREQ/SET_DETECTOR in RTA never call `configure_swp`), bench `state_regression` + `hw-test` |

---

## 13. Daily commands

```bash
make ci                      # all hardware-free gates (tests/static/contracts/guards/build)
make run | make stop         # start/stop the service (supervisor + worker)
make restart | make status   # restart the service / pid, uptime, memory, CPU, log path+size, live link
make e2e-fake                # no hardware: ui_smoke (29 checks) + state_regression (72 checks) on the fake backend, same as CI
make hw-test                 # hardware: tinySA smoke + 24-command sweep + UI state regression (45 checks)
make bench                   # compare frame rate/switch latency/CPU against the baseline
python3 tools/bench.py --write-baseline tools/bench_baseline.json   # re-record (verify a clean state first)
python3 tools/command_sweep.py        # command-layer contract only (hardware)
python3 tools/e2e/state_regression.py # UI state regression only (hardware)
python3 tools/check_registrations.py  # registration reachability
python3 tools/quality/architecture_guard.py --baseline   # show the current architecture metrics
```

---

## 14. Known open work (pointer)

See `ARCH_REVIEW.md` §9.3: fake backend + e2e in CI (highest value now), Firefox e2e coverage, split
`DeviceState` per mode, the remaining unused exports in `controls.ts`, ESLint (blocked by the upstream
`typescript-eslint` / TypeScript 7 incompatibility), archiving the root scratch TODO, and per-command
STATUS trimming. **This guide evolves together with that list**: finishing an item updates both places.
