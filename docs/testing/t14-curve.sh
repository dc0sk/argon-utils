#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# T14 analysis: the discharge curve, and what it implies about the pack.
#
# Separate from t14-discharge-record.sh on purpose. bash reads a script lazily, so editing the
# recorder while it is running can change what the running shell executes next; this file can
# therefore be worked on during a live run.
#
# Usage:
#     docs/testing/t14-curve.sh [logfile]          # default ~/argon-t14/timeline.log
#     ARGON_T14_WATTS=5.0 docs/testing/t14-curve.sh
#
# ARGON_T14_WATTS is the assumed running draw (Pi plus the UPS's conversion losses) used only
# for the pack-energy estimate. It is an assumption, not a measurement, and the output says so.

set -euo pipefail

LOG=${1:-$HOME/argon-t14/timeline.log}
WATTS=${ARGON_T14_WATTS:-4.6}

[ -r "$LOG" ] || {
    echo "t14-curve: cannot read $LOG" >&2
    exit 1
}

echo "T14 discharge curve -- $LOG"
echo

# --- per-point timings -------------------------------------------------------------------
# The first sample at each percentage is the moment the gauge changed, which is the only
# timestamp the device actually justifies. Sampling is every 10 s, so each dt carries +/-10 s.

echo "Time per percentage point (first sample at each value; +/-10s from the sample interval):"
echo
awk '
/^t=/ {
    split($1, a, "="); t = a[2]
    split($3, l, "="); lvl = l[2]
    split($4, p, "="); pct = p[2]
    split($7, c, "="); temp = c[2]
    split($8, d, "="); load = d[2]
    if (pct == "") next
    on_batt = (lvl != "on-mains" && lvl != "unknown")
    if (pct != last_pct) {
        # The step across the mains->battery boundary is not a discharge interval: it is the
        # gauge re-reading under load. Counting it produced a spurious 10s "point".
        boundary = (last_lvl == "on-mains" || last_lvl == "" || last_lvl == "unknown")
        if (last_t != 0 && on_batt && !boundary) {
            dt = t - last_t
            printf "  %3s%%  after %6ds  this point took %4ds  %sC  load %s\n", \
                pct, t - start_t, dt, temp, load
            n++; sum += dt
            if (dt > max) max = dt
            if (min == 0 || dt < min) min = dt
            if (pct + 0 > last_pct + 0) bounces++
        } else if (on_batt) {
            start_t = t
            printf "  %3s%%  after %6ds  (first reading on battery)  %sC  load %s\n", \
                pct, 0, temp, load
        }
        last_t = t; last_pct = pct
    }
    last_lvl = lvl
}
END {
    if (n > 0) {
        printf "\n  %d points, mean %.0fs, min %ds, max %ds\n", n, sum / n, min, max
        if (bounces > 0) {
            printf "  %d of them went UP: the gauge is not monotonic, so single readings\n", bounces
            printf "  cannot be trusted as a trend.\n"
        } else {
            printf "  None went up: the gauge was strictly monotonic over this run.\n"
        }
    }
}' "$LOG"

# --- shape, in five-point segments -------------------------------------------------------
# A well-calibrated coulomb counter at constant load gives a flat column here. A rising then
# falling column is the signature of a voltage-derived state of charge on a lithium pack:
# steep at the top, flat across the 3.8-3.7V plateau, steep again past the knee.

echo
echo "Mean seconds per point, in five-point bands (the shape is the interesting part):"
echo
awk '
/^t=/ {
    split($1, a, "="); t = a[2]
    split($3, l, "="); lvl = l[2]
    split($4, p, "="); pct = p[2]
    if (pct == "") next
    on_batt = (lvl != "on-mains" && lvl != "unknown")
    if (pct != last_pct) {
        boundary = (last_lvl == "on-mains" || last_lvl == "" || last_lvl == "unknown")
        if (last_t != 0 && on_batt && !boundary) {
            band = int(pct / 5) * 5
            sum[band] += t - last_t
            cnt[band]++
        }
        last_t = t; last_pct = pct
    }
    last_lvl = lvl
}
END {
    for (b = 100; b >= 0; b -= 5) {
        if (cnt[b] > 0) {
            mean = sum[b] / cnt[b]
            bar = ""
            for (i = 0; i < int(mean / 10); i++) bar = bar "#"
            printf "  %3d-%3d%%  %5.0fs  %s\n", b, b + 4, mean, bar
        }
    }
}' "$LOG"

# --- what it implies about the pack ------------------------------------------------------

echo
awk -v watts="$WATTS" '
/^t=/ {
    split($1, a, "="); t = a[2]
    split($3, l, "="); lvl = l[2]
    split($4, p, "="); pct = p[2]
    if (pct == "" || lvl == "on-mains" || lvl == "unknown") next
    if (first_pct == "") { first_pct = pct; first_t = t }
    last_pct = pct; last_t = t
}
END {
    dropped = first_pct - last_pct
    if (dropped <= 0) { print "Pack estimate: not enough discharge yet."; exit }
    secs = last_t - first_t
    per_point = secs / dropped
    wh_per_point = watts * per_point / 3600
    printf "Pack estimate, from this run:\n\n"
    printf "  observed        %d points in %d min (mean %.0fs per point)\n", dropped, secs / 60, per_point
    printf "  ASSUMED draw    %.1f W  (Pi plus UPS conversion losses -- not measured)\n", watts
    printf "  => energy       %.2f Wh per point, so a full pack is roughly %.0f Wh\n", wh_per_point, wh_per_point * 100
    printf "  => last 10%%     about %.1f Wh\n\n", wh_per_point * 10
    printf "  The Wh figures are only as good as the assumed draw. Measure it with an inline\n"
    printf "  USB meter on the mains side to turn this into a real number.\n"
}' "$LOG"

# --- the halt-depletion measurement ------------------------------------------------------
# After the clean poweroff, the halted Pi and the UPS's own electronics keep draining the
# pack. Nobody publishes that figure, and it decides whether a full depletion for calibration
# takes two hours or a day.

echo
echo "Halt depletion (drain with the machine off):"
echo
last_sample=$(grep '^t=' "$LOG" | tail -1 | awk '{split($1,a,"="); print a[2]}')
last_pct=$(grep '^t=' "$LOG" | tail -1 | awk '{split($4,p,"="); print p[2]}')
boot_line=$(journalctl -u argond -b 0 -o short-unix --no-pager 2>/dev/null |
    grep -m1 -E 'ups: unknown -> ' || true)

if [ -z "$boot_line" ]; then
    echo "  No post-reboot reading yet: this is still the same boot, or argond has not"
    echo "  reported its first reading. Run this again after booting with mains connected."
elif [ -n "${last_sample:-}" ]; then
    boot_t=$(printf '%s\n' "$boot_line" | cut -d. -f1)
    boot_pct=$(printf '%s\n' "$boot_line" | sed -n 's/.* at \([0-9]*\)%.*/\1/p')
    gap=$((boot_t - last_sample))
    if [ "$gap" -gt 0 ] && [ -n "${boot_pct:-}" ] && [ -n "${last_pct:-}" ]; then
        drop=$((last_pct - boot_pct))
        printf '  last sample while running   %s%% at %s\n' "$last_pct" "$(date -Is -d "@$last_sample")"
        printf '  first reading after boot    %s%% at %s\n' "$boot_pct" "$(date -Is -d "@$boot_t")"
        printf '  interval                    %dh %dm\n' $((gap / 3600)) $(((gap % 3600) / 60))
        printf '  dropped                     %s points while off\n\n' "$drop"
        if [ "$drop" -gt 0 ]; then
            per_point=$((gap / drop))
            printf '  => %dh %dm per percentage point at halt\n' \
                $((per_point / 3600)) $(((per_point % 3600) / 60))
            printf '  => a full depletion from 10%% would take about %dh\n' $((per_point * 10 / 3600))
        elif [ "$drop" -lt 0 ]; then
            # Rising while off is not drain. A voltage-derived gauge reads higher once the load
            # is removed, because cell voltage recovers at rest; charging on mains before the
            # first reading also adds some. The two cannot be separated from this log.
            printf '  The gauge ROSE by %s point(s) while off. That is not a drain figure:\n' $((-drop))
            printf '  a voltage-derived gauge reads higher once the load is gone (cell voltage\n'
            printf '  recovers at rest), and charging on mains before the first reading adds\n'
            printf '  some. This log cannot separate the two, and gives no halt-drain rate.\n'
        else
            printf '  The gauge did not move: either the drain is below its resolution over\n'
            printf '  this interval, or the pack was already at its floor.\n'
        fi
        echo
        echo "  Caveats: the clock may have come from a UPS RTC that deep depletion reset, so"
        echo "  check the interval against your own wall-clock note. Booting with mains adds a"
        echo "  few seconds of charge, under a tenth of a point at typical charge rates."
    fi
fi
