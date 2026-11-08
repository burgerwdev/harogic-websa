# Licensing implementation and review

This records the local review at base revision `ee0c0b8c` on 2026-11-08.
The new license applies only to copyright interests the granting holders can
authorize. [Scope](../../LICENSING.md), [standard AGPL text](../../LICENSE),
[section 7 exception](../../LICENSE-EXCEPTION) and [third-party inventory](../../THIRD_PARTY_NOTICES.md)
must be read together. This record is not a finding that all distribution
rights have been cleared.

## Evidence and authorization boundary

The working tree was clean before this task. Local history contains 760
commits on HEAD, one recorded author/committer identity, Tommy Wang
`<hui@gang.st>`, and no `Co-authored-by` or `Signed-off-by` trailers found.
No CLA, copyright assignment or employer clearance document was found in the
repository. A single Git identity cannot prove that every line is owned by
that person; imported work and separately licensed contributions are excluded.
The task authorizes changes only for project-owned rights that can be granted.

The previous root MIT notice is preserved byte for byte in
[LICENSES/MIT-legacy.txt](../../LICENSES/MIT-legacy.txt). Earlier MIT grants are
not withdrawn. Upstream MIT, Apache, GPL and other notices stay effective for
their own material, including modifications already maintained under those terms.
The standard AGPL text was downloaded without alteration from
[GNU](https://www.gnu.org/licenses/agpl-3.0.txt), SHA-256
`0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0`.

## Linking and distribution boundaries

| Dependency | Repository evidence | Resulting boundary |
|---|---|---|
| HAROGIC SDK | `Makefile` links `backend/sdk/san90_sdk.cpp` to external `libhtraapi.so.0.55.100`; Rust loads the project shim. `htra_api.py` uses ctypes. | SDK stays separately acquired under `/opt/htraapi`; no SDK headers/libraries are added to packages. Exception supplies only the project's permission for the combination. |
| FDK AAC | `dsp/core/build.rs` uses native `pkg-config --libs fdk-aac` (no `--static`), and static `libfdk.a` on wasm32. | Covered by the exception; the full Fraunhofer NOTICE remains applicable, including its source and patent conditions. |
| libxaac | Same build script statically links its archive on wasm32 only. | Remains Apache-2.0 with its LICENSE/NOTICE; it is not one of the two excepted dependencies. |
| ggmorse / DeepFilterNet / other Rust libraries | C++ compile/archive linkage and Cargo dependencies | Retain upstream terms and notices; no generic proprietary-module exemption. |
| ONNX Runtime / models | Dynamic library / graphs loaded by native NR | Original library/model terms and notices remain. |
| RTTY and other providers / Soapy driver | Installer scripts and existing pipes/network integration | Independent installation and existing GPL/LGPL boundaries remain unchanged. |

The local ELF/runtime check confirms `libfdk-aac.so.2` as a service dependency:
`pkg-config` reports FDK **3.0.0**, and the running process maps
`/usr/lib/libfdk-aac.so.2.0.4`. The SDK shim maps the separately installed
`/opt/htraapi/lib/x86_64/libhtraapi.so.0.55.100`. These identify the installed
binaries, not the exact source/patch inputs of the committed WASM archives.

The exception permits static/dynamic linking, runtime loading and conveying the
combination with HAROGIC SDK and/or FDK AAC. It preserves project source duties,
including modified versions, adapters and build/install scripts, and AGPL
section 13's network offer. It does not remove a third party's own source
requirements or authorize a vendor SDK redistribution that its terms prohibit.

## Source entry and packaging

`npm run build` (or `npm run dev`) uses `tools/prepare_legal.py` through Vite to
prepare local license/notice copies and a versioned source archive. It needs
Python 3.10+ and Git in a checkout. The footer links to the license page and
the source archive for that build, with the source ID in the link title.
No upload or external release is involved.
The development server prepares a startup snapshot; restart it after source
changes to refresh that offer. Production builds always prepare a new snapshot.

The source ID hashes actual file contents, paths and executable bits, so local
edits and new project source files are represented even before a commit.
`source-manifest.json` records the Git base, file hashes and archive digest.
The archive includes repository source, lockfiles, fixtures and build inputs;
it excludes runtime outputs, generated legal bundles, Git internals, and the
vendor Python wrapper whose redistribution authorization is unresolved. It
does not collect anything from `/opt/htraapi`. An extracted archive can be
rebuilt without Git using its included `SOURCE-MANIFEST.json`.

Rebuild the service and frontend from matching inputs before packaging.
`tools/package-runtime.sh` carries the frontend license/source bundle into
runtime without copying SDK libraries, and verifies that the source snapshot
matches the current project inputs. Keep the bundle alongside the served
assets, free to download. If an operator changes a network version, regenerate
the matching offer; a link to a repository branch or an older release alone is
not an offer of that operator's modified source.

This snapshot supplies the project tree; it does not certify complete source
availability for every third-party binary. Before distribution, supply any
additional required dependency sources, patches, notices and installation
information. In particular, neither a link to FDK's current upstream branch
nor a lockfile resolves the missing exact source provenance of a committed
codec archive. Existing native/JS/WASM artifacts are not relicensed merely by
editing these documents; historical MIT binaries keep their previous grants.

## Specific questions for legal and provenance review

| Item | Concrete evidence | Still to establish |
|---|---|---|
| Ownership / external contributions | One Git identity; generic previous copyright notice says "contributors"; no assignment/CLA found. | Confirm authorship, employment/contract rights, and permission to grant both AGPL-3.0-only and the exception for any externally supplied material. Preserve or separately license any unconfirmed contribution. |
| HAROGIC SDK and Python wrapper | No LICENSE, NOTICE or EULA found in installed `/opt/htraapi`; development docs identify `htra_api.py` as HAROGIC's wrapper. SDK bundles other libraries including liquid, MKL, USB and OpenMP implementations. | Obtain the actual applicable SDK agreement: combination/linking/loading permission, wrapper/header copying and modification rights, redistribution limits, and terms for each bundled library. Public download availability is not proof of those rights. |
| FDK AAC | [NOTICE](../../dsp/core/fdk/NOTICE) §2 requires full notices, free complete codec source/changes for binary recipients, change/date markings and modified-version naming; §3 grants no patents and refers to appropriate patent licenses. | Confirm fulfillment for actual binaries and applicable patent permissions. Exception grants neither patent rights nor permission to ignore these conditions. |
| Codec binary provenance | CI pins FDK 2.0.3 commit `716f4394641d53f0d79c9ddac3fa93b03a49f278`; archive build scripts accept an arbitrary source directory. Local FDK clone is `22128505f539b90a25c7f7463abd00b5d544dee8` with encoder edits; libxaac clone is `2fbadd57bca46693b7a077bcf1513131106d960c` with decoder edits. | Neither local clone proves the committed archives' inputs. Record exact revisions/patches and provide matching source; preserve FDK changes/naming and Apache §4 notices for libxaac. No codec is rebuilt or substituted by this change. |
| DRM provenance | `dsp/core/src/digital/drm/mod.rs` describes GPL-2.0 Dream/DecDRM as algorithm references with no code copied; `dsp/util.rs` calls helpers a port. Earlier "MIT-clean" assertions did not establish rights. | Review original expressions, table origins and any copying/adaptation. `dsp/util.rs` is excluded from the new grant pending clarification. If GPL-2.0-only content was incorporated, assess its actual compatibility/author permissions; this exception cannot waive it. |
| DFN3 model | Committed `DeepFilterNet3_onnx.tar.gz` contains model/config files without a license; vendored DeepFilterNet code has MIT/Apache-2.0 notices. | Map the exact model artifact to its upstream release, license and hash; do not infer model terms solely from code licensing. |
| CTY country data | `gen_cty.py` identifies country-files.com / AD1C; no license accompanies `cty.dat`. | Obtain terms permitting redistribution and generated-data use, and retain any required attribution. No AGPL claim over the data is made. |
| Fonts | Build subsets installed LXGW WenKai fonts; copied installed OFL text; upstream [current OFL](https://github.com/lxgw/LxgwWenKai/blob/main/OFL.txt) also describes reserved names/additional webfont permission. | Map committed subsets to their exact font release and applicable reserved-name/subsetting terms. Do not assume the current upstream terms are the historical artifact's terms. |
| Other dependencies / providers | Existing upstream files, lockfiles, setup scripts and model notices retain their terms. | Verify the actual distribution's complete dependency notices/source duties, including any bundled native libraries; process separation alone is not a universal legal compatibility finding. |

GNU AGPL [sections 7 and 13](https://spdx.org/licenses/AGPL-3.0-only.html)
support the additional-permission mechanism and modified network source offer.
The drafted permission is limited by the granting holders' authority; the
questions above remain open for counsel/rightsholder confirmation before a
new external distribution is treated as cleared.
