#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Fails if a built binary contains the builder's home directory or the source tree's path.
# Rust embeds source paths for panic messages; without --remap-path-prefix (debian/rules) every
# binary carried "/home/<user>/.cargo/registry/..." -- identifying data in a published package.
set -eu
[ $# -gt 0 ] || { echo "usage: $0 BINARY..." >&2; exit 2; }
home=${HOME:-/nonexistent}
here=$(cd "$(dirname "$0")/.." && pwd)
bad=0
for f in "$@"; do
    # A missing binary -- an unexpanded glob, a renamed target -- must not pass as clean.
    [ -f "$f" ] || { echo "$f: not a file" >&2; bad=1; continue; }
    # grep -c reads all of its input: no early exit, so strings is never cut off mid-stream.
    n=$(strings -a "$f" | grep -c -F -e "$home/" -e "$here/" -e /root/ || true)
    if [ "$n" -gt 0 ]; then
        echo "$f: $n string(s) with a build path, e.g. $(strings "$f" | grep -m1 -o -F -e "$home/" -e "$here/")" >&2
        bad=1
    fi
done
[ "$bad" = 0 ] && echo "binary paths: $# binaries, no build paths"
exit "$bad"
