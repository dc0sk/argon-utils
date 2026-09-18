#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# T14 -- full-discharge test recorder.
#
# Records what the UPS does from mains loss to the automatic poweroff, so the run yields a
# number nobody has yet: how long this machine actually survives on battery, and how long the
# fall from `low` to `critical` takes.
#
# Deliberately does NOT open the UPS serial port. argond owns it, and CDC-ACM has no
# arbitration: a second reader would split the byte stream and both would desynchronise. This
# reads only what argond publishes (/run/argon-utils/ups.state) plus logind's own view of any
# scheduled shutdown -- both read-only, neither able to perturb the thing being measured.
#
# Usage:
#     docs/testing/t14-discharge-record.sh start     # detaches; survives the terminal closing
#     docs/testing/t14-discharge-record.sh status    # is it running, and where is it up to
#     docs/testing/t14-discharge-record.sh report    # analyse the log, after the machine is back
#     docs/testing/t14-discharge-record.sh stop
#
# Everything lands in ~/argon-t14/, which is on persistent storage. /tmp would not survive the
# poweroff this test ends with.

set -euo pipefail

DIR=${ARGON_T14_DIR:-$HOME/argon-t14}
LOG=$DIR/timeline.log
PIDFILE=$DIR/recorder.pid
STATE_FILE=${ARGON_T14_STATE:-/run/argon-utils/ups.state}
INTERVAL=${ARGON_T14_INTERVAL:-10}

# --- reading the machine, without touching the UPS --------------------------------------

state_value() {
    # $1 = key. Empty string if the file or key is missing.
    [ -r "$STATE_FILE" ] || return 0
    sed -n "s/^$1=//p" "$STATE_FILE" | head -1
}

logind_shutdown() {
    busctl get-property org.freedesktop.login1 /org/freedesktop/login1 \
        org.freedesktop.login1.Manager ScheduledShutdown 2>/dev/null |
        tr -d '\n' || true
}

cpu_temp_c() {
    local raw
    raw=$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null || echo 0)
    echo $((raw / 1000))
}

load_1min() {
    cut -d' ' -f1 /proc/loadavg 2>/dev/null || echo "?"
}

# --- preflight ---------------------------------------------------------------------------

preflight() {
    local fail=0

    if ! systemctl is-active --quiet argond.service; then
        echo "T14: argond is not running. Start it first: sudo systemctl start argond" >&2
        fail=1
    fi

    local mode
    mode=$(sed -n 's/^mode *= *"\(.*\)"/\1/p' /etc/argon-utils/config.toml 2>/dev/null | head -1)
    if [ "$mode" != "full" ]; then
        echo "T14: argond is in mode \"${mode:-unknown}\", so it will NOT power the machine" >&2
        echo "T14: off. This test measures the real poweroff, so set mode = \"full\" in" >&2
        echo "T14: /etc/argon-utils/config.toml and restart argond." >&2
        fail=1
    fi

    if [ ! -r "$STATE_FILE" ]; then
        echo "T14: cannot read $STATE_FILE -- is argond publishing?" >&2
        fail=1
    else
        local updated age
        updated=$(state_value updated)
        age=$(( $(date +%s) - ${updated:-0} ))
        if [ "$age" -gt 60 ]; then
            echo "T14: $STATE_FILE is ${age}s old; argond may be stuck. Check:" >&2
            echo "T14:   journalctl -u argond -n 20" >&2
            fail=1
        fi
    fi

    case "$(logind_shutdown)" in
    *poweroff*)
        echo "T14: a poweroff is ALREADY scheduled. Cancel it before starting:" >&2
        echo "T14:   sudo shutdown -c" >&2
        fail=1
        ;;
    esac

    local level
    level=$(state_value level)
    if [ "$level" != "on-mains" ]; then
        echo "T14: level is \"$level\", not on-mains. Plug mains in and let it settle first," >&2
        echo "T14: so the log starts from a known state." >&2
        fail=1
    fi

    [ "$fail" -eq 0 ] || exit 1
}

# --- the recording loop ------------------------------------------------------------------

record() {
    mkdir -p "$DIR"
    local start_epoch boot_id
    start_epoch=$(date +%s)
    boot_id=$(cat /proc/sys/kernel/random/boot_id)

    {
        echo "# T14 discharge test"
        echo "# started        $(date -Is)  (epoch $start_epoch)"
        echo "# boot_id        $boot_id"
        echo "# host           $(uname -n), kernel $(uname -r)"
        echo "# argond         $(argond --version 2>/dev/null || echo unknown)"
        echo "# thresholds     $(grep -E '^(low_percent|critical_percent|confirmations|shutdown_delay_min|min_uptime_s)' \
            /etc/argon-utils/config.toml 2>/dev/null | tr '\n' ' ')"
        echo "# ups poll       $(sed -n '/^\[ups\]/,$ s/^poll_interval_s *= *//p' \
            /etc/argon-utils/config.toml 2>/dev/null | head -1)s"
        echo "# sample every   ${INTERVAL}s"
        echo "#"
        echo "# Columns are key=value so the report parser cannot drift from the writer."
    } >>"$LOG"

    local last_level="" unplug_epoch=0 low_epoch=0 critical_epoch=0

    while :; do
        local now level percent shutdown_at pending temp load elapsed
        now=$(date +%s)
        level=$(state_value level)
        percent=$(state_value percent)
        shutdown_at=$(state_value shutdown_at)
        pending=$(logind_shutdown)
        temp=$(cpu_temp_c)
        load=$(load_1min)

        # Elapsed since mains was lost, which is the number this test exists to produce.
        if [ "$unplug_epoch" -ne 0 ]; then
            elapsed=$((now - unplug_epoch))
        else
            elapsed=""
        fi

        printf 't=%s iso=%s level=%s percent=%s on_battery_s=%s shutdown_at=%s temp_c=%s load=%s logind=%s\n' \
            "$now" "$(date -Is -d "@$now")" "${level:-?}" "${percent:-}" "$elapsed" \
            "${shutdown_at:-}" "$temp" "$load" "${pending:-unreadable}" >>"$LOG"

        # --- transitions, marked with the elapsed time so the log reads on its own -------
        if [ "$level" != "$last_level" ]; then
            case "$level" in
            on-battery | low | critical)
                if [ "$unplug_epoch" -eq 0 ]; then
                    unplug_epoch=$now
                    echo "# MARK mains lost at $(date -Is -d "@$now") (${percent:-?}%)" >>"$LOG"
                fi
                ;;
            esac
            case "$level" in
            low)
                if [ "$low_epoch" -eq 0 ]; then
                    low_epoch=$now
                    echo "# MARK low at $(date -Is -d "@$now") (${percent:-?}%), $((now - unplug_epoch))s on battery" >>"$LOG"
                fi
                ;;
            critical)
                if [ "$critical_epoch" -eq 0 ]; then
                    critical_epoch=$now
                    echo "# MARK critical at $(date -Is -d "@$now") (${percent:-?}%), $((now - unplug_epoch))s on battery" >>"$LOG"
                    echo "# MARK poweroff expected within the configured delay from here" >>"$LOG"
                fi
                ;;
            on-mains)
                if [ -n "$last_level" ] && [ "$last_level" != "on-mains" ]; then
                    echo "# MARK mains back at $(date -Is -d "@$now") (${percent:-?}%) -- test aborted by replugging" >>"$LOG"
                fi
                ;;
            esac
            last_level=$level
        fi

        # Flushed every sample. The machine is going to lose power at the end of this test,
        # and an unflushed tail is the part that matters most.
        sync -f "$LOG" 2>/dev/null || sync

        sleep "$INTERVAL"
    done
}

# --- the report --------------------------------------------------------------------------

report() {
    [ -r "$LOG" ] || {
        echo "T14: no log at $LOG" >&2
        exit 1
    }

    echo "T14 discharge test -- report"
    echo
    sed -n 's/^# //p' "$LOG" | grep -E '^(started|boot_id|host|argond|thresholds|ups poll|sample every)' || true
    echo

    echo "Marks:"
    grep '^# MARK' "$LOG" | sed 's/^# MARK /  /' || echo "  (none)"
    echo

    # Durations, computed from the marks' own timestamps rather than from wall-clock guesses.
    local unplug low crit
    unplug=$(awk '/^t=/ && $3 != "level=on-mains" && $3 != "level=unknown" {print $1; exit}' "$LOG" | cut -d= -f2)
    low=$(awk '/^t=/ && $3 == "level=low" {print $1; exit}' "$LOG" | cut -d= -f2)
    crit=$(awk '/^t=/ && $3 == "level=critical" {print $1; exit}' "$LOG" | cut -d= -f2)

    if [ -n "${unplug:-}" ]; then
        local first_pct last_line last_pct last_t
        first_pct=$(awk -v u="$unplug" '/^t=/ {split($1,a,"="); if (a[2]==u) {split($4,p,"="); print p[2]; exit}}' "$LOG")
        last_line=$(grep '^t=' "$LOG" | tail -1)
        last_pct=$(echo "$last_line" | awk '{split($4,p,"="); print p[2]}')
        last_t=$(echo "$last_line" | awk '{split($1,a,"="); print a[2]}')

        echo "Durations:"
        printf '  on battery, first reading   %s (%s%%)\n' "$(date -Is -d "@$unplug")" "${first_pct:-?}"
        [ -n "${low:-}" ] && printf '  mains loss -> low           %s  (%s min)\n' \
            "$((low - unplug))s" "$(((low - unplug) / 60))"
        [ -n "${crit:-}" ] && printf '  mains loss -> critical      %s  (%s min)\n' \
            "$((crit - unplug))s" "$(((crit - unplug) / 60))"
        [ -n "${low:-}" ] && [ -n "${crit:-}" ] && printf '  low -> critical             %s  (%s min)\n' \
            "$((crit - low))s" "$(((crit - low) / 60))"
        printf '  last reading                %s (%s%%)\n' "$(date -Is -d "@$last_t")" "${last_pct:-?}"
        echo

        # Discharge rate, from the percentages actually observed. Reported as an observation,
        # not extrapolated to a full charge: the curve near empty is exactly where a fuel
        # gauge is least trustworthy, which is why the thresholds are where they are.
        if [ -n "${first_pct:-}" ] && [ -n "${last_pct:-}" ] && [ "$first_pct" -gt "$last_pct" ]; then
            local dropped minutes
            dropped=$((first_pct - last_pct))
            minutes=$(((last_t - unplug) / 60))
            echo "Observed discharge:"
            printf '  %s%% in %s min' "$dropped" "$minutes"
            [ "$minutes" -gt 0 ] && printf '  (%s min per percentage point)' "$((minutes / dropped))"
            printf '\n\n'
        fi
    else
        echo "The log never left on-mains: mains was not unplugged, or not for long enough."
        echo
    fi

    echo "Did the machine power off?"
    if grep -q 'logind=.*poweroff' "$LOG"; then
        echo "  A poweroff was scheduled and visible in logind:"
        grep -m1 'logind=.*poweroff' "$LOG" | sed 's/^/    /'
    else
        echo "  No scheduled poweroff was ever recorded."
    fi
    local now_boot log_boot
    now_boot=$(cat /proc/sys/kernel/random/boot_id)
    log_boot=$(sed -n 's/^# boot_id *//p' "$LOG" | tail -1)
    if [ "$now_boot" != "$log_boot" ]; then
        echo "  The machine has rebooted since the log was written, which is the expected end."
        echo "  The previous boot's journal is also persistent here:"
        echo "    journalctl -u argond -b -1 | tail -40"
    else
        echo "  Still the same boot: the machine has not powered off (yet)."
    fi
}

status() {
    if [ -r "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "T14: recording, pid $(cat "$PIDFILE"), every ${INTERVAL}s -> $LOG"
    else
        echo "T14: not running"
    fi
    [ -r "$LOG" ] && { echo "--- last 5 samples:"; grep '^t=' "$LOG" | tail -5; }
    [ -r "$STATE_FILE" ] && { echo "--- argond publishes now:"; sed 's/^/    /' "$STATE_FILE"; }
    true
}

start() {
    if [ -r "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "T14: already recording (pid $(cat "$PIDFILE"))" >&2
        exit 1
    fi
    preflight
    mkdir -p "$DIR"

    # setsid, so the recorder outlives the terminal, the SSH connection and the session that
    # started it. This test ends with the machine powering off; the recorder must not be the
    # first thing to die.
    setsid nohup "$0" __run >>"$DIR/recorder.out" 2>&1 &
    local pid=$!
    echo "$pid" >"$PIDFILE"
    sleep 1

    echo "T14: recording every ${INTERVAL}s -> $LOG  (pid $pid)"
    echo
    echo "Now: unplug mains. Then leave it alone."
    echo
    echo "The machine will power off on its own once the battery is confirmed critical."
    echo "SAVE YOUR WORK FIRST -- that poweroff is the point of the test."
    echo
    echo "  watch progress:  $0 status"
    echo "  abort:           plug mains back in (cancels within ~2 readings)"
    echo "  hard abort:      sudo shutdown -c && sudo systemctl stop argond"
    echo
    echo "After it comes back up:  $0 report"
}

stop() {
    if [ -r "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        kill "$(cat "$PIDFILE")"
        rm -f "$PIDFILE"
        echo "T14: stopped"
    else
        echo "T14: not running"
    fi
}

case "${1:-}" in
start) start ;;
__run) record ;;
status) status ;;
report) report ;;
stop) stop ;;
*)
    sed -n '3,27p' "$0" | sed 's|^# \{0,1\}||'
    exit 1
    ;;
esac
