#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Validates the contamination canary against a known-pass AND a known-fail input.
#
# A checker nobody has ever seen fail is not a checker -- it is a green light of unknown
# wiring. This runs in CI alongside cleanroom-check.sh so that an accidentally-inert
# canary (a bad path list, a typo'd grep, an over-broad exclude) fails the build instead
# of silently passing everything forever.
set -euo pipefail
cd "$(dirname "$0")/.."

CANARY=./scripts/cleanroom-check.sh
SABOTAGE_DIR=crates/argon-proto/src/.cleanroom-sabotage
cleanup() { rm -rf "$SABOTAGE_DIR"; }
trap cleanup EXIT

# 1. Known-pass: the real tree must be clean.
if ! "$CANARY" >/dev/null 2>&1; then
  echo "FAIL: canary rejects the clean tree"; "$CANARY"; exit 1
fi
echo "  ok: clean tree passes"

# 2. Known-fail: a planted upstream identifier must be caught.
#
# The identifier is assembled at runtime from fragments so that the literal never appears
# in this file. Otherwise this test would trip the very canary it is testing, and the
# obvious fix -- excluding this file from the search -- would blunt the check for everyone.
mkdir -p "$SABOTAGE_DIR"
PLANT="argon""sysinfo""_getcputemp"
printf 'fn %s() -> f32 { 0.0 }\n' "$PLANT" > "$SABOTAGE_DIR/tainted.rs"
if "$CANARY" >/dev/null 2>&1; then
  echo "FAIL: canary is INERT -- it passed a tree containing a known upstream identifier"
  exit 1
fi
echo "  ok: planted violation is caught"

# 3. Known-pass again: removing the plant must restore a pass (no sticky state).
cleanup
if ! "$CANARY" >/dev/null 2>&1; then
  echo "FAIL: canary still rejects after the plant was removed"; exit 1
fi
echo "  ok: clean tree passes again"

echo "test-cleanroom-check: canary validated (pass / fail / pass)"
