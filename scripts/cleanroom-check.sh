#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Contamination canary. Fails if our source contains identifier spellings that only exist
# in Argon40's unlicensed implementation. This is a cheap tripwire, NOT a proof of
# clean-room provenance -- the real safeguard is rule 1 of CLEANROOM.md. It exists to catch
# a copy-paste slip, not a determined one.
#
# docs/ is exempt: naming upstream's files is exactly how we describe what we are replacing.
set -euo pipefail
cd "$(dirname "$0")/.."

PATTERN_FILE=scripts/upstream-identifiers.txt

# Candidate roots. Not all exist yet -- crates appear as the project grows -- and a
# nonexistent path makes grep exit non-zero, which would silently disable this check even
# while violations are present. So filter to what is actually there, and fail loudly if
# that leaves nothing to search.
CANDIDATE_PATHS=(crates sim fuzz xtask packaging scripts)
SEARCH_PATHS=()
for p in "${CANDIDATE_PATHS[@]}"; do
  [ -d "$p" ] && SEARCH_PATHS+=("$p")
done
if [ ${#SEARCH_PATHS[@]} -eq 0 ]; then
  echo "cleanroom-check: no source directories found -- refusing to report a vacuous pass"
  exit 1
fi

fail=0

while IFS= read -r pat; do
  [[ -z "$pat" || "$pat" == \#* ]] && continue
  # Exclude this script and the pattern list, which necessarily contain every pattern.
  if hits=$(grep -rInI --exclude-dir=target \
                --exclude=cleanroom-check.sh \
                --exclude=upstream-identifiers.txt \
                -- "$pat" "${SEARCH_PATHS[@]}" 2>/dev/null); then
    echo "CLEANROOM VIOLATION: upstream identifier '$pat' found in our source:"
    echo "$hits" | sed 's/^/    /'
    fail=1
  fi
done < "$PATTERN_FILE"

if [ "$fail" -ne 0 ]; then
  echo
  echo "See CLEANROOM.md. If this is a false positive, rename our symbol -- do not"
  echo "add an exception, because the whole value of this check is that it is dumb."
  exit 1
fi

echo "cleanroom-check: clean ($(grep -cvE '^\s*(#|$)' "$PATTERN_FILE") patterns, paths: ${SEARCH_PATHS[*]})"
