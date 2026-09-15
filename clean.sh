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
rm -rf frontend/modern/dist
if [ "$keep_deps" = 1 ]; then
  echo "OK: cleaned (caches/logs/dist; node_modules kept; run ./build.sh to rebuild frontend)"
else
  rm -rf frontend/modern/node_modules
  echo "OK: cleaned (caches/logs/artifacts; run ./build.sh to rebuild frontend)"
fi
