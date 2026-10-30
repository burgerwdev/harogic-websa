#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the standalone GPL helper against an external OnAir source checkout.
# Usage: bash tools/pluto/build-digital-source.sh [ONAIR_ROOT [OUTPUT]]
# Requires a C++20 compiler, Git, Cargo and rustc. Cargo runs offline.
set -euo pipefail

if (( $# > 2 )); then
    printf 'Usage: %s [ONAIR_ROOT [OUTPUT]]\n' "$0" >&2
    exit 2
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
onair_root=${1:-${ONAIR_ROOT:-"$script_dir/../../../OnAir"}}
output=${2:-"$script_dir/pluto_digital_source"}
build_dir=${PLUTO_DIGITAL_BUILD_DIR:-"${XDG_CACHE_HOME:-$HOME/.cache}/harogic-websa/pluto-digital"}
pinned_commit=6b97328b1939f569b161c5066fbea8053a76c130
cxx=${CXX:-c++}
cargo=${CARGO:-cargo}

for tool in git "$cxx" "$cargo"; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'Missing required build tool: %s\n' "$tool" >&2
        exit 1
    fi
done
if ! actual_commit=$(git -C "$onair_root" rev-parse HEAD); then
    printf 'OnAir source checkout not found: %s\nSet ONAIR_ROOT or supply the checkout as the first argument.\n' "$onair_root" >&2
    exit 1
fi
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
# Upstream cmake/OnairRust.cmake uses this aggregate archive. Its bare feature
# provides the no_std panic handler; AX.25 CRC and AIS framing require Rust.
env -u MAKEFLAGS -u MFLAGS -u CARGO_MAKEFLAGS \
    CARGO_TARGET_DIR="$build_dir/rust" \
    "$cargo" build --offline --release --manifest-path "$onair_root/rust/Cargo.toml" \
    -p onair --features bare

# Section GC discards receiver functions (and their decoder dependencies) from
# shared encoder/decoder translation units. Do not use --whole-archive.
"$cxx" -std=c++20 -O2 -ffunction-sections -fdata-sections -pthread \
    -I"$onair_root/core/include" -I"$onair_root/core/src/hfdig" \
    "$script_dir/pluto_digital_source.cpp" "${sources[@]}" \
    "$build_dir/rust/release/libonair_rs.a" -Wl,--gc-sections -o "$output"
printf 'Built %s\nOnAir license/provenance: %s/LICENSE, %s/REUSE.toml, %s/THIRD_PARTY.md\n' \
    "$output" "$onair_root" "$onair_root" "$onair_root" >&2
