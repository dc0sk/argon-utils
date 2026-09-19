#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# packaging/oneup/takeover.sh and restore.sh against a fake root: the takeover changes exactly
# what it says, restore puts config.txt back byte for byte, and bad input is refused.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
R=$(mktemp -d)
trap 'rm -rf "$R"' EXIT
fail() { echo "FAIL: $*"; exit 1; }

mkdir -p "$R/boot/firmware/overlays" "$R/usr/share/argon-utils/overlays" "$R/etc/argon-utils" "$R/bin"
printf 'dtparam=i2c_arm=on\n[cm5]\ndtoverlay=dwc2,dr_mode=host\n\n[all]\ndtparam=nvme\n' > "$R/boot/firmware/config.txt"
cp "$R/boot/firmware/config.txt" "$R/original-config.txt"
echo overlay > "$R/usr/share/argon-utils/overlays/argon-oneup-lid.dtbo"
printf 'mode = "read-only"\n[ups]\nsource = "oneup"\n' > "$R/etc/argon-utils/config.toml"
# A systemctl that logs what it is asked, and says argononeupd is enabled.
cat > "$R/bin/systemctl" <<'STUB'
#!/bin/sh
echo "$*" >> "$ARGON_ROOT/systemctl.log"
[ "$1" = is-enabled ] && echo enabled
exit 0
STUB
chmod +x "$R/bin/systemctl"
export ARGON_ROOT="$R" SYSTEMCTL="$R/bin/systemctl"
t="$here/packaging/oneup/takeover.sh"
r="$here/packaging/oneup/restore.sh"

"$t" lock >/dev/null 2>&1 && fail "an unknown argument was accepted"
[ -e "$R/var/lib/argon-utils/oneup-takeover" ] && fail "a refused run left a record"

"$t" --full >/dev/null
grep -q '^dtoverlay=argon-oneup-lid$' "$R/boot/firmware/config.txt" || fail "no overlay line"
[ -e "$R/boot/firmware/overlays/argon-oneup-lid.dtbo" ] || fail "overlay not installed"
grep -q '^HandleLidSwitch=ignore$' "$R/etc/systemd/logind.conf.d/50-argon-oneup-lid.conf" || fail "no drop-in"
grep -q '^mode = "full"$' "$R/etc/argon-utils/config.toml" || fail "mode not full"
grep -q '^disable --now argononeupd$' "$R/systemctl.log" || fail "argononeupd not disabled"
"$t" >/dev/null 2>&1 && fail "a second takeover was accepted"

"$r" >/dev/null
cmp -s "$R/original-config.txt" "$R/boot/firmware/config.txt" || fail "config.txt not restored: $(diff "$R/original-config.txt" "$R/boot/firmware/config.txt")"
[ -e "$R/boot/firmware/overlays/argon-oneup-lid.dtbo" ] && fail "overlay left in /boot"
[ -e "$R/etc/systemd/logind.conf.d" ] && fail "drop-in directory left"
grep -q '^mode = "read-only"$' "$R/etc/argon-utils/config.toml" || fail "mode not restored"
grep -q '^enable argononeupd$' "$R/systemctl.log" || fail "argononeupd not re-enabled"
[ -e "$R/var/lib/argon-utils/oneup-takeover" ] && fail "record left behind"

# Without --full the mode is left as it was.
"$t" >/dev/null
grep -q '^mode = "read-only"$' "$R/etc/argon-utils/config.toml" || fail "mode changed without --full"
"$r" >/dev/null
cmp -s "$R/original-config.txt" "$R/boot/firmware/config.txt" || fail "second restore not exact"
echo "takeover and restore: exact"
