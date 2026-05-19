# Contributing

## Branching

`master` is the main branch on the `agicash-rs` remote. The workspace also
maintains a `master-merger` integration branch — feature branches merge into
`master-merger`, the operator promotes `master-merger` to `master` once a
slice is green. No PR-required workflow; merges are direct.

Keep feature branches short-lived and branched off the latest
`agicash-rs/master`.

## Formatting + lints

Rust workspace is formatted with `cargo fmt` and linted with `cargo clippy`.
Use the shell functions:

```sh
afmt           # cargo fmt --all
aclippy        # cargo clippy --workspace --all-targets -- -D warnings
```

Run both before committing. CI rejects non-conforming code on `master`.

### Git hooks (prek)

A fast [`prek`](https://github.com/j178/prek) hook chain (config:
`.pre-commit-config.yaml`, tracked) stops trivial formatting/lint
failures before they reach CI:

- **pre-commit** (fast, no compile): text hygiene + `rustfmt --check`
  on the staged `.rs` files.
- **pre-push** (warm-cache fast): `cargo clippy -D warnings` across the
  workspace — the same gate CI runs, caught before the push.

One-time setup per machine + per checkout/worktree:

```sh
nix profile install nixpkgs#prek          # once per machine, puts prek on PATH
prek install --hook-type pre-commit --hook-type pre-push   # once per checkout/worktree
```

Run commits inside the nix dev shell (direnv auto-loads it on
`cd` into the repo) so `rustfmt`/`cargo` are on PATH. `PREK_ALLOW_NO_CONFIG=1`
is **no longer needed** — the config above is real, not a no-op.

## Comments and doc

Default to no comments. The bar for adding one: a future reader couldn't
recover the information from the code itself — a protocol quirk, a library
bug being worked around, a perf tradeoff with bounds, a named external
constraint. Link the spec/issue/PR when relevant. Verify the reason before
writing it.

Don't write comments that explain things the code or its surroundings
already show — where something is used, what a refactor was for, or a
step-by-step of the code. Let well-named identifiers carry the meaning;
push history into the commit message.

Doc comments belong on public surfaces: exported traits + their methods,
exported types and their fields, the `WalletClient` API. Skip them on
internal helpers, trivial getters, and per-platform view code.

## File naming

`kebab-case.rs` for rust modules. `snake_case` for rust identifiers.
`camelCase` and `PascalCase` for swift/kotlin per platform convention.

## Tests

See `docs/testing.md`. Add a test alongside any behaviour change. For
sans-IO state machines, prefer an event-pump test over an integration
test.
