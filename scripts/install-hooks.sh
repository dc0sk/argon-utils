#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Points git at the repo's hooks. Run once after cloning.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath scripts/hooks
echo "hooks installed: core.hooksPath = scripts/hooks"
echo "  pre-commit runs ./scripts/gate.sh; bypass with 'git commit --no-verify'"
echo "  commit-msg strips Claude-Session trailers (session links are internal)"
echo "  pre-push   refuses a push that would publish identifying or internal data"
denylist="${ARGON_DENYLIST:-${XDG_CONFIG_HOME:-$HOME/.config}/argon-utils/denylist}"
if [ ! -r "$denylist" ]; then
  echo
  echo "  NOTE: pre-push needs a denylist of this machine's real identifiers (serials, host names,"
  echo "  home paths), one extended regex per line, at $denylist."
  echo "  It stays outside the repository -- never commit it. Until it exists, pushes are refused."
fi
