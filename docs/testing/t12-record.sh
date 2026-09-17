#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Task T12: records a once-per-second timeline to a directory that survives a reboot.
#
# Why: if the test ends with the machine powering off, nothing else survives. /tmp is tmpfs,
# and Raspberry Pi OS sets journald to Storage=volatile, so the journal of the boot that shut
# down is gone too. $HOME is on persistent storage.
#
# Read-only: it reads logind's ScheduledShutdown property, the T12 status file and the vendor
# unit states. It changes nothing.
#
# Usage: docs/testing/t12-record.sh [output-dir]      (default: ~/argon-t12)
set -u
OUT="${1:-$HOME/argon-t12}"
mkdir -p "$OUT"
LOG="$OUT/timeline.log"

# The boot id tells a post-reboot reader which boot each block of lines came from.
printf '# T12 timeline started %s, boot %s\n' "$(date -Is)" "$(cat /proc/sys/kernel/random/boot_id)" >> "$LOG"
echo "recording to $LOG (Ctrl-C or kill to stop)"

n=0
while :; do
  sched=$(busctl get-property org.freedesktop.login1 /org/freedesktop/login1 \
            org.freedesktop.login1.Manager ScheduledShutdown 2>/dev/null)
  # An empty kind and u64::MAX mean nothing is pending; say so plainly.
  case "$sched" in
    *'"" 18446744073709551615'*) sched="none" ;;
  esac
  state=""
  if [ -r /tmp/argon-t12.state ]; then state=$(tr '\n' ' ' < /tmp/argon-t12.state); fi
  units=$(systemctl is-active argonupsrtcd argononeupsd 2>/dev/null | paste -sd, -)
  printf '%s  shutdown=%s  vendor=%s  status=[%s]\n' \
    "$(date +%H:%M:%S)" "$sched" "$units" "$state" >> "$LOG"

  # A clean poweroff flushes filesystems anyway; syncing every few seconds also keeps most of
  # the timeline if the battery ever runs out for real.
  n=$((n + 1))
  if [ $((n % 5)) -eq 0 ]; then sync -f "$LOG" 2>/dev/null || sync; fi
  sleep 1
done
