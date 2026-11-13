#!/usr/bin/env bash
# One source-build entry point; provider installers retain their pinned sources.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

CHECK=0
GPS=1
DTMB=1
SDK_ROOT="${SDK_ROOT:-/opt/htraapi}"
export WEBSA_DECODERS_PREFIX="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
PREFIX="$WEBSA_DECODERS_PREFIX"
while (($#)); do
  case "$1" in
    --check) CHECK=1; shift ;;
    --without-gps) GPS=0; shift ;;
    --without-dtmb) DTMB=0; shift ;;
    --help|-h)
      cat <<'HELP'
Usage: ./setup.sh [--check] [--without-gps] [--without-dtmb]
Prepare the frontend, native service, NR runtime, multimon-ng, rtl_433,
OnAir, RTTY, DTMB and GPS decoders. Installed decoders are reused. Builds use at most two jobs.
If SDK_ROOT (default /opt/htraapi) contains the HAROGIC SDK, also build its shim
and Soapy driver. Without the SDK, the fixture service remains available.
--check only checks prerequisites; it does not download, build or install.
--without-gps omits the large GNSS-SDR source build and its build dependencies.
--without-dtmb omits the DTMB companion and AVS+ television player.
WEBSA_DECODERS_PREFIX overrides the shared user-local decoder directory.
No root commands are run. Install system build dependencies before setup:
  Rust >=1.95, Node 22.22+/24.15+/>=26, npm, Python >=3.10, C/C++20,
  make, CMake >=3.15, pkg-config, curl, Git, tar, coreutils, util-linux;
  FDK AAC >=2.0.3 development files.
DTMB additionally needs patch, perl, xz, FFTW3 and x264 development files.
GPS additionally needs the development libraries listed by
  bash tools/setup-gnss-decoder.sh --help
and build-time Python with Mako (WEBSA_BUILD_PYTHON can select its interpreter).
Hardware/Soapy additionally needs SoapySDR development files, SoapyRemote,
SoapySDRUtil and patch. The proprietary SDK must be obtained separately.
After setup: make run (hardware) or WEBSA_FAKE=1 make run (fixtures).
After rebuilding an existing service: make restart, then make decoders-status.
HELP
      exit 0 ;;
    *) printf 'Unknown argument: %s (see --help)\n' "$1" >&2; exit 2 ;;
  esac
done

[[ "$(uname -s)/$(uname -m)" == Linux/x86_64 ]] || {
  echo 'Full setup currently requires Linux x86_64 (the pinned OnAir/NR bundles).' >&2
  exit 1
}
MISSING=()
for command in cargo rustc node npm python3 cc "${CXX:-c++}" make cmake pkg-config curl git tar sha256sum flock; do
  command -v "$command" >/dev/null || MISSING+=("command: $command")
done
if command -v pkg-config >/dev/null; then
  pkg-config --atleast-version=2.0.3 fdk-aac || MISSING+=('development library: fdk-aac >=2.0.3')
fi
if command -v python3 >/dev/null; then
  python3 -c 'import sys; sys.exit(sys.version_info < (3, 10))' || MISSING+=('Python >=3.10')
fi
if command -v rustc >/dev/null; then
  RUST_VERSION="$(rustc --version)"
  [[ "$RUST_VERSION" =~ ^rustc\ 1\.([0-9]+)\. && "${BASH_REMATCH[1]}" -ge 95 ]] || MISSING+=('Rust >=1.95')
fi
if command -v node >/dev/null; then
  node -e 'const [a,b]=process.versions.node.split(".").map(Number); process.exit((a===22&&b>=22)||(a===24&&b>=15)||a>=26?0:1)' || MISSING+=('Node 22.22+/24.15+/>=26')
fi
NEED_GPS=0
if [[ "$DTMB" == 1 ]]; then
  command -v patch >/dev/null || MISSING+=('DTMB command: patch')
  command -v perl >/dev/null || MISSING+=('DTMB command: perl')
  command -v xz >/dev/null || MISSING+=('DTMB command: xz')
  if [[ ! -x "$PREFIX/bin/websa-dtmb" ]] && ! pkg-config --exists fftw3f; then
    MISSING+=('DTMB development library: fftw3f')
  fi
  if [[ ! -x "$PREFIX/bin/websa-dtmb-ffmpeg" ]] && ! pkg-config --exists x264; then
    MISSING+=('DTMB development library: x264')
  fi
fi
if [[ "$GPS" == 1 && ! -x "$PREFIX/bin/gnss-sdr" ]]; then
  NEED_GPS=1
  for library in gnuradio-runtime protobuf fftw3f; do
    if ! command -v pkg-config >/dev/null || ! pkg-config --exists "$library"; then
      MISSING+=("GPS development library: $library")
    fi
  done
  BUILD_PYTHON="${WEBSA_BUILD_PYTHON:-python3}"
  "$BUILD_PYTHON" -c 'import mako' 2>/dev/null || MISSING+=("GPS build Python: Mako for $BUILD_PYTHON (or select WEBSA_BUILD_PYTHON)")
  if command -v "${CXX:-c++}" >/dev/null; then
    printf '#include <boost/version.hpp>\n' | "${CXX:-c++}" -x c++ -E - >/dev/null 2>&1 || MISSING+=('GPS development headers: Boost')
  fi
fi
HARDWARE=0
if [[ -f "$SDK_ROOT/inc/htra_api.h" && -f "$SDK_ROOT/lib/x86_64/libhtraapi.so.0.55.100" ]]; then
  HARDWARE=1
  for command in patch SoapySDRUtil SoapySDRServer; do
    command -v "$command" >/dev/null || MISSING+=("hardware command: $command")
  done
  if ! command -v pkg-config >/dev/null || ! pkg-config --exists SoapySDR; then
    MISSING+=('hardware development library: SoapySDR')
  fi
fi
if ((${#MISSING[@]})); then
  printf 'Install the missing prerequisites, then rerun make setup:\n' >&2
  printf '  - %s\n' "${MISSING[@]}" >&2
  echo 'See ./setup.sh --help and docs/en/GETTING_STARTED.md (中文: docs/zh-CN/GETTING_STARTED.md).' >&2
  exit 1
fi
printf 'Prerequisites OK. Decoder directory: %s\n' "$PREFIX"
[[ "$CHECK" == 0 ]] || exit 0

# A build may replace the executable even if a later provider installation fails.
trap 'echo "If WebSA is already running, use make restart to load the prepared files."' EXIT
make frontend native
[[ -f runtime/linux-x86_64/libonnxruntime.so.1.23.2 ]] || bash tools/fetch-nr-runtime.sh
PROVIDERS=()
[[ -x "$PREFIX/bin/multimon-ng" ]] || PROVIDERS+=(multimon)
[[ -x "$PREFIX/bin/rtl_433" ]] || PROVIDERS+=(rtl433)
[[ -x "$PREFIX/bin/onair-server" ]] || PROVIDERS+=(onair)
if ((${#PROVIDERS[@]})); then
  bash tools/setup-sdr-decoders.sh "${PROVIDERS[@]}"
fi
[[ -x "$PREFIX/bin/websa-rtty" ]] || bash tools/setup-rtty-decoder.sh
if [[ "$DTMB" == 1 ]]; then
  [[ -x "$PREFIX/bin/websa-dtmb" ]] || bash tools/setup-dtmb-decoder.sh
  [[ -x "$PREFIX/bin/websa-dtmb-ffmpeg" ]] || bash tools/setup-dtmb-player.sh
fi
if [[ "$GPS" == 1 ]]; then
  if [[ ! -x "$PREFIX/bin/websa-gnss" ]]; then
    cargo build -j 1 --release --manifest-path backend/Cargo.toml --bin websa-gnss
    install -Dm755 backend/target/release/websa-gnss "$PREFIX/bin/websa-gnss"
  fi
  [[ "$NEED_GPS" == 0 ]] || bash tools/setup-gnss-decoder.sh
fi
if [[ "$HARDWARE" == 1 ]]; then
  make sdk-shim soapy-driver SDK_ROOT="$SDK_ROOT"
else
  printf 'HAROGIC SDK not found at %s; use WEBSA_FAKE=1 make run.\n' "$SDK_ROOT"
  echo 'For hardware, install the SDK and rerun make setup SDK_ROOT=/path/to/sdk.'
fi
echo 'Setup complete. Start with make run, then inspect make decoders-status.'
