#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# packaging/mcu/takeover.sh and restore.sh against a fake root: the takeover changes exactly what
# it says, refuses without a stated dialect, rolls itself back when argond does not take the fan,
# and restore puts the config back byte for byte.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
R=$(mktemp -d)
trap 'rm -rf "$R"' EXIT
fail() { echo "FAIL: $*"; exit 1; }

mkdir -p "$R/etc/argon-utils" "$R/bin"
printf 'mode = "read-only"\n\n[mcu]\ndialect = "legacy"\nbus = "auto"\n' > "$R/etc/argon-utils/config.toml"
cp "$R/etc/argon-utils/config.toml" "$R/original.toml"
# A systemctl that logs what it is asked, and reports argononed enabled and active.
cat > "$R/bin/systemctl" <<'STUB'
#!/bin/sh
echo "$*" >> "$ARGON_ROOT/systemctl.log"
case "$1" in
    is-enabled) echo enabled ;;
    is-active) echo active ;;
esac
exit 0
STUB
# A check that argond has the fan, and one that says it has not.
printf '#!/bin/sh\necho "argond has the fan off at 40.0C, decided 1s ago."\n' > "$R/bin/took"
printf '#!/bin/sh\necho "argond is running but not driving the fan"\n' > "$R/bin/declined"
chmod +x "$R/bin/systemctl" "$R/bin/took" "$R/bin/declined"
t="$here/packaging/mcu/takeover.sh"
r="$here/packaging/mcu/restore.sh"
export ARGON_ROOT="$R" SYSTEMCTL="$R/bin/systemctl" ARGON_RESTORE="$r"
rec="$R/var/lib/argon-utils/mcu-takeover"

# No dialect, or a made-up one, is refused -- and leaves nothing behind.
ARGON_CHECK="$R/bin/took" "$t" >/dev/null 2>&1 && fail "ran without --dialect"
ARGON_CHECK="$R/bin/took" "$t" --dialect auto >/dev/null 2>&1 && fail "accepted --dialect auto"
[ -e "$rec" ] && fail "a refused run left a record"
cmp -s "$R/etc/argon-utils/config.toml" "$R/original.toml" || fail "a refused run changed the config"

# A good run: argononed retired, dialect and mode set, argond restarted, everything recorded.
ARGON_CHECK="$R/bin/took" "$t" --dialect register >/dev/null || fail "takeover failed"
grep -q '^dialect = "register"$' "$R/etc/argon-utils/config.toml" || fail "dialect not set"
grep -q '^mode = "full"$' "$R/etc/argon-utils/config.toml" || fail "mode not set to full"
grep -q 'disable --now argononed' "$R/systemctl.log" || fail "argononed not retired"
grep -q '^dialect_before=legacy$' "$rec" || fail "old dialect not recorded"
grep -q '^mode_before=read-only$' "$rec" || fail "old mode not recorded"
ARGON_CHECK="$R/bin/took" "$t" --dialect register >/dev/null 2>&1 && fail "took over twice"

# Restore: the config byte for byte, argononed enabled and started, the record gone -- and argond
# stopped BEFORE argononed starts, so the two never drive the MCU at once.
: > "$R/systemctl.log"
"$r" >/dev/null || fail "restore failed"
cmp -s "$R/etc/argon-utils/config.toml" "$R/original.toml" || fail "restore did not put the config back"
grep -q '^enable argononed$' "$R/systemctl.log" || fail "argononed not re-enabled"
[ -e "$rec" ] && fail "restore left the record"
stop=$(grep -n '^stop argond$' "$R/systemctl.log" | cut -d: -f1)
start=$(grep -n '^start argononed$' "$R/systemctl.log" | cut -d: -f1)
[ -n "$stop" ] && [ -n "$start" ] && [ "$stop" -lt "$start" ] || fail "argononed started before argond stopped"

# If argond does not take the fan, the takeover undoes itself and fails: nobody may be left
# driving it. This is the property the check exists for.
: > "$R/systemctl.log"
ARGON_CHECK="$R/bin/declined" "$t" --dialect register >/dev/null 2>&1 && fail "claimed success with nobody driving the fan"
cmp -s "$R/etc/argon-utils/config.toml" "$R/original.toml" || fail "a failed takeover left the config changed"
[ -e "$rec" ] && fail "a failed takeover left a record"
grep -q '^start argononed$' "$R/systemctl.log" || fail "a failed takeover did not give the fan back"

echo "mcu takeover: all checks passed"
