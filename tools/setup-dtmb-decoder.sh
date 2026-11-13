#!/usr/bin/env bash
set -euo pipefail
revision=2da80af5597927d83d99fd832901cd51bf38dfb4
prefix="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
source_dir=""
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
companion_dir="$script_dir/companions/dtmb"
[[ -d "$companion_dir" ]] || companion_dir="$script_dir"
while (($#)); do
  case "$1" in
    --prefix) prefix="${2:?}"; shift 2 ;;
    --source) source_dir="${2:?}"; shift 2 ;;
    --help|-h) printf '%s\n' 'Usage: tools/setup-dtmb-decoder.sh [--prefix DIR] [--source ONAIR_CHECKOUT]' 'Builds an isolated CPU DTMB companion from pinned GPL OnAir source; requires C++20, git and fftw3f development files.'; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
[[ -n "$prefix" ]]
read -ra fftw_flags <<< "$(pkg-config --cflags --libs fftw3f)"
mkdir -p "$prefix/bin" "$prefix/sources" "$prefix/share/licenses/websa-dtmb"
build_dir="$(mktemp -d "$prefix/sources/dtmb-build.XXXXXX")"
trap 'rm -rf -- "$build_dir"' EXIT
if [[ -z "$source_dir" && -f "$script_dir/onair-source.tar.gz" ]]; then
  cp "$script_dir/onair-source.tar.gz" "$build_dir/"
else
  if [[ -z "$source_dir" ]]; then
    source_dir="$prefix/sources/onair-dtmb"
    [[ -d "$source_dir/.git" ]] || git init "$source_dir"
    git -C "$source_dir" cat-file -e "$revision^{commit}" 2>/dev/null || git -C "$source_dir" fetch --depth 1 https://github.com/adswill/OnAir.git "$revision"
  fi
  git -C "$source_dir" archive "$revision" core LICENSE | gzip > "$build_dir/onair-source.tar.gz"
fi
tar -xf "$build_dir/onair-source.tar.gz" -C "$build_dir"
patch -s -d "$build_dir" -p1 < "$companion_dir/centered-input.patch"
patch -s -d "$build_dir" -p1 < "$companion_dir/fftw.patch"
patch -s -d "$build_dir" -p1 < "$companion_dir/leakage.patch"
patch -s -d "$build_dir" -p1 < "$companion_dir/fec-throughput.patch"
sources=()
for unit in defs front sync demod map ldpc ldpc_tables chain rx; do sources+=("$build_dir/core/src/dtmb/dtmb_$unit.cpp"); done
optimization=()
for flag in -march=native -fcx-limited-range -fcomplex-arithmetic=basic; do
  if "${CXX:-c++}" "$flag" -x c++ -c /dev/null -o "$build_dir/probe.o" 2>/dev/null; then optimization+=("$flag"); fi
done
"${CXX:-c++}" -std=c++20 -O3 -DNDEBUG "${optimization[@]}" -fno-math-errno -fno-trapping-math -fno-signed-zeros -fassociative-math -pthread -DDECT2_PORTABLE -DWEBSA_DTMB_CENTERED -DWEBSA_DTMB_FFTW "-DWEBSA_ONAIR_REV=\"$revision\"" \
  -I "$build_dir/core/include" "$companion_dir/main.cpp" "$companion_dir/fft.cpp" "${sources[@]}" \
  "$build_dir/core/src/dsp/dsp_compat.cpp" "$build_dir/core/src/dsp/gpu_ldpc_stub.cpp" "${fftw_flags[@]}" -o "$build_dir/websa-dtmb"
install -m755 "$build_dir/websa-dtmb" "$prefix/bin/websa-dtmb"
install -m644 "$build_dir/LICENSE" "$prefix/share/licenses/websa-dtmb/LICENSE"
cp "$companion_dir/"*.cpp "$companion_dir/"*.patch "$build_dir/"
cp "${BASH_SOURCE[0]}" "$build_dir/build.sh"
install -m644 "$build_dir/"*.cpp "$build_dir/"*.patch "$build_dir/build.sh" "$build_dir/onair-source.tar.gz" "$prefix/share/licenses/websa-dtmb/"
printf '%s\n' "OnAir DTMB: https://github.com/adswill/OnAir/tree/$revision" 'Companion and linked decoder: GPL-3.0-or-later. Corresponding source is included.' > "$prefix/share/licenses/websa-dtmb/NOTICE"
printf 'Installed %s\n' "$prefix/bin/websa-dtmb"
