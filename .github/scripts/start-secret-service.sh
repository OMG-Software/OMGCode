#!/usr/bin/env bash
# Start an unlocked gnome-keyring Secret Service on the user session bus for
# the Linux credential tests. DBUS_SESSION_BUS_ADDRESS must name that bus.
#
# The daemon runs in the foreground under a background subshell, which keeps
# its output and records how it exited in $RUNNER_TEMP/secret-service; a
# daemonized gnome-keyring sends its output to /dev/null. After a failure,
# secret-service-diagnostics.sh prints that record.
set -euo pipefail

state_dir="${RUNNER_TEMP:?}/secret-service"
log="$state_dir/daemon.log"
mkdir -p "$state_dir"

bash "$(dirname "$0")/apt-install-ubuntu.sh" gnome-keyring

echo "$(date -u +%Y-%m-%dT%H:%M:%S.%NZ) starting gnome-keyring-daemon" >"$log"
# One empty line is an empty password. It creates and unlocks the login
# collection, which becomes the default alias; empty input (no newline)
# creates no collection. Bash reports death by signal N as status 128 + N,
# which an ordinary exit code can also be, so the log names the signal only
# as a possibility. Bash's own job line before it (such as "Segmentation
# fault") and the core dump are what show that a signal ended the daemon.
(
  status=0
  printf '\n' | gnome-keyring-daemon --foreground --unlock --components=secrets || status=$?
  reason="status $status"
  if [ "$status" -gt 128 ] && signal=$(kill -l "$((status - 128))" 2>/dev/null); then
    reason="$reason (SIG$signal if a signal ended it)"
  fi
  echo "$(date -u +%Y-%m-%dT%H:%M:%S.%NZ) gnome-keyring-daemon exited with $reason"
) </dev/null >>"$log" 2>&1 &
wrapper_pid=$!

# Wait up to $1 tenths of a second for the daemon, the wrapper's child, to end.
daemon_ended_within() {
  for _ in $(seq 1 "$1"); do
    pgrep -P "$wrapper_pid" >/dev/null || return 0
    sleep 0.1
  done
  ! pgrep -P "$wrapper_pid" >/dev/null
}
on_exit() {
  local status=$? shown
  if [ "$status" -ne 0 ]; then
    echo "--- $log" >&2
    cat "$log" >&2
    shown=$(wc -l <"$log")
    # Stop the daemon started here, so it cannot take the name later: TERM,
    # then KILL after 5 seconds. Every wait is bounded.
    pkill -P "$wrapper_pid" || true
    if ! daemon_ended_within 50; then
      pkill -KILL -P "$wrapper_pid" || true
    fi
    if daemon_ended_within 10; then
      wait "$wrapper_pid" || true
      tail -n "+$((shown + 1))" "$log" >&2
    else
      echo "::warning::gnome-keyring-daemon under PID $wrapper_pid did not stop after SIGKILL." >&2
    fi
  fi
}
trap on_exit EXIT

bus_driver() {
  busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus "$@"
}
# The PID that owns org.freedesktop.secrets, or nothing if no process does.
secrets_owner() {
  bus_driver GetConnectionUnixProcessID s org.freedesktop.secrets 2>/dev/null | awk '{print $2}' || true
}
not_our_daemon() {
  local owner="no process"
  if [ -n "$1" ]; then owner="PID $1"; fi
  echo "::error::org.freedesktop.secrets is owned by $owner, not by the daemon started here ($2)." >&2
  if [ -n "$1" ]; then ps -ww -o pid,ppid,args -p "$1" >&2 || true; fi
  exit 1
}

# Wait through the bus driver, which never starts a service. A call to
# org.freedesktop.secrets while no process owns that name makes D-Bus
# activate the packaged gnome-keyring-daemon.service, whose login collection
# is locked.
for _ in $(seq 1 50); do
  [ "$(bus_driver NameHasOwner s org.freedesktop.secrets)" = "b true" ] && break
  sleep 0.2
done
daemon_pid=$(secrets_owner)
if [ -z "$daemon_pid" ] || [ "$(ps -o ppid= -p "$daemon_pid" | tr -d ' ')" != "$wrapper_pid" ]; then
  not_our_daemon "$daemon_pid" "a child of PID $wrapper_pid"
fi
echo "$daemon_pid" >"$state_dir/daemon.pid"

secret_service() {
  busctl --user "$1" org.freedesktop.secrets "${@:2}"
}
for _ in $(seq 1 50); do
  case "$(secret_service call /org/freedesktop/secrets org.freedesktop.Secret.Service \
    ReadAlias s default 2>/dev/null || true)" in */collection/*) break ;; esac
  sleep 0.2
done
alias_path=$(secret_service call /org/freedesktop/secrets org.freedesktop.Secret.Service \
  ReadAlias s default)
collection=${alias_path#o \"}
collection=${collection%\"}
case "$collection" in
  /org/freedesktop/secrets/collection/*) ;;
  *)
    echo "::error::The Secret Service has no default collection: $alias_path" >&2
    exit 1
    ;;
esac
locked=$(secret_service get-property "$collection" org.freedesktop.Secret.Collection Locked)
if [ "$locked" != "b false" ]; then
  echo "::error::The default collection $collection is locked: $locked" >&2
  exit 1
fi
# The calls above would have activated a replacement daemon if this one had
# left the bus meanwhile, so the owner is checked again after the last one.
owner_now=$(secrets_owner)
if [ "$owner_now" != "$daemon_pid" ]; then
  not_our_daemon "$owner_now" "PID $daemon_pid"
fi
echo "gnome-keyring-daemon PID $daemon_pid owns org.freedesktop.secrets; default collection $collection is unlocked; output in $log"
