#!/bin/bash
# Inspect recorded process ownership and its actual HTTP endpoint.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lifecycle.sh"
QUIET=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h) echo 'Usage: ./status.sh [--instance NAME] [--quiet]'; echo 'Uses saved host/port, default instance WEBSA_INSTANCE or websad. Exit 0: owned process and HTTP healthy; 1: stopped or unhealthy.'; exit 0 ;;
    --instance) [[ $# -ge 2 && -n "$2" ]] || exit 2; INSTANCE="$2"; shift 2 ;;
    --quiet) QUIET=1; shift ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -z "${WEBSA_UNIT:-}" ]] || { echo 'WEBSA_UNIT was replaced by WEBSA_INSTANCE' >&2; exit 2; }
lifecycle_init
if ! load_state || ! owned_process; then
  [[ "$QUIET" == 1 ]] || echo "Instance $INSTANCE is not running (missing or stale ownership state)"
  exit 1
fi
health_url
if [[ "$QUIET" != 1 ]]; then
  echo "Rust websad: $INSTANCE (PID $PID, fixture=$FAKE)"
  echo "Log: $LOG"
  echo "Rust HTTP health: $URL"
fi
owns_listener || { [[ "$QUIET" == 1 ]] || echo 'Recorded process does not own the listening port' >&2; exit 1; }
if [[ "$QUIET" == 1 ]]; then
  curl --noproxy '*' -fsS -m 3 "$URL" >/dev/null 2>&1
else
  curl --noproxy '*' -fsS -m 3 "$URL"
  printf '\n'
fi
owned_process
