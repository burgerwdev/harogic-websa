#!/bin/bash
# Clean: caches/logs/build artifacts (keep sources).
#   ./clean.sh               full clean (also removes node_modules)
#   ./clean.sh --keep-deps   dev clean (keeps node_modules; still forces a frontend rebuild)
cd "$(dirname "$0")"
keep_deps=0
[ "${1:-}" = "--keep-deps" ] && keep_deps=1
rm -rf web_sa/**/__pycache__ web_sa/**/**/__pycache__ 2>/dev/null
find . -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null
rm -rf .pytest_cache .ruff_cache
rm -f /tmp/websa.log /tmp/websa.log.* /tmp/websa.err /tmp/websa.err.*
rm -rf frontend/dist
if [ "$keep_deps" = 1 ]; then
  echo "OK: cleaned (caches/logs/dist; node_modules + wasm target dirs kept; run ./build.sh to rebuild frontend)"
else
  rm -rf frontend/node_modules wasm/target wasm-dfn/target
  # A full wipe also removes published WASM cores. Otherwise timestamp checks may skip
  # rebuilding them after `wasm/target` was deleted.
  rm -f frontend/public/dsp.wasm frontend/public/dfn/df_bg.wasm
  echo "OK: cleaned (caches/logs/artifacts + wasm build dirs + published wasm cores; run make build to rebuild everything - the WASM cores need Rust)"
fi
