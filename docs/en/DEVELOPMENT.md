# Development Guide (v1.6.0)

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
| `measurements/framer.py` | **The only** frame encoder (FREQ/POWR/RTAF/AUDF) | Depend on hardware |
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

Add the encoder to `measurements/framer.py` -> add a retention policy row to `FRAME_POLICY` in
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
   client-side fallback timer) and then names the outcome (`applied`/`ok`/`no_signal`/`no_data`), so a
   refusal is never silent;
4. nothing about the control is disabled while the action runs - a mode that locks the user out of the
   very field it is adjusting reads as a bug (that is what the old tracking `Auto` did);
5. background *safety* correction is separate, always armed, and rate-limited (IF overload, a trace that
   left the display window) - never something the user has to switch on.

---

## 6. Testing strategy: what to assert at which layer

| Layer | Tool | Assert | Counter-example (do not do this) |
|---|---|---|---|
| Pure logic | vitest / pytest | Algorithms, state machines, contracts (i18n parity, frame fixtures, schema, slot semantics) | - |
| Session/device boundary | pytest + stub device | Result assembly, policy, error paths (**no vendor library needed**) | Connecting to the real device just to test logic |
| Protocol | Golden fixtures on both sides | Byte layout | Testing only one side |
| **End to end (no hardware)** | `make e2e-fake`: `ui_smoke.py` (rendering and wiring, 22 checks) + `state_regression.py` (parameter state-machine contract, 59 checks) on one fake service; runs in CI | Canvas pixels, controls reaching the backend, mode switches/tabs/waterfall/i18n/keypad, the peak list off its threshold slot; slots/in-flight/hand-off/Preset/reload/rapid switching; a one-shot Auto Scale (glow -> one step -> `ok` with no reconfiguration) | Asserting only datasets/counters; **relaxing an assertion to make the fake pass** (it weakens the bench run too - use `require_device=True` for device-only checks instead) |
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
| Acquisition timeouts / worker restart loops | `tail -50 /tmp/websa.log` (incl. the `faulthandler` dump), supervisor exit codes | DLL hang or native crash; the watchdog scales with sweep time |
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
  stack of every thread (`faulthandler`), and the supervisor's exit codes/restarts land in the same log.

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
| `./test.sh` failed on a clean checkout | Incomplete dependency declaration | Three files: runtime / dev / lock | CI installs in a clean environment |

---

## 13. Daily commands

```bash
make ci                      # all hardware-free gates (tests/static/contracts/guards/build)
make run | make stop         # start/stop the service (supervisor + worker)
make e2e-fake                # no hardware: ui_smoke (22 checks) + state_regression (59 checks) on the fake backend, same as CI
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
