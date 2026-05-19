#!/usr/bin/env bash
# Pre-commit hygiene: reject staged files larger than a size limit.
# Pure-shell stand-in for pre-commit-hooks `check-added-large-files`.
#
# Usage (from .pre-commit-config.yaml): first arg is the max size in KB
# (via `args: ["2048"]`); remaining args are the staged filenames prek
# passes because `pass_filenames: true`.
set -euo pipefail

max_kb="${1:-2048}"
shift || true
max_bytes=$(( max_kb * 1024 ))

status=0
for f in "$@"; do
    [ -f "$f" ] || continue
    size=$(wc -c <"$f" | tr -d ' ')
    if [ "$size" -gt "$max_bytes" ]; then
        printf '%s is %d KB, exceeds the %d KB limit\n' \
            "$f" "$(( size / 1024 ))" "$max_kb"
        status=1
    fi
done
exit "$status"
