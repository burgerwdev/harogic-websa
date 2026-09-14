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
7. **The i18n dictionary is structurally unsafe**: of 452 English keys exactly **one** lacks a Chinese
   translation (the first version of this review claimed 77; that was an extraction-script bug, see
   §8.1-4). The real problems are thirteen `Object.assign` blocks, an `I18nKey` type covering only the 194
   base-literal keys, five duplicated keys and no consistency test at all.

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

#### P0-1 i18n: structural defect plus one missing zh key (the first version's "77" was wrong)

- **Symptom**: `t()` falls back to English when the zh key is missing (`i18n.ts:171-179`). The dictionary is
  assembled from a base literal (`i18n.ts:5-164`) plus **thirteen** `Object.assign(dict.en|zh, {...})` blocks.
- **Corrected evidence**: extracted with brace matching (not the flawed regex of the first version, which
  ignored keys containing `-` and truncated at nested braces, turning one missing key into 77): the tree
  before the refactor had **en 452 / zh 451**, the only gap being `sdr_snap_tip` ("Set as active marker").
  After the refactor it is 452/452 (§9.1).
- **The actual defects (independent of the missing-key count)**:
  1. `type I18nKey = keyof typeof dict['en']` (`i18n.ts:166`) is evaluated **before** the `Object.assign`
     calls, so the type covers only **194 of 452** keys: a typo in a key added by a later block compiles;
  2. five keys are defined twice (`avg`, `tip_gain`, `tip_lang`, `tip_theme`, `tip_trg_edge`) and the later
     definition silently wins at runtime;
  3. there is no consistency test, so bilingual drift is invisible: most `ui/` text is resolved at runtime
     through `data-i18n`, and a missing key only shows up as English text in the Chinese UI.
- **Impact**: copy defects cannot be caught in CI and the type protection is ineffective.
- **Recommendation** (implemented in §9.1): merge into one declaration so `keyof typeof dict.en` covers every
  key, add `__tests__/i18n.test.ts` (equal key sets, no empty values, identical `{placeholder}`s, type
  coverage of keys added by the old blocks) and add the one missing translation.

  Reproduce (corrected script; keys may contain `-`, and blocks must be delimited by brace matching):
  ```bash
  python3 - <<'PY'
  import json, re, subprocess
  src = open('frontend/modern/src/core/i18n.ts', encoding='utf-8').read()

  def block(s, i):                      # string-aware brace matching
      depth = 0; j = i; q = None; esc = False
      while j < len(s):
          c = s[j]
          if q:
              if esc: esc = False
              elif c == '\\': esc = True
              elif c == q: q = None
          else:
              if c in ('"', "'"): q = c
              elif c == '{': depth += 1
              elif c == '}':
                  depth -= 1
                  if depth == 0: return j + 1
          j += 1

  segs = []
  m = re.search(r'const dict = \{', src)
  segs.append(src[m.start():block(src, src.index('{', m.start()))] + ';')
  for m in re.finditer(r'Object\.assign\(dict\.(en|zh),', src):     # legacy layout only
      i = src.index('{', m.end()); segs.append(src[m.start():block(src, i)] + ');')

  js = '\n'.join(segs) + """
  const flat = {};
  for (const l of ['en', 'zh']) flat[l] = Object.keys(dict[l]);
  process.stdout.write(JSON.stringify(flat));"""
  r = subprocess.run(['node', '-e', js], capture_output=True, text=True)
  if r.returncode: raise SystemExit(r.stderr[:400])
  d = json.loads(r.stdout); en, zh = set(d['en']), set(d['zh'])
  print('en', len(en), 'zh', len(zh), 'en-only', sorted(en - zh))
  PY
  ```

  **Lesson** (recorded in §8.1-4): a script that supports a conclusion must first be checked against a
  sample whose answer is known; the wrong headline finding came from an unchecked regex.

#### P0-2 The frontend UI/orchestration layer has no automated guard rails

- **Symptom**: of 61 frontend source modules only **23 (38%)** are directly referenced by a unit test. The
  38 uncovered ones cluster in `render/` (7/7 uncovered), `meas/` (6/6), `ui/` (13/19, including
  `controls.ts`/`keypad.ts`/`trigger*.ts`/`limits.ts`), plus `core/ws.ts`, `core/i18n.ts`, `core/theme.ts`,
  `core/fmt.ts`, `core/markerCommon.ts`, `dsp/normalize.ts` and `audio/sdrAudio*`. What *is* covered is
  `dsp/*`, `core/{params,level,frequency,units,refclock,store}` and
  `ui/{displayRef,graphMode,sdrState,swpState,normPub,railMath}` — i.e. the state-model and pure-DSP layers.
  The only end-to-end regression, `tools/e2e/state_regression.py`, needs real hardware plus a running service and
  is deliberately excluded from `test.sh` (`test.sh:20-26`).
- **Evidence**: import targets of `__tests__/*.test.ts` (script in Appendix B);
  `tools/e2e/state_regression.py:31-36` (`from playwright.sync_api import ...`, needs a live device).
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
| i18n parity test | `__tests__/i18n.test.ts` | Exposes the structural problems first (type covers 194/452, five duplicated keys); passes once the dictionary is merged |
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

> **Relation to section 7**: from the "easy to extend later" angle, five seams are still missing (E-1 parameter
> schema, E-2 capability table, E-3 session Protocol instead of mode branches, E-4 frame codec table,
> E-5 frontend registration points). They overlap Phase 2/3, so **fold them in** rather than running another
> round: E-1/E-2 with the command registry, E-3 with the publisher de-branching, E-4/E-5 in Phase 3.

---

## 5. Quick wins (half a day each, very low risk)

1. Add ruff/playwright/fonttools to a dev requirements file (P0-4) — otherwise the README's test steps are wrong.
2. `__tests__/i18n.test.ts` + merge the dictionary and add the one missing Chinese key (P0-1).
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

## 7. Modularization & feature-extensibility supplement

Section 3 asked "what is wrong today"; this section changes the yardstick to **how large the blast radius of one
new feature is**. That is the real modularization metric — directory layering is only step one, the
**extension seams** are what matters.

### 7.1 Measuring modularization by blast radius (measured in history)

| What was added | What actually changed | Files | Note |
|---|---|---|---|
| One new parameter (SWP detector `SET_DETECTOR`) | `web/ws.py` (command set + validation + dispatch), `hardware/device.py` (state + profile application), `web/http_api.py` (STATUS field), `frontend/index.html` (control), `core/i18n.ts` (×2), `core/ws.ts`, `ui/controls.ts` | **8** (+1 test) | Commit `12b5b72`: 52 changed lines spread over 8 files |
| One new hardware mode (SDR) | `demod/` (5 files), `sdk_bindings`, `device.py`, `measurements/{sdr,__init__}`, `web/{ws,http_api,publisher,client_stream}`, frontend `{ws,controls,audio}` = **16 product files**, plus 15 probe/doc files | **31** (1837 lines) | Commit `ce92d7a` |
| One new frame type | encoder + the retention branch in `web/client_stream.py` + the parse branch in `core/ws.ts` + tests | **4** | No codec registry |
| One new device model (e.g. SAN-200) | `config.py` (table), `ws.py` (many hard-coded limits), `rta.py` (`FULL_SPAN_HZ`/`DISPLAY_POINTS`), `http_api.py` (`rta_defaults`/`points`), frontend fallbacks | **5+** | The capability table is not the single source of limits |

Conclusion: the blast radius grows **linearly** with the number of existing features, because every extension
axis requires editing a central `if/elif`, a central dict, the frontend dispatch, two i18n dictionaries and
`index.html`. The five missing seams are listed below, ordered by payoff.

### 7.2 The five missing seams

**E-1 (highest payoff) there is no single schema for parameters/commands.**
Every parameter is described **four times**: (1) the `DeviceState` field and its default; (2) the range
literals in `_validate_command`; (3) the `build_status` key names and `req/swp/rta/sdr` nesting
(`points: 3328` and `rta_defaults` are hard-coded); (4) the frontend `params.ts` slot + `index.html` control
+ i18n. The eight files touched by `SET_DETECTOR` are the direct consequence.

Proposal: declare `ParamSpec(name, type, min, max, unit, default, modes, scope, group, render)` once and derive
(a) command validation, (b) the STATUS shape and `/api/schema`, (c) frontend controls and slots
**generated** from it. Payoff: a normal parameter drops from 8 files to 1–2, and the frontend no longer
hand-writes a control and an i18n entry per parameter. Boundary: let the schema cover numbers/enums/toggles
only; graphics and context-dependent buttons stay hand-written — do not over-generate.

**E-2 device capabilities are not the single source of limits.**
Validation in `ws.py` hard-codes `rbw ≤ 10e6`, `points ≤ 4000`, `rta span ≤ 50.78125e6`, `ifbw ≤ 500000`,
`decimate ≤ 2048`, `atten ≤ 33`, `pnm 1..9e6`; `50.78125e6` is written in four files and `3328` in both
`rta.py` and `http_api.py`.

Proposal: converge the capability set into `DeviceCapabilities` (`rbw_max`/`points_max`/`rta_span_max`/
`ifbw_max`/`decimate_max`/`demod_modes`/`features{pnm,rta,sdr,trigger}`…), with conservative defaults for
unknown models, and replace the `pnm_supported` special case with `supports('pnm')`. Payoff: supporting new
firmware/models becomes one table row; the frontend can disable controls from it too (today it has hard-coded
fallbacks plus greying logic).

**E-3 the session interface is inconsistent, so mode policy leaks into the scheduler.**
`std` has no `reconfigure()` (it goes through `dev.configure_swp()`), and neither do `harmonic`/`pnm` — only
`rta`/`sdr` do (`ws.py:338-343`). Meanwhile `publisher.py` uses `mode in ('rta','sdr')` for the timeout
(`:19`), `mode == 'std'` for FREQ de-duplication (`:71`) and `mode == 'sdr'` for the 0/2 ms pacing (`:99`);
`ws.py` has nine `mode ==`/`sess.name ==` branches.

Proposal: express mode policy as session attributes instead of scheduler branches:
`acquisition_timeout()`, `pacing() -> float`, `dedupe_policy()`, `reconfigure()`, `reset_defaults()`,
`is_ready()`, `status_view()`, `health()`, `param_specs()` (together with E-1). Payoff: a new mode no longer
touches `publisher`/`ws`/`build_status`/`client_stream`, and the private `_ready` handshake (finding P1-9) goes
away. Declare the interface as a `typing.Protocol` and assert structurally that every session satisfies it.

**E-4 frame types have no codec/retention table.**
`client_stream.publish_bytes` hard-codes retention semantics with `if magic == b'FREQ' / elif AUDF / else
latest-wins` (`:63-79`), so a new frame type means editing `client_stream`, `ws.ts`, the encoder and both sets
of tests. Proposal: `FRAME_POLICY = {FREQ: retain, AUDF: fifo(20), POWR: latest, RTAF: latest}` with unknown
magics defaulting to latest; mirror it on the TS side as one decode table plus a failure counter (together
with the golden tests from finding P0-3).

**E-5 the frontend has no registration points.**
`renderAll()` hand-dispatches on `viewMode` with `if/elif` and manually shows/hides four table elements
(`spectrum.ts:383-432`); a new measurement panel must edit `renderAll`, the tab list in `ui/measure.ts`, both
i18n dictionaries and `index.html`. Proposal: `registerRenderer(viewMode, {render, tables, statusBlocks})`
and `registerMeasurementTab(...)`; split i18n into `namespace.*` files merged at startup, with the parity test
checking per namespace. Payoff: a new measurement module is one new file plus one registration line, and
`renderAll` stops growing.

### 7.3 Order of work and dependencies

1. **The command registry (findings P1-1/P1-2) is the prerequisite for E-1**: centralise
   validation+dispatch+mode constraints first, then introduce the schema.
2. **E-2 (capability table) can run in parallel and must come early** — otherwise the registry simply moves the
   hard-coded limits into a new home.
3. **E-3 (session Protocol) lands together with removing the publisher's mode branches**, in one pass, to avoid
   two regression rounds.
4. **E-4/E-5 belong to Phase 3** (frontend decoupling) and share the "registry + baseline counters" guard rails.

**Fake seams to avoid**: do not build a generic plugin system / dynamic loading (`importlib` scanning a
`plugins/` directory and so on). The only extender here is the author, so the payoff is small and the debugging
cost high. Do not try to generate *all* of the UI from the schema either.

### 7.4 Extensibility acceptance checklist (definition of done for a new feature)

- [ ] A new parameter does **not** touch `_dispatch`; it adds only a registry/spec entry
- [ ] Its bounds come from the spec/capability table (test asserts `ws.py` gained no limit literal)
- [ ] A new mode does **not** touch the `mode == ...` branches in `publisher.py`/`ws.py`
- [ ] A new frame type adds one codec table row plus one parse site
- [ ] A new panel adds one registration plus one i18n namespace file
- [ ] `madge --circular` = 0, i18n parity passes, tsc adds no new `any`
- [ ] The new panel's required ids are in the startup self-check list (finding P1-5)

### 7.5 Quantified guard rails (recommended for CI, against re-drift)

| Guard rail | Form | Baseline (today) |
|---|---|---|
| Mode branches do not grow | Assert the `mode ==`/`sess.name ==` occurrence count stays at baseline | 13 (`ws.py` 9 + `publisher.py` 3 + `device.py` 1) |
| The command layer stops bloating | Assert length caps on `_dispatch` (313) and `_validate_command` (135) | 350 / 150 |
| Limits do not enter the validation layer | Assert the number of `maximum=<literal>` in `ws.py` matches the capability table | See the E-2 list |
| Circular dependencies do not grow | `madge --circular` output count | 14 |
| i18n does not drift | en/zh key sets equal + placeholders identical | 452/451 at review time (missing `sdr_snap_tip`); fixed in §9.1 and pinned by vitest |

---

## 8. Self-audit of this review (against industry practice)

**Verdict: the conclusions hold and the recommendations match common industry practice; nothing recommended
is anti-pattern.** The audit corrected one factual error and two statements that needed qualification, and
filled three gaps the original review missed.

### 8.1 Corrections applied

| # | Problem | Correction |
|---|---|---|
| 1 | P0-2 claimed `render/`, `ui/` (except rail) and `meas/` have "zero unit tests" — inaccurate | Restated per module: of 61 modules, 23 (38%) are directly referenced by a test; `ui/{displayRef,graphMode,sdrState,swpState,normPub,railMath}`, `core/{params,level,frequency,units,refclock,store}` and `dsp/*` are covered, while the gaps cluster in `render/`, `meas/`, the UI glue and `core/ws.ts` |
| 2 | The P0/P1/P2 labels did not state their basis | Now explicit: they are **action priority** (P0 = delivery reliability/correctness, do first), not a production-incident severity scale |
| 3 | E-1 ("generate frontend controls from the schema") was easy to misread as "automate all UI" | Qualified: the schema covers numbers/enums/toggles only; graphics and context-dependent buttons stay hand-written |
| 4 | **P0-1's "77 missing i18n keys" was wrong** (the most serious defect of this review): the regex behind it ignored keys containing `-` and truncated at nested braces | Recomputed with brace matching: en 452 / zh 451, only `sdr_snap_tip` missing. The finding changed from "many untranslated keys" to "structurally unsafe dictionary" (type covers 194/452, five duplicated keys, no consistency test). §3 P0-1, §5, §7.5, Appendix A and the summary above are corrected |

### 8.2 Mapping to industry practice

| Recommendation | Practice it corresponds to | Verdict |
|---|---|---|
| CI (pytest/ruff/tsc/vitest) | CI gate, minimum for trunk-based development | Sound |
| Protocol golden tests | Contract testing / consumer-driven contracts | Sound |
| i18n parity + typed keys | i18n lint / localization CI check | Sound |
| `madge --circular` = 0 | Architecture fitness function (same idea as ArchUnit / dependency-cruiser) | Sound |
| Single-source version + CHANGELOG | SemVer + release automation | Sound |
| Complete dependency declaration | Reproducible builds | **Incomplete** → gap G-2 added |
| ParamSpec as one schema | The command-tree + parameter-metadata pattern common in instrument software (SCPI-like) | Sound (with the 8.1-3 qualification) |
| Session Protocol + policy pushed into sessions | Ports-and-adapters / strategy pattern (hexagonal) | Sound |
| No plugin system, no frontend framework | YAGNI / stability of the chosen stack | Sound |
| Splitting the god objects | SRP | Sound, but must be staged (the report already says so) |

The ordering is also conventional: **build guard rails before refactoring (refactor under test)**, which is why
Phase 0 must precede Phase 2/3.

### 8.3 Gaps the original review missed (added here)

**G-1 No performance/resource baseline.** The original review was static-only yet still wrote conclusions such
as "the main bottlenecks are handled" without measurements. → Added: `tools/bench.py` (repeatable
latency/frame-rate/CPU baseline) plus a baseline table, so optimisation claims become verifiable.

**G-2 Dependencies are not pinned.** `aiohttp>=3.9`, `numpy>=1.24` have no upper bound and the frontend
`package.json` uses `^` (though `package-lock.json` exists); the Python side has no constraints file.
Reproducible builds require pinning or at least a documented, tested version matrix.

**G-3 No hardware-in-the-loop (HIL) entry point.** The repo has `tools/hardware_smoke.py` and the Playwright
e2e, but no single command, no baseline and no "must run before release" rule. Instrument software
conventionally has `make hw-test` (real-hardware smoke + state-machine regression) in the release checklist.
→ Implemented here (see §9).

### 8.4 Confirmed as unnecessary

- No structured logging / distributed tracing (single-user, single-process instrument: low payoff);
- No 100% coverage target (test budget belongs in protocol contracts and state machines);
- No DI container / framework rewrite (constructor injection of `dev` is enough).

---

## 9. Implementation record (branch `refactor/arch-review-improvements`)

Roadmap phases 0-3 are implemented (21 commits since the review commit; 24 from master including the three review-doc commits), each verified with `make ci` + `make hw-test`
+ `make bench`.

### 9.1 Implemented

| Finding | What | Verified by |
|---|---|---|
| P0-1 | i18n merged into one declaration (452/452), the one missing zh key added, `I18nKey` covers every key, parity test | `__tests__/i18n.test.ts` |
| P0-3 | `core/frames.ts` is the single TS frame definition; golden fixtures generated from the Python encoders, asserted on both sides | 6 + 7 tests |
| P0-4 / G-2 | Dependencies split runtime/dev/lock with two-sided bounds | clean-checkout `./test.sh` |
| P0-5 | Version single-sourced, `--check` in CI | breaking package.json fails |
| G-3 | Seven vendor modules skipped when the library is absent; `make ci`/`hw-test`/`bench` | **78** tests pass offline |
| G-1 | `tools/bench.py` (fixed device configuration before measuring; it caught a false 2.5x regression) + baseline | passes repeatedly |
| P1-1/P1-2 | **Command layer as a declarative table** (`CommandSpec` + guard flags; ws.py transport-only) | 14 table tests + `command_sweep.py` 30/30 on the bench |
| P1-3 | `rta.py` no longer imports `htra_api`; `sdk_bindings` re-exports the missing types | guard 3 -> 0 |
| P1-4 | **Frontend cycles 14 -> 0** (redraw seam, plot geometry, four leaf modules) | guard + hardware UI regression |
| P1-5 | **`controls.ts` split into 9 `ui/panels/*` modules** (1548 -> 934 lines) + **DOM id contract check** (which found `#cur-ifgain` was never displayed) | tsc + bench + `check_dom_ids.py` |
| P1-6 | **Parameter slot migration completed**: trigger group, waterfall group (range/pause/fade/bins), display group (unit/offset/smoothing); **measurement results moved to `core/results.ts`** and shared types to `core/model.ts` (store re-exports them, call sites unchanged). The rule is now written down in `ARCHITECTURE.md` (parameters use slots, results use a store, never call `get()` inside a hot loop). The remaining store globals `spanStepHz/spanStepAuto`, `dbPerDiv` and `levelUnit` are single-writer display/frontend state, not the multi-writer bug class this finding targets | 141 frontend tests + bench; see the performance lesson in 9.3 |
| P1-7 | Session `health()`, device `auto_reference_view()` | STATUS unchanged |
| P1-8 | **`AutoReferenceController` extracted** + 16 focused tests | 16 new + 7 existing behaviour tests |
| P1-9 | **Session lifecycle protocol** `request_stop()`/`is_ready()` + `SessionManager` | table tests + bench |
| P1-10 | `web/recovery.py` owns `fatal()` | 2 tests |
| P1-11 | `web/jsonutil.py` + single serialization per broadcast | backend + frontend tests |
| E-1 | **`ParamSpec` schema** (capability-callable bounds) + `GET /api/schema` (auth-protected) | 6 tests |
| E-2 | Hardware limits in `DeviceCapabilities` | `test_model_limits_come_from_capabilities` |
| E-3 | Acquisition policy pushed into sessions (publisher has no mode branch) | `test_publisher.py` |
| E-4 | `FRAME_POLICY` retention table + coverage test | 2 tests |
| E-5 | **Three registration points**: views (`render/registry.ts`), measurement tabs (`ui/measureRegistry.ts`) and **i18n split into per-domain namespaces** (core 371 / trigger 50 / sdr 5 / limits 19 / keypad 7 keys per language, merged at import) | 6 registry tests + i18n parity |
| P2-1 | `core/store` no longer touches the DOM at import (`initStore()` + fail loud) | `__tests__/store.test.ts` |
| P2-2/P2-3 | `noUnusedLocals/Parameters`; ESLint see 9.3 (blocked upstream) | tsc clean |
| P2-4 | **HiDPI** (backing store x DPR, logical coordinates unchanged) | store tests + bench |
| P2-5 | Bilingual doc structure check (7 file pairs) | `make ci` |
| P2-7 | **Measurement result assembly tests**: the harmonic sequence is driven through a stub device (order list, dBc reference, amplitude tracking, frequency-limit stop, parameter clamping) and the PNM payload became a pure `pnm_payload()` helper | 6 hardware-free tests |
| P2-8 | Correction: `fit_span` is used by the harmonic session, not a legacy leftover | - |
| §7.5 | `tools/quality/architecture_guard.py` + baseline | `make ci` |

### 9.2 Objective progress (guard metrics, before -> now)

| Metric | Before | Now |
|---|---|---|
| `frontend_cycles` | 14 | **0** |
| `backend_cycles` | 1 | 1 (`http_api ⇄ ws`, transport) |
| `dispatch_lines` (longest handler) | 313 | **33** |
| `validate_lines` (longest validator) | 144 | **16** |
| `command_specs` | - | 24 |
| `mode_branches` | 18 | **0** |
| `htra_imports_outside_bindings` | 3 | **0** |
| `validation_limit_literals` | 35 | **0** |
| Backend tests | 87 | **147** |
| Frontend tests | 115 | **152** |
| Hardware-free (CI) tests | 0 | **78** |
| `ui/controls.ts` | 1548 lines | **934 lines** (exports 97 -> 16; + 9 panel modules) |
| `core/store.ts` | ~285 lines / 70 mutable globals | **226 lines / 37** (+ `results.ts` 89 + `model.ts` 48) |
| `DeviceState` fields | ~100 | **83** (control loop extracted) |
| e2e assertions (hardware) | 27 | **45** |
| Production bundle | 178 kB | 156 kB |

### 9.3 Open items (complete list after the v1.5.6 re-check)

| Finding | Status | Note and next step |
|---|---|---|
| **Phase 4: fake backend + Playwright in CI** | **Open (highest value now)** | The e2e is the only test that asserts user-visible results (canvas pixels, DOM text, real clicks), yet it can only run by hand on a machine with the hardware. The blank-canvas defect was caught by exactly that check. Next: build a fake device behind the `web_sa/hardware/device.py` boundary (synthetic SWP/RTA/SDR frames), point `tools/e2e/state_regression.py` at it and run it in CI. |
| **e2e coverage for Firefox** | Open | Playwright has only Chromium installed here; manual testing uses Firefox. Next: `playwright install firefox` and run the e2e as a two-browser matrix. |
| **P1-8 step 2: split `DeviceState` per mode** | Open | The control loop (`AutoReferenceController`) is extracted and the field count dropped from ~100 to 83; the rest is still one dataclass. Structural payoff, no current defect driving it. Next: `SwpParams/RtaParams/SdrParams/TriggerParams` sub-structures with the STATUS shape unchanged. |
| **P2-2 remainder: unused exports in `controls.ts`** | Partial | After the panel split `controls.ts` has 16 exports, 11 of which nothing outside references (they can lose `export`). Pure hygiene. |
| **P2-3: ESLint** | Blocked (upstream) | `typescript-eslint@8` requires `typescript <6.1` while this project uses TypeScript 7.0.2 (npm ERESOLVE reproduced). Covered meanwhile by `tsc --strict` + `noUnusedLocals/Parameters` plus the architecture, registration, DOM-id and docs guards. Adopt once upstream supports it. |
| **P2-6: 27 KB scratch TODO in the repo root** | Open | It is the author's local working note (`.git/info/exclude`); deleting or moving it is their call. Suggestion: fold its design decisions into `DEVELOPMENT.md` and drop the stale debugging logs. |
| **P2-9: echo only changed STATUS fields for high-rate commands** | Open | Frontend slot confirmation relies on the full STATUS; it is an optimisation, not a structural problem, with little payoff at the current client count. Revisit if STATUS grows. |
| **Frontend unit coverage (P0-2 remainder)** | Partial (deliberate) | 31 of 88 modules are directly referenced by a unit test (including five i18n data files); the rest are DOM layers (`ui/`, `render/`, `meas/`, `ui/panels/`) covered by the e2e (45 assertions) instead. Add unit tests when one of them grows pure logic, rather than chasing module coverage. |
| **Bench client-count comparability** | Recorded, not automated | Extra WS clients (several browser tabs) multiply the fan-out work. The bench records the device warning state but not the client count; check that only one client is connected before comparing. |

### 9.4 Verification record (this bench, SAN-90 + tinySA attached)

```
make ci        -> pytest 147 passed / ruff clean / i18n+frames parity / DOM id contract /
                  bilingual doc structure / architecture guard (cycles 0, mode branches 0) /
                  build OK
make hw-test   -> tinySA CW at 100.2 MHz measured -25.7 dBm (SWP) / -25.3 dBm (RTA)
                  tools/command_sweep.py: all 24 commands and all 6 guard rejections as expected
                  39 UI state-machine checks pass, no page errors
make bench     -> matches the baseline (fixed configuration: points 1001, auto RBW, ref -30,
                  atten auto, spur bypass): SWP 174 fps, RTA 214 fps, SDR 19 + 50 audio fps,
                  switches 465/310 ms
HTRA_API_LIB=/nonexistent python3 -m pytest tests/ -q  -> 78 passed
per-commit     -> every commit on the branch was re-checked with `git worktree` + pytest
                  (it caught one "test committed before its implementation" and this round's
                  hot-path performance regression)
```

### 9.5 Goal completion re-check (v1.5.6)

Every recommendation in this document was re-checked against the code at v1.5.6 (`make ci` green):

| Goal | Status | Evidence |
|---|---|---|
| Phase 0: CI, pinned deps, single-source version, i18n/frame contracts, performance baseline, HIL entry, four guard classes (architecture / DOM id / docs / registration reachability) | **Done** | `make ci` runs them in sequence and passes |
| Phase 1: P1-1...P1-11 | **Done** | command table, cycles 0, panel split, parameter slots, health/auto-ref views, session protocol, fatal exit, JSON boundary |
| Phase 2: E-1...E-5 | **Done** | `ParamSpec` + `/api/schema`, capability limits, session-owned policy, frame retention table, the three registration points (views, tabs, i18n namespaces) |
| Phase 3: frontend decoupling | **Done** | cycles 14 -> 0, panel split, results module, frame-parser tests, canvas DPR |
| G-1/G-2/G-3 | **Done** | performance baseline, dependency lock, `make hw-test` |
| §7 extension seams E-1...E-5 | **Done** | as above; `check_registrations.py` now guards seam reachability |
| Remaining open items | see §9.3 | Only two carry real value: "fake backend + e2e in CI" and "Firefox e2e"; the rest are blocked upstream or low-payoff |

**Two real defects happened after this review, which it had not covered:**

1. **Blank canvas (the user reported "the system is unusable")**: after the cycles were broken, nothing imported `render/spectrum.ts`, so its module-scope `setRenderer(renderAll)` never ran and `requestRender()` was a no-op. No exception, no console error; every existing test passed because they assert **proxies** (frame counters, datasets, control values). Lesson: a registration side effect requires an explicit entry-point import, guarded by `tools/check_registrations.py`, and tests must assert user-visible results (the e2e now counts non-transparent canvas pixels).
2. **Three UI defects** (RTA centre unit/keypad dead, waterfall not disabled while measuring, IF-overflow warning invisible until Ref was raised): root causes were a field-key/input-id mismatch (`rta_center` vs `input-rta-center`), two features competing for one display slot, and a warning that is only drawn in the frame loop while an overflowing IF sends no frames. Lesson: resolve cross-module string identifiers through one helper (`inputForField`/`fieldForInput`), give each display slot a single owner, and repaint on state transitions instead of relying on the data flow.

Both incidents are recorded in the lesson ledger of `DEVELOPMENT.md`, and every derived rule landed as a guard or a test.

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
| i18n keys | en 452 / zh 451 (1 missing; the first version's 402/325 was an extraction bug, §8.1-4) |
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

# 5) Frontend unit-test coverage map (which of the 61 source modules a test imports)
python3 - <<'PY'
import os, re, collections
root='frontend/modern/src'; tdir=os.path.join(root,'__tests__')
covered=set()
for fn in os.listdir(tdir):
    if not fn.endswith('.ts'): continue
    for m in re.finditer(r"from\s+'(\.[^']+)'", open(os.path.join(tdir,fn)).read()):
        c=os.path.normpath(os.path.join(tdir,m.group(1)))
        for ext in ('','.ts','.js'):
            if os.path.exists(c+ext): covered.add(c+ext); break
srcs=[os.path.join(dp,fn) for dp,_,fns in os.walk(root) for fn in fns
      if fn.endswith(('.ts','.js')) and '__tests__' not in dp]
un=sorted(s for s in srcs if s not in covered)
print(f'{len(srcs)} modules, {len(srcs)-len(un)} covered, {len(un)} uncovered')
c=collections.Counter(os.path.dirname(os.path.relpath(s,root)) for s in un)
for d,n in sorted(c.items()): print(f'  {n:3d}  {d}/')
PY
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
