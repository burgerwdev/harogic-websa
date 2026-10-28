#!/bin/bash
# Build the official LGPL SoapyHarogic driver with the user's installed SDK.
# Download only the five required driver source/license files, never SDK files.
set -euo pipefail

REV=762dbfa1512c705e1d8b86e820b06e8a10e08a92
UPSTREAM=https://github.com/HAROGIC-Technologies/soapy-htra
RAW="https://raw.githubusercontent.com/HAROGIC-Technologies/soapy-htra/$REV/soapy-htra"
SDK_ROOT="${SDK_ROOT:-/opt/htraapi}"
PREFIX="${SOAPY_PREFIX:-$HOME/.local/share/harogic-websa/soapy}"
CACHE="${SOAPY_SRC_CACHE:-$HOME/.cache/harogic-websa/soapy-htra}"
APPLY_PATCH=1
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PATCH_SOURCE="$ROOT/tools/soapy-htra-stream-format.patch"
CHECK_SOURCE="$ROOT/tools/checks/soapy_stream_format_check.cpp"

usage() {
  cat <<EOF
Usage: tools/build-soapy.sh [--sdk-root DIR] [--prefix DIR]
       tools/build-soapy.sh --check-patch
       tools/build-soapy.sh --check-stream [--sdk-root DIR]

Download five source/license files from official SoapyHarogic revision:
  $REV
Build with the existing HTRA SDK and install locally. No SDK binaries,
upstream installers, root commands, or dependency installers are used.

  --sdk-root DIR   Installed SDK root (SDK_ROOT; default /opt/htraapi)
  --prefix DIR     Install prefix (SOAPY_PREFIX; default
                   \$HOME/.local/share/harogic-websa/soapy)
  --build          Explicit build action (also the default)
  --check-patch    SDK-free and hardware-free patch applicability check. Downloads
                   only pinned driver sources; does not compile or install.
  --check-stream   Hardware-free stream format, ring, lifecycle, and error
                   contracts using real SDK headers and an HTRA function stub.
                   Requires installed SDK headers; does not install or open hardware.
  --help, -h       Show this help without fetching, building, or installing

Build requires curl, cmake, pkg-config, realpath, patch, a C++ compiler,
SoapySDR 0.8 development files, and vendor SDK headers/libraries. Builds use
one compile job. --check-patch requires only curl and patch beyond standard
shell utilities. --check-stream requires curl, patch, pkg-config, realpath,
c++, SoapySDR development files, and vendor SDK headers; it links SoapySDR
only. Both checks download into a private temporary directory and remove it
on exit; neither downloads proprietary SDK files.
SOAPY_SRC_CACHE selects the source cache (default
\$HOME/.cache/harogic-websa/soapy-htra). Pinned sources and LGPL LICENSE
remain there after installation; temporary download/build files are removed.
Every build applies tools/soapy-htra-stream-format.patch to a private copy
before compilation. The cached revision stays unmodified. The patch supports
CF32, CS16 and CS8 output, with native integer transport for SoapyRemote.
AUTO selects CS16 up to 60 MSps and CS8 above it before stream setup. Setup
fixes the SDK width until closeStream; integer output requires a matching
native_format. CF32 conversion uses signed full scales 128, 32768 and 2^31.
CS32 is an SDK input width only. Unsupported width combinations are rejected.
The transfer MTU stays 16384 samples before and after activation, independent
of SDK packet size. Active reads return available samples up to the requested
count. Inactive reads obey their timeout; fatal worker errors return a stream
error and a full host ring reports SOAPY_SDR_OVERFLOW.
The host ring holds 50 ms of IQ, with a two-packet/16384-sample floor;
activation allocates storage and subsequent stream changes reuse it.
USB discovery uses Device_List without opening devices. One device handle
serves capability queries and streams until device destruction, including
close/setup cycles. Live controls stop and join acquisition before configuration;
unchanged settings skip it and inactive settings batch at activation.
Requested rates map to the nearest advertised rate and activation reads the SDK rate.

Module directory: <prefix>/lib/SoapySDR/modules0.8
Set WEBSA_SOAPY_MODULE_PATH to that directory for a custom prefix.
For direct SoapySDR tools, set SOAPY_SDR_PLUGIN_PATH to the same directory.
The manager accepts the driver when SoapySDRUtil --info lists the lowercase
harogic factory.
EOF
}

resolve_sdk_inc() {
  SDK_ROOT="$(realpath -e -- "$SDK_ROOT")"
  if [[ -f "$SDK_ROOT/inc/htra_api.h" || -f "$SDK_ROOT/inc/HTRA_API.h" ]]; then
    SDK_INC="$SDK_ROOT/inc"
  elif [[ -f "$SDK_ROOT/include/htra_api.h" || -f "$SDK_ROOT/include/HTRA_API.h" ]]; then
    SDK_INC="$SDK_ROOT/include"
  else
    echo "HTRA headers missing under $SDK_ROOT/inc or $SDK_ROOT/include" >&2
    exit 1
  fi
}

prepare_check_sources() {
  local work="$1" source_file
  [[ -f "$PATCH_SOURCE" ]] || { echo "integration patch missing: $PATCH_SOURCE" >&2; exit 1; }
  for source_file in HarogicDevice.cpp HarogicDevice.hpp; do
    curl --fail --location --silent --show-error \
      "$RAW/$source_file" --output "$work/$source_file"
  done
  patch --forward --silent --force -p1 -d "$work" < "$PATCH_SOURCE"
}

check_patch() (
  [[ $# -eq 1 ]] || { echo "--check-patch takes no arguments" >&2; exit 2; }
  for required in curl patch; do
    command -v "$required" >/dev/null || { echo "$required is required; install dependencies yourself" >&2; exit 1; }
  done
  local work
  work="$(mktemp -d)"
  trap 'rm -rf -- "$work"' EXIT
  prepare_check_sources "$work"
  printf '%s\n' "Patch applies to SoapyHarogic revision $REV (no SDK or hardware required)."
)

check_stream() (
  shift
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --sdk-root)
        [[ $# -ge 2 && -n "$2" ]] || { echo "--sdk-root requires a directory" >&2; exit 2; }
        SDK_ROOT="$2"; shift 2 ;;
      --sdk-root=*) SDK_ROOT="${1#--sdk-root=}"; shift ;;
      *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
  done
  for required in curl patch pkg-config realpath c++; do
    command -v "$required" >/dev/null || { echo "$required is required; install dependencies yourself" >&2; exit 1; }
  done
  resolve_sdk_inc
  SOAPY_CFLAGS="$(pkg-config --cflags SoapySDR)"
  SOAPY_LIBS="$(pkg-config --libs SoapySDR)"
  [[ -f "$CHECK_SOURCE" ]] || { echo "stream contract source missing: $CHECK_SOURCE" >&2; exit 1; }
  local work
  work="$(mktemp -d)"
  trap 'rm -rf -- "$work"' EXIT
  prepare_check_sources "$work"
  # Link SoapySDR only. The stub supplies HTRA symbols; libhtraapi stays out.
  # shellcheck disable=SC2086
  c++ -std=c++17 -O2 -Wall -Wextra -pthread \
    -I"$work" -I"$SDK_INC" $SOAPY_CFLAGS \
    -o "$work/soapy_stream_format_check" \
    "$CHECK_SOURCE" "$work/HarogicDevice.cpp" \
    $SOAPY_LIBS
  "$work/soapy_stream_format_check"
)

case "${1:-}" in
  --check-patch) check_patch "$@"; exit 0 ;;
  --check-stream) check_stream "$@"; exit 0 ;;
esac

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --build) APPLY_PATCH=1; shift ;;
    --sdk-root|--prefix)
      [[ $# -ge 2 && -n "$2" ]] || { echo "$1 requires a directory" >&2; exit 2; }
      if [[ "$1" == --sdk-root ]]; then SDK_ROOT="$2"; else PREFIX="$2"; fi
      shift 2
      ;;
    --sdk-root=*) SDK_ROOT="${1#--sdk-root=}"; shift ;;
    --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
[[ -n "$SDK_ROOT" && -n "$PREFIX" && -n "$CACHE" ]] || {
  echo 'SDK root, prefix, and cache must be non-empty' >&2; exit 2;
}
for required in curl cmake pkg-config realpath patch; do
  command -v "$required" >/dev/null || { echo "$required is required; install dependencies yourself" >&2; exit 1; }
done
resolve_sdk_inc
PREFIX="$(realpath -m -- "$PREFIX")"
CACHE="$(realpath -m -- "$CACHE")"
SOAPY_VERSION="$(pkg-config --modversion SoapySDR)"
case "$SOAPY_VERSION" in
  0.8.*) ;;
  *) echo "SoapySDR 0.8 development files required; found $SOAPY_VERSION" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64) LIB_ARCH=x86_64 ;;
  aarch64|arm64) LIB_ARCH=aarch64 ;;
  *) echo 'Supported SDK architectures: x86_64 and aarch64' >&2; exit 1 ;;
esac
SDK_LIB=""
for candidate in \
  "$SDK_ROOT/lib/$LIB_ARCH/libhtraapi.so" \
  "$SDK_ROOT/lib/$LIB_ARCH/libhtraapi.so.0" \
  "$SDK_ROOT/lib/libhtraapi.so" \
  "$SDK_ROOT/lib64/libhtraapi.so"
do
  if [[ -f "$candidate" ]]; then SDK_LIB="$candidate"; break; fi
done
[[ -n "$SDK_LIB" ]] || { echo "libhtraapi missing under $SDK_ROOT for $LIB_ARCH" >&2; exit 1; }
SDK_LIB_DIR="$(dirname "$SDK_LIB")"

mkdir -p -- "$CACHE"
WORK="$(mktemp -d "$CACHE/.build-XXXXXX")"
# Remove only this invocation's private staging/build directory.
trap 'rm -rf -- "$WORK"' EXIT
FILES=(CMakeLists.txt HarogicDevice.cpp HarogicDevice.hpp LICENSE cmake/FindLibHTRA.cmake)
if [[ "$APPLY_PATCH" -eq 1 && ! -f "$PATCH_SOURCE" ]]; then
  echo "integration patch missing: $PATCH_SOURCE" >&2
  exit 1
fi
mkdir -p -- "$WORK/source/cmake"
for source_file in "${FILES[@]}"; do
  curl --fail --location --silent --show-error \
    "$RAW/$source_file" --output "$WORK/source/$source_file"
done
# Update only the managed files after all downloads succeed. Retain upstream
# copyright notices, source and LGPL text alongside the exact source revision.
SRC="$CACHE/$REV"
mkdir -p -- "$SRC/cmake"
for source_file in "${FILES[@]}"; do
  install -m 644 -- "$WORK/source/$source_file" "$SRC/$source_file"
done
{
  printf '%s\n' \
    "SoapyHarogic source: $UPSTREAM" \
    "Revision: $REV" \
    'License: GNU LGPL v2.1 or later; see LICENSE and source copyright notices.' \
    'Only driver source/license files were downloaded; the proprietary SDK remains external.'
  if [[ "$APPLY_PATCH" -eq 1 ]]; then
    printf '%s\n' \
      'Integration: tools/soapy-htra-stream-format.patch (harogic-websa) is applied to a private build copy.' \
      'The patch supports CF32/CS16/CS8 output and matching native integer transport; SDK width is fixed at setup.' \
      'CF32 conversion uses signed full scales 128, 32768 and 2^31; CS32 is an SDK input width only.' \
      'Bulk ring transfers, a stable 16384-sample MTU, inactive timeouts and explicit fatal/overflow errors are preserved.' \
      'USB discovery uses Device_List; capability queries and stream recreation share a handle until device destruction.' \
      'Live controls stop/join acquisition before configuration, skip no-ops and report SDK configuration failures.' \
      'The cached revision in this directory is unmodified.'
  fi
} > "$SRC/UPSTREAM"

if [[ "$APPLY_PATCH" -eq 1 ]]; then
  BUILD_SRC="$WORK/patched"
  mkdir -p -- "$BUILD_SRC"
  cp -a -- "$SRC/." "$BUILD_SRC/"
  # Idempotent on the private copy only. Reverse first when a previous apply is
  # already present, then apply once. The cached revision is never patched.
  if patch --dry-run --reverse --silent --force -p1 -d "$BUILD_SRC" < "$PATCH_SOURCE" >/dev/null 2>&1; then
    patch --reverse --silent --force -p1 -d "$BUILD_SRC" < "$PATCH_SOURCE"
  fi
  patch --forward --silent --force -p1 -d "$BUILD_SRC" < "$PATCH_SOURCE"
  printf '%s\n' \
    "Applied tools/soapy-htra-stream-format.patch to private build copy." \
    "Upstream revision $REV in $SRC is unmodified." \
    >> "$BUILD_SRC/UPSTREAM"
else
  BUILD_SRC="$SRC"
fi

BUILD="$WORK/build"
# Explicit paths prevent FindLibHTRA from selecting another installed SDK.
# Force lib rather than a platform-dependent lib64/multiarch install directory.
# SDK dependencies stay external; libhtraapi itself uses RUNPATH=$ORIGIN.
cmake -S "$BUILD_SRC" -B "$BUILD" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DCMAKE_INSTALL_LIBDIR=lib \
  -DLibHTRA_INCLUDE_DIRS="$SDK_INC" \
  -DLibHTRA_LIBRARIES="$SDK_LIB" \
  -DCMAKE_BUILD_RPATH="$SDK_LIB_DIR" \
  -DCMAKE_INSTALL_RPATH="$SDK_LIB_DIR" \
  -DCMAKE_SKIP_RPATH=OFF
cmake --build "$BUILD" --parallel 1 --target HarogicSupport
cmake --install "$BUILD"
MODULE_DIR="$PREFIX/lib/SoapySDR/modules0.8"
MODULE="$MODULE_DIR/libHarogicSupport.so"
# Check this invocation's install manifest as well as the installed module,
# so a stale preexisting module cannot satisfy the destination assertion.
if [[ ! -f "$MODULE" ]] || ! grep -Fxq -- "$MODULE" "$BUILD/install_manifest.txt"; then
  echo "CMake did not install the module to the required destination: $MODULE" >&2
  exit 1
fi
printf '%s\n' "Installed $MODULE" \
  "WEBSA_SOAPY_MODULE_PATH=$MODULE_DIR" \
  "SOAPY_SDR_PLUGIN_PATH=$MODULE_DIR" \
  "SDK: $SDK_LIB (not copied). LGPL source and LICENSE: $SRC"
