#!/bin/bash
# Clean: caches/logs/build artifacts (keep sources).
#   ./clean.sh               full clean (also removes node_modules)
#   ./clean.sh --keep-deps   dev clean (keeps node_modules; still forces a frontend rebuild)
#   ./clean.sh --with-wasm   ALSO remove the published WASM cores (needs Rust to restore)
#
# Published offline DSP artifacts and runtime/ are committed and kept; --with-wasm removes only DSP artifacts.
cd "$(dirname "$0")"
keep_deps=0
with_wasm=0
for arg in "$@"; do
  case "$arg" in
    --help|-h) echo 'Usage: ./clean.sh [--keep-deps] [--with-wasm] (clean caches/dist; full clean also removes dependencies and Rust targets)'; exit 0 ;;
    --keep-deps) keep_deps=1 ;;
    --with-wasm) with_wasm=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
find . -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null
rm -rf .pytest_cache .ruff_cache
rm -rf frontend/dist
if [ "$keep_deps" = 1 ]; then
  echo "OK: cleaned caches/dist; dependencies and all Rust targets kept"
else
  rm -rf frontend/node_modules dsp/core/target dsp/deepfilter/target dsp/deepfilter/pkg backend/target
  if [ "$with_wasm" = 1 ]; then
    rm -f frontend/public/dsp.wasm frontend/public/dfn/df_bg.wasm
    echo "OK: cleaned caches, dependencies, Rust targets and published WASM cores; use make build and make wasm to restore"
  else
    echo "OK: cleaned caches, dependencies and Rust targets; committed runtime and WASM cores kept"
  fi
fi
