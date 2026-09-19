#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Argon ONE UP: hand the battery and the lid from the vendor's argononeupd to argon-utils.
#
#   sudo /usr/libexec/argon-utils/oneup-takeover LID_ACTION [--full]
#
# LID_ACTION is what closing the lid does, for logind's HandleLidSwitch=: ignore, lock or
# poweroff. (suspend and hibernate are refused: this Pi cannot sleep -- /sys/power/state is
# empty -- so logind would try and fail on every close.)
#
# --full also sets argond's mode to "full", which lets it power the machine off when the
# battery is confirmed critical. Without it argond keeps monitoring in its current mode.
#
# Nothing takes effect until you reboot: the lid overlay is read at boot. Undo with oneup-restore,
# which puts back exactly what this changed; everything it changes is listed below and in
# /var/lib/argon-utils/oneup-takeover.
set -eu

die() { echo "takeover: $*" >&2; exit 1; }

# For testing only: ARGON_ROOT prefixes every path, and SYSTEMCTL replaces systemctl.
R=${ARGON_ROOT:-}
SYSTEMCTL=${SYSTEMCTL:-systemctl}
[ -n "$R" ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"
action=${1:-}
case "$action" in
    ignore|lock|poweroff) ;;
    suspend|hibernate|hybrid-sleep|suspend-then-hibernate)
        die "$action needs sleep states this Pi does not have; use ignore, lock or poweroff" ;;
    *) die "usage: oneup-takeover ignore|lock|poweroff [--full]" ;;
esac
full=no
[ "${2:-}" = --full ] && full=yes

overlay_src=$R/usr/share/argon-utils/overlays/argon-oneup-lid.dtbo
overlay_dst=$R/boot/firmware/overlays/argon-oneup-lid.dtbo
config_txt=$R/boot/firmware/config.txt
dropin=$R/etc/systemd/logind.conf.d/50-argon-oneup-lid.conf
argon_conf=$R/etc/argon-utils/config.toml
record=$R/var/lib/argon-utils/oneup-takeover

[ -r "$overlay_src" ] || die "$overlay_src missing: install argon-utils 0.1.14 or later"
[ -w "$config_txt" ] || die "$config_txt not found"
grep -q '^source = "oneup"' "$argon_conf" || die "set [ups] source = \"oneup\" in $argon_conf first"
[ -e "$record" ] && die "already taken over ($record exists); run oneup-restore first"

mkdir -p "$(dirname "$record")"
stamp=$(date +%Y%m%d-%H%M%S)
{
    echo "# argon-utils ONE UP takeover, $(date -Is)"
    echo "argononeupd_enabled=$($SYSTEMCTL is-enabled argononeupd 2>/dev/null || echo unknown)"
    echo "config_txt_backup=$config_txt.argon-$stamp"
    echo "mode_before=$(sed -n 's/^mode = "\(.*\)"/\1/p' "$argon_conf")"
} > "$record"

# 1. The vendor daemon: it holds GPIO27, which the kernel needs, and acts on the battery.
$SYSTEMCTL disable --now argononeupd

# 2. The lid overlay, read at the next boot.
cp "$config_txt" "$config_txt.argon-$stamp"
install -m 0644 "$overlay_src" "$overlay_dst"
if ! grep -q '^dtoverlay=argon-oneup-lid$' "$config_txt"; then
    # A last line without its newline would otherwise run into "[all]".
    [ -z "$(tail -c1 "$config_txt")" ] || echo >> "$config_txt"
    printf '[all]\n# argon-utils: the lid as a lid switch (undo with oneup-restore)\ndtoverlay=argon-oneup-lid\n' >> "$config_txt"
fi

# 3. What closing the lid does. HandleLidSwitchExternalPower= follows it when unset.
mkdir -p "$(dirname "$dropin")"
printf '# argon-utils: the Argon ONE UP lid (undo with oneup-restore)\n[Login]\nHandleLidSwitch=%s\n' \
    "$action" > "$dropin"

# 4. Optionally, let argond power off on a critical battery.
if [ "$full" = yes ]; then
    sed -i 's/^mode = ".*"/mode = "full"/' "$argon_conf"
fi
$SYSTEMCTL restart argond

echo "Done. Changed:"
echo "  argononeupd        disabled and stopped"
echo "  $overlay_dst  installed; config.txt backed up to $config_txt.argon-$stamp"
echo "  $config_txt  + dtoverlay=argon-oneup-lid"
echo "  $dropin  HandleLidSwitch=$action"
echo "  argond mode        $(sed -n 's/^mode = "\(.*\)"/\1/p' "$argon_conf")"
echo
echo "Reboot for the lid to work. Until then the lid does nothing."
