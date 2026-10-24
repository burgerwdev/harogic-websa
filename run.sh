#!/bin/bash
# Start the Rust service in a systemd user unit.
set -euo pipefail
cd "$(dirname "$0")"
PORT="${WEBSA_PORT:-8080}"
HOST="${WEBSA_HOST:-127.0.0.1}"
UNIT="${WEBSA_UNIT:-websad}"
LOCAL_SHIM="$HOME/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so"
[[ -f "$LOCAL_SHIM" ]] || LOCAL_SHIM="$(pwd)/runtime/linux-x86_64/libwebsa_san90_sdk_session.so"
SHIM="${WEBSA_RUST_IQS_SHIM:-$LOCAL_SHIM}"
BIN="${WEBSA_BIN:-}"
if [[ -z "$BIN" ]]; then
  if [[ -x native-ddc/target/release/websad ]]; then
    BIN="$(pwd)/native-ddc/target/release/websad"
  else
    BIN="$(pwd)/runtime/linux-x86_64/websad"
  fi
fi
STATIC="${WEBSA_STATIC:-}"
if [[ -z "$STATIC" ]]; then
  if [[ -f frontend/dist/index.html ]]; then STATIC="$(pwd)/frontend";
  else STATIC="$(pwd)/runtime/frontend"; fi
fi
FAKE="${WEBSA_FAKE:-0}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h) echo 'Usage: ./run.sh [--port PORT] [--host HOST] [--shim PATH] [--static PATH] [--unit NAME] [--fake]'; echo 'Defaults: hardware service on 127.0.0.1:8080, systemd user unit websad. Prefers local builds, falls back to runtime/; WEBSA_BIN overrides the executable. WEBSA_FAKE=1 selects fixtures.'; exit 0 ;;
    --port|--host|--shim|--static|--unit) [[ $# -ge 2 ]] || { echo "Missing value for $1" >&2; exit 2; }; case "$1" in --port) PORT="$2";; --host) HOST="$2";; --shim) SHIM="$2";; --static) STATIC="$2";; --unit) UNIT="$2";; esac; shift 2 ;;
    --fake) FAKE=1; shift ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -x "$BIN" ]] || { echo "Rust service missing at $BIN: run make native or restore runtime/linux-x86_64/websad" >&2; exit 1; }
BIN="$(realpath "$BIN")"
[[ -f "$STATIC/dist/index.html" ]] || { echo "Frontend missing at $STATIC/dist/index.html: run make frontend or restore runtime/frontend/dist" >&2; exit 1; }
STATIC="$(realpath "$STATIC")"
if [[ "$FAKE" != 1 && ! -f "$SHIM" ]]; then
  echo "SDK shim missing: $SHIM. Run make sdk-shim or set WEBSA_RUST_IQS_SHIM; use WEBSA_FAKE=1 for explicit fixture mode." >&2
  exit 1
fi
if [[ "$FAKE" != 1 ]]; then
  SHIM="$(realpath "$SHIM")"
fi
[[ "$PORT" =~ ^[0-9]+$ ]] && (( PORT > 0 && PORT <= 65535 )) || { echo "Invalid port: $PORT" >&2; exit 2; }
command -v ss >/dev/null || { echo 'ss is required to check TCP port ownership (install iproute2)' >&2; exit 1; }
LISTENERS="$(ss -H -ltn "sport = :$PORT")"
if [[ -n "$LISTENERS" ]]; then
  echo "TCP port $PORT is already listening; leaving existing services unchanged" >&2; exit 1
fi
UNIT="${UNIT%.service}.service"
if systemctl --user is-active --quiet "$UNIT"; then
  echo "$UNIT already active; use WEBSA_UNIT=${UNIT%.service} ./stop.sh first" >&2; exit 1
fi
systemctl --user reset-failed "$UNIT" 2>/dev/null || true
# env -u prevents an inherited hardware shim from turning a fixture run into a device owner.
ENV_ARGS=(env -u WEBSA_RUST_IQS_SHIM "WEBSA_HOST=$HOST" "WEBSA_PORT=$PORT" "WEBSA_STATIC=$STATIC")
[[ "$FAKE" == 1 ]] || ENV_ARGS+=("WEBSA_RUST_IQS_SHIM=$SHIM")
systemd-run --user --unit="$UNIT" --property=KillMode=process \
  --property=Restart=on-failure --property=RestartSec=2 --property="WorkingDirectory=$(pwd)" \
  "${ENV_ARGS[@]}" "$BIN"
STARTED=1
cleanup_failed_start() {
  if [[ "$STARTED" == 1 ]]; then systemctl --user stop "$UNIT"; fi
}
trap cleanup_failed_start EXIT
HEALTH_HOST="$HOST"
case "$HEALTH_HOST" in 0.0.0.0) HEALTH_HOST=127.0.0.1;; ::) HEALTH_HOST='[::1]';; *:*) HEALTH_HOST="[$HEALTH_HOST]";; esac
for _ in {1..50}; do
  MAIN_PID="$(systemctl --user show "$UNIT" --property=MainPID --value)"
  if systemctl --user is-active --quiet "$UNIT" && [[ "$MAIN_PID" =~ ^[1-9][0-9]*$ ]] && kill -0 "$MAIN_PID" 2>/dev/null && curl -sf -m 1 "http://$HEALTH_HOST:$PORT/api/state" >/dev/null; then
    STARTED=0
    echo "Rust websad healthy at http://$HEALTH_HOST:$PORT ($UNIT, Restart=on-failure)"; exit 0
  fi
  sleep 0.2
done
journalctl --user -u "$UNIT" -n 20 --no-pager >&2 || true
echo "Rust websad did not become healthy on port $PORT" >&2
exit 1
