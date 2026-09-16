#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Points git at the repo's hooks. Run once after cloning.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath scripts/hooks
echo "hooks installed: core.hooksPath = scripts/hooks"
echo "  pre-commit runs ./scripts/gate.sh; bypass with 'git commit --no-verify'"
