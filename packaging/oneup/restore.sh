#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Argon ONE UP: undo oneup-takeover -- give the battery and the lid back to argononeupd.
#
#   sudo /usr/libexec/argon-utils/oneup-restore
#
# Reads what oneup-takeover recorded and puts back exactly that. Reboot afterwards: the lid
# overlay stays in the running kernel until then, holding GPIO27, so argononeupd is enabled
# now and starts at the next boot.
set -eu

die() { echo "restore: $*" >&2; exit 1; }

# For testing only: ARGON_ROOT prefixes every path, and SYSTEMCTL replaces systemctl.
R=${ARGON_ROOT:-}
SYSTEMCTL=${SYSTEMCTL:-systemctl}
[ -n "$R" ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"
record=$R/var/lib/argon-utils/oneup-takeover
[ -r "$record" ] || die "no $record: nothing was taken over by oneup-takeover"
val() { sed -n "s/^$1=//p" "$record"; }

config_txt=$R/boot/firmware/config.txt
argon_conf=$R/etc/argon-utils/config.toml
backup=$(val config_txt_backup)

# Exactly the three lines oneup-takeover appended: "[all]", its comment, the overlay. Anything
# else in config.txt -- including edits made since -- is left alone.
tmp=$(mktemp)
awk '
    { line[NR] = $0 }
    END {
        for (i = 1; i <= NR; i++) {
            if (line[i] == "[all]" && line[i+1] == "# argon-utils: the lid as a lid switch (undo with oneup-restore)" && line[i+2] == "dtoverlay=argon-oneup-lid") { i += 2; continue }
            print line[i]
        }
    }' "$config_txt" > "$tmp"
cat "$tmp" > "$config_txt"
rm -f "$tmp"
rm -f "$R/boot/firmware/overlays/argon-oneup-lid.dtbo"
rm -f "$R/etc/systemd/logind.conf.d/50-argon-oneup-lid.conf"
rmdir "$R/etc/systemd/logind.conf.d" 2>/dev/null || true

mode=$(val mode_before)
[ -n "$mode" ] && sed -i "s/^mode = \".*\"/mode = \"$mode\"/" "$argon_conf"
$SYSTEMCTL restart argond

if [ "$(val argononeupd_enabled)" = enabled ]; then $SYSTEMCTL enable argononeupd; fi

rm -f "$record"
echo "Restored. argond mode: $mode; argononeupd: $($SYSTEMCTL is-enabled argononeupd)."
echo "The config.txt backup from the takeover is still at $backup."
echo "Reboot to finish: the lid overlay leaves the kernel, and argononeupd starts."
