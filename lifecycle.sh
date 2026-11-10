#!/bin/bash
# Shared Linux process ownership and instance state for the root launchers.
ROOT="$(realpath "$(dirname "${BASH_SOURCE[0]}")")"
INSTANCE="${WEBSA_INSTANCE:-websad}"

lifecycle_init() {
  [[ "$INSTANCE" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ && ${#INSTANCE} -le 80 ]] || {
    echo "Invalid instance name: $INSTANCE" >&2; return 2;
  }
  command -v flock >/dev/null || { echo 'flock (util-linux) is required' >&2; return 1; }
  umask 077
  local base key
  if [[ -n "${XDG_RUNTIME_DIR:-}" && -d "$XDG_RUNTIME_DIR" && ! -L "$XDG_RUNTIME_DIR" && -O "$XDG_RUNTIME_DIR" && -w "$XDG_RUNTIME_DIR" && "$(stat -c %a "$XDG_RUNTIME_DIR")" == 700 ]]; then
    base="$XDG_RUNTIME_DIR"
  else
    base="${XDG_STATE_HOME:-$HOME/.local/state}"
  fi
  [[ "$base" == /* ]] || { echo 'Lifecycle state directory must be absolute' >&2; return 1; }
  base="$base/harogic-websa"
  mkdir -p "$base"
  [[ -d "$base" && ! -L "$base" && -O "$base" && "$(stat -c %a "$base")" == 700 ]] || {
    echo "Unsafe lifecycle state directory: $base (must be owned by you, mode 700)" >&2; return 1;
  }
  key="$(printf '%s' "$ROOT" | sha256sum)"
  STATE_DIR="$base/${key%% *}"
  mkdir -p "$STATE_DIR"
  [[ ! -L "$STATE_DIR" && -O "$STATE_DIR" && "$(stat -c %a "$STATE_DIR")" == 700 ]] || return 1
  STATE="$STATE_DIR/$INSTANCE.state"
  MONITOR="$STATE_DIR/$INSTANCE.monitor"
  LOG="$STATE_DIR/$INSTANCE.log"
  [[ ! -L "$STATE_DIR/$INSTANCE.lock" && ! -L "$STATE" && ! -L "$MONITOR" && ! -L "$LOG" ]] || {
    echo 'Refusing symlinked lifecycle files' >&2; return 1;
  }
  exec 9>"$STATE_DIR/$INSTANCE.lock"
  flock -n 9 || { echo "Instance $INSTANCE is busy with another lifecycle command" >&2; return 1; }
}

owned_monitor() {
  [[ -f "$MONITOR" ]] || return 1
  local -a values=() fields=() arguments=()
  local raw
  mapfile -t values 2>/dev/null < "$MONITOR" || return 1
  [[ ${#values[@]} == 5 && "${values[0]}" =~ ^[1-9][0-9]*$ ]] || return 1
  MONITOR_PID="${values[0]}"
  [[ -O "/proc/$MONITOR_PID" && "${values[2]}" == "$(< /proc/sys/kernel/random/boot_id)" && "${values[4]}" == "$ROOT" ]] || return 1
  IFS= read -r raw 2>/dev/null < "/proc/$MONITOR_PID/stat" || return 1
  read -r -a fields <<< "${raw##*) }"
  [[ "${fields[0]}" != Z && "${fields[0]}" != X && "${fields[19]}" == "${values[1]}" &&
     "$(stat -Lc '%d:%i' "/proc/$MONITOR_PID/exe" 2>/dev/null)" == "${values[3]}" &&
     "$(readlink "/proc/$MONITOR_PID/cwd" 2>/dev/null)" == "$ROOT" ]] || return 1
  mapfile -d '' -t arguments 2>/dev/null < "/proc/$MONITOR_PID/cmdline" || return 1
  [[ "${arguments[1]:-}" == "$ROOT/supervise.py" && "${arguments[2]:-}" == "$STATE" ]]
}

terminate_monitor() {
  owned_monitor || return 0
  kill -TERM "$MONITOR_PID" 2>/dev/null || true
  local attempt
  for ((attempt=0; attempt<60; attempt++)); do
    owned_monitor || return 0
    sleep 0.1
  done
  echo "Instance $INSTANCE monitor did not exit; keeping ownership state" >&2
  return 1
}

process_identity() {
  local raw
  [[ "$PID" =~ ^[1-9][0-9]*$ && -O "/proc/$PID" ]] || return 1
  IFS= read -r raw 2>/dev/null < "/proc/$PID/stat" || return 1
  raw="${raw##*) }"
  local fields
  read -r -a fields <<< "$raw"
  [[ "${fields[0]}" != Z && "${fields[0]}" != X ]] || return 1
  CURRENT_START="${fields[19]}"
}

load_state() {
  [[ -f "$STATE" ]] || return 1
  local -a values=()
  mapfile -t values 2>/dev/null < "$STATE" || return 1
  [[ ${#values[@]} == 9 ]] || return 1
  PID="${values[0]}"; START="${values[1]}"; BOOT="${values[2]}"
  EXE="${values[3]}"; EXE_ID="${values[4]}"; WORKDIR="${values[5]}"
  HOST="${values[6]}"; PORT="${values[7]}"
  # FAKE is part of the launchers' shared state.
  # shellcheck disable=SC2034
  FAKE="${values[8]}"
  [[ "$START" =~ ^[0-9]+$ && "$PORT" =~ ^[0-9]+$ && "$WORKDIR" == "$ROOT" && "$EXE" == /* ]] || return 1
}

owned_process() {
  process_identity && [[ "$CURRENT_START" == "$START" && "$(< /proc/sys/kernel/random/boot_id)" == "$BOOT" ]] &&
    [[ ( "$(readlink "/proc/$PID/exe" 2>/dev/null)" == "$EXE" || "$(readlink "/proc/$PID/exe" 2>/dev/null)" == "$EXE (deleted)" ) &&
       "$(stat -Lc '%d:%i' "/proc/$PID/exe" 2>/dev/null)" == "$EXE_ID" &&
       "$(readlink "/proc/$PID/cwd" 2>/dev/null)" == "$WORKDIR" ]]
}

# /proc avoids a mandatory ss/lsof dependency. Check all addresses conservatively.
port_listening() {
  local table line fields wanted
  printf -v wanted '%04X' "$PORT"
  for table in /proc/net/tcp /proc/net/tcp6; do
    [[ -r "$table" ]] || continue
    while IFS= read -r line; do
      read -r -a fields <<< "$line"
      [[ "${fields[1]##*:}" == "$wanted" && "${fields[3]}" == 0A ]] && return 0
    done < "$table"
  done
  return 1
}

owns_listener() {
  local table line fields wanted descriptor socket
  printf -v wanted '%04X' "$PORT"
  for table in /proc/net/tcp /proc/net/tcp6; do
    [[ -r "$table" ]] || continue
    while IFS= read -r line; do
      read -r -a fields <<< "$line"
      [[ "${fields[1]##*:}" == "$wanted" && "${fields[3]}" == 0A ]] || continue
      socket="socket:[${fields[9]}]"
      for descriptor in /proc/"$PID"/fd/*; do
        [[ "$(readlink "$descriptor" 2>/dev/null)" == "$socket" ]] && return 0
      done
    done < "$table"
  done
  return 1
}

health_url() {
  local health_host="$HOST"
  case "$health_host" in 0.0.0.0) health_host=127.0.0.1;; ::) health_host='[::1]';; *:*) health_host="[$health_host]";; esac
  # The calling launcher performs the HTTP request.
  # shellcheck disable=SC2034
  URL="http://$health_host:$PORT/api/state"
}

terminate_owned() {
  owned_process || return 0
  kill -TERM "$PID" 2>/dev/null || true
  local attempt
  for ((attempt=0; attempt<50; attempt++)); do
    owned_process || return 0
    sleep 0.1
  done
  if owned_process; then kill -KILL "$PID" 2>/dev/null || true; fi
  for ((attempt=0; attempt<20; attempt++)); do
    owned_process || return 0
    sleep 0.1
  done
  echo "Instance $INSTANCE did not exit; keeping ownership state" >&2
  return 1
}
