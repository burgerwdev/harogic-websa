# Harogic WebSA licensing scope

Copyright (c) 2026 Tommy Wang.

The copyright holders' **project-owned code for which they have authority to
grant these terms** is licensed under **AGPL-3.0-only**, with the narrowly scoped
section 7 permission in [LICENSE-EXCEPTION](LICENSE-EXCEPTION). The complete,
unaltered GNU AGPL version 3 is in [LICENSE](LICENSE). The example "or later"
notice in that standard text is not this project's license election: this
project elects version 3 **only**.

`LicenseRef-Harogic-WebSA` denotes these combined terms; it is a local SPDX
license reference, not a registered SPDX linking exception. [LICENSE-PACKAGE](LICENSE-PACKAGE)
is the package license notice. Cargo uses `license-file` because a bare
`AGPL-3.0-only` field would omit the additional permission, and inventing an
SPDX `WITH` exception identifier would misrepresent registration.

## Project-owned scope

Subject to the exclusions below, the new terms apply to the project's original
Rust backend, C++ SDK adapter and bridges, Rust DSP implementation, TypeScript
browser application, original Python/shell tools, tests, build scripts and
documentation. Paths include `backend/`, `dsp/core/`, `frontend/`, `tools/`,
`tests/`, `contracts/` and original files at the repository root. Being located
under one of these paths does not make third-party work project-owned.

The new terms cover only copyright interests the granting holders control.
They do not assert ownership of imported work, vendor interfaces, reference
implementations, third-party contributions without the necessary authorization,
or data merely present in this checkout. Existing copyright and attribution
notices remain effective. Original and third-party portions of a mixed file
must be read with their respective notices.

## Exclusions and retained licenses

The following are excluded from this relicensing, including local changes
already maintained under their upstream terms:

- `htra_api.py`: HAROGIC's SDK wrapper; vendor authorization is unresolved.
- `dsp/deepfilter/`, `tools/vendor/ggmorse/`, and the `vendor/` subtrees under
  `dsp/core/src/digital/sstv/` and `dsp/core/src/digital/radiofax/`.
- FDK AAC and libxaac archives and imported codec material, including
  `tools/fixtures/aac_huff_tables.py` and `tools/fixtures/aac_sfb_offsets.py`.
  Project-written codec shims/build scripts remain project-owned.
- Imported FT8 constants in `dsp/core/src/digital/ft8/tables.rs` and
  `tests/fixtures/ft8/tables.json` (ft8_lib, MIT), and imported SSTV timing
  tables in `tests/fixtures/sstv/modes.json` (sstv, MIT).
- `tools/fixtures/cty.dat` and the imported data in `frontend/src/sdr/ctyData.ts`;
  the CTY redistribution terms need confirmation.
- `dsp/core/src/digital/drm/dsp/util.rs`: provenance is unresolved because its
  header describes a port from GPL references. No new license or linking
  exception is asserted over material whose origin/permission is unconfirmed.
- Model weights/graphs in `backend/models/nr/` and `frontend/public/models/`,
  third-party binaries/WASM, generated third-party JS glue, fonts, Cargo/npm/Python
  dependencies, and their license and notice files.
- `backend/src/bin/websa_gnss/schema.rs` retains **BSD-3-Clause** and its
  Carles Fernandez-Prades copyright/notice, copied in `LICENSES/BSD-3-Clause-GNSS-SDR.txt`.
- `tools/companions/rtty/main.cpp`, `tools/pluto/pluto_digital_source.cpp` and
  `tools/pluto/build-digital-source.sh` remain **GPL-3.0-or-later**. Independently
  installed decoders/drivers/simulators retain their respective licenses.
- Existing `runtime/` artifacts retain the terms of the source/dependencies
  from which they were built. A source license edit does not retroactively
  relicense or rebuild old executable, JS or WASM bytes.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for provenance and license
locations, and the [licensing review](docs/en/LICENSING.md)
([中文](docs/zh-CN/LICENSING.md)) for the evidence and unresolved questions.

## Source and distribution

The exception permits linking, loading and distributing a combination with
HAROGIC SDK and/or FDK AAC only to the extent the granting copyright holders
can permit it. It neither licenses the vendor SDK nor changes any third-party
terms. Obtain the HAROGIC SDK separately; do not add its libraries or headers
to runtime packages or source bundles. This does not exempt other SDK-bundled
libraries or arbitrary proprietary modules.

Provide the applicable Corresponding Source for project-owned code and all
modifications, adapters, interfaces and build/install scripts, including the
network source offer required by AGPL section 13 for modified network versions.
The UI offers license texts and a versioned local source snapshot; see the
[build/source procedure](docs/en/LICENSING.md#source-entry-and-packaging).
Keep the source snapshot matched to the actual service/frontend and retain
upstream notices and any source obligations of third-party components. The
snapshot is not a certification that the unresolved third-party source and
distribution questions have been cleared.

## Earlier MIT releases and contributions

Copies previously released under MIT retain that permission under its terms.
This change does not withdraw, replace or invalidate those grants. The exact
previous repository notice is preserved as [LICENSES/MIT-legacy.txt](LICENSES/MIT-legacy.txt),
including its copyright and warranty disclaimer. Recipients may continue to
use those earlier copies under MIT. New changes are offered under the new
terms only to the extent their copyright holders authorize them.

The local Git author list is evidence of recorded authorship, not proof of
copyright ownership, employment clearance or an assignment. No CLA or assignment
is inferred. Future contributions to project-owned code should explicitly
identify authorization for AGPL-3.0-only **and this section 7 permission**; do
not assume that permission to contribute AGPL-only code includes permission to
add the linking exception. Preserve separately licensed contributions and do
not apply the exception to another holder's work without authority.
