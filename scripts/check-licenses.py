#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Every third-party dependency's licence must be on deny.toml's allow-list.

CI runs cargo-deny for this; the local gate did not, so a dependency outside the policy
(ksni, Unlicense, added with the tray) passed every local check and would only have failed
in CI. This reads the allow-list from deny.toml itself rather than keeping a copy, so the two
cannot disagree.

SPDX handling covers what the dependency graph actually contains: OR alternatives (any one
allowed suffices), AND terms (all must be allowed), and the legacy "/" separator.
"""

import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parent.parent
allow = set(tomllib.loads((root / "deny.toml").read_text())["licenses"]["allow"])

meta = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
)


def allowed(expr: str) -> bool:
    if not expr:
        return False
    alternatives = re.split(r"\s+OR\s+|/", expr)
    return any(
        all(term.strip(" ()") in allow for term in re.split(r"\s+AND\s+", alt))
        for alt in alternatives
        if alt.strip()
    )


third_party = [p for p in meta["packages"] if p["source"] is not None]
bad = [(p["name"], p["version"], p.get("license") or "(none)") for p in third_party
       if not allowed(p.get("license") or "")]

if bad:
    for name, version, lic in bad:
        print(f"licence not allowed by deny.toml: {name} {version}: {lic}", file=sys.stderr)
    sys.exit(1)
print(f"{len(third_party)} dependencies, all licences allowed")
