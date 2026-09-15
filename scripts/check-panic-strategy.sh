#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Architecture fitness: the release profile must not use `panic = "abort"`.
#
# argon-device::safety::FanSafeGuard restores a running fan duty from a Drop impl. Under
# `panic = "abort"` a Drop impl does not run at all, so the guard would pass every one of its
# unit tests (which build under the test profile) while being completely inert in the builds
# that actually ship. That is a worse position than having no guard, because it also removes
# the motivation to build a real one.
#
# If the panic strategy genuinely needs to change, delete the Drop guard in the same commit
# and replace it with something that works under abort -- do not silence this check.
set -euo pipefail
cd "$(dirname "$0")/.."

guard='crates/argon-device/src/safety.rs'

if ! grep -q 'impl.*Drop for FanSafeGuard' "$guard" 2>/dev/null; then
  echo "check-panic-strategy: the Drop guard is gone; this check may no longer be needed"
  exit 0
fi

if grep -E '^\s*panic\s*=\s*"abort"' Cargo.toml >/dev/null 2>&1; then
  echo "FAIL: Cargo.toml sets panic = \"abort\", which disables Drop."
  echo
  echo "  ${guard} relies on Drop to restore a running fan duty when the"
  echo "  daemon stops. Under abort it will never run, and its tests will keep passing."
  echo
  grep -nE '^\s*panic\s*=' Cargo.toml | sed 's/^/    /'
  exit 1
fi

echo "check-panic-strategy: release profile keeps Drop working"
