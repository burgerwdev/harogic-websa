#!/bin/bash
# Start a checkout-scoped native service with a portable failure monitor.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lifecycle.sh"
cd "$ROOT"
# All setup helpers install here; the portable runtime needs the same discovery
# path as a source build, without sourcing provider-specific shell files.
export WEBSA_DECODERS_PREFIX="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
export PATH="$WEBSA_DECODERS_PREFIX/bin:$PATH"
PORT="${WEBSA_PORT:-8080}"
HOST="${WEBSA_HOST:-127.0.0.1}"
LOCAL_SHIM="$HOME/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so"
[[ -f "$LOCAL_SHIM" ]] || LOCAL_SHIM="$ROOT/runtime/linux-x86_64/libwebsa_san90_sdk_session.so"
SHIM="${WEBSA_RUST_IQS_SHIM:-$LOCAL_SHIM}"
BIN="${WEBSA_BIN:-}"
if [[ -z "$BIN" ]]; then
  if [[ -x backend/target/release/websad ]]; then BIN="$ROOT/backend/target/release/websad";
  else BIN="$ROOT/runtime/linux-x86_64/websad"; fi
fi
STATIC="${WEBSA_STATIC:-}"
if [[ -z "$STATIC" ]]; then
  if [[ -f frontend/dist/index.html ]]; then STATIC="$ROOT/frontend";
  else STATIC="$ROOT/runtime/frontend"; fi
fi
FAKE="${WEBSA_FAKE:-0}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      echo 'Usage: ./run.sh [--port PORT] [--host HOST] [--bin PATH] [--shim PATH] [--static PATH] [--instance NAME] [--fake]'
      echo 'Defaults: hardware on 127.0.0.1:8080, background instance websad. WEBSA_INSTANCE selects a checkout-scoped instance; WEBSA_BIN overrides local/runtime executable selection.'
      exit 0 ;;
    --port|--host|--bin|--shim|--static|--instance)
      [[ $# -ge 2 && -n "$2" ]] || { echo "Missing value for $1" >&2; exit 2; }
      case "$1" in --port) PORT="$2";; --host) HOST="$2";; --bin) BIN="$2";; --shim) SHIM="$2";; --static) STATIC="$2";; --instance) INSTANCE="$2";; esac
      shift 2 ;;
    --fake) FAKE=1; shift ;;
    *) echo "Unknown argument: $1 (use --instance for a named process)" >&2; exit 2 ;;
  esac
done
[[ -z "${WEBSA_UNIT:-}" ]] || { echo 'WEBSA_UNIT was replaced by WEBSA_INSTANCE' >&2; exit 2; }
[[ "$PORT" =~ ^[0-9]{1,5}$ ]] || { echo "Invalid port: $PORT" >&2; exit 2; }
PORT=$((10#$PORT))
(( PORT > 0 && PORT <= 65535 )) || { echo "Invalid port: $PORT" >&2; exit 2; }
[[ "$FAKE" == 0 || "$FAKE" == 1 ]] || { echo 'WEBSA_FAKE must be 0 or 1' >&2; exit 2; }
[[ "$HOST" != *$'\n'* && "$HOST" != *$'\r'* ]] || exit 2
command -v curl >/dev/null || { echo 'curl is required for HTTP health checks' >&2; exit 1; }
command -v python3 >/dev/null || { echo 'python3 is required for automatic crash recovery' >&2; exit 1; }
[[ -x "$BIN" ]] || { echo "Rust service missing at $BIN: run make native or restore runtime/linux-x86_64/websad" >&2; exit 1; }
BIN="$(realpath "$BIN")"
[[ "$BIN" != *$'\n'* && "$ROOT" != *$'\n'* ]] || exit 2
[[ -f "$STATIC/dist/index.html" ]] || { echo "Frontend missing at $STATIC/dist/index.html: run make frontend or restore runtime/frontend/dist" >&2; exit 1; }
STATIC="$(realpath "$STATIC")"
if [[ "$FAKE" != 1 ]]; then
  [[ -f "$SHIM" ]] || { echo "SDK shim missing: $SHIM. Run make sdk-shim or set WEBSA_RUST_IQS_SHIM; use WEBSA_FAKE=1 for fixtures." >&2; exit 1; }
  SHIM="$(realpath "$SHIM")"
fi
lifecycle_init
# Loading previous metadata must not overwrite the requested launch configuration.
if owned_monitor || (load_state && owned_process); then
  echo "Instance $INSTANCE already running; use ./stop.sh --instance $INSTANCE first" >&2
  exit 1
fi
rm -f "$STATE" "$MONITOR"
if port_listening; then
  echo "TCP port $PORT is already listening; leaving existing services unchanged" >&2
  exit 1
fi
ENV_ARGS=(env -u WEBSA_RUST_IQS_SHIM "WEBSA_HOST=$HOST" "WEBSA_PORT=$PORT" "WEBSA_STATIC=$STATIC")
[[ "$FAKE" == 1 ]] || ENV_ARGS+=("WEBSA_RUST_IQS_SHIM=$SHIM")
DETACH=()
command -v setsid >/dev/null && DETACH=(setsid)
EXE="$BIN"; EXE_ID="$(stat -Lc '%d:%i' "$BIN")"; WORKDIR="$ROOT"
BOOT="$(< /proc/sys/kernel/random/boot_id)"
PID=''; START=''; STARTED=1
# Invoked by the EXIT trap below.
# shellcheck disable=SC2329
cleanup_failed_start() {
  [[ "$STARTED" == 1 ]] || return 0
  terminate_monitor || true
  if [[ "$STARTED" == 1 && -n "$START" ]]; then
    if terminate_owned; then rm -f "$STATE"; fi
  fi
}
trap cleanup_failed_start EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Close the lifecycle lock in the child; nohup keeps it alive after terminal exit.
nohup "${DETACH[@]}" "${ENV_ARGS[@]}" python3 "$ROOT/supervise.py" "$STATE" "$BIN" "$HOST" "$PORT" "$FAKE" >> "$LOG" 2>&1 < /dev/null 9>&- &
# The monitor serializes every child launch against lifecycle commands.
for _ in {1..100}; do
  owned_monitor && break
  sleep 0.02
done
owned_monitor || { echo "Monitor failed to start; log: $LOG" >&2; exit 1; }
flock -u 9
health_url
for _ in {1..150}; do
  # A cold native binary and decoder capability probes can outlast 10 seconds.
  # Follow the current owned child, including a restart during startup.
  if ! load_state; then sleep 0.2; continue; fi
  if owned_process && owns_listener && curl --noproxy '*' -sf -m 1 "$URL" >/dev/null && owned_process; then
    STARTED=0
    echo "Rust websad healthy at ${URL%/api/state} (instance $INSTANCE, PID $PID)"
    echo "Log: $LOG"
    exit 0
  fi
  sleep 0.2
done
echo "Rust websad did not become healthy on port $PORT; log: $LOG" >&2
exit 1
