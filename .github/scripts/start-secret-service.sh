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
# creates no collection. A status above 128 is 128 plus the signal number.
(
  status=0
  printf '\n' | gnome-keyring-daemon --foreground --unlock --components=secrets || status=$?
  if [ "$status" -gt 128 ]; then
    reason="signal $(kill -l "$((status - 128))")"
  else
    reason="status $status"
  fi
  echo "$(date -u +%Y-%m-%dT%H:%M:%S.%NZ) gnome-keyring-daemon exited with $reason"
) </dev/null >>"$log" 2>&1 &
wrapper_pid=$!

on_exit() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    # Stop the daemon started here, so it cannot take the name later.
    pkill -P "$wrapper_pid" || true
    wait "$wrapper_pid" || true
    echo "--- $log" >&2
    cat "$log" >&2
  fi
}
trap on_exit EXIT

bus_driver() {
  busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus "$@"
}

# Wait through the bus driver, which never starts a service. A call to
# org.freedesktop.secrets before the daemon owns that name would make D-Bus
# activate the packaged gnome-keyring-daemon.service, whose login collection
# is locked.
for _ in $(seq 1 50); do
  [ "$(bus_driver NameHasOwner s org.freedesktop.secrets)" = "b true" ] && break
  sleep 0.2
done
owner_pid=$(bus_driver GetConnectionUnixProcessID s org.freedesktop.secrets | awk '{print $2}')
if [ "$(ps -o ppid= -p "$owner_pid" | tr -d ' ')" != "$wrapper_pid" ]; then
  echo "::error::org.freedesktop.secrets is owned by PID $owner_pid, not by the daemon started here." >&2
  ps -ww -o pid,ppid,args -p "$owner_pid" >&2 || true
  exit 1
fi
echo "$owner_pid" >"$state_dir/daemon.pid"

default_alias() {
  busctl --user call org.freedesktop.secrets /org/freedesktop/secrets \
    org.freedesktop.Secret.Service ReadAlias s default
}
for _ in $(seq 1 50); do
  case "$(default_alias 2>/dev/null || true)" in */collection/*) break ;; esac
  sleep 0.2
done
alias_path=$(default_alias)
case "$alias_path" in
  */collection/*) ;;
  *)
    echo "::error::The Secret Service has no default collection: $alias_path" >&2
    exit 1
    ;;
esac
echo "gnome-keyring-daemon PID $owner_pid owns org.freedesktop.secrets; default collection $alias_path; output in $log"
