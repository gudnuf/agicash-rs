//! `agicash-driver` — the resumption driver layer.
//!
//! Rust port of React's `useProcessXTasks` retry loop: re-fires the
//! per-machine "advance this stuck row" service method on every
//! unresolved row in a [`agicash_wallet::PendingStateSnapshot`]. The
//! React app's recoverability rests on exactly this loop; without it a
//! `send_swap` (or melt) that hit a transient mint error or a process
//! death leaves proofs reserved with nothing to drive the row forward.
//!
//! ## Lane A scope (plan 2026-05-21 §7)
//!
//! - The crate skeleton + the [`run_sweep`] core. `run_sweep()` is a
//!   plain `async fn` — no spawn, no triggers, no fallback timer, no
//!   coalescing latch (those are Lanes B–C). A caller invokes it once
//!   against a `&WalletClient`; the function calls
//!   [`agicash_wallet::WalletClient::refresh_pending_state`] and
//!   dispatches the §2 service method per row state, returning a
//!   [`SweepReport`].
//!
//! ## Idempotency, [`InvalidTransition`], double-pay
//!
//! - The four service methods are machine-guarded (plan §6.1, verified
//!   against ref `5fb1ab45` — see `agicash-cashu/src/{send_swap,
//!   receive_swap, mint_quote, melt_quote}/service.rs`). Re-firing on a
//!   terminal or wrong-state row is either a safe no-op or an
//!   `InvalidTransition` — never a double-spend.
//! - `InvalidTransition` (and `QuoteNotPaid` / `QuoteNotPending`) flatten
//!   through `WalletError::From<...>` to [`agicash_wallet::WalletError::Cashu(String)`]
//!   carrying the literal `"invalid state transition"` /
//!   `"quote not yet paid"` / `"quote not yet pending"` prefix. The
//!   driver classifies those as **benign no-ops** — the row resolved
//!   between the `list_*` read and the act, exactly React's
//!   `if (!swap) return;` guard.
//! - **Melt `PENDING` rows go ONLY through `poll_send_lightning`**
//!   (which calls `melt_quote_service::poll_until_complete` — reconcile,
//!   not re-pay). The driver NEVER calls `initiate_melt` (plan §6.2's
//!   double-pay bug).
//!
//! ## Failure modes
//!
//! - Per-row retry with exponential backoff for *transient* errors
//!   ([`agicash_wallet::WalletError::Network`] / `Concurrency`), capped
//!   at [`RetryConfig::max_attempts`].
//! - A classified domain error → no retry, row dropped from this sweep.
//! - **One bad row never breaks the sweep loop.** Each row's outcome is
//!   isolated; sweep returns a report with success / no-op / failure
//!   counts so callers and tests can assert progress.
//! - The next trigger (Lane B: realtime reconnect, foreground edge,
//!   fallback tick) re-sweeps. Failed rows get another chance.

pub mod error;
pub mod report;
pub mod sweep;

pub use error::{ClassifiedError, ErrorClass};
pub use report::{RowKind, RowOutcome, SweepReport};
pub use sweep::{run_sweep, run_sweep_with_config, RetryConfig};
