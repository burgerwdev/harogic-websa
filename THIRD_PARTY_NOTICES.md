# Third-party components and provenance

Third-party material retains its original copyright, license and notices.
Neither [LICENSE](LICENSE) nor [LICENSE-EXCEPTION](LICENSE-EXCEPTION) replaces
these terms. This inventory records the repository evidence; unresolved items
are detailed in the [review](docs/en/LICENSING.md) ([中文](docs/zh-CN/LICENSING.md)).

| Component / location | Integration | Retained terms and notice |
|---|---|---|
| HAROGIC SDK, `libhtraapi` and vendor headers | External SDK; project C++ adapter links `libhtraapi`, Rust loads the adapter | Vendor terms; not supplied by this project. Installed SDK has no identifiable license/EULA text. |
| `htra_api.py` | Official vendor Python wrapper with project adaptations; ctypes loads SDK | HAROGIC origin recorded in the development docs; redistribution/changes authorization still needs the vendor's text. Excluded from new source bundles pending confirmation. |
| FDK AAC / `dsp/core/fdk/libfdk.a` | Native `pkg-config` linkage; static WASM archive | Fraunhofer FDK AAC license, full [NOTICE](dsp/core/fdk/NOTICE) and [module marker](dsp/core/fdk/MODULE_LICENSE_FRAUNHOFER). Also covers generated AAC fixture tables in `tools/fixtures/`. No patent license; binary recipients must receive free access to complete FDK sources/changes under its own terms. |
| Ittiam libxaac / `dsp/core/xaac/libxaac.a` | Static WASM diagnostic decoder | [Apache-2.0](dsp/core/xaac/LICENSE), original [NOTICE](dsp/core/xaac/NOTICE). Archive revision/patch provenance still needs confirmation. No special linking exception is granted for libxaac. |
| DeepFilterNet / `dsp/deepfilter/`, DFN WASM/JS and DFN3 model | Vendored Rust, statically linked native code; separately committed browser diagnostics/model | Code: [MIT OR Apache-2.0](dsp/deepfilter/LICENSE), [MIT](dsp/deepfilter/LICENSE-MIT), [Apache-2.0](dsp/deepfilter/LICENSE-APACHE); Hendrik Schröter and upstream contributors. Model: exact artifact license/version mapping remains unconfirmed. |
| ggmorse / `tools/vendor/ggmorse/` | C++ compiled into native service | [MIT](tools/vendor/ggmorse/LICENSE); Georgi Gerganov. Revision/local changes in [UPSTREAM](tools/vendor/ggmorse/UPSTREAM). |
| sstv 0.3.1 / `dsp/core/src/digital/sstv/vendor/` and imported SSTV fixture mode tables | Vendored Rust | [MIT](dsp/core/src/digital/sstv/vendor/LICENSE); Tobias Klockau. Local changes remain under these terms. |
| rasterwave 0.5.0 / `dsp/core/src/digital/radiofax/vendor/` | Vendored Rust | [MIT](dsp/core/src/digital/radiofax/vendor/LICENSE); boybook and Rasterwave contributors. Revision/local changes in [UPSTREAM](dsp/core/src/digital/radiofax/vendor/UPSTREAM). |
| ft8_lib constants / Rust FT8 tables and `tests/fixtures/ft8/tables.json` | Imported protocol constants | [MIT](LICENSES/MIT-ft8_lib.txt); Copyright (c) 2018 Kārlis Goba. Sources/generator in `tools/fixtures/port_ft8_tables.py`. |
| Dream / DecDRM references in `dsp/core/src/digital/drm/` | Described as algorithm references; `dsp/util.rs` also says "ported from" | Existing comments identify GPL-2.0 references and assert no copied code. Those statements are not a verified provenance/compatibility finding. The ambiguous helper file is excluded from relicensing pending review. |
| GTCRN, UL-UNAS, FastEnhancer models / `backend/models/nr/` | Original ONNX graphs loaded natively | MIT; exact upstream revisions, hashes and individual `LICENSE.*` files in [model README](backend/models/nr/README.txt). |
| ONNX Runtime / `runtime/linux-x86_64/libonnxruntime.so.1.23.2` | Dynamically loaded CPU library | MIT plus dependency notices; [license](backend/models/nr/LICENSE.onnxruntime), [ThirdPartyNotices](backend/models/nr/ThirdPartyNotices.onnxruntime.txt). Copies remain in `runtime/linux-x86_64/nr-notices/`. |
| LXGW WenKai / `frontend/public/fonts/` | Browser WOFF2 subsets | SIL OFL-1.1; copied [license and copyright](frontend/public/fonts/LICENSE.txt) from the installed font package. Confirm the precise original font version and any reserved-name terms before redistribution. |
| AD1C CTY data / `tools/fixtures/cty.dat`, generated `frontend/src/sdr/ctyData.ts` | Callsign country lookup | Jim Reisert AD1C / country-files.com provenance in `tools/fixtures/gen_cty.py`; no redistribution license found in the checkout. No project relicensing claim over this data. |
| Cargo libraries | Rust linkage; exact graphs in `backend/Cargo.lock`, `dsp/core/Cargo.lock`, `dsp/deepfilter/Cargo.lock` | Each upstream package's original license/NOTICE applies, including bundled native components. Retain their notices with redistributions; lockfiles are version records, not replacement licenses. |
| npm and Python packages | Build/test/reference dependencies pinned by lock/requirements files | Each package's original terms; no relicensing by changing project metadata. |
| Lit HTML / `frontend/node_modules/lit-html` | Browser incremental templates, exact version in `frontend/package-lock.json` | BSD-3-Clause, Google LLC; [license and copyright](LICENSES/BSD-3-Clause-lit-html.txt). |
| SoapyHarogic | Independently downloaded driver, project patch applied in a private build | LGPL-2.1-or-later, pinned revision in `tools/build-soapy.sh`; retain upstream source/notices. No SDK redistribution permission is inferred. |
| multimon-ng, rtl_433, OnAir, GNSS-SDR and other optional providers | Separately installed executables, pipes/network interfaces | Their original GPL and component-specific notices; installer scripts preserve sources/notices. They are not covered by this project's two-dependency exception. |
| `tools/companions/rtty/main.cpp` | Separate executable linked to OnAir's decoder | Its existing SPDX **GPL-3.0-or-later** remains. `backend/src/external/rtty.rs` is project-owned pipe integration. |
| Pluto digital encoder / `tools/pluto/pluto_digital_source.cpp`, `tools/pluto/build-digital-source.sh` | Separate GPL helper built from pinned OnAir source in the user cache | Existing **GPL-3.0-or-later** remains. Installer retains LICENSE, REUSE, THIRD_PARTY, available LICENSES and helper sources; source revision and binary/helper/build hashes are recorded beside the executable. |
| GNSS protobuf wire declarations / `backend/src/bin/websa_gnss/schema.rs` | Adapted message/tag declarations used by the project GNSS process bridge | **BSD-3-Clause**; Carles Fernandez-Prades, 2018–2021. Full original header preserved in the source and [notice copy](LICENSES/BSD-3-Clause-GNSS-SDR.txt). |
| gps-sdr-sim | Separately installed bench encoder; verified pinned source archive | MIT; installer keeps encoder source, original LICENSE, navigation fixture, provenance and SHA256SUMS in the user cache. No SDK material is copied into the repository. |

Generated runtime artifacts inherit the notices and license scope of their
actual inputs; a rebuilt bundle can contain differently licensed parts. Retain
the old [MIT notice](LICENSES/MIT-legacy.txt) for earlier MIT project portions
and separately licensed contributions. Do not remove embedded source notices
when copying or modifying third-party code. Review the complete dependency
graph and its own source/notice requirements for each distribution.
