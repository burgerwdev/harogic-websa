#!/bin/bash
# Status: process, uptime, resource use and log details for the running WebSA service.
#   ./status.sh
# Reads the same environment as run.sh (WEBSA_LOGFILE, WEBSA_PORT, WEBSA_HOST).
set -u
cd "$(dirname "$0")"

LOG_FILE="${WEBSA_LOGFILE:-/tmp/websa.log}"
PORT="${WEBSA_PORT:-8080}"
HOST="${WEBSA_HOST:-127.0.0.1}"
case "$HOST" in 0.0.0.0|::) HOST="127.0.0.1" ;; esac

# Seconds -> "3d 04h 12m 05s" (only the leading non-zero units are printed).
human_seconds() {
  local s=$1 d h m
  d=$((s / 86400)); h=$(((s % 86400) / 3600)); m=$(((s % 3600) / 60)); s=$((s % 60))
  if [ "$d" -gt 0 ]; then printf '%dd %02dh %02dm %02ds' "$d" "$h" "$m" "$s"
  elif [ "$h" -gt 0 ]; then printf '%dh %02dm %02ds' "$h" "$m" "$s"
  elif [ "$m" -gt 0 ]; then printf '%dm %02ds' "$m" "$s"
  else printf '%ds' "$s"; fi
}

# One process block: pid, ppid, elapsed, cpu%, mem%, RSS, VSZ, start time, command.
show_process() {
  local label=$1 pid=$2 line ppid etimes cpu mem rss vsz lstart cmd
  line=$(ps -o pid=,ppid=,etimes=,%cpu=,%mem=,rss=,vsz= -p "$pid" 2>/dev/null) || return
  read -r pid ppid etimes cpu mem rss vsz <<<"$(echo "$line" | awk '{$1=$1;print}')"
  lstart=$(ps -o lstart= -p "$pid" 2>/dev/null | awk '{$1=$1;print}')
  cmd=$(ps -o cmd= -p "$pid" 2>/dev/null | awk '{$1=$1;print}')
  printf '%-12s PID %-7s PPID %-7s up %s\n' "$label" "$pid" "$ppid" "$(human_seconds "$etimes")"
  printf '%-12s CPU %s%%  MEM %s%%  RSS %.1f MB  VSZ %.1f MB\n' "" "$cpu" "$mem" \
    "$(awk -v v="$rss" 'BEGIN {print v/1024}')" "$(awk -v v="$vsz" 'BEGIN {print v/1024}')"
  printf '%-12s started %s\n' "" "$lstart"
  printf '%-12s cmd     %s\n' "" "$cmd"
}

# PIDs whose command line matches the pattern AND whose executable is python (a shell that
# merely mentions the module name in its own command line must not be counted).
python_pids() {
  local pid comm
  for pid in $(pgrep -f "$1" 2>/dev/null || true); do
    comm=$(ps -o comm= -p "$pid" 2>/dev/null) || continue
    case "$comm" in *python*) printf '%s\n' "$pid" ;; esac
  done
}

supervisor_pids=$(python_pids "python3 -m web_sa.supervisor")
worker_pids=$(python_pids "python3 -m web_sa.main")

echo "WebSA service status"
echo "--------------------"
if [ -z "$supervisor_pids" ] && [ -z "$worker_pids" ]; then
  echo "State:       NOT RUNNING"
  echo "Log file:    $LOG_FILE$([ -f "$LOG_FILE" ] && echo " ($(du -h "$LOG_FILE" | cut -f1), $(wc -l < "$LOG_FILE") lines)" || echo " (absent)")"
  exit 0
fi
echo "State:       RUNNING"
echo "Endpoint:    http://${HOST}:${PORT}"
echo

for pid in $supervisor_pids; do show_process "supervisor" "$pid"; done
for pid in $worker_pids; do show_process "worker" "$pid"; done

# Combined resource use (supervisor + worker) when both run.
all_pids=$(printf '%s\n%s\n' "$supervisor_pids" "$worker_pids" | grep -E '^[0-9]+$' | sort -u | tr '\n' ',' | sed 's/,$//')
if [ -n "$all_pids" ]; then
  ps -o rss=,%cpu= -p "$all_pids" 2>/dev/null | awk '
    {rss += $1; cpu += $2}
    END {printf "Combined:    RSS %.1f MB  CPU %.1f%% (average since start)\n", rss/1024, cpu}'
fi
echo

echo "Log:"
if [ -f "$LOG_FILE" ]; then
  echo "  path:      $LOG_FILE"
  echo "  size:      $(du -h "$LOG_FILE" | cut -f1)  ($(stat -c%s "$LOG_FILE") bytes)"
  echo "  lines:     $(wc -l < "$LOG_FILE")"
  echo "  modified:  $(date -r "$LOG_FILE" '+%Y-%m-%d %H:%M:%S')"
  for rotated in "$LOG_FILE".*; do
    [ -f "$rotated" ] || continue
    echo "  rotated:   $rotated ($(du -h "$rotated" | cut -f1))"
  done
  echo "  last lines:"
  tail -n 5 "$LOG_FILE" | sed 's/^/    /'
else
  echo "  path:      $LOG_FILE (absent)"
fi
echo

# Live device/link state, from the same REST endpoint the page uses.
state=$(curl -s -m 2 "http://${HOST}:${PORT}/api/state" 2>/dev/null || true)
if [ -n "$state" ]; then
  echo "Device (from /api/state):"
  printf '%s' "$state" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
print("  connected: %s" % ("yes" if d.get("connected") else "no"))
print("  device:    %s" % (d.get("device") or "-"))
print("  mode:      %s" % (d.get("mode") or "-"))
err = d.get("last_error") or ""
if err:
    print("  last err:  %s" % err)
' 2>/dev/null || true
else
  echo "Device:      no answer from /api/state (worker busy or starting)"
fi
