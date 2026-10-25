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
  LOG="$STATE_DIR/$INSTANCE.log"
  [[ ! -L "$STATE_DIR/$INSTANCE.lock" && ! -L "$STATE" && ! -L "$LOG" ]] || {
    echo 'Refusing symlinked lifecycle files' >&2; return 1;
  }
  exec 9>"$STATE_DIR/$INSTANCE.lock"
  flock -n 9 || { echo "Instance $INSTANCE is busy with another lifecycle command" >&2; return 1; }
}

process_identity() {
  local raw
  [[ "$PID" =~ ^[1-9][0-9]*$ && -O "/proc/$PID" ]] || return 1
  IFS= read -r raw < "/proc/$PID/stat" 2>/dev/null || return 1
  raw="${raw##*) }"
  local fields
  read -r -a fields <<< "$raw"
  [[ "${fields[0]}" != Z && "${fields[0]}" != X ]] || return 1
  CURRENT_START="${fields[19]}"
}

load_state() {
  [[ -f "$STATE" ]] || return 1
  local values
  mapfile -t values < "$STATE"
  [[ ${#values[@]} == 9 ]] || return 1
  PID="${values[0]}"; START="${values[1]}"; BOOT="${values[2]}"
  EXE="${values[3]}"; EXE_ID="${values[4]}"; WORKDIR="${values[5]}"
  HOST="${values[6]}"; PORT="${values[7]}"; FAKE="${values[8]}"
  [[ "$START" =~ ^[0-9]+$ && "$PORT" =~ ^[0-9]+$ && "$WORKDIR" == "$ROOT" && "$EXE" == /* ]] || return 1
}

owned_process() {
  process_identity && [[ "$CURRENT_START" == "$START" && "$(< /proc/sys/kernel/random/boot_id)" == "$BOOT" ]] &&
    [[ ( "$(readlink "/proc/$PID/exe" 2>/dev/null)" == "$EXE" || "$(readlink "/proc/$PID/exe" 2>/dev/null)" == "$EXE (deleted)" ) &&
       "$(stat -Lc '%d:%i' "/proc/$PID/exe" 2>/dev/null)" == "$EXE_ID" &&
       "$(readlink "/proc/$PID/cwd" 2>/dev/null)" == "$WORKDIR" ]]
}

save_state() {
  local temporary
  temporary="$(mktemp "$STATE_DIR/.state.XXXXXXXX")"
  printf '%s\n' "$PID" "$START" "$BOOT" "$EXE" "$EXE_ID" "$WORKDIR" "$HOST" "$PORT" "$FAKE" > "$temporary"
  mv -f "$temporary" "$STATE"
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
  URL="http://$health_host:$PORT/api/state"
}

terminate_owned() {
  owned_process || return 0
  kill -TERM "$PID" 2>/dev/null || true
  local attempt
  for attempt in {1..50}; do
    owned_process || return 0
    sleep 0.1
  done
  if owned_process; then kill -KILL "$PID" 2>/dev/null || true; fi
  for attempt in {1..20}; do
    owned_process || return 0
    sleep 0.1
  done
  echo "Instance $INSTANCE did not exit; keeping ownership state" >&2
  return 1
}
