#!/bin/bash
# Stop service: graceful SIGTERM first (device handle released via main.py finally),
# then SIGKILL as fallback.
cd "$(dirname "$0")"
for pid in $(pgrep -f "python3 -m web_sa.main"); do
  kill "$pid" 2>/dev/null
done
for i in 1 2 3 4 5; do
  sleep 1
  pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1 || break
done
pkill -9 -f "python3 -m web_sa.main" 2>/dev/null   # fallback
sleep 1
if pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1; then
  echo "Could not stop fully (remaining: $(pgrep -f 'python3 -m web_sa.main' | tr '\n' ' '))"
  exit 1
fi
echo "OK: service stopped"
