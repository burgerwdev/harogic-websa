#!/bin/bash
# Stop this checkout's WebSA instance (supervisor + worker) and verify it is gone.
#
# run.sh starts the supervisor through setsid, so it LEADS its own process group and the
# worker (its direct child) stays inside it: stopping the group takes both, and only ours.
# The group leader's pid is recorded in the pid file at start. A stale or missing pid file
# falls back to a command-line match — the one path that can reach another checkout's
# instance on this machine, which is why it warns on stderr.
set -e
cd "$(dirname "$0")"
PIDFILE="${WEBSA_PIDFILE:-/tmp/websa.pid}"

alive() { kill -0 "$1" 2>/dev/null; }

# Still one of ours? Guards against the pid having been reused by another process.
cmdline_is_websa() {
  [ -r "/proc/$1/cmdline" ] && tr '\0' ' ' < "/proc/$1/cmdline" | grep -q "web_sa"
}

stop_process_group() {  # $1 = leader pid; TERM the group, escalate to KILL
  local pid="$1" pgid
  if ! alive "$pid" || ! cmdline_is_websa "$pid"; then
    return 0
  fi
  pgid=$(ps -o pgid= -p "$pid" 2>/dev/null | tr -d ' ')
  if [ -n "$pgid" ]; then
    kill -TERM -- "-$pgid" 2>/dev/null || true
  else
    kill -TERM "$pid" 2>/dev/null || true
  fi
  for i in 1 2 3 4 5; do
    sleep 1
    alive "$pid" || return 0
  done
  if [ -n "$pgid" ]; then
    kill -KILL -- "-$pgid" 2>/dev/null || true
  else
    kill -KILL "$pid" 2>/dev/null || true
  fi
  sleep 1
  alive "$pid" && return 1
  return 0
}

rc=0
if [ -r "$PIDFILE" ]; then
  pid=$(cat "$PIDFILE" 2>/dev/null || true)
  if [ -n "$pid" ]; then
    stop_process_group "$pid" || { echo "Could not stop WebSA process $pid" >&2; rc=1; }
  fi
  rm -f "$PIDFILE"
else
  pids=$(pgrep -f "python3 -m web_sa.(supervisor|main)" || true)
  if [ -n "$pids" ]; then
    echo "warning: no pid file ($PIDFILE); stopping by command-line match, which may hit other checkouts" >&2
    for pid in $pids; do
      stop_process_group "$pid" || { echo "Could not stop WebSA process $pid" >&2; rc=1; }
    done
  fi
fi
if [ "$rc" -eq 0 ]; then
  echo "OK: service stopped"
fi
exit "$rc"
