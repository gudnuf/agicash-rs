#!/usr/bin/env bash
# Pre-commit hygiene: ensure each staged text file ends with exactly one
# newline (no missing newline, no trailing blank lines). Pure-shell
# stand-in for pre-commit-hooks `end-of-file-fixer`. prek passes staged
# filenames as $@. Offending files are fixed in place and the hook exits
# non-zero so the commit is blocked and the dev re-stages the fix.
set -euo pipefail

status=0
for f in "$@"; do
    [ -f "$f" ] || continue
    [ -s "$f" ] || continue
    # Skip binary files.
    grep -Iq . "$f" 2>/dev/null || continue

    tmp="$(mktemp)"
    # awk: buffer trailing blank lines; emit them only when a non-blank
    # line follows. End: print a single terminating newline. This both
    # adds a missing final newline AND collapses trailing blank lines.
    awk '
        { lines[NR] = $0 }
        END {
            last = NR
            while (last > 0 && lines[last] ~ /^[ \t]*$/) last--
            for (i = 1; i <= last; i++) print lines[i]
        }
    ' "$f" >"$tmp"

    if ! cmp -s "$f" "$tmp"; then
        cat "$tmp" >"$f"
        echo "Fixed end-of-file: $f"
        status=1
    fi
    rm -f "$tmp"
done
exit "$status"
