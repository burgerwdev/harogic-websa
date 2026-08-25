#!/bin/bash
# Run: single-shot start (no retry loop) - clean residue -> start -> health check (exit when ready)
# Usage: ./run.sh (modern TS frontend)
set -e
cd "$(dirname "$0")"

# Frontend check: dist must exist
if [ ! -f frontend/modern/dist/index.html ]; then
  echo "Frontend not built, run ./build.sh first"
  exit 1
fi

pkill -9 -f "python3 -m web_sa.main" 2>/dev/null || true
sleep 2
setsid nohup python3 -m web_sa.main > /tmp/san90-web.log 2>&1 < /dev/null &
for i in $(seq 1 25); do
  sleep 1
  if curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:8080/api/state 2>/dev/null | grep -q 200; then
    echo "OK: service ready (http://localhost:8080)"
    exit 0
  fi
done
echo "FAIL: service not ready (single-shot, no retry)"
tail -5 /tmp/san90-web.log
exit 1
