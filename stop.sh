#!/bin/bash
# Stop the supervisor and any orphaned worker, then verify the device process is gone.
set -e
cd "$(dirname "$0")"

for pid in $(pgrep -f "python3 -m web_sa.supervisor" || true); do
  kill "$pid" 2>/dev/null || true
done
for i in 1 2 3 4 5; do
  sleep 1
  if ! pgrep -f "python3 -m web_sa.supervisor" > /dev/null 2>&1 && \
     ! pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1; then
    break
  fi
done
pkill -9 -f "python3 -m web_sa.supervisor" 2>/dev/null || true
pkill -9 -f "python3 -m web_sa.main" 2>/dev/null || true
sleep 1
if pgrep -f "python3 -m web_sa.supervisor" > /dev/null 2>&1 || \
   pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1; then
  echo "Could not stop WebSA processes"
  exit 1
fi
echo "OK: service stopped"
