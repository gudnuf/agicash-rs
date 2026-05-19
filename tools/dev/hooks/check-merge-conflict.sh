#!/usr/bin/env bash
# Pre-commit hygiene: block commits that still contain merge-conflict
# markers. Pure-shell stand-in for pre-commit-hooks
# `check-merge-conflict`. prek passes staged filenames as $@.
set -euo pipefail

status=0
for f in "$@"; do
    [ -f "$f" ] || continue
    # Skip binary files.
    grep -Iq . "$f" 2>/dev/null || continue
    # Anchored conflict markers at start of line (7 chars + space/EOL).
    if grep -nE '^(<<<<<<< |=======$|>>>>>>> )' "$f" >/dev/null 2>&1; then
        echo "Merge conflict marker found in: $f"
        grep -nE '^(<<<<<<< |=======$|>>>>>>> )' "$f" || true
        status=1
    fi
done
exit "$status"
