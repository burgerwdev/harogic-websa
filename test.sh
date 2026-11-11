#!/bin/bash
# Test native service contracts, Python tools, and frontend.
set -euo pipefail
case "${1:-}" in
  --help|-h) echo 'Usage: ./test.sh (Rust service contracts, retained Python tool tests, Ruff, frontend Vitest)'; exit 0 ;;
  '') ;;
  *) echo "Unknown argument: $1" >&2; exit 2 ;;
esac
cd "$(dirname "$0")"

echo "==== Rust service contracts ===="
# Share the release dependencies with make native and avoid a separate debug build.
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib --bins --tests -- --test-threads=1
echo 'Native tests complete; use make native to build websad, then restart its running instance.'

echo "==== Python tool tests (pytest) ===="
python3 -m pytest tests/ -q

echo
echo "==== Python static checks (ruff) ===="
python3 -m ruff check tests tools supervise.py

echo
echo "==== Frontend tests (vitest) ===="
if [ -d frontend/node_modules ]; then
  (cd frontend && npm test -- --reporter=dot)
else
  echo "Frontend deps not installed, run ./build.sh first"
  exit 1
fi

echo "OK: tests complete"
