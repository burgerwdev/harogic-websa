#!/bin/bash
# Stop service
cd "$(dirname "$0")"
pkill -9 -f "python3 -m web_sa.main" 2>/dev/null
sleep 1
if pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1; then
  echo "Could not stop fully (remaining: $(pgrep -f 'python3 -m web_sa.main' | tr '\n' ' '))"
  exit 1
fi
echo "OK: service stopped"
