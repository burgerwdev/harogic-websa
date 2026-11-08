#!/usr/bin/env bash
# Optional independent decoder tools. Never starts a decoder or installs services.
# Keep downloaded source archives and upstream license notices with the binaries.
set -euo pipefail

MULTIMON_REF=b5f7d71c5e9b5a76093ba55c515968dc40f92804
MULTIMON_SHA=35e0d2342937856c5a2426411c4fcbbb0861c9e0425688bc164fb222785b6a35
RTL433_REF=f35183aaf1d2d905d422fd70bcae4eb7349d6186
RTL433_SHA=6920df3b291359ed36fd9ba04d9cf67d82a235f609612071d3208f925b85523f
ONAIR_VERSION=3.3.0
ONAIR_SHA=c210f8ef192129e705cf76d54731393693d751e33df70b1dd0251a732afa6597
ONAIR_BUNDLE="onair-${ONAIR_VERSION}-linux-x86_64-portable"
ONAIR_RELEASE="https://github.com/adswill/OnAir/releases/download/v${ONAIR_VERSION}"
PREFIX="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
PROVIDERS=()

usage() {
  cat <<EOF
Usage: tools/setup-sdr-decoders.sh [--prefix DIR] [multimon|rtl433|onair ...]

Install optional providers locally; no arguments selects all three. Linux only;
OnAir's pinned portable release additionally requires x86_64. No sudo, package
manager, udev changes, service changes, or decoder execution is performed.

  --prefix DIR   Install directory (WEBSA_DECODERS_PREFIX; default
                 \${XDG_DATA_HOME:-\$HOME/.local/share}/websa/decoders)
  --help, -h     Show help without downloading or building

Requires bash, curl (HTTPS support), tar with gzip support, sha256sum, and
standard shell utilities. Source builds additionally require CMake >= 3.15,
make, and a C toolchain with standard development headers (CC is respected).
Builds run serially, using the Unix Makefiles generator and --parallel 1.
OnAir retains its bundled libraries and notices; any additional runtime system
libraries required by upstream must already be installed.

Pins:
  multimon-ng 1.6.2: $MULTIMON_REF
  rtl_433:          $RTL433_REF
  OnAir:           v$ONAIR_VERSION, Linux x86_64 portable
All downloaded archives are SHA256 checked before extraction. Verified archives
remain in <prefix>/downloads; source trees and licenses for source-built tools
remain in <prefix>/sources. OnAir's complete upstream portable bundle, including
all license notices, remains in <prefix>/$ONAIR_BUNDLE.
Per-provider SOURCE.txt files record upstream URLs, hashes, and build options
under <prefix>/share/websa-decoders. OnAir's matching source release URL and
checksum are recorded there; its source archive is not needed to run the binary
and is not downloaded. No separate recordings or sample libraries are fetched.

After installation, source <prefix>/env.sh in bash before launching WebSA.
It exports WEBSA_MULTIMON_PATH, WEBSA_RTL433_PATH, and/or WEBSA_ONAIR_SERVER
for installed providers. Restart your own WebSA process with that environment;
this script does not alter an existing process or its environment.
EOF
}

fail() { printf 'setup-sdr-decoders: %s\n' "$*" >&2; exit 1; }

while (($#)); do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --prefix)
      (($# >= 2)) && [[ -n "$2" ]] || fail '--prefix requires a directory'
      PREFIX="$2"; shift 2 ;;
    --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
    multimon|rtl433|onair) PROVIDERS+=("$1"); shift ;;
    *) fail "unknown argument: $1 (see --help)" ;;
  esac
done
[[ -n "$PREFIX" ]] || fail 'the installation prefix must not be empty'
((${#PROVIDERS[@]})) || PROVIDERS=(multimon rtl433 onair)
[[ "$(uname -s)" == Linux ]] || fail 'this setup supports Linux only'

for required in curl tar sha256sum mktemp mkdir rm mv cp chmod; do
  command -v "$required" >/dev/null || fail "$required is required; install prerequisites yourself"
done
for provider in "${PROVIDERS[@]}"; do
  if [[ "$provider" == onair ]]; then
    [[ "$(uname -m)" == x86_64 ]] || fail 'the pinned OnAir bundle requires Linux x86_64'
  else
    for required in cmake make; do
      command -v "$required" >/dev/null || fail "$required is required for source builds"
    done
  fi
done

mkdir -p -- "$PREFIX"
PREFIX="$(cd -- "$PREFIX" && pwd -P)"
DOWNLOADS="$PREFIX/downloads"
SOURCES="$PREFIX/sources"
NOTICES="$PREFIX/share/websa-decoders"
mkdir -p -- "$DOWNLOADS" "$SOURCES" "$NOTICES"
WORK="$(mktemp -d "$PREFIX/.setup-sdr-decoders.XXXXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

verify_archive() {
  local archive="$1" expected="$2" actual
  actual="$(sha256sum -- "$archive")"
  # Compare just the digest, so spaces in the user-selected prefix are harmless.
  [[ "${actual%% *}" == "$expected" ]]
}

download() {
  local url="$1" name="$2" expected="$3"
  ARCHIVE="$DOWNLOADS/$name"
  if [[ -f "$ARCHIVE" ]]; then
    verify_archive "$ARCHIVE" "$expected" || fail "cached archive checksum mismatch: $ARCHIVE; remove it before retrying"
    return
  fi
  printf 'Downloading %s\n' "$url"
  curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' \
    "$url" --output "$WORK/$name"
  verify_archive "$WORK/$name" "$expected" || fail "download checksum mismatch: $url"
  mv -- "$WORK/$name" "$ARCHIVE"
}

install_source_provider() {
  local provider="$1" ref="$2" digest="$3" upstream="$4"
  shift 4
  local source="$SOURCES/$provider-$ref" build="$WORK/$provider-build"
  local url="https://codeload.github.com/$upstream/tar.gz/$ref"
  local notice="$NOTICES/$provider"
  download "$url" "$provider-$ref.tar.gz" "$digest"
  mkdir -p -- "$source" "$notice"
  tar -xzf "$ARCHIVE" --strip-components=1 -C "$source"
  cp -- "$source/COPYING" "$notice/COPYING"
  {
    printf 'Upstream: https://github.com/%s\nCommit: %s\nArchive: %s\nSHA256: %s\n' \
      "$upstream" "$ref" "$url" "$digest"
    printf 'Preserved source archive: %s\nSource tree: %s\n' "$ARCHIVE" "$source"
    printf 'Configure: cmake -G %q -S %q -B %q -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=%q' \
      'Unix Makefiles' "$source" "$build" "$PREFIX"
    printf ' %q' "$@"
    printf '\nBuild: cmake --build %q --parallel 1\nInstall: cmake --install %q\n' "$build" "$build"
    printf 'The temporary build directory is removed when setup exits.\n'
  } > "$notice/SOURCE.txt"
  cmake -G 'Unix Makefiles' -S "$source" -B "$build" \
    -DCMAKE_BUILD_TYPE=Release "-DCMAKE_INSTALL_PREFIX=$PREFIX" "$@"
  cmake --build "$build" --parallel 1
  cmake --install "$build"
}

# Selection order is explicit and every build completes before the next begins.
for provider in "${PROVIDERS[@]}"; do
  case "$provider" in
    multimon)
      install_source_provider multimon "$MULTIMON_REF" "$MULTIMON_SHA" EliasOenal/multimon-ng \
        -DX11_SUPPORT=OFF -DPULSE_AUDIO_SUPPORT=OFF -DSDL3_SCOPE=OFF \
        -DBUILD_GEN_NG=OFF -DBUILD_TESTING=OFF
      ;;
    rtl433)
      # Both physical-device backends are disabled: only WebSA supplies samples.
      install_source_provider rtl433 "$RTL433_REF" "$RTL433_SHA" merbanan/rtl_433 \
        -DENABLE_RTLSDR=OFF -DENABLE_SOAPYSDR=OFF -DENABLE_OPENSSL=OFF \
        -DBUILD_TESTING=OFF -DBUILD_DOCUMENTATION=OFF
      ;;
    onair)
      download "$ONAIR_RELEASE/$ONAIR_BUNDLE.tar.gz" "$ONAIR_BUNDLE.tar.gz" "$ONAIR_SHA"
      # Extract the complete versioned directory so relative library paths and
      # all bundled dependency licenses remain intact. Run no upstream scripts.
      tar -xzf "$ARCHIVE" -C "$PREFIX"
      [[ -x "$PREFIX/$ONAIR_BUNDLE/bin/onair-server" ]] || fail 'OnAir archive lacks the expected bin/onair-server executable'
      mkdir -p -- "$NOTICES/onair"
      cat > "$NOTICES/onair/SOURCE.txt" <<EOF
Upstream: https://github.com/adswill/OnAir
Release: https://github.com/adswill/OnAir/releases/tag/v$ONAIR_VERSION
Binary archive: $ONAIR_RELEASE/$ONAIR_BUNDLE.tar.gz
SHA256: $ONAIR_SHA
Preserved binary archive: $ARCHIVE
Portable bundle and license notices: $PREFIX/$ONAIR_BUNDLE
Corresponding source: $ONAIR_RELEASE/onair-$ONAIR_VERSION-source.tar.gz
Source SHA256: 71a4748fae76beeda0e1b5169ce63940c7e987d3f49bd5ca4cd3ebebf68aa120
See LICENSE, LICENSE-NOTICES-BINARIES.md, THIRD_PARTY.md, LICENSES/, and licenses/
in the portable bundle for upstream and bundled component terms and provenance.
EOF
      ;;
  esac
done

# Include providers installed by previous invocations of this same local prefix.
{
  printf '# Generated by setup-sdr-decoders.sh; source this file in bash.\n'
  if [[ -x "$PREFIX/bin/multimon-ng" ]]; then
    printf 'export WEBSA_MULTIMON_PATH=%q\n' "$PREFIX/bin/multimon-ng"
  fi
  if [[ -x "$PREFIX/bin/rtl_433" ]]; then
    printf 'export WEBSA_RTL433_PATH=%q\n' "$PREFIX/bin/rtl_433"
  fi
  if [[ -x "$PREFIX/$ONAIR_BUNDLE/bin/onair-server" ]]; then
    printf 'export WEBSA_ONAIR_SERVER=%q\n' "$PREFIX/$ONAIR_BUNDLE/bin/onair-server"
  fi
} > "$WORK/env.sh"
mv -- "$WORK/env.sh" "$PREFIX/env.sh"
printf '\nInstalled optional decoders under %s\n' "$PREFIX"
printf 'Before launching WebSA from bash:\n  source %q\n' "$PREFIX/env.sh"
while IFS= read -r line; do
  [[ "$line" == export\ * ]] && printf '  %s\n' "$line"
done < "$PREFIX/env.sh"
