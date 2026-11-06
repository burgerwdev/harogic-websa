#!/bin/bash
# Inspect recorded process ownership and its actual HTTP endpoint.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lifecycle.sh"
QUIET=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h) echo 'Usage: ./status.sh [--instance NAME] [--quiet]'; echo 'Shows a concise status summary using the saved host/port; --quiet checks health without output. Default instance: WEBSA_INSTANCE or websad. Exit 0: owned process and HTTP healthy; nonzero: stopped or unhealthy.'; exit 0 ;;
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
  # Keep the health contract independent of optional summary formatting.
  # Automation uses --quiet / exit status; full diagnostics remain at $URL.
  STATUS_BODY="$(curl --noproxy '*' -fsS -m 3 "$URL")"
  if command -v python3 >/dev/null; then
    printf '%s' "$STATUS_BODY" | python3 -c '
import json
import math
import sys

def obj(value):
    return value if isinstance(value, dict) else {}

def text(value):
    return " ".join(str(value).split())[:180] if value is not None else "unknown"

def hz(value, unit="Hz"):
    if not isinstance(value, (int, float)) or not math.isfinite(value):
        return "unknown"
    for scale, prefix, digits in [(1e9, "G", 9), (1e6, "M", 6), (1e3, "k", 3)]:
        if abs(value) >= scale:
            number = f"{value / scale:.{digits}f}".rstrip("0").rstrip(".")
            return f"{number} {prefix}{unit}"
    return f"{value:g} {unit}"

try:
    state = json.load(sys.stdin)
except (ValueError, UnicodeError):
    print("HTTP: reachable | status summary unavailable (invalid JSON)")
    sys.exit(0)
if not isinstance(state, dict):
    print("HTTP: reachable | status summary unavailable (invalid state)")
    sys.exit(0)

actual = obj(state.get("actual"))
sdr = obj(state.get("sdr"))
mode = state.get("mode")
link = {True: "connected", False: "disconnected"}.get(state.get("connected"), "unknown")
print("HTTP: healthy")
print("Device: {} | {} | mode {}".format(text(obj(state.get("caps")).get("name")), link, text(mode).upper()))
if mode == "server":
    server = obj(state.get("server"))
    print("Server: {} | {}:{}".format(text(server.get("state")), text(server.get("bind_address")), text(server.get("port"))))
else:
    ref = state.get("ref")
    ref = f"{ref:.1f} dBm" if isinstance(ref, (int, float)) and math.isfinite(ref) else "unknown"
    print("RF: center {} | span {} | ref {}".format(hz(state.get("center")), hz(state.get("span")), ref))
if mode == "sdr":
    print("SDR: listen {} | {} | IF {} | IQ {}".format(hz(sdr.get("listen")), text(sdr.get("demod")).upper(), hz(sdr.get("if_bw")), hz(actual.get("iq_rate"), "S/s")))
    branches = obj(actual.get("receiver_workers")).get("branches") or []
    active = sum(b.get("lifecycle") == "active" and not b.get("fault") for b in branches)
    faults = [b for b in branches if b.get("fault")]
    print(f"Decoders: {active} active | {len(faults)} failed")
    for branch in faults:
        print("Fault C{}: {}".format(text(branch.get("channel")), text(obj(branch["fault"]).get("error"))))
    if sdr.get("receiver_error"):
        print("Receiver error: " + text(sdr["receiver_error"]))
if state.get("status_warning"):
    print("Device warning: " + text(state["status_warning"]))
if state.get("last_error"):
    print("Device error: " + text(state["last_error"]))
if mode == "server" and server.get("error"):
    print("Server error: " + text(server["error"]))
' || echo 'HTTP: healthy | status summary unavailable'
  else
    echo 'HTTP: healthy (install python3 for device summary)'
  fi
fi
owned_process
