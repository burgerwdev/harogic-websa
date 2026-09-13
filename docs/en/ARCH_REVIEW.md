# Architecture & Code Review (v1.5.5)

> Branch: `analysis/arch-review` (created from `master@03b592c`; this branch adds documentation only, no product code changed)
> Date: 2026-09-13
> Scope: the whole repository (backend `web_sa/`, frontend `frontend/modern/src/`, `tests/`, `tools/`, build and docs)
> Nature: **evaluation + recommendations**, no implementation. Every claim carries reproducible evidence.

---

## 0. Verdict

This is a codebase whose maturity is clearly above that of typical single-author projects of this size.
The layering intent is explicit, the documentation is thorough, the pure DSP layer is unit tested, and the
data plane (backpressure, framing, recovery) has been hardened against real hardware. All local quality
gates are green: pytest 87, ruff clean, vitest 115, `tsc --strict` passing.

The problems are not about "does it work" but about **evolvability**:

1. **No CI** — every gate depends on someone running `./test.sh` by hand;
2. **The frontend UI/orchestration layer has almost no automated guard rails** (all 115 tests target pure
   DSP/utility modules), even though that layer has the highest churn;
3. **The binary protocol is implemented twice** (Python + TypeScript) with no contract test, and bad
   frames are silently dropped;
4. **The "single DLL contact point" rule is already broken** (`rta.py` imports `htra_api` directly in three places);
5. **Two god modules**: backend `web/ws.py:_dispatch` (313-line if/elif chain) and frontend
   `ui/controls.ts` (1547 lines / 39 imports);
6. **14 circular dependencies** in the frontend, with `render/spectrum.ts` as the hub;
7. **77 i18n keys have no Chinese translation** (the trigger panel, virtual keypad, waterfall and limit
   lines fall back to English in the Chinese UI).

Scores (out of 10, subjective but grounded in the evidence above):

| Dimension | Score | Note |
|---|---|---|
| Feature completeness | 9 | SWP/RTA/SDR, measurements, triggers, limits; docs kept in step |
| Layering / boundary clarity | 6 | Clear directories, but real cross-boundary access on both sides |
| Cohesion / coupling | 5 | God modules + 14 frontend cycles + global mutable singletons |
| Data plane (protocol/backpressure/recovery) | 9 | latest-wins, separate FREQ retention, audio FIFO, supervisor recovery |
| Test effectiveness | 6 | Excellent on pure functions; missing protocol contract and UI orchestration |
| Engineering (CI/deps/version/release) | 4 | No CI; incomplete dependency declaration; version in 4 places; no CHANGELOG |
| Observability / diagnosability | 8 | `faulthandler`, structured STATUS, health fields, probe scripts |
| Secure defaults | 8 | Loopback validation, token, Origin allowlist, static path traversal guard |
| Documentation quality | 8 | Bilingual, records trade-offs and measured limits; no sync check |
| Performance / resources | 7 | Main bottlenecks handled; per-client re-serialization, blurry canvas scaling |

**The three things worth doing first**: add CI (including the i18n parity and protocol contract tests),
converge hardware access and the command layer, and give the frontend UI layer guard rails (frame-parser
unit tests first, then a mock-backend e2e).

---

## 1. Method and baseline

Method: read every core module, take static measurements (AST/regex scripts), and actually run all quality
gates. No dynamic hardware testing (no RF source on this machine); live SDR/RTA behaviour is taken from the
docs and unit tests.

Baseline (actually executed for this report, all green):

```
python3 -m pytest tests/ -q              -> 87 passed
python3 -m ruff check web_sa tests tools -> All checks passed
npm test (vitest run)                    -> 12 files / 115 tests passed
npx tsc --noEmit                         -> passing (strict: true)
```

Size (`git ls-files` based):

| Area | Lines | Files | Notes |
|---|---|---|---|
| `web_sa/` (backend) | 5,296 | 26 | Largest: `sdr.py` 1000, `device.py` 921, `ws.py` 604 |
| `frontend/modern/src/` (no tests) | 9,826 | 60 | Largest: `controls.ts` 1547, `spectrum.ts` 728, `i18n.ts` 676 |
| `frontend/modern/src/__tests__/` | 1,386 | 12 | Pure-function tests only |
| `tests/` (backend) | 1,299 | 11 | |
| `tools/` (probes + e2e) | 2,700 | 20 | 14 `sdr_probe/` scripts + 1 Playwright e2e |
| `frontend/modern/index.html` | 646 | 1 | 206 `id`s, 112 `data-action`s |
| `htra_api.py` (vendor) | 924 | 1 | Must not be modified |
| Git history | 221 commits | — | Current version 1.5.5 |

---

## 2. What is done well (pin this down before refactoring)

| ID | Strength | Evidence |
|---|---|---|
| S-1 | **Mature data plane**: one sender per client, latest-wins for POWR/RTAF, FREQ retained separately (a dropped power frame can never orphan a frequency axis), small audio FIFO, STATUS coalescing | `web/client_stream.py:25-160`; 6 tests in `tests/test_client_stream.py` |
| S-2 | **Real reliability engineering**: supervisor restarts only on native-crash/timeout codes with exponential backoff; acquisition watchdog scaled to sweep time; session snapshot/restore; a device-wide re-entrant lock serializes every DLL call | `supervisor.py:18-19,49-53`, `publisher.py:18-24`, `measurements/base.py:33-98`, `device.py:169-174` |
| S-3 | **Parameter-slot model** (`confirmed`/`desired`/`epoch` + TTL + one persistence entry point + scoped `resetAll`) kills a real recurring bug class, with 15 unit tests | `core/params.ts`, `__tests__/params.test.ts` |
| S-4 | **Testable pure DSP layer**: S-G smoothing, three-stage peak engine, parabola fit, limits, level crossing, channel measurements are pure functions with tests | `dsp/*.ts`, `__tests__/{dsp,limits,levelCross,channel,grid}.test.ts` |
| S-5 | **Secure defaults**: non-loopback listening requires a token (or explicit opt-in), Origin allowlist, `commonpath` static-path guard, `nosniff`/`DENY` headers | `config.py:validate`, `web/http_api.py:30-58,265-272`; 3 tests |
| S-6 | **Diagnosability**: `faulthandler` dumps the Python stack of the thread that crashed natively; STATUS exposes `rta_health`/`sdr.health`/`status_warning`; systematic probe scripts under `tools/sdr_probe/` | `main.py:76-79`, `http_api.py:60-67` |
| S-7 | **Documented trade-offs**: `ARCHITECTURE.md` / `MODE_STATE_FLOW.md` / `KNOWN_ISSUES.md` (23 measured limitations), bilingual | `docs/{en,zh-CN}/` |

---

## 3. Findings

Severity: **P0** = delivery reliability/correctness, do first; **P1** = structural, slows development
continuously; **P2** = hygiene/optimisation.

### P0

#### P0-1 77 i18n keys have no Chinese translation (trigger/keypad/waterfall/limits render in English)

- **Symptom**: `t()` falls back to English when the zh key is missing (`i18n.ts:171-179`), and the dictionary
  is split into a base literal (`i18n.ts:5-164`) plus eight `Object.assign(dict.en|zh, {...})` blocks (lines 219-677).
- **Evidence**: an automatic comparison (script below) yields `en=402` keys / `zh=325` keys, with `en-zh=77`
  English-only keys — the whole trigger panel (`trg_source`, `trg_level`, `trg_chip_wait`, `tip_trg_*`),
  the virtual keypad (`kp_ok`, `kp_drag`), waterfall (`wf_range`, `wf_auto`), the limits canvas
  (`limit_canvas_pass|fail`), `tip_version`, `export_csv`.
- **Related risk**: `type I18nKey = keyof typeof dict['en']` (`i18n.ts:166`) is evaluated **before** the
  `Object.assign` calls, so the 210 keys added there are **not in the type**; `avg` and `tip_gain` are
  duplicated between the base block and an assign block (later silently wins).
- **Impact**: Chinese users see mixed language; the key set has neither type nor test protection, so it will
  drift again.
- **Recommendation**:
  1. Merge into one declarative dictionary (`const dict = { en: {...}, zh: {...} } as const`) and drop `Object.assign`;
  2. Add `__tests__/i18n.test.ts` asserting `Object.keys(en)` equals `Object.keys(zh)`, no duplicate keys, and
     identical `{name}` placeholders on both sides;
  3. Type `t()`'s key parameter as `I18nKey` so tsc catches typos.

  Reproduce:
  ```bash
  python3 - <<'PY'
  import re
  src=open('frontend/modern/src/core/i18n.ts',encoding='utf-8').read()
  lines=src.split('\n')
  base_en=set(re.findall(r"'([a-z0-9_]+)':", '\n'.join(lines[4:86])))
  base_zh=set(re.findall(r"'([a-z0-9_]+)':", '\n'.join(lines[86:163])))
  add={'en':set(),'zh':set()}
  for lang,body in re.findall(r"Object\.assign\(dict\.(en|zh),\s*\{(.*?)\n\}\);", src, re.S):
      add[lang] |= set(re.findall(r"'([a-z0-9_]+)':", body))
  en,zh = base_en|add['en'], base_zh|add['zh']
  print(len(en), len(zh), 'en-only:', sorted(en-zh))
  PY
  ```

#### P0-2 The frontend UI/orchestration layer has no automated guard rails

- **Symptom**: all 12 test files target `dsp/` and `core/` (mostly pure functions); `render/`, `ui/`
  (except `railMath`), `meas/`, `core/ws.ts` and `audio/` have **zero unit tests**. The only end-to-end
  regression, `tools/e2e/state_regression.py`, needs real hardware plus a running service and is deliberately
  excluded from `test.sh` (`test.sh:20-26`).
- **Evidence**: `find frontend/modern/src/__tests__`; `tools/e2e/state_regression.py:31-36`
  (`from playwright.sync_api import ...`, needs a live device).
- **Impact**: the highest-churn files — `controls.ts` (59 commits), `index.html` (67), `core/ws.ts` (48) —
  have no automated regression at all; mistakes are only found by manual clicking.
- **Recommendation** (by cost/benefit):
  1. Unit-test the **frame parser** first (see P0-3): pure, dependency-free, highest payoff;
  2. Then add integration tests for `updateStatus()`/`processTraces()` ("feed STATUS JSON + synthetic frames →
     assert slots and store"), jsdom is already wired up;
  3. Finally add a **mock backend** (`web_sa/hardware/device.py` already has a clean mock boundary — replace the
     DLL entry points in `sdk_bindings`, or run a small aiohttp stub that only emits synthetic frames) so the
     Playwright e2e can run in CI.

#### P0-3 The binary protocol is written twice, with no contract test

- **Symptom**: magic/offsets/dtypes exist in both languages: `FREQ`/`POWR`
  (`measurements/framer.py:14-36` ↔ `core/ws.ts:372-385`), `RTAF`
  (`measurements/rta.py:510-522` ↔ `core/ws.ts:162-190`), `AUDF` (`measurements/sdr.py:76-78` ↔ `core/ws.ts`).
  The TS side **silently `return`s** on a length mismatch (`ws.ts:175,373,380`), so a bad frame shows up as
  "the picture stopped".
- **Evidence**: `tests/test_framer.py` only asserts dtypes (3 tests); the `ws.ts` parser has no tests.
- **Impact**: a single field-order or length change can silently break the display with no error and no log.
- **Recommendation**:
  1. Reduce the frame spec to **one source** (the table in `docs/API.md` plus a `protocol.json`/constants file)
     from which Python and TS derive their constants (at minimum generate the header struct);
  2. Add golden tests on both sides: Python emits bytes → stored as a fixture; a TS test parses that fixture and
     asserts the values; then the reverse;
  3. Make TS parse failures observable (rate-limited `console.warn` + a counter surfaced via `dataset`) for field diagnosis.

#### P0-4 Incomplete dependency declaration — a clean environment cannot run `./test.sh`

- **Symptom**: `test.sh` calls `python3 -m ruff`, `build.sh` calls `python3 -m fontTools.subset`, and the e2e
  needs `playwright`; none of the three are in `requirements.txt` (aiohttp/numpy/pytest/pytest-asyncio/pyserial only).
  ruff only appears under `[project.optional-dependencies] dev` in `pyproject.toml`.
- **Impact**: a new machine/contributor following the README (`pip install -r requirements.txt && ./test.sh`)
  is guaranteed to fail; a CI copied from the README would fail too.
- **Recommendation**: split into `requirements.txt` (runtime) + `requirements-dev.txt`
  (ruff/playwright/fonttools/pytest…), or point at `pip install -e '.[dev]'`, and update the README.

#### P0-5 No CI, and no single source for the version

- **Symptom**: no `.github/` and no CI configuration at all; the version is hard-coded in four places
  (`pyproject.toml:3`, `frontend/modern/package.json:3`, `frontend/modern/index.html:640`, release commit
  messages), there is no `CHANGELOG.md`, and releases follow the manual `chore(release): 1.5.5` pattern.
- **Impact**: version drift, forgotten test runs, forgotten `npm run build` (the project's own TODO already
  records "forgetting to build means testing the old bundle — happened in this session").
- **Recommendation**:
  1. Add minimal CI (GitHub Actions or a local `pre-push` hook): pytest + ruff + tsc + vitest — all green
     without hardware;
  2. Single-source the version (`pyproject.toml` as the source, injected into `index.html`'s `tip_version` at
     build time, or generated from `package.json` into a TS constant) and have the release script update
     everything plus a CHANGELOG section in one go.

### P1

#### P1-1 The backend command layer is misnamed: `ws.py` is the command bus, and it circularly depends on HTTP

- **Evidence**: `web/http_api.py:17` has a top-level `from .ws import CommandError, _dispatch, ...`;
  `ws.py:268,272` does `from .http_api import build_status` inside functions (to dodge the cycle). The script
  in Appendix B confirms the module-level cycle `http_api ⇄ ws`.
- **Impact**: the naming is misleading (changing the HTTP transport means editing `ws.py`); the cycle is only
  held together by function-local imports, which newcomers will trip over; `_dispatch` mixes validation,
  permissions, state writes, hardware calls and return codes, so it can only be tested end-to-end with stubs.
- **Recommendation**: extract `web/commands.py`:
  ```
  @command('SET_RBW', validate=..., modes=('std','rta'), session_exclusive=...)
  async def set_rbw(dev, data): ...
  ```
  The registry performs validation / mode exclusion / session exclusion centrally (today scattered across
  `_validate_command` and four `if`s in `_dispatch`); `ws.py` and `http_api.py` become pure transport adapters.

#### P1-2 Dispatch is a 313-line if/elif chain, with another 135 lines of validation

- **Evidence**: `web/ws.py:292-604` (`_dispatch`), `web/ws.py:105-240` (`_validate_command`),
  `web/ws.py:50-64` (`_COMMANDS` set).
- **Impact**: adding or changing a command means touching **three** places; validation and behaviour live in
  different functions, so parameter semantics drift (e.g. `SET_SWEEP`'s mode values are written twice, in
  validate and in dispatch).
- **Recommendation**: fold into the declarative registry from P1-1, with a per-command schema (the existing
  `_number`/`_choice` primitives are good, keep them). Add a table-driven test that asserts `_COMMANDS` and the
  registry cannot drift apart, instead of syncing them by hand.

#### P1-3 The "single DLL contact point" rule is already broken

- **Evidence**: `hardware/sdk_bindings.py:1-10` claims to be the only module touching `libhtraapi`, but
  `measurements/rta.py:96,366,390` does `import htra_api as T` and calls `T.dll.RTA_*`, `T.pointer`,
  `T.RTA_FrameInfo_TypeDef` directly; `sdr.py`/`ddc.py` reach the DLL through a local `T = sb` alias;
  `phase_noise.py` and `device.py` also call `sb.dll.*` (via re-export, which blurs the boundary further).
- **Impact**: swapping the library or mocking hardware requires editing several modules; a software-only
  simulation cannot be built by replacing a single layer.
- **Recommendation**:
  1. Add the missing re-exports (`RTA_FrameInfo_TypeDef`, …) to `sdk_bindings`; target: every other module only
     imports `from ..hardware import sdk_bindings as sb`;
  2. Add a constraint test (or an `import-linter` contract) asserting that no `web_sa` module except
     `sdk_bindings.py` imports `htra_api` or touches `dll.` directly;
  3. While there, replace `T = sb`-style aliases with explicit `sb.dll.X`.

#### P1-4 14 circular dependencies in the frontend, with `render/spectrum.ts` as the hub

- **Evidence** (script-detected, Appendix B):
  `meas/harmonic.ts ⇄ render/spectrum.ts ⇄ meas/phaseNoise.ts ⇄ ui/measure.ts`,
  `render/spectrum.ts ⇄ {dsp/peaks, meas/amplitude, meas/channel, meas/harmOverlay, meas/harmOverlay2,
  render/markerTable, render/peaklist}`, `dsp/traces.ts ⇄ dsp/normalize.ts`, `core/ws.ts ⇄ ui/controls.ts`.
- **Impact**: ESM cycle evaluation order is not controllable (an `import`-time binding may still be
  uninitialised), modules cannot be tested in isolation, and moving/splitting code is expensive.
- **Recommendation**: split `spectrum.ts` into (a) `render/orchestrator.ts` that only chooses a renderer per
  mode, and (b) measurement modules that **only export pure data computation and pure drawing functions** and
  never read back spectrum globals. Use `madge --circular` in CI as the acceptance gate: target 0.

#### P1-5 `ui/controls.ts` god module + string coupling to `index.html`

- **Evidence**: `controls.ts` is 1547 lines with 39 imports from 35 modules; `bindActions()` is 232 lines
  (`:1141`) and `bindCanvas()` 100 lines (`:1398`, silently returning via `if (!canvas) return`); `index.html` is 646 lines with 206 `id`s and 112
  `data-action`s; the frontend performs 295 DOM lookups with string literals.
- **Impact**: `index.html` is the most-committed file (67 times); renaming an id produces no compile error and
  fails silently at runtime (the `if (el)` / `?.` style hides it).
- **Recommendation**:
  1. Split `controls.ts` by panel domain (`ui/panels/{freq,refLevel,sdr,trigger,amp}.ts`) behind a shared
     `data-action` registry;
  2. Define a "required id" list and self-check at startup in `main.ts`: throw on a missing id (fail fast beats
     failing silently); a better long-term move is semantic attributes (`data-field="center"`) with ids reserved
     for anchors.

#### P1-6 Global mutable singletons are still the frontend's state backbone

- **Evidence**: `core/store.ts` is imported by **33 modules** and holds ~70 `export let`s plus 62
  `export function setX` mutators. The `params.ts` slot model currently covers only
  `freqState`/`refState`/`swpState`/`sdrState`/`displayRef`/`graphMode`. Trigger state
  (`swpArmed`/`trigHit`/`trigOverlay`), waterfall (`wfLoDbm`/`wfHiDbm`/`waterfallOn`), measurement results
  (`harm`/`pnmData`/`chanRes`/`m3dB`/`peakMarks`), `traces`/`markers` etc. are still bare mutable variables
  written from several modules.
- **Impact**: this is exactly the "multiple writers, no owner" bug class described in the `params.ts` header;
  the migration is half done, so it is unclear to newcomers which pattern to use.
- **Recommendation**:
  1. Keep migrating **parameters** onto `params.ts` (RTA centre/span, trigger threshold group, waterfall range);
  2. Do **not** push measurement results/traces into parameter slots — they are a "data store": give them
     `core/results.ts` with explicit `set/get/subscribe` and no desired/confirmed semantics;
  3. Document the rule in `ARCHITECTURE.md`: parameters use slots, results use a store.

#### P1-7 `build_status` reads internal state through private attributes

- **Evidence**: in `web/http_api.py:70-215` (146 lines): `getattr(dev, '_auto_ref', {})`,
  `getattr(dev, '_pending_auto_ref', None)`, `getattr(session, '_error_streak')`, `_recovery_attempts`,
  `_packets_ok/_err` (`:60-67`).
- **Impact**: the serialization layer is coupled to internals; renaming a private field raises nothing at
  compile time or in tests (`getattr` defaults swallow the error and simply report 0/empty).
- **Recommendation**: make everything externally visible explicit — `DeviceState.auto_ref_view()`,
  `Session.health()` (plus the existing `status_warning`) — and let `build_status` only assemble the payload.

#### P1-8 `DeviceState` god object + `HarogicDevice` god class

- **Evidence**: `DeviceState` (`device.py:51-153`) has ~100 fields spanning SWP/RTA/SDR/trigger/GNSS/
  calibration/measurement results; `HarogicDevice` (`device.py:159-921`) owns lifecycle, SWP profile
  construction, buffer management, the auto-reference control loop, GNSS queries, reference-clock calibration
  and session hosting. Auto reference is a hidden state machine (`_auto_ref` dicts +
  `_auto_ref_geometry_seen` + `_pending_auto_ref`, with `_observe_reference_peak_locked` at 76 lines).
- **Impact**: any change requires reading ~900 lines of context; tests can only cover fragments (the 13 tests in
  `test_device_state.py` already do their best).
- **Recommendation** (incremental, low risk):
  1. Extract `AutoReferenceController` first (inputs: trace + geometry + mode; output: the ref to apply) — it is
     naturally unit-testable and lifts the hardest state machine out of the I/O class;
  2. Then split `DeviceState` into `SwpParams`/`RtaParams`/`SdrParams`/`TriggerParams` sub-structures while
     keeping the STATUS shape identical (frontend unaffected);
  3. Leave `HarogicDevice` with lifecycle + session hosting + one `step`.

#### P1-9 Implicit session lifecycle, held together by a private flag

- **Evidence**: `measurements/__init__.py:16-24` has `make_session()` calling `dev.session.exit()` internally;
  `ws.py:528-539` sets `old_sess._ready = False` before switching and reads `sess._ready` afterwards to decide
  success.
- **Impact**: the enter/exit/ready triad has no explicit interface; failure handling relies on a convention
  (`_ready` is private and maintained separately by SDR and RTA).
- **Recommendation**: define `SessionManager.switch(name) -> Session` with `enter()/exit()/is_ready()` on the
  session; `SET_MODE` only calls the manager and raises `CommandError` on failure (the existing
  `mode_not_ready` becomes an interface contract instead of an attribute convention).

#### P1-10 Process-level suicide is scattered across business code

- **Evidence**: `os._exit(70)` appears in `web/publisher.py:69,89` and `web/ws.py:330,388`;
  `supervisor.should_restart()` depends on exit code 70 / negative codes.
- **Impact**: the exit code is a cross-process contract hard-coded in four business locations; "should exit"
  paths are hard to unit test; log format is inconsistent.
- **Recommendation**: centralise as `web/recovery.py: fatal(reason) -> NoReturn` (one log format, includes
  `last_error`, `os._exit(70)`); business code only calls it. Feed `should_restart` from a constant
  `EXIT_FATAL = 70`.

#### P1-11 Duplicated JSON sanitation + per-client re-serialization

- **Evidence**: `client_stream._finite_json` (`:14-22`) and `http_api._json_safe` (`:20-28`) have the same
  semantics; `ClientStream.publish_json` (`:87-101`) runs `json.dumps` **per client**
  (N clients = N serializations + N recursive finiteness walks).
- **Impact**: negligible today with few clients, but a latent cost that only appears at scale, and the two
  copies can drift (one changes the NaN policy, the other does not).
- **Recommendation**: extract `web/jsonutil.py`; have the publisher expose `broadcast_json(obj)` that serializes
  once and reuses the string across client queues.

### P2

| ID | Issue | Evidence | Recommendation |
|---|---|---|---|
| P2-1 | `store.ts` touches the DOM at **import time** (`document.getElementById('spectrum')`), so importing has side effects and tests require jsdom | `core/store.ts:34-36` | Add `initStore()`/lazy getters; inject `canvas/ctx/W/H` at init |
| P2-2 | 58 exported `controls.ts` symbols have no external reference (API surface pollution); `noUnusedLocals/Parameters=false` hides unused locals | script in Appendix B | Drop unnecessary `export`s; enable `noUnusedLocals` and clean up incrementally |
| P2-3 | No frontend lint/format (no ESLint/Prettier config); style is convention-only | `package.json` scripts are dev/build/preview/test | Add ESLint (typescript-eslint) + Prettier and wire them into CI; start style rules as warnings |
| P2-4 | Canvas is fixed at 860×480 but CSS scales it (`width:100%`) with no `devicePixelRatio` backing store → blurry on wide/HiDPI screens | `index.html:76`, `style.css:219-221` | Size `canvas.width/height` from the container and DPR (keep logical-pixel drawing coordinates); make `store.W/H` dynamic accordingly |
| P2-5 | No sync check for the bilingual docs; lengths already differ (`ARCHITECTURE` en 126 / zh 118 lines; `KNOWN_ISSUES` 62/51) | `wc -l docs/{en,zh-CN}/*.md` | Add a lightweight CI check that the two sides have the same **section headings** (wording may be translated, structure must match) |
| P2-6 | The 27 KB scratch file `TODO-frontend-state.md` lives in the repo root, excluded only via `.git/info/exclude` (local-only) | file header comment | Archive finished parts under `docs/dev/` (or delete); turn open items into issues / `docs/dev/ROADMAP.md` to avoid the "docs say undone, code says done" drift (section 4 of this report is an instance) |
| P2-7 | Uneven backend test coverage: `harmonic.py`, `phase_noise.py`, `main.py`, `logging_setup.py` have no direct tests | test name inventory | Add tests for harmonic/PNM result parsing and parameter clamping (no hardware needed: feed synthetic structs) |
| P2-8 | Legacy helper and probe scripts mixed in | `config.py:153-154` `fit_span` self-described as a "legacy helper"; 14 scripts in `tools/sdr_probe/` | Delete the helper once confirmed unreferenced; keep probes where they are but add a `tools/README` describing purpose and hardware needs |
| P2-9 | Every command rebuilds the full STATUS (146-line dict + recursive finiteness), wasteful for high-rate commands | `ws.py:274`, `http_api.py:70` | Echo only fields relevant to the command (or reuse the last serialization) — note the frontend relies on full STATUS for slot confirmation, so check the protocol first |

---

## 4. Refactor roadmap

Principle: **build guard rails before touching structure**; `./test.sh` and `tsc` must be green at the end of
every phase, and each phase should be independently mergeable.

### Phase 0 — Freeze the baseline (0.5–1 day, zero risk)

| Task | Deliverable | Acceptance |
|---|---|---|
| Add CI (pytest + ruff + tsc + vitest) | `.github/workflows/ci.yml` or a `pre-push` hook + README note | Green in a clean venv |
| Complete dependency declaration | `requirements-dev.txt` (ruff/playwright/fonttools/pytest*) | `pip install -r requirements.txt -r requirements-dev.txt && ./test.sh` passes |
| i18n parity test | `__tests__/i18n.test.ts` | Test fails first (exposing the 77 missing keys) → passes after filling them in |
| Protocol golden-test skeleton | Python-generated fixtures + TS assertions (FREQ/POWR/RTAF/AUDF) | All four frame types agree across languages |
| Circular-dependency gate | `madge --circular` output (14) recorded as a **baseline** (may only decrease) | Baseline count in CI |
| Single-source the version | Build-time version injection + CHANGELOG template | `index.html` no longer hard-codes the version |

### Phase 1 — Low-risk consistency (2–4 days)

Merge i18n into one typed dictionary; remove the dead exports from P2-2; lazy-init the `store` DOM access
(P2-1); centralise `fatal()` (P1-10); de-duplicate `_finite_json` and serialize once for broadcast (P1-11);
turn `build_status` into an explicit view API (P1-7).

Acceptance: behaviour-preserving refactor; `test.sh`/`tsc`/e2e all green.

### Phase 2 — Backend command layer and hardware boundary (1–2 weeks)

1. Add the `web/commands.py` registry (name/validation/mode constraints/session exclusion/handler) and reduce
   `ws.py`/`http_api.py` to transport (P1-1, P1-2);
2. Complete the `sdk_bindings` RTA re-exports, ban `import htra_api` elsewhere, add the constraint test (P1-3);
3. Extract `AutoReferenceController` with unit tests (P1-8 step 1);
4. Make `SessionManager` explicit (P1-9).

Acceptance: the existing 87 backend tests pass **without changing assertions** (import paths only); a new command
is one registry entry plus one table-driven test; the constraint test proves no out-of-layer DLL access.

### Phase 3 — Frontend decoupling (2–3 weeks)

1. Break up the `spectrum.ts` hub (P1-4) until `madge --circular` reports 0;
2. Split `controls.ts` by panel and add the required-id self-check (P1-5);
3. Migrate the remaining parameters to `params.ts` and measurement results to `core/results.ts` (P1-6);
4. Add frame-parser and `updateStatus` integration tests (P0-2 steps 1–2);
5. Canvas DPR/resize (P2-4).

Acceptance: `madge --circular` = 0; vitest covers the new parser/state tests; the hardware e2e stays green.

### Phase 4 — As needed

Mock backend + Playwright in CI (P0-2 step 3); split `DeviceState` per mode (P1-8 step 2); doc structure check (P2-5).

---

## 5. Quick wins (half a day each, very low risk)

1. Add ruff/playwright/fonttools to a dev requirements file (P0-4) — otherwise the README's test steps are wrong.
2. `__tests__/i18n.test.ts` + fill in the 77 Chinese keys (P0-1).
3. Re-export `RTA_FrameInfo_TypeDef` etc. from `sdk_bindings` and replace the three `import htra_api as T`
   lines in `rta.py` with `_sb` (P1-3; purely mechanical).
4. Remove the `export`s in `controls.ts` that nothing outside references (P2-2).
5. `supervisor.EXIT_FATAL = 70` + a centralised `fatal()` (P1-10).
6. Archive the finished parts of `TODO-frontend-state.md` (P2-6) — this review already found that its
   "still to do: SWP/RTA migration" contradicts the code (`swpState`/`freqState`/`displayRef`/`graphMode`
   are all slot-based now).

---

## 6. Non-goals (explicitly not recommended)

1. **Do not adopt a frontend framework/state library** (React/Vue/Redux…). Native DOM + `params.ts` already
   expresses the single-owner model this project needs; a rewrite costs far more than it returns and would
   discard the existing e2e contracts.
2. **Do not write UI snapshot tests for coverage's sake.** Prefer protocol contract tests and state-machine
   unit tests; assert *observable behaviour* in Playwright (the existing e2e direction is right — extend it,
   do not replace it).
3. **Do not modify `htra_api.py`** (vendor file, HAROGIC copyright). All adaptation belongs in `sdk_bindings`.
4. **Do not split processes / build a multi-process service without a concrete requirement.** `KNOWN_ISSUES.md`
   item 1 already defers "Web/SDK process separation" to the VSA phase; the current `os._exit` + supervisor
   design is a **deliberate** engineering trade-off, not a defect.
5. **Do not unify naming/directories for tidiness alone.** Prioritise problems with concrete costs (cycles, god
   modules, boundary violations); leave pure style to the linter.

---

## Appendix A: Metrics

| Metric | Value |
|---|---|
| Backend Python lines / files | 5,296 / 26 |
| Frontend source lines / files (no tests) | 9,826 / 60 |
| Frontend test lines / files | 1,386 / 12 (115 cases) |
| Backend test lines / files | 1,299 / 11 (87 cases) |
| Longest backend functions | `web/ws.py:_dispatch` 313, `measurements/sdr.py:step` 193, `measurements/rta.py:_configure_locked` 170, `web/http_api.py:build_status` 146, `web/ws.py:_validate_command` 135 |
| Longest frontend functions | `core/ws.ts:connectWS` 301, `ui/controls.ts:bindActions` 232, `core/ws.ts:updateStatus` 170, `render/spectrum.ts:renderRta` 113 |
| Frontend circular dependencies | 14 |
| Backend circular dependencies | 1 (`web/http_api ⇄ web/ws`) |
| Cross-module import edges | 274 (75 modules) |
| `store.ts` importers / mutable exports / setters | 33 / ~70 / 62 |
| `index.html` ids / data-actions | 206 / 112 |
| DOM lookups (string literals) | 295 |
| i18n keys | en 402 / zh 325 (77 missing) |
| Default-branch commits / version | 221 / 1.5.5 |

## Appendix B: Reproduction scripts

```bash
# 1) All gates
python3 -m pytest tests/ -q && python3 -m ruff check web_sa tests tools
(cd frontend/modern && npx tsc --noEmit && npm test -- --reporter=dot)

# 2) Frontend circular dependencies + import edges
python3 - <<'PY'
import os, re, collections
root='frontend/modern/src'
files=[os.path.join(dp,fn) for dp,_,fns in os.walk(root) for fn in fns if fn.endswith(('.ts','.js'))]
g=collections.defaultdict(set)
def res(b,s):
    p=os.path.normpath(os.path.join(os.path.dirname(b),s))
    return next((c for c in (p+'.ts',p+'.js',os.path.join(p,'index.ts')) if os.path.exists(c)),None)
for f in files:
    for m in re.finditer(r"from\s+['\"](\.[^'\"]+)['\"]", open(f).read()):
        r=res(f,m.group(1))
        if r: g[f].add(r)
color={}; st=[]; cyc=[]
def dfs(n):
    color[n]=1; st.append(n)
    for m in sorted(g[n]):
        if color.get(m,0)==0: dfs(m)
        elif color.get(m)==1: cyc.append(st[st.index(m):]+[m])
    st.pop(); color[n]=2
for f in files:
    if color.get(f,0)==0: dfs(f)
print(len(files),'modules,',sum(len(v) for v in g.values()),'imports,',len({frozenset(c) for c in cyc}),'cycles')
for c in sorted({frozenset(c) for c in cyc}, key=len, reverse=True):
    print('  '+' -> '.join(os.path.relpath(x,root) for x in c))
PY

# 3) Longest functions
python3 - <<'PY'
import ast, os
for dp,_,fns in os.walk('web_sa'):
    for fn in fns:
        if fn.endswith('.py'):
            p=os.path.join(dp,fn); t=ast.parse(open(p).read())
            for n in ast.walk(t):
                if isinstance(n,(ast.FunctionDef,ast.AsyncFunctionDef)) and n.end_lineno-n.lineno+1>=100:
                    print(n.end_lineno-n.lineno+1, f'{p}:{n.lineno}', n.name)
PY

# 4) Exports never referenced by another module (frontend API surface)
#    See P0-1 / P2-2 for the script: parse export names, then count named/namespace usages.
```

## Appendix C: Relationship to the existing TODO (important)

The "**still to do: SWP/RTA migration (the only remaining item)**" section in the root
`TODO-frontend-state.md` (27 KB, local-only) is **out of date**: in the code, `ui/freqState.ts`,
`ui/swpState.ts`, `ui/refState.ts`, `ui/displayRef.ts` and `ui/graphMode.ts` all use `core/params.ts`
(commits `83ed00c` for SWP parameter slots and `ee2f987` for the graphMode/displayRef rebuild).
This review follows the **code as it is**; consider archiving that file per P2-6 so future reviews are not misled.

Items that genuinely remain (with their IDs in this report):
- The multi-writer problem for `displayRef` is resolved, but **trigger/waterfall/measurement results** are still
  bare globals (P1-6);
- Migration of the SWP/RTA tuning parameters is **done**; on the RTA side only the trigger parameter group and
  some `store` copies remain (P1-6, item 1).
