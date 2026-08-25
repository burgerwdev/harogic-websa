#!/bin/bash
# Clean: caches/logs/build artifacts (keep sources)
cd "$(dirname "$0")"
rm -rf web_sa/**/__pycache__ web_sa/**/**/__pycache__ 2>/dev/null
find . -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null
rm -rf .pytest_cache .ruff_cache
rm -f /tmp/san90-web.log
rm -rf frontend/modern/dist
rm -rf frontend/modern/node_modules
echo "OK: cleaned (caches/logs/artifacts; run ./build.sh to rebuild frontend)"
