#!/usr/bin/env bash
# Install the independent MIT gps-sdr-sim encoder and its 2022-01-01 RINEX fixture.
# Usage: bash tools/setup-gps-simulator.sh [--prefix DIRECTORY]
# Default: $XDG_CACHE_HOME/harogic-websa/gps-sdr-sim/<revision> (outside the repo).
# Requires curl, tar, sha256sum and a C compiler. No RF or waveform is generated.
# Use --source-dir DIRECTORY with tools/pluto_tx.py gps for a custom prefix.
set -euo pipefail
revision=28ca29a6719475195e3aabd5930c4ed02d67190f
archive_sha256=046209d533e11628ba9cf44abf9717809f981f4de3eff13bd5a6d799f5671560
prefix=${WEBSA_GPS_SIM_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/harogic-websa/gps-sdr-sim/$revision}
if (( $# )); then
    if [[ $# != 2 || $1 != --prefix || -z $2 ]]; then
        printf 'Usage: bash %s [--prefix DIRECTORY]\n' "$0" >&2
        exit 2
    fi
    prefix=$2
fi
compiler=${CC:-cc}
for tool in curl tar sha256sum "$compiler"; do
    command -v "$tool" >/dev/null || { printf 'Missing required tool: %s\n' "$tool" >&2; exit 1; }
done
mkdir -p -- "$prefix"
work=$(mktemp -d "$prefix/.install.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
url="https://codeload.github.com/osqzss/gps-sdr-sim/tar.gz/$revision"
curl --fail --location --retry 3 --output "$work/source.tar.gz" "$url"
printf '%s  %s\n' "$archive_sha256" "$work/source.tar.gz" | sha256sum --check --status
# Extract only the small standalone encoder, its license, and the navigation fixture.
tar -xzf "$work/source.tar.gz" -C "$work" --strip-components=1 \
    "gps-sdr-sim-$revision/gpssim.c" "gps-sdr-sim-$revision/gpssim.h" \
    "gps-sdr-sim-$revision/LICENSE" "gps-sdr-sim-$revision/brdc0010.22n"
"$compiler" -O3 "$work/gpssim.c" -lm -o "$work/gps-sdr-sim"
printf '{"project":"gps-sdr-sim","revision":"%s","archive_sha256":"%s","url":"%s","license":"MIT","rinex":"brdc0010.22n","scenario_time":"2022/01/01,00:00:00","position":[25.2,55.36,10]}\n' \
    "$revision" "$archive_sha256" "$url" > "$work/provenance.json"
# The manifest also binds the installed executable to this installation.
(cd "$work" && sha256sum gps-sdr-sim gpssim.c gpssim.h LICENSE brdc0010.22n provenance.json > SHA256SUMS)
for file in gps-sdr-sim gpssim.c gpssim.h LICENSE brdc0010.22n provenance.json SHA256SUMS; do
    mv -- "$work/$file" "$prefix/$file"
done
printf 'Installed MIT gps-sdr-sim %s in %s\n' "$revision" "$prefix" >&2
printf 'Inspect encoder without hardware: python3 tools/pluto_tx.py gps --source-dir %q --dry-run\n' "$prefix" >&2
