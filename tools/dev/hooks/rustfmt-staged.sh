#!/usr/bin/env bash
# Pre-commit Rust formatting gate.
#
# Runs `rustfmt --check` ONLY on the staged .rs files prek passes as $@.
#
# Why staged-files, not `cargo fmt --all --check`:
#   - The workspace-wide check (what CI runs) also fails on PRE-EXISTING
#     formatting drift already on master. Gating every commit on the whole
#     workspace would block unrelated commits from every concurrent
#     contributor until that backlog is cleaned — punishing people for
#     code they didn't touch. The point of a pre-commit hook is to stop
#     the dev from INTRODUCING new unformatted code; that's exactly the
#     staged set.
#   - `rustfmt` directly (no `cargo fmt` wrapper) skips cargo manifest
#     resolution => even faster, still parse-only (no compile).
#   - CI still runs the authoritative `cargo fmt --all --check`; cleaning
#     the pre-existing backlog is a separate, owned task.
#
# Edition: the crates/ workspace pins edition 2021 (crates/Cargo.toml).
set -euo pipefail

# Only .rs files (prek already filters via `types: [rust]`, belt+braces).
files=()
for f in "$@"; do
    case "$f" in
        *.rs) [ -f "$f" ] && files+=("$f") ;;
    esac
done
[ ${#files[@]} -eq 0 ] && exit 0

if ! command -v rustfmt >/dev/null 2>&1; then
    echo "rustfmt not found on PATH — run commits inside the nix dev shell" >&2
    echo "(direnv auto-loads it on \`cd ~/agicash\`; or \`nix develop -c git commit ...\`)" >&2
    exit 1
fi

# --check: report-only, never rewrites; non-zero exit on any diff.
rustfmt --edition 2021 --check "${files[@]}"
