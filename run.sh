#!/bin/bash
# Run: supervisor + WebSA worker, with health check and native-crash recovery.
# Usage: ./run.sh (modern TS frontend)
set -e
cd "$(dirname "$0")"

# Frontend check: dist must exist
if [ ! -f frontend/modern/dist/index.html ]; then
  echo "Frontend not built, run ./build.sh first"
  exit 1
fi

# Already running? stop gracefully first (device handle released by worker exit)
if pgrep -f "python3 -m web_sa.supervisor" > /dev/null 2>&1 || \
   pgrep -f "python3 -m web_sa.main" > /dev/null 2>&1; then
  echo "Service already running - stopping first..."
  ./stop.sh
  sleep 2
fi
setsid nohup python3 -m web_sa.supervisor > /tmp/san90-web.log 2>&1 < /dev/null &
health_port="${WEBSA_PORT:-8080}"
health_host="${WEBSA_HOST:-127.0.0.1}"
case "$health_host" in 0.0.0.0|::) health_host="127.0.0.1" ;; esac
for i in $(seq 1 25); do
  sleep 1
  curl_args=(-s -o /dev/null -w "%{http_code}")
  if [ -n "${WEBSA_TOKEN:-}" ]; then
    curl_args+=(-H "Authorization: Bearer ${WEBSA_TOKEN}")
  fi
  if curl "${curl_args[@]}" "http://${health_host}:${health_port}/api/state" 2>/dev/null | grep -q 200; then
    echo "OK: service ready (http://localhost:${health_port})"
    exit 0
  fi
done
echo "FAIL: service not ready (single-shot, no retry)"
tail -5 /tmp/san90-web.log
exit 1
