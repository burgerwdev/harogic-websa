# Test fixtures

Everything here is **regenerable from a script** except one file. Nothing is hand-edited:
a fixture that drifted from its generator fails `make ci`, so the bytes always match the
documented recipe.

## Regeneration

| Directory | Generator | Check (CI-enforced) | Size |
|---|---|---|---|
| `dsp/` (22 files) | `python3 tools/gen_dsp_fixtures.py` | `--check` in CI | 948 KB |
| `frames/` | `python3 tools/gen_frame_fixtures.py` | `--check` in CI | 24 KB |
| `ft8/*.bin`, `ft8/tables.json` | `python3 tools/gen_ft8_fixtures.py` | `--check` in CI | ~15 MB |

All three are deterministic (seeded synthesis from the Python reference DSP): the same
command reproduces the same bytes, which is what lets the Rust tests (`wasm/tests/*.rs`,
`include_bytes!`), the Python tests and `tools/dsp_parity.py` agree numerically on both
sides of the WASM boundary.

## The one real capture (kept on purpose)

`ft8/real_slot_ja0txu.iq` (+ its `.json` sidecar, 5.7 MB) is a real over-the-air FT8 slot.
No script can synthesize it: it carries real noise floor, real drift, real propagation —
exactly the things the synthetic fixtures idealize away. It is the sensitivity/regression
baseline for the decoder (`wasm/tests/ft8_real_slot.rs`, `ft8_snr_sweep.rs`); a decoder
change that passes the synthetic fixtures but regresses on the real slot is a regression.
5.7 MB is the accepted cost of that guarantee.

## Related baselines (outside this directory, same policy)

- `screenshots/resolution-baseline/` (5.7 MB, 29 files): the viewport/resolution gate's
  reference images, recorded per attached output via `make viewport-baseline`, verified by
  `make viewport-check`. Regenerable, kept because the gate is meaningless without a
  reference to compare against.
- `tools/e2e/viewport_baseline.json`: the machine-readable half of the same gate.

## Deliberately not in the repository

Raw hardware probe captures (`tools/sdr_probe/captures/*.npz`, ~320 MB) were removed from
the history: nothing in tests, tools or docs referenced them, they are one-off bench
artifacts of the SDR probe sessions, and no gate consumes them. They stay on the bench
machine that recorded them; re-capture is a hardware session, not a repo checkout.
