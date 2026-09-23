#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Undo mcu-takeover: give the fan MCU back to the vendor's argononed.
#
#   sudo /usr/libexec/argon-utils/mcu-restore
#
# Puts back exactly what mcu-takeover recorded. argond is stopped before argononed starts and
# started again after, so the two never both drive the MCU, and argond then declines the fan
# because argononed holds it.
set -eu

die() { echo "mcu-restore: $*" >&2; exit 1; }

# For testing only: ARGON_ROOT prefixes every path, and SYSTEMCTL replaces systemctl.
R=${ARGON_ROOT:-}
SYSTEMCTL=${SYSTEMCTL:-systemctl}
[ -n "$R" ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"
record=$R/var/lib/argon-utils/mcu-takeover
argon_conf=$R/etc/argon-utils/config.toml
[ -r "$record" ] || die "no $record: nothing was taken over by mcu-takeover"
val() { sed -n "s/^$1=//p" "$record"; }

$SYSTEMCTL stop argond

d=$(val dialect_before)
m=$(val mode_before)
[ -n "$d" ] && sed -i "s/^dialect = \".*\"/dialect = \"$d\"/" "$argon_conf"
[ -n "$m" ] && sed -i "s/^mode = \".*\"/mode = \"$m\"/" "$argon_conf"

# `disable` was used, not `mask`, so these are the exact inverses.
[ "$(val argononed_enabled)" = enabled ] && $SYSTEMCTL enable argononed
[ "$(val argononed_active)" = active ] && $SYSTEMCTL start argononed

$SYSTEMCTL start argond
rm -f "$record"
echo "Restored: argononed $($SYSTEMCTL is-active argononed 2>/dev/null || echo inactive), [mcu] dialect $d, argond mode $m."
