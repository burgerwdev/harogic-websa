#!/bin/bash
# Stop only the selected Rust systemd user unit.
set -euo pipefail
UNIT="${WEBSA_UNIT:-websad}"
case "${1:-}" in --help|-h) echo 'Usage: WEBSA_UNIT=websad ./stop.sh [--unit NAME]'; exit 0;; --unit) [[ $# == 2 ]] || exit 2; UNIT="$2";; '') ;; *) echo "Unknown argument: $1" >&2; exit 2;; esac
UNIT="${UNIT%.service}.service"
systemctl --user stop "$UNIT"
echo "Stopped Rust service $UNIT"
