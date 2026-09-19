#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# The ONE UP lid overlay compiles, and -- where Raspberry Pi's dtmerge and a CM5 base tree are
# present -- applies to that tree, with its GPIO reference landing on the RP1 controller's
# line named GPIO27. A broken overlay is otherwise found at boot, on someone's laptop.
set -eu
src=packaging/dt/argon-oneup-lid-overlay.dts
command -v dtc >/dev/null || { echo "dtc not installed (device-tree-compiler)"; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
dtc -q -@ -W no-unit_address_vs_reg -I dts -O dtb -o "$tmp/lid.dtbo" "$src"

base=/boot/firmware/bcm2712-rpi-cm5-cm5io.dtb
if ! command -v dtmerge >/dev/null || [ ! -r "$base" ]; then
    echo "compiled; dtmerge or $base not available, merge not checked"
    exit 0
fi
dtmerge "$base" "$tmp/merged.dtb" "$tmp/lid.dtbo"
dtc -q -I dtb -O dts -o "$tmp/merged.dts" "$tmp/merged.dtb"

# The lid's gpios cell: <phandle 0x1b flags>.
ph=$(awk '/argon_lid \{/{f=1} f && /gpios = </{sub(/.*</,""); split($0,a," "); print a[1]; exit}' "$tmp/merged.dts")
line=$(awk '/argon_lid \{/{f=1} f && /gpios = </{sub(/.*</,""); split($0,a," "); print a[2]; exit}' "$tmp/merged.dts")
# The node carrying that phandle, and its 28th line name (offset 27).
name=$(awk -v ph="phandle = <$ph>;" '
    /gpio-line-names = /{names=$0}
    index($0, ph){print names; exit}' "$tmp/merged.dts" \
    | sed 's/.*= //; s/;$//' | tr -d '"' | tr ',' '\n' | sed 's/^ *//' | sed -n "$((line + 1))p")
grep -q 'linux,input-type = <0x05>;' "$tmp/merged.dts" || { echo "not EV_SW"; exit 1; }
grep -q 'linux,code = <0x00>;' "$tmp/merged.dts" || { echo "not SW_LID"; exit 1; }
if [ "$line" != 0x1b ] || [ "$name" != GPIO27 ]; then
    echo "lid gpio resolves to line $line named '$name', not GPIO27"
    exit 1
fi
echo "merged into $(basename "$base"): SW_LID on $name"
