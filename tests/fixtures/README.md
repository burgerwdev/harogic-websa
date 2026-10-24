# Test fixtures

Committed fixtures provide deterministic DSP inputs, wire-format golden data and
real decoder captures. Keep their manifests and provenance alongside the data.

| Directory | Purpose | Regeneration |
|---|---|---|
| `contract/` | Native fixture service schema and initial state | Maintain with the service contract; the native service embeds these JSON files. |
| `frames/` | Binary protocol frames shared by native and frontend decoder checks | `python3 tools/fixtures/gen_frame_fixtures.py` |
| `dsp/` | DDC and analog demodulator reference inputs and expected outputs | `python3 tools/fixtures/gen_dsp_fixtures.py` |
| `ft8/` | Synthetic FT8 IQ, reference tables and a real received slot | `python3 tools/fixtures/gen_ft8_fixtures.py` regenerates synthetic data; `python3 tools/fixtures/port_ft8_tables.py` maintains decoder tables. |
| `sstv/` | Robot36 IQ, expected pixels and mode definitions | `python3 tools/fixtures/gen_sstv_fixtures.py`; mode definitions come from `tools/fixtures/port_sstv_modes.py`. |
| `drm/` | DRM physical-layer, service and audio codec fixtures, including real captures | Follow [the DRM fixture recipes](drm/README.md) and manifests; each signal/codec fixture has its own provenance and recipe. |

The DSP, frame and FT8 generators support `--check` to compare generated output
with committed data. Python reference implementations under `tools/reference/`
remain part of the reproducible DSP fixture chain.

`ft8/real_slot_ja0txu.iq` and its JSON sidecar contain an over-the-air FT8 slot
used for decoder regression and sensitivity checks. Real DRM AAC/HE-AAC/xHE-AAC
captures are also retained with their manifests. These recordings cannot be
recreated by running a synthetic generator; preserve the original bytes and
capture metadata.

Viewport screenshots and JSON observations are generated inspection output, not
fixture inputs. `make viewport-check` evaluates current geometry without reading
a stored image or JSON baseline.
