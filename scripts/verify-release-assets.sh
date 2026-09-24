#!/usr/bin/env bash
set -euo pipefail

assets=${1:?release assets directory required}
test -s "$assets/SHA256SUMS.txt"
test -s "$assets/delve-planner-sbom.cdx.json"
test -s "$assets/latest.json"
test -n "$(find "$assets" -name '*.dmg' -print -quit)"
test -n "$(find "$assets" -name '*setup.exe' -print -quit)"
test -n "$(find "$assets" -name '*.app.tar.gz.sig' -print -quit)"
test -n "$(find "$assets" -name '*setup.exe.sig' -print -quit)"
test -s "$assets/windows-signatures.txt"
grep -Eq 'Valid|Status[[:space:]]*:[[:space:]]*Valid' "$assets/windows-signatures.txt"
