#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Every XML file the package installs must be well-formed.
#
# A malformed file in /usr/share/dbus-1/system.d/ can make dbus-daemon reject its configuration,
# and a malformed polkit action is silently ignored -- so the action would not exist and every
# check against it would fail. Neither shows up in any Rust test. The first draft of both files
# had "--" inside an XML comment, which is not allowed, and nothing else noticed.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v xmllint >/dev/null || { echo "xmllint not installed (libxml2-utils)" >&2; exit 1; }
mapfile -t files < <(find packaging -type f \( -name '*.conf' -o -name '*.policy' -o -name '*.xml' \) \
    -exec grep -l '<?xml' {} +)
[ ${#files[@]} -gt 0 ] || { echo "no packaging XML found; the check is not looking where it should" >&2; exit 1; }
xmllint --noout "${files[@]}"
echo "${#files[@]} packaging XML file(s) well-formed"
