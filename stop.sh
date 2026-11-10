#!/bin/bash
# Stop only the validated process recorded for this checkout and instance.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lifecycle.sh"
case "${1:-}" in
  --help|-h) echo 'Usage: ./stop.sh [--instance NAME]'; echo 'Default: WEBSA_INSTANCE or websad in this checkout. Already stopped succeeds.'; exit 0 ;;
  --instance) [[ $# == 2 && -n "$2" ]] || { echo 'Missing instance name' >&2; exit 2; }; INSTANCE="$2" ;;
  '') [[ $# == 0 ]] || exit 2 ;;
  *) echo "Unknown argument: $1" >&2; exit 2 ;;
esac
[[ -z "${WEBSA_UNIT:-}" ]] || { echo 'WEBSA_UNIT was replaced by WEBSA_INSTANCE' >&2; exit 2; }
lifecycle_init
WAS_MONITORED=0
if owned_monitor; then WAS_MONITORED=1; fi
terminate_monitor
if ! load_state || ! owned_process; then
  rm -f "$STATE" "$MONITOR"
  if [[ "$WAS_MONITORED" == 1 ]]; then echo "Stopped monitored Rust service $INSTANCE"
  else echo "Instance $INSTANCE is not running (stale state cleared)"; fi
  exit 0
fi
terminate_owned
rm -f "$STATE" "$MONITOR"
echo "Stopped Rust service $INSTANCE (PID $PID)"
