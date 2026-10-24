#!/bin/bash
# Publish only project-built runtime outputs; never copy vendor SDK libraries.
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
  --help|-h) echo 'Usage: tools/package-runtime.sh [SDK_SHIM] (package existing Linux x86_64 builds into runtime/; run make build sdk-shim first)'; exit 0 ;;
esac
[[ $# -le 1 ]] || { echo 'Expected at most one SDK shim path' >&2; exit 2; }
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || { echo 'Runtime packaging requires Linux x86_64' >&2; exit 1; }
SHIM="${1:-${SDK_SHIM:-$HOME/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so}}"
[[ -x native-ddc/target/release/websad ]] || { echo 'Rust service missing: run make native' >&2; exit 1; }
[[ -f frontend/dist/index.html && -d frontend/dist/assets ]] || { echo 'Frontend missing: run make frontend' >&2; exit 1; }
[[ -f "$SHIM" ]] || { echo "SDK adapter missing at $SHIM: run make sdk-shim" >&2; exit 1; }
command -v strip >/dev/null || { echo 'strip is required to package runtime binaries (install binutils)' >&2; exit 1; }
mkdir -p runtime
STAGING="$(mktemp -d runtime/.package-XXXXXX)"
trap 'rm -rf -- "$STAGING"' EXIT
mkdir -p "$STAGING/linux-x86_64" "$STAGING/frontend"
install -m 755 native-ddc/target/release/websad "$STAGING/linux-x86_64/websad"
install -m 755 "$SHIM" "$STAGING/linux-x86_64/libwebsa_san90_sdk_session.so"
strip --strip-unneeded "$STAGING/linux-x86_64/websad" "$STAGING/linux-x86_64/libwebsa_san90_sdk_session.so"
cp -R frontend/dist "$STAGING/frontend/dist"
# Replace the published output so obsolete Vite chunks cannot accumulate.
rm -rf runtime/linux-x86_64 runtime/frontend
mv "$STAGING/linux-x86_64" runtime/linux-x86_64
mv "$STAGING/frontend" runtime/frontend
printf '%s\n' 'Packaged runtime/linux-x86_64 and runtime/frontend/dist (system libraries and proprietary HAROGIC SDK remain external).'
