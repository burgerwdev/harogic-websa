#!/usr/bin/env bash
# Build the independent GPL GNSS-SDR receiver; never start a receiver or service.
set -euo pipefail
REF=cca9e3f04d90488d28379afd7988754f5b90b9eb
SHA=f82ecb86b24cf3c0cf14c122d482335dd51e56fe7dd4dd267aea32134cc83356
PREFIX="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
ARCHIVE_OVERRIDE=
BRIDGE=
DEPENDENCY_PREFIX=
BUILD_PYTHON="${WEBSA_BUILD_PYTHON:-python3}"
while (($#)); do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --archive) ARCHIVE_OVERRIDE="$2"; shift 2 ;;
    --bridge) BRIDGE="$2"; shift 2 ;;
    --dependency-prefix) DEPENDENCY_PREFIX="$2"; shift 2 ;;
    --python) BUILD_PYTHON="$2"; shift 2 ;;
    --help|-h)
      cat <<'HELP'
Usage: bash tools/setup-gnss-decoder.sh [--prefix DIR] [--archive FILE] [--bridge FILE]
Build pinned GNSS-SDR 0.0.20 serially in a local prefix. No sudo, service changes,
RF access, tests, simulations or profile execution. --archive reuses the exact
SHA256-checked source archive. Source, licenses and build tree remain in prefix.
--bridge installs the separately built native backend/target/release/websa-gnss.
If omitted, build it later with cargo build --release --bin websa-gnss and copy
it to <prefix>/bin/websa-gnss. Source <prefix>/gnss-env.sh before WebSA startup.
--dependency-prefix DIR adds a local development-library prefix (for example
an extracted Arch package's usr directory) to CMake's dependency search paths.
GNU Radio runtime libraries alone are insufficient: Boost and other development
headers must be installed. A missing Boost_INCLUDE_DIR requires Arch's boost
package in addition to boost-libs. When extracting packages into a local prefix,
extract matching-version boost AND boost-libs into that same prefix: Boost's
CMake package files reference their own prefix's shared-library paths.
--python FILE (or WEBSA_BUILD_PYTHON) selects build-time Python with Mako installed,
including a private virtual environment. It is not used by the runtime bridge.
Arch dependencies (install separately): gcc make cmake pkgconf git boost
boost-libs libvolk gnuradio blas lapack hdf5 openssl pugixml libmatio protobuf
libpcap python-mako. Upstream can fetch missing Armadillo/Abseil dependencies.
Requires curl, tar, sha256sum. Hardware sources, tests and GPU support disabled.
HELP
      exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
for tool in cmake make curl tar sha256sum pkg-config; do
  command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }
done
BUILD_PYTHON="$(command -v "$BUILD_PYTHON")" || { printf 'Build Python unavailable\n' >&2; exit 1; }
"$BUILD_PYTHON" -c 'import mako' || {
  printf 'Install Mako in a private build virtual environment and select its interpreter with --python.\n' >&2; exit 1;
}
pkg-config --exists gnuradio-runtime protobuf fftw3f || {
  printf 'Install GNU Radio, protobuf and FFTW development dependencies first.\n' >&2; exit 1;
}
mkdir -p "$PREFIX"
PREFIX="$(cd "$PREFIX" && pwd -P)"
SOURCE="$PREFIX/sources/gnss-sdr-$REF"
BUILD="$PREFIX/build/gnss-sdr-$REF"
NOTICE="$PREFIX/share/websa-decoders/gnss-sdr"
ARCHIVE="$PREFIX/downloads/gnss-sdr-$REF.tar.gz"
URL="https://codeload.github.com/gnss-sdr/gnss-sdr/tar.gz/$REF"
mkdir -p "$PREFIX/downloads" "$SOURCE" "$BUILD" "$NOTICE" "$PREFIX/bin"
if [[ -n "$ARCHIVE_OVERRIDE" ]]; then
  digest="$(sha256sum "$ARCHIVE_OVERRIDE")"
  [[ "${digest%% *}" == "$SHA" ]] || { printf 'Source archive checksum mismatch\n' >&2; exit 1; }
  [[ "$ARCHIVE_OVERRIDE" == "$ARCHIVE" ]] || cp "$ARCHIVE_OVERRIDE" "$ARCHIVE"
elif [[ ! -f "$ARCHIVE" ]]; then
  download_temp="$(mktemp "$PREFIX/downloads/.gnss-download.XXXXXX")"
  trap 'rm -f -- "$download_temp"' EXIT
  curl --fail --location --proto '=https' --proto-redir '=https' "$URL" --output "$download_temp"
  digest="$(sha256sum "$download_temp")"
  [[ "${digest%% *}" == "$SHA" ]] || { printf 'Source archive checksum mismatch\n' >&2; exit 1; }
  mv "$download_temp" "$ARCHIVE"
fi
digest="$(sha256sum "$ARCHIVE")"
[[ "${digest%% *}" == "$SHA" ]] || { printf 'Cached source archive checksum mismatch\n' >&2; exit 1; }
tar -xzf "$ARCHIVE" --strip-components=1 -C "$SOURCE"
cp -R "$SOURCE/LICENSES" "$NOTICE/"
FLAGS=(-DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_FLAGS_RELEASE=-O2\ -DNDEBUG
  -DCMAKE_C_FLAGS_RELEASE=-O2\ -DNDEBUG "-DCMAKE_INSTALL_PREFIX=$PREFIX"
  -DENABLE_UHD=OFF -DENABLE_ZMQ=OFF -DENABLE_OSMOSDR=OFF
  -DENABLE_UNIT_TESTING=OFF -DENABLE_SYSTEM_TESTING=OFF -DENABLE_UNIT_TESTING_EXTRA=OFF
  -DENABLE_SYSTEM_TESTING_EXTRA=OFF -DENABLE_GNSS_SIM_INSTALL=OFF -DENABLE_INSTALL_TESTS=OFF
  -DENABLE_OPENCL=OFF -DENABLE_CUDA=OFF -DENABLE_FPGA=OFF -DENABLE_PROFILING=OFF
  -DENABLE_BENCHMARKS=OFF)
FLAGS+=("-DPython3_EXECUTABLE=$BUILD_PYTHON" "-DPython_EXECUTABLE=$BUILD_PYTHON" "-DPYTHON_EXECUTABLE=$BUILD_PYTHON")
BUILD_ENV=()
if [[ -n "$DEPENDENCY_PREFIX" ]]; then
  FLAGS+=("-DCMAKE_PREFIX_PATH=$DEPENDENCY_PREFIX" "-DBOOST_ROOT=$DEPENDENCY_PREFIX")
  # Imported GNU Radio targets do not carry a locally extracted Boost include
  # path. CPATH also reaches nested external builds without changing CMake's
  # cached optimization flags or modifying the caller's environment.
  BUILD_ENV+=("CPATH=$DEPENDENCY_PREFIX/include${CPATH:+:$CPATH}")
fi
{
  printf 'GNSS-SDR release 0.0.20\nCommit: %s\nArchive: %s\nSHA256: %s\nSource: %s\n' "$REF" "$URL" "$SHA" "$SOURCE"
  printf 'Configure flags:'; printf ' %q' "${FLAGS[@]}"; printf '\nBuild: cmake --build <build> --parallel 1\n'
  if ((${#BUILD_ENV[@]})); then printf 'Build environment:'; printf ' %q' "${BUILD_ENV[@]}"; printf '\n'; fi
  printf 'Separate GPL executable; MIT WebSA does not link its implementation.\n'
} > "$NOTICE/SOURCE.txt"
if ! env "${BUILD_ENV[@]}" cmake -S "$SOURCE" -B "$BUILD" -G 'Unix Makefiles' "${FLAGS[@]}"; then
  printf 'GNSS-SDR configuration failed. Install the development dependencies listed by --help, or use --dependency-prefix for extracted local packages. Build retained at %s\n' "$BUILD" >&2
  exit 1
fi
env "${BUILD_ENV[@]}" cmake --build "$BUILD" --parallel 1
env "${BUILD_ENV[@]}" cmake --install "$BUILD"
if [[ -n "$BRIDGE" ]]; then install -m 755 "$BRIDGE" "$PREFIX/bin/websa-gnss"; fi
{
  printf '# Generated by setup-gnss-decoder.sh; source before launching WebSA.\n'
  printf 'export WEBSA_GNSS_SDR_PATH=%q\n' "$PREFIX/bin/gnss-sdr"
  printf 'export WEBSA_GNSS_BRIDGE_PATH=%q\n' "$PREFIX/bin/websa-gnss"
} > "$PREFIX/gnss-env.sh"
printf 'GNSS-SDR installed. Native bridge must also exist at %s/bin/websa-gnss\nSource %s/gnss-env.sh before WebSA startup.\n' "$PREFIX" "$PREFIX"
