#!/bin/bash
# Inspect the selected Rust user unit, journal and HTTP health.
set -uo pipefail
UNIT="${WEBSA_UNIT:-websad}"
PORT="${WEBSA_PORT:-8080}"
HOST="${WEBSA_HOST:-127.0.0.1}"
case "${1:-}" in --help|-h) echo 'Usage: WEBSA_UNIT=websad WEBSA_HOST=127.0.0.1 WEBSA_PORT=8080 ./status.sh [--unit NAME]'; exit 0;; --unit) [[ $# == 2 ]] || exit 2; UNIT="$2";; '') ;; *) echo "Unknown argument: $1" >&2; exit 2;; esac
UNIT="${UNIT%.service}.service"
case "$HOST" in 0.0.0.0) HOST=127.0.0.1;; ::) HOST='[::1]';; *:*) HOST="[$HOST]";; esac
echo "Rust websad: $UNIT"
systemctl --user show "$UNIT" --property=ActiveState,SubState,MainPID,NRestarts,ExecMainStartTimestamp,MemoryCurrent,CPUUsageNSec
journalctl --user -u "$UNIT" -n 10 --no-pager
printf '\nRust HTTP health: http://%s:%s/api/state\n' "$HOST" "$PORT"
curl -fsS -m 3 "http://$HOST:$PORT/api/state"
