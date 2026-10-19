#!/bin/bash
# Clean: caches/logs/build artifacts (keep sources).
#   ./clean.sh               full clean (also removes node_modules)
#   ./clean.sh --keep-deps   dev clean (keeps node_modules; still forces a frontend rebuild)
#   ./clean.sh --with-wasm   ALSO remove the published WASM cores (needs Rust to restore)
#
# The published WASM cores (frontend/public/dsp.wasm, dfn/df_bg.wasm) are COMMITTED: a
# machine without Rust builds the frontend with them. A plain clean therefore keeps them —
# removing them here used to turn every clean into "needs a Rust toolchain to work again".
cd "$(dirname "$0")"
keep_deps=0
with_wasm=0
for arg in "$@"; do
  case "$arg" in
    --keep-deps) keep_deps=1 ;;
    --with-wasm) with_wasm=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
rm -rf web_sa/**/__pycache__ web_sa/**/**/__pycache__ 2>/dev/null
find . -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null
rm -rf .pytest_cache .ruff_cache
rm -f /tmp/websa.log /tmp/websa.log.* /tmp/websa.err /tmp/websa.err.*
rm -rf frontend/dist
if [ "$keep_deps" = 1 ]; then
  echo "OK: cleaned (caches/logs/dist; node_modules + wasm target dirs kept; run ./build.sh to rebuild frontend)"
else
  rm -rf frontend/node_modules wasm/target wasm-dfn/target
  if [ "$with_wasm" = 1 ]; then
    rm -f frontend/public/dsp.wasm frontend/public/dfn/df_bg.wasm
    echo "OK: cleaned (caches/logs/artifacts + wasm build dirs + published wasm cores; run make build to rebuild everything - the WASM cores need Rust)"
  else
    echo "OK: cleaned (caches/logs/artifacts + wasm build dirs; the committed WASM cores are kept - pass --with-wasm to remove them too)"
  fi
fi
