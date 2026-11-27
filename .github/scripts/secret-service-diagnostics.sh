#!/usr/bin/env bash
# After a Linux job fails, print what is known about the Secret Service that
# start-secret-service.sh started, so a gnome-keyring-daemon that crashed or
# left the session bus leaves evidence. It only reads state and logs, never
# calls org.freedesktop.secrets (which could start another daemon), and runs
# every section even when one fails.
set -uo pipefail

state_dir="${RUNNER_TEMP:?}/secret-service"

echo "::group::Daemon started by this job"
if [ -f "$state_dir/daemon.log" ]; then
  cat "$state_dir/daemon.log"
else
  echo "No $state_dir/daemon.log: the Secret Service step did not start a daemon."
fi
if [ -f "$state_dir/daemon.pid" ]; then
  pid=$(cat "$state_dir/daemon.pid")
  if ps -p "$pid" >/dev/null; then
    echo "PID $pid is still running:"
    ps -ww -o pid,ppid,lstart,etime,rss,stat,args -p "$pid"
  else
    echo "PID $pid is no longer running."
  fi
fi
echo "::endgroup::"

echo "::group::gnome-keyring processes and the org.freedesktop.secrets owner now"
ps -ww -eo pid,ppid,lstart,etime,rss,stat,args | awk 'NR == 1 || (/gnome-keyring/ && !/awk/)'
busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus \
  GetConnectionUnixProcessID s org.freedesktop.secrets
echo "::endgroup::"

# One query over the system and user journals: journalctl --user would omit
# systemd-oomd, a system unit. USER_UNIT= entries are the user manager's own
# lines, such as the session bus stopping or restarting.
echo "::group::Journal: gnome-keyring, the session bus, and systemd-oomd"
sudo journalctl --no-pager -o short-precise -b \
  SYSLOG_IDENTIFIER=gnome-keyring-daemon \
  + _SYSTEMD_USER_UNIT=gnome-keyring-daemon.service \
  + USER_UNIT=gnome-keyring-daemon.service \
  + USER_UNIT=gnome-keyring-daemon.socket \
  + _SYSTEMD_USER_UNIT=dbus.service \
  + USER_UNIT=dbus.service \
  + _SYSTEMD_USER_UNIT=dbus-broker.service \
  + USER_UNIT=dbus-broker.service \
  + _SYSTEMD_UNIT=systemd-oomd.service
echo "::endgroup::"

echo "::group::Core dumps of gnome-keyring-daemon"
if command -v coredumpctl >/dev/null; then
  sudo coredumpctl --no-pager info /usr/bin/gnome-keyring-daemon
else
  echo "coredumpctl is not installed; kernel.core_pattern is $(cat /proc/sys/kernel/core_pattern)"
fi
echo "::endgroup::"

echo "::group::Kernel messages about memory pressure and crashes"
sudo dmesg -T | grep -iE 'out of memory|oom|killed process|segfault|general protection|traps:|gnome-keyring' \
  || echo "No matching kernel messages."
free -m
echo "::endgroup::"
