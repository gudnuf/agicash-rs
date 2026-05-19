#!/usr/bin/env bash
# Pre-commit hygiene: strip trailing whitespace from staged text files.
# Pure-shell stand-in for pre-commit-hooks `trailing-whitespace` so the
# hook needs no Python/uv/PyPI (see .pre-commit-config.yaml rationale).
#
# prek passes the staged filenames as $@. Offending files are rewritten
# in place and the hook exits non-zero so the commit is blocked and the
# dev re-stages the cleaned file (same UX as upstream). Uses awk for
# portability (BSD/macOS sed lacks GNU \t / \+ semantics).
set -euo pipefail

status=0
tab="$(printf '\t')"
for f in "$@"; do
    [ -f "$f" ] || continue
    # Skip binary files.
    grep -Iq . "$f" 2>/dev/null || continue

    tmp="$(mktemp)"
    awk -v T="$tab" '{ gsub("[ " T "]+$", ""); print }' "$f" >"$tmp"
    if ! cmp -s "$f" "$tmp"; then
        cat "$tmp" >"$f"
        echo "Fixed trailing whitespace: $f"
        status=1
    fi
    rm -f "$tmp"
done
exit "$status"
