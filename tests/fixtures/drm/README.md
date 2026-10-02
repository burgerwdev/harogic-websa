# DRM ground-truth fixtures

These fixtures let the Rust/WASM DRM decoder be verified without a live shortwave
DRM broadcast (which is not reliably receivable on a SAN-90 in a lab).

## `drm_modeB_so3_48k.f32` + `manifest.json`

A synthesised DRM30 signal with **known** content: robustness mode B, spectrum
occupancy 3 (10 kHz), 64-QAM SM MSC, 16-QAM SDC, long interleaving, EEP protection
level 1. The SDC carries a station label `SAN90 DRM TEST` and an AAC (24 kHz SBR
mono) audio service; the FAC carries service id `0x123456`, one audio service, no
data services. Full ground truth lives in `manifest.json`.

- `drm_modeB_so3_48k.f32` — interleaved I/Q `float32` (little-endian), 48 kHz
  complex, 6.0 s (288000 complex samples).
- `manifest.json` — format, signal/FAC/SDC/MSC ground truth, and the generator's
  self-check result.

## Provenance and licence

The fixture is **data** emitted by running the DecDRM transmitter:

- Project: https://github.com/CasualArclamp/DecDRM (v0.5.1, commit
  `b1fbd87f3d3e4709ff838d5120724fb4506253f6`), licence **GPL-2.0-or-later**.
- Generator: `crates/decdrm-core/examples/gen_fixture.rs` (written for this repo;
  transmits the signal, then decodes it back with the DecDRM receiver as a
  self-check).

The I/Q samples describe a signal defined by the ETSI ES 201 980 standard and are
not GPL source; only the *generator tool* is GPL. DecDRM is therefore kept out of
this repository and used solely as an external fixture generator / oracle — the
in-tree Rust/WASM decoder is an independent implementation.

## Regenerating

Clone DecDRM (outside this repo), drop in the `gen_fixture.rs` example, and run:

```sh
cargo build --release -p decdrm-core --example gen_fixture
./target/release/examples/gen_fixture B 3 6.0 tests/fixtures/drm/drm_modeB_so3_48k.f32
```

Then paste the printed summary back into `manifest.json`'s `self_check` block if it
changed. The parameters are `gen_fixture <mode A-D> <so 0-5> <seconds> <out.f32>`.

## Real off-air recordings

For later validation against real broadcasts, the DecDRM test suite references a set
of off-air DRM recordings (e.g. `DW_ModeB_10kHz.flac`, `RTL_ModeB_10kHz.flac`,
`Deutschlandradio_ModeA_10kHz.flac`) with known mode/occupancy; see
`crates/decdrm-core/tests/recordings.rs` in DecDRM. Those recordings are not
committed there and are intentionally out of scope for the committed fixture set.
