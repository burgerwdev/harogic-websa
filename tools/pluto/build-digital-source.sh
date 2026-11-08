#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the standalone GPL helper against an external OnAir source checkout.
# Usage: bash tools/pluto/build-digital-source.sh [ONAIR_ROOT [OUTPUT]]
# With no source argument, reuse a clean pinned checkout or fetch one in cache.
# Requires a C++20 compiler, Git, Python, Cargo and rustc; no radio access.
set -euo pipefail

if (( $# > 2 )); then
    printf 'Usage: %s [ONAIR_ROOT [OUTPUT]]\n' "$0" >&2
    exit 2
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
build_dir=${PLUTO_DIGITAL_BUILD_DIR:-"${XDG_CACHE_HOME:-$HOME/.cache}/harogic-websa/pluto-digital"}
pinned_commit=6b97328b1939f569b161c5066fbea8053a76c130
onair_root=${1:-${ONAIR_ROOT:-}}
output=${2:-"$build_dir/bin/pluto_digital_source"}
cxx=${CXX:-c++}
cargo=${CARGO:-cargo}

for tool in git python3 "$cxx" "$cargo"; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'Missing required build tool: %s\n' "$tool" >&2
        exit 1
    fi
done
if [[ -z "$onair_root" ]]; then
    candidate="$script_dir/../../../OnAir"
    if [[ "$(git -C "$candidate" rev-parse HEAD 2>/dev/null || true)" == "$pinned_commit" ]] &&
       git -C "$candidate" diff --quiet HEAD -- core/include core/src/hfdig core/src/packet core/src/ais rust; then
        onair_root=$candidate
    else
        onair_root="$build_dir/sources/OnAir-$pinned_commit"
        if [[ ! -d "$onair_root/.git" ]]; then git init -q "$onair_root"; fi
        if ! git -C "$onair_root" cat-file -e "$pinned_commit^{commit}" 2>/dev/null; then
            printf 'Downloading pinned OnAir encoder sources…\n' >&2
            git -C "$onair_root" fetch --depth 1 https://github.com/adswill/OnAir.git "$pinned_commit"
        fi
        git -C "$onair_root" -c advice.detachedHead=false checkout --detach "$pinned_commit"
    fi
fi
if ! actual_commit=$(git -C "$onair_root" rev-parse HEAD); then
    printf 'OnAir source checkout not found: %s\nSet ONAIR_ROOT or supply the checkout as the first argument.\n' "$onair_root" >&2
    exit 1
fi
git -C "$onair_root" diff --exit-code HEAD -- core/include core/src/hfdig core/src/packet core/src/ais rust
if [[ "$actual_commit" != "$pinned_commit" ]]; then
    printf 'OnAir revision mismatch: expected %s, found %s\nUse a checkout of the pinned revision.\n' "$pinned_commit" "$actual_commit" >&2
    exit 1
fi
printf 'Building standalone GPL-3.0-or-later helper from OnAir %s (%s)\n' "$pinned_commit" "$onair_root" >&2

sources=(
    core/src/hfdig/hfdig_ftx.cpp
    core/src/hfdig/hfdig_ftx_msg.cpp
    core/src/hfdig/hfdig_ftx_tables.cpp
    core/src/hfdig/hfdig_wspr.cpp
    core/src/packet/packet_gen.cpp
    core/src/packet/packet_ax25.cpp
    core/src/packet/packet_ax25_rs.cpp
    core/src/packet/packet_rx.cpp
    core/src/ais/ais_gen.cpp
    core/src/ais/ais_proto.cpp
    core/src/ais/ais_proto_rs.cpp
)
for index in "${!sources[@]}"; do
    sources[$index]="$onair_root/${sources[$index]}"
    if [[ ! -f "${sources[$index]}" ]]; then
        printf 'Required OnAir source missing: %s\n' "${sources[$index]}" >&2
        exit 1
    fi
done

mkdir -p -- "$build_dir" "$(dirname -- "$output")"
work=$(mktemp -d "$build_dir/.encoder.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
# Upstream cmake/OnairRust.cmake uses this aggregate archive. Its bare feature
# provides the no_std panic handler; AX.25 CRC and AIS framing require Rust.
env -u MAKEFLAGS -u MFLAGS -u CARGO_MAKEFLAGS \
    CARGO_TARGET_DIR="$build_dir/rust" \
    "$cargo" build --locked --release -j 1 --manifest-path "$onair_root/rust/Cargo.toml" \
    -p onair --features bare

# Section GC discards receiver functions (and their decoder dependencies) from
# shared encoder/decoder translation units. Do not use --whole-archive.
"$cxx" -std=c++20 -O2 -ffunction-sections -fdata-sections -pthread \
    -I"$onair_root/core/include" -I"$onair_root/core/src/hfdig" \
    "$script_dir/pluto_digital_source.cpp" "${sources[@]}" \
    "$build_dir/rust/release/libonair_rs.a" -Wl,--gc-sections -o "$work/pluto_digital_source"
install -m755 "$work/pluto_digital_source" "$output.new-$$"
mv -f "$output.new-$$" "$output"
mkdir -p "$output.licenses"
cp "$onair_root/LICENSE" "$onair_root/REUSE.toml" "$onair_root/THIRD_PARTY.md" "$output.licenses/"
[[ ! -d "$onair_root/LICENSES" ]] || cp -R "$onair_root/LICENSES" "$output.licenses/"
cp "$script_dir/pluto_digital_source.cpp" "$script_dir/build-digital-source.sh" "$output.licenses/"
python3 - "$output" "$pinned_commit" "$onair_root" "$script_dir" <<'PY'
import hashlib, json, sys
from pathlib import Path
output, revision, source, script = sys.argv[1:]
digest = lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
metadata = {'revision': revision, 'source': str(Path(source).resolve()),
            'encoder_sha256': digest(output),
            'wrapper_sha256': digest(Path(script) / 'pluto_digital_source.cpp'),
            'build_sha256': digest(Path(script) / 'build-digital-source.sh'),
            'license': 'GPL-3.0-or-later'}
Path(output + '.json').write_text(json.dumps(metadata, indent=2) + '\n')
PY
printf 'Built %s\nOnAir license/provenance: %s/LICENSE, %s/REUSE.toml, %s/THIRD_PARTY.md\n' \
    "$output" "$onair_root" "$onair_root" "$onair_root" >&2
