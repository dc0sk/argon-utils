#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Hand an Argon case's fan MCU from the vendor's argononed to argon-utils.
#
#   sudo /usr/libexec/argon-utils/mcu-takeover --dialect legacy|register
#
# --dialect is required, with no default and no detection: the transaction that would tell the
# two apart is harmless on one and pins the other's fan at full (ADR-0002). The ONE V1 speaks
# legacy; the ONE V3 speaks register. If you do not know which your case speaks, do not guess.
#
# argond is also set to mode "full", because it only drives the fan in full mode, and a retired
# vendor daemon beside a read-only argond would leave nobody driving it.
#
# If argond has not taken the fan within a few seconds, everything is put back at once and this
# exits non-zero: a fan with nobody driving it sits at whatever duty was last set, which may be off.
#
# Undo with mcu-restore, which puts back exactly what this changed; the package's removal runs it
# too. Everything changed is listed at the end and recorded in /var/lib/argon-utils/mcu-takeover.
#
# Not done by argond, as far as is known: acting on the case button's pulses, and whatever
# argononed may tell the MCU at shutdown. See the argon-utils packaging README.
set -eu

die() { echo "mcu-takeover: $*" >&2; exit 1; }

# For testing only: ARGON_ROOT prefixes every path, SYSTEMCTL replaces systemctl, and
# ARGON_CHECK replaces the command that confirms argond has the fan.
R=${ARGON_ROOT:-}
SYSTEMCTL=${SYSTEMCTL:-systemctl}
CHECK=${ARGON_CHECK:-argonctl fan}
RESTORE=${ARGON_RESTORE:-$(dirname "$0")/mcu-restore}
[ -n "$R" ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"

dialect=
case "${1:-}" in
    --dialect)
        dialect=${2:-}
        case "$dialect" in
            legacy | register) ;;
            *) die "--dialect must be legacy or register" ;;
        esac
        ;;
    *) die "usage: mcu-takeover --dialect legacy|register  (there is no default: see ADR-0002)" ;;
esac

argon_conf=$R/etc/argon-utils/config.toml
record=$R/var/lib/argon-utils/mcu-takeover
[ -w "$argon_conf" ] || die "$argon_conf not found"
grep -q '^dialect = ' "$argon_conf" || die "no [mcu] dialect line in $argon_conf"
[ -e "$record" ] && die "already taken over ($record exists); run mcu-restore first"

mkdir -p "$(dirname "$record")"
{
    echo "# argon-utils MCU takeover, $(date -Is)"
    echo "argononed_enabled=$($SYSTEMCTL is-enabled argononed 2>/dev/null || echo unknown)"
    echo "argononed_active=$($SYSTEMCTL is-active argononed 2>/dev/null || echo unknown)"
    echo "dialect_before=$(sed -n 's/^dialect = "\(.*\)"/\1/p' "$argon_conf")"
    echo "mode_before=$(sed -n 's/^mode = "\(.*\)"/\1/p' "$argon_conf")"
} > "$record"

# 1. The vendor daemon holds the MCU and the button line, and argond will not share either.
$SYSTEMCTL disable --now argononed 2>/dev/null || true

# 2. The dialect the operator stated, and full mode so argond drives the fan at all.
sed -i "s/^dialect = \".*\"/dialect = \"$dialect\"/" "$argon_conf"
sed -i 's/^mode = ".*"/mode = "full"/' "$argon_conf"
$SYSTEMCTL restart argond

# 3. Confirm argond really has the fan. If not, hand it straight back.
took=no
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if $CHECK 2>/dev/null | grep -q "argond has the fan"; then
        took=yes
        break
    fi
    sleep 1
done
if [ "$took" != yes ]; then
    echo "mcu-takeover: argond did not take the fan; putting argononed back." >&2
    "$RESTORE" >&2 || echo "mcu-takeover: WARNING: the restore failed too" >&2
    exit 1
fi

echo "Done. Changed:"
echo "  argononed        disabled and stopped"
echo "  [mcu] dialect    $dialect"
echo "  argond mode      full"
echo "  argond           $($CHECK 2>/dev/null | grep "argond has the fan" | head -1)"
echo
echo "Not done by argond: acting on the case button, and whatever argononed told the MCU at"
echo "shutdown. Undo everything with: sudo /usr/libexec/argon-utils/mcu-restore"
