# Slice 12 — WalletClient Facade — Checkpoint

**Date**: 2026-05-17  
**Worker**: Slice 12 worker on `feat/slice-12-walletclient`  
**Branch**: `feat/slice-12-walletclient` off `master-merger` (parent commit `ed3b4fb5`)  
**Worktree**: `~/agicash/.claude/worktrees/slice-12-walletclient`

## Status

**Code written; verification incomplete due to disk-full on nous (ENOSPC during cargo build).**

Initial `cargo check -p agicash-wallet` (lib only) **passed** with two minor `unused_imports` warnings (fixed in subsequent edits). After fixing the warnings, attempted `cargo test -p agicash-wallet --tests` surfaced 4 compile errors in test code (fixed via edits, NOT re-verified due to disk full).

## What was built

### New crate `agicash-wallet` source tree

```
crates/agicash-wallet/
├── Cargo.toml         (deps: agicash-{domain,money,traits,cashu,exchange-rate,
│                       lightning-address}, async-trait, cdk, chrono, hex,
│                       rust_decimal, serde, serde_json, thiserror, tokio[sync], uuid)
└── src/
    ├── lib.rs         (module + re-exports)
    ├── error.rs       (WalletError + From impls)
    ├── types.rs       (result records)
    ├── auth.rs        (AuthClient trait + Session + FakeAuth test impl)
    ├── builder.rs     (WalletClientBuilder)
    └── client.rs      (WalletClient struct + all methods)
```

### WalletClient public API surface

```rust
// Auth
async fn auth_guest(&self) -> Result<Session, WalletError>;
async fn auth_login(&self, email: &str, password: &str) -> Result<Session, WalletError>;
async fn auth_signup(&self, email: &str, password: &str, name: Option<&str>) -> Result<Session, WalletError>;
async fn auth_logout(&self) -> Result<(), WalletError>;
async fn auth_status(&self) -> Result<AuthStatus, WalletError>;
async fn set_session(&self, session: Session) -> Result<(), WalletError>;
async fn get_persisted_session(&self) -> Result<Option<Session>, WalletError>;

// Accounts + balance
async fn list_accounts(&self) -> Result<Vec<AccountSummary>, WalletError>;
async fn get_account(&self, account_id: AccountId) -> Result<AccountSummary, WalletError>;
async fn balance(&self, account_id: Option<AccountId>) -> Result<BalanceSummary, WalletError>;
async fn set_default_account(&self, account_id: AccountId, currency: Currency) -> Result<(), WalletError>;
   // returns Unsupported — see scope notes

// Mint management
async fn add_mint(&self, mint_url: String, currency: Currency) -> Result<AccountSummary, WalletError>;
async fn list_mints(&self) -> Result<Vec<MintSummary>, WalletError>;
async fn remove_mint(&self, account_id: AccountId) -> Result<(), WalletError>;
   // returns Unsupported

// Send
async fn quote_send_token(&self, account_id: Option<AccountId>, amount: Money) -> Result<SendTokenQuote, WalletError>;
async fn send_token(&self, account_id: Option<AccountId>, amount: Money, token_version: TokenVersion) -> Result<SendTokenReceipt, WalletError>;
async fn quote_send_lightning(&self, account_id: Option<AccountId>, invoice: String) -> Result<SendLightningQuote, WalletError>;
async fn send_lightning(&self, account_id: Option<AccountId>, invoice: String) -> Result<SendLightningHandle, WalletError>;
async fn complete_send_lightning(&self, quote_id: Uuid) -> Result<SendLightningReceipt, WalletError>;
async fn send_to_lightning_address(&self, account_id: Option<AccountId>, address: String, amount: Money) -> Result<SendLightningReceipt, WalletError>;

// Receive
async fn receive_cashu_token(&self, token: &str) -> Result<ReceiveReceipt, WalletError>;
async fn quote_receive_lightning(&self, account_id: Option<AccountId>, amount: Money) -> Result<ReceiveLightningHandle, WalletError>;
async fn poll_receive_lightning(&self, quote_id: Uuid) -> Result<ReceiveLightningSnapshot, WalletError>;
async fn complete_receive_lightning(&self, quote_id: Uuid) -> Result<ReceiveReceipt, WalletError>;

// Transactions (stubbed)
async fn list_transactions(&self, filter: TransactionFilter) -> Result<TransactionPage, WalletError>;
async fn get_transaction(&self, id: Uuid) -> Result<Transaction, WalletError>;

// Exchange rate
async fn exchange_rate(&self, from: Currency, to: Currency) -> Result<ExchangeRateSnapshot, WalletError>;

// Events (stubbed)
fn subscribe(&self) -> Result<(), WalletError>;
```

**Total: 27 public methods.**

### Builder

```rust
WalletClientBuilder::new()
    .auth(Arc<dyn AuthClient>)                          // required
    .user_storage(Arc<dyn UserStorage>)                 // required
    .cashu_provider(Arc<dyn CashuProvider>)             // required
    .cashu_receive_storage(Arc<dyn CashuReceiveSwapStorage>) // required
    .cashu_send_storage(Arc<dyn CashuSendSwapStorage>)  // required
    .cashu_mint_quote_storage(Arc<dyn CashuMintQuoteStorage>) // required
    .cashu_melt_quote_storage(Arc<dyn CashuMeltQuoteStorage>) // required
    .exchange_rate(Arc<dyn ExchangeRateProvider>)       // optional (returns Unsupported if absent)
    .build()                                            // -> Result<Arc<WalletClient>, WalletError>
```

## Composition pattern

The facade is **purely trait-based** — it takes `Arc<dyn …>` provider/storage handles
and never depends on the concrete `agicash-auth-opensecret` or
`agicash-storage-supabase` crates. This keeps the wallet crate
structurally wasm-friendly and lets consumers (Leptos PWA SSR, iOS FFI,
CLI, MCP server, MUTINY mocks) plug in whichever concrete backend they need.

The `AuthClient` trait was introduced in this slice (`src/auth.rs`)
because the existing `agicash-auth-opensecret` doesn't expose a
trait — it exposes free functions on `OpenSecretClient`. Consumers that
want to use it wrap it in their own `AuthClient` impl (5-10 LOC adapter).

## Deliberate scope cuts from the plan

The plan (`docs/superpowers/plans/2026-05-16-slice-12-walletclient-facade.md`)
listed 20 tasks. This slice ships **only** the facade itself and stubs the
storage/wiring gaps. Punted to follow-ups:

| Plan task | Why deferred |
|-----------|--------------|
| Task 12: `TransactionStorage::list_transactions` | Plan §6.1 is an open question (table vs view). Needs schema decision. Method returns `Unsupported`. |
| Task 14: `subscribe()` event bus | `agicash-cache` is empty. Method returns `Unsupported`. |
| Task 16: CLI swap-over | User explicitly scoped out: "Don't migrate FFI, expose the facade". |
| Task 17: FFI swap-over | Same. |
| Task 18: Checkpoint doc in `~/athanor/...` | Replaced by this in-crate checkpoint. |
| Task 19: CI gate `cargo build --target wasm32-unknown-unknown -p agicash-wallet` | Wasm-build requires tokio-feature surgery on `agicash-cashu` (uses workspace `rt-multi-thread`); out of slice scope. Facade IS structurally wasm-friendly (no opensecret/supabase deps), but transitively needs cashu's tokio features pinned. |
| Task 20: Update spec §16.12 | Spec doc update can follow when slice is integrated. |

Also stubbed (returns `Unsupported`):
- `set_default_account` — needs focused Supabase RPC. Plan §6.4 deferred.
- `remove_mint` — same. Plan §6.4 deferred.

## What tests cover what

Unit tests live in each module under `#[cfg(test)] mod tests`:

- **`error.rs`**: `From` impls + validation constructor (4 tests).
- **`types.rs`**: AccountSummary builder, BalanceSummary default, JSON
  roundtrip for AuthStatus, snake_case for ReceiveStatus (5 tests).
- **`auth.rs`**: `FakeAuth` in-memory impl with `register_guest` →
  `get_session` and `logout` → clear-session (2 tests).
- **`builder.rs`**: missing-dep `Validation` error (1 test).
- **`client.rs`**: `pick_cashu_account` selector across no-match,
  ambiguous, requested-id, and currency-match paths (5 tests);
  `unit_matches_currency`; `money_to_msat` for Sat/Msat/Cent (3 tests).

**No integration test against real testnut + Supabase + opensecret.**
The plan's task 15 requires running services; this worker doesn't have
them on, and the user's task said "TDD where it makes sense — fakes for
storage/auth/provider already exist in `crates/agicash-testing/`." The
testing crate is still empty (only `lib.rs` with one doc comment), so
the integration test that the plan envisioned would have to start by
shipping the fakes for `UserStorage`, `CashuProvider`, `CashuReceiveSwapStorage`,
`CashuSendSwapStorage`, `CashuMintQuoteStorage`, `CashuMeltQuoteStorage`
— that's a separate ~500-1000 LOC lane.

## Test fixes pending re-verification

After the first `cargo test -p agicash-wallet`, four compile errors surfaced
in test-only code; all four are now fixed but I could not re-run
`cargo test` due to disk-full on nous. Fixes applied:

1. **`auth.rs::FakeAuth`**: `[u8; 64]` doesn't impl `Default`; replaced
   `derive(Default)` with manual `impl Default`.
2. **`client.rs::tests`**: missing `agicash_domain::UserId` import; added.
3. **`client.rs::tests::pick_cashu_account_*`**: temporary-value-dropped
   errors on `&[a1.clone(), a2.clone()]` patterns; refactored to bind to
   `let accounts = vec![...]` first.

## Known wasm-build status

`cargo check --target wasm32-unknown-unknown -p agicash-wallet` was
**not** run as part of this slice — the wasm build of `agicash-cashu`
(transitive dep) currently fails because the workspace `tokio` feature
set includes `rt-multi-thread`, which pulls `mio` into wasm.

The facade itself is wasm-friendly:
- Depends only on `agicash-domain`, `agicash-money`, `agicash-traits`,
  `agicash-cashu`, `agicash-exchange-rate`, `agicash-lightning-address`.
- Has no direct `keyring`, `opensecret`, `postgrest`, `rustls-platform-verifier`
  imports (those live in opensecret/supabase crates, which the facade does NOT depend on).

Slice 13's wasm port can either:
- (a) gate the tokio features per-target in `agicash-cashu` (single `Cargo.toml`
  conditional, ~5 LOC), OR
- (b) cfg-gate the cashu-using methods in the facade behind
  `#[cfg(not(target_arch = "wasm32"))]` so wasm consumers get an
  always-`Unsupported` surface (more invasive).

The plan §7 task 4 explicitly chooses path (a)'s spirit: "facade itself
wasm-clean even though some providers under it are not — gated behind
`#[cfg]` features."

## Receive-cashu-token method signature for L4 / Leptos PWA

The user's task referenced `// TODO[slice-12]` markers in the Leptos PWA's
`components/cashu_token_paste_view.rs:177`. The actual file path doesn't
exist in this worktree (the leptos receive token branch
`feat/leptos-receive-token` isn't merged yet), so I couldn't directly
inspect the call site. The signature I shipped:

```rust
pub async fn receive_cashu_token(&self, token: &str) -> Result<ReceiveReceipt, WalletError>;
```

…matches the task's specification: "single `&str` input, returns
something with amount + mint info." `ReceiveReceipt` carries
`{ status, amount: Money, fee: Money, account_id, mint_url, token_hash }`.

If the leptos branch expects a `String` rather than `&str`, that's a
2-character change with no other downstream impact (`&str` is the more
forgiving signature).

## Sibling worker collision risk

- `feat/android-tls-init` (in flight) — different crate (agicash-ffi
  android target), no overlap.
- `feat/ios-send-cashu` (B's branch, not merged) — exposes `prepare_send_quote`
  + `create_send_swap` directly on the FFI surface. My facade's
  `send_token` composes those internally. NO file collision since I
  didn't touch agicash-ffi. When B's branch merges, FFI can either keep
  the granular methods or wrap `wallet.send_token()`; no API churn either way.
- `feat/leptos-receive-token` (L4's branch, not merged) — the
  `// TODO[slice-12]` markers in `components/cashu_token_paste_view.rs`
  call into a placeholder. When the branch merges, L4 plugs
  `wallet.receive_cashu_token(&token_str)` in at that TODO site.
  Signature compatible as documented above.

## Verification not yet run

Per the user's task, the following remain unverified:
- [ ] `cargo test -p agicash-wallet` — blocked by disk full
- [ ] `cargo clippy --workspace -- -D warnings` — blocked by disk full
- [ ] `cargo build --target wasm32-unknown-unknown -p agicash-wallet`
      — known to fail due to upstream cashu tokio features (out of scope)
- [ ] Commit + push to `agicash-rs`

The slice-12 module surface is in place; first-pass compile of the lib
(not tests) succeeded green. Tests need disk recovery to re-run.

## Recovery instructions

1. Free disk on nous — at minimum `rm -rf
   /Users/claude/.cache/agicash-cargo-target/wasm32-unknown-unknown` (the
   wasm probe build I ran earlier consumed multi-GB).
2. From this worktree, run:
   ```
   cd ~/agicash/.claude/worktrees/slice-12-walletclient/crates
   nix develop /Users/claude/agicash/.claude/worktrees/slice-12-walletclient -c cargo test -p agicash-wallet
   nix develop /Users/claude/agicash/.claude/worktrees/slice-12-walletclient -c cargo clippy -p agicash-wallet -- -D warnings
   ```
3. If green:
   ```
   PREK_ALLOW_NO_CONFIG=1 git add crates/agicash-wallet
   PREK_ALLOW_NO_CONFIG=1 git commit -m "feat(wallet): slice 12 WalletClient facade"
   git push -u agicash-rs feat/slice-12-walletclient
   ```
