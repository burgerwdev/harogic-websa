#!/bin/bash
# Test: backend pytest + frontend vitest (if deps installed)
set -e
cd "$(dirname "$0")"
echo "==== Backend tests (pytest) ===="
python3 -m pytest tests/ -v 2>&1 | tail -15

echo
echo "==== Frontend tests (vitest) ===="
if [ -d frontend/modern/node_modules ]; then
  (cd frontend/modern && npx vitest run 2>&1 | tail -12)
else
  echo "Frontend deps not installed, skipped (cd frontend/modern && npm install then rerun)"
fi
echo "OK: tests complete"
