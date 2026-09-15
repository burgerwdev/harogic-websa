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

# Logging (web_sa/logging_setup.py): the worker owns the rotating file (5 MiB + 3 backups,
# a 20 MiB ceiling) and appends across restarts; the supervisor appends through a
# WatchedFileHandler that reopens the file after the worker rotated it. stdout is dropped
# because every record already reaches that file, while stderr keeps interpreter-level
# output - uncaught tracebacks and faulthandler dumps - which the logging module never
# sees and which is how a native crash gets diagnosed.
export WEBSA_LOGFILE="${WEBSA_LOGFILE:-/tmp/websa.log}"
WEBSA_ERR="${WEBSA_ERR:-/tmp/websa.err}"
setsid nohup python3 -m web_sa.supervisor > /dev/null 2>> "$WEBSA_ERR" < /dev/null &
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
    echo "    log:    $WEBSA_LOGFILE (5 MiB + 3 backups)"
    echo "    stderr: $WEBSA_ERR (crash output only)"
    exit 0
  fi
done
echo "FAIL: service not ready (single-shot, no retry)"
tail -5 "$WEBSA_LOGFILE" 2>/dev/null || true
tail -5 "$WEBSA_ERR" 2>/dev/null || true
exit 1
