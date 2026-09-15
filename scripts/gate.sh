#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Everything CI runs, locally, with honest exit codes.
#
# Exists because `cargo clippy | grep error | head` reports the exit status of `head`, not of
# clippy -- so a failing lint reads as a clean run. Every check here captures the real status
# of the command that matters, and the script fails if any one of them fails.
set -uo pipefail
cd "$(dirname "$0")/.."

# Debian's rust-clippy package installs /usr/bin/cargo-clippy, which shadows rustup's shim
# and dies with "can't find crate for core". Put rustup first.
export PATH="$HOME/.cargo/bin:$PATH"

failed=()
run() {
  local name="$1"; shift
  printf '  %-28s ' "$name"
  local out
  if out=$("$@" 2>&1); then
    echo "ok"
  else
    echo "FAIL"
    failed+=("$name")
    echo "$out" | tail -25 | sed 's/^/      /'
  fi
}

echo "argon-utils gate"
echo "----------------"
run "format"            cargo fmt --all --check
run "clippy"            cargo clippy --workspace --all-targets -- -D warnings
run "tests"             cargo test --workspace --locked
run "no_std (bare ARM)" cargo build -p argon-proto --target thumbv7em-none-eabihf
run "panic strategy"     ./scripts/check-panic-strategy.sh
run "cleanroom canary"  ./scripts/cleanroom-check.sh
run "canary validation" ./scripts/test-cleanroom-check.sh

echo
if [ ${#failed[@]} -eq 0 ]; then
  echo "gate: all checks passed"
  exit 0
fi
echo "gate: ${#failed[@]} check(s) failed: ${failed[*]}"
exit 1
