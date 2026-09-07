#!/bin/bash
# Test backend, static checks, and frontend. Any failed stage fails the script.
set -euo pipefail
cd "$(dirname "$0")"

echo "==== Backend tests (pytest) ===="
python3 -m pytest tests/ -q

echo
echo "==== Python static checks (ruff) ===="
python3 -m ruff check web_sa tests tools

echo
echo "==== Frontend tests (vitest) ===="
if [ -d frontend/modern/node_modules ]; then
  (cd frontend/modern && npm test -- --reporter=dot)
else
  echo "Frontend deps not installed, run ./build.sh first"
  exit 1
fi

echo "OK: tests complete"
