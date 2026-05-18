# Slice 12 — WalletClient Facade — Rescue Outcome

**Date**: 2026-05-18
**Operation**: Protect → rebase → verify → push stranded slice-12 work.
**Result**: SUCCESS. Branch pushed, gate green (except known wasm), NOT merged to master.

## Recovery floor (inviolable)

**Protective snapshot SHA: `d1bfbb64d877ce9ae4cfca83e3141f7143347248`**

- Created before any rebase, on stale base `ed3b4fb5`.
- Message: `wip(wallet): slice-12 facade — protective snapshot (unverified, stale base ed3b4fb5)`.
- Preserved in reflog (`d1bfbb64 HEAD@{...}: commit`). The clean final commit
  was produced by rebase + `commit --amend`; the protective SHA is NOT
  reachable from the branch tip but remains recoverable via reflog /
  `git checkout d1bfbb64`.
- Staged explicitly (not `git add -A`) to keep the untracked
  `crates/certs/localhost-{cert,key}.pem` dev keypair (incl. a private key)
  OUT of history. Verified absent from the final commit.

## Rebase outcome

**CLEAN — no conflicts.** Only the single protective commit was replayed
onto `agicash-rs/master` (`ce9e4b65deb8f1253d1d680f3cb8bce454153413`),
114 commits ahead of the old base.

Surprise / key finding: master **already declared `agicash-wallet` as a
workspace member** with a placeholder crate (2-line `lib.rs` stub +
minimal `Cargo.toml`, deps: domain/money/traits/cache/services). Git's
3-way merge cleanly took my full crate over the placeholder and
recognized the workspace `crates/Cargo.toml` member entry as already
present — **`crates/Cargo.toml` is byte-identical to master**, zero
duplicate member, no hand-merge of `Cargo.lock` needed.

## API drift

**None.** `cargo build --workspace` passed on the first try after rebase
with zero source edits. The composed crates
(`agicash-cashu`, `agicash-exchange-rate`, `agicash-lightning-address`)
APIs held stable across the 114 commits — no signature reconciliation
required. (Notable: the 114 commits added `agicash-realtime` +
`WalletEventListener` FFI from slice-10; deliberately NOT wired in.)

## Gate result lines (literal)

| Check | Command | Result |
|-------|---------|--------|
| Build | `cargo build --workspace` | `Finished dev profile ... in 4.60s` — **clean** |
| Test (unit) | `cargo test -p agicash-wallet` | `test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` |
| Test (doc) | (same run) | `test result: ok. 0 passed; 0 failed; 1 ignored` |
| Clippy | `cargo clippy -p agicash-wallet -- -D warnings` | `Finished dev profile` — **clean** (after 2 fixes, see below) |
| Fmt | `cargo fmt --all --check` | 0 `Diff in` lines — **clean** (after formatting agicash-wallet only) |
| Wasm | `cargo check --target wasm32-unknown-unknown -p agicash-wallet` | **FAILS** (expected) — `mio v1.2.0` `compile_error!` via transitive `agicash-cashu` tokio `net`/`rt-multi-thread`. Out of slice-12 scope (slice-13). NOT fixed, NOT blocked. |

The 4 prior never-re-run test-compile fixes from the checkpoint are
confirmed good: all 20 unit tests pass.

## Edits made (strictly in-scope, agicash-wallet only)

1. **clippy `needless_lifetimes`** — `client.rs::pick_cashu_account`:
   removed explicit `'a` (elided per clippy suggestion).
2. **clippy `map_unwrap_or`** — `client.rs` mint_url resolution:
   `.map(str::to_string).unwrap_or(mint_url)` → `.map_or(mint_url, str::to_string)`.
3. **`cargo fmt -p agicash-wallet`** — original slice-12 code was never
   formatted; reflowed `builder.rs` + `client.rs`. (Scoped to this crate;
   did NOT run `fmt --all` write to avoid touching other crates.)
4. **`subscribe()` scope-guard comment** — added a `FOLLOW-UP (slice 29)`
   code comment noting `agicash-realtime`/`WalletEventListener` now exist
   on master; stub kept returning `Unsupported`, deliberately NOT wired.

No other crate touched. No API redesign, no feature add, no realtime
wiring, no CLI/FFI migration, no wasm tokio surgery, no cache pruning.

## Final state

- **Final pushed SHA: `c363ca65b3313f01c3addd207b134c3b2c1ec629`**
- Branch `feat/slice-12-walletclient` pushed fresh to `agicash-rs`
  (new branch, not force-push), tracking set.
- Single clean commit `feat(wallet): slice 12 WalletClient facade`
  directly on `ce9e4b65` (linear history).
- 9 files, 2517 insertions: agicash-wallet src tree + Cargo.lock +
  Cargo.toml + SLICE-12-CHECKPOINT.md.
- **NOT merged to master** — integration is the operator's call after
  code review per agicash-rust PROCESS phase gate.

## Open items for operator / follow-up

- `feature-dev:code-reviewer` review still pending (PROCESS step 4).
- Slice 13: wasm port — pin `agicash-cashu` tokio features per-target.
- Slice 29: wire `agicash-realtime` into `subscribe()` (comment marker
  left in `client.rs`).
- Still `Unsupported` by design: `set_default_account`, `remove_mint`,
  `list_transactions`, `subscribe` (see SLICE-12-CHECKPOINT.md scope cuts).
- `crates/certs/localhost-{cert,key}.pem` is local dev cruft (untracked,
  contains a private key) — left out of history intentionally; consider
  adding to a `.gitignore` if it keeps reappearing.

---

# Slice 12 — Review-blocker fix pass (H1/H2/M1)

**Date**: 2026-05-18
**Operation**: Fix exactly the 3 reviewer-identified blockers, re-gate, ff-merge.
Tightly scoped: no redesign, no scope-cut changes, no MED/LOW/NIT.
**Base**: `agicash-rs/master` = `ce9e4b65` (unchanged since the rescue push;
ff is clean).

## Blockers fixed

### H1 — `money_to_msat` (`crates/agicash-wallet/src/client.rs:~1157`)

Old code branched on raw `amount.unit()` and used `Decimal::try_into`,
which silently truncated fractional values and rejected
`Unit::Major` / `Unit::Cent` as `unsupported_unit`.

Rewritten to the canonical pattern (`money_to_minor_units` in
`agicash-cashu/src/mint_quote/service.rs:567`, `amount_as_u64` in
`send_swap/service.rs:670`): `amount.to_unit(Unit::Msat)` then
`Decimal::to_u64()`. Non-integer msat is now an EXPLICIT `bad_amount`
validation error (no silent truncation). BTC `Unit::Major` converts
correctly. USD/USDB still error (no `(Usd, Msat)` factor in
`agicash-money`) — correct, since Lightning send is BTC-only.

### H2 — `send_to_lightning_address` (`crates/agicash-wallet/src/client.rs:~650`)

No currency check existed before `money_to_msat`. The Lightning send
path always settles against a BTC Cashu account (`send_lightning` →
`pick_cashu_account(.., Currency::Btc)`). Added an explicit guard before
conversion: `amount.currency() != Currency::Btc` → `currency_mismatch`
validation error via `WalletError::validation`.

### M1 — `get_account` (`crates/agicash-wallet/src/client.rs:~141`)

Bound `require_session()` to `_session` and discarded it, fetching purely
by `account_id` (`get_account` is not user-scoped at the storage layer).
Now uses the real `session` and applies the same ownership re-check the
quote methods use (`complete_send_lightning:~612`,
`poll_receive_lightning:~771`): `account.user_id != session.user_id` →
`wrong_owner` validation error.

## Tests

Added to the `client.rs` test module:

- `money_to_msat_handles_btc_major_unit` — 0.00000001 BTC `Unit::Major`
  → 1000 msat (would have caught H1's `Unit::Major` rejection).
- `money_to_msat_rejects_fractional_msat` — 1500.5 msat `Unit::Msat`
  → `bad_amount` error (would have caught H1's silent truncation).

`get_account` ownership test: **TODO'd, not added.** Exercising
`get_account` requires constructing a full `WalletClient` (11
`Arc<dyn …>` deps incl. 4 concrete cashu service structs + an
`AuthClient` impl for `require_session`). No test builder or
`UserStorage` fake exists; this far exceeds the ~50 LOC inline-double
budget and would require the deferred fakes lane (explicitly out of
scope). Marked `TODO[slice-12-followup]` in the test module.

## Gate result lines (literal)

| Check | Command | Result |
|-------|---------|--------|
| Build | `cargo build --workspace` | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 5.27s` |
| Test (unit) | `cargo test -p agicash-wallet` | `test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` |
| Test (doc) | (same run) | `test result: ok. 0 passed; 0 failed; 1 ignored` |
| Clippy | `cargo clippy -p agicash-wallet -- -D warnings` | `Finished` — clean, zero lint warnings/errors |
| Fmt | `cargo fmt --all --check` | no diff — clean (no `cargo fmt -p` needed) |
| Wasm | — | out of scope (known slice-13 failure); not run |

22 unit tests (was 19 in the rescue pass + 3 new = 22). All green.

## Edits (strictly in-scope, agicash-wallet only)

1. `client.rs::money_to_msat` — H1 rewrite to canonical conversion.
2. `client.rs::send_to_lightning_address` — H2 currency guard.
3. `client.rs::get_account` — M1 ownership re-check (use real session).
4. `client.rs` test module — 2 H1 regression tests + M1 follow-up TODO.

No other crate, no API redesign, no scope-cut changes, no MED/LOW/NIT.

## Merge

- History: `c363ca65` (slice-12 facade) + one `fix(wallet): slice-12
  review blockers H1/H2/M1` commit — linear on `ce9e4b65`.
- `git fetch agicash-rs master` → still `ce9e4b65`; ff clean.
- ff-merge SHA + final `git log` recorded below after push.
