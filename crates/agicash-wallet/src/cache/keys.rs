//! Cache key types — the surface that mirrors React TanStack Query's
//! cache keys (`AccountsCache.Key`, `TransactionsCache.Key`,
//! `PendingCashuReceiveQuotesCache.Key`, ...).
//!
//! Two roles:
//!
//! 1. [`CacheKind`] is a discriminator used in [`crate::cache::CacheUpdate`]
//!    so a downstream observer can decide which key it cares about
//!    without re-reading the whole cache.
//! 2. The DB-naming convention (`cashu_receive_quote`, NOT `mint_quote`)
//!    is honored in this enum for new code; smell S9 in
//!    `~/athanor/projects/agicash-rust/smells.md` covers the broader
//!    rename of the underlying rust types as a separate lane.

use serde::{Deserialize, Serialize};

/// One of the table-shaped cache slices the wallet maintains.
///
/// Used as a topic tag on the [`crate::cache::CacheUpdate`] broadcast
/// channel. Consumers (Leptos signal handlers, FFI bridges) match on it
/// to decide which derived view to recompute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CacheKind {
    /// `wallet.accounts` — keyed by [`agicash_domain::AccountId`].
    /// Mirrors React's `AccountsCache.Key` (`'accounts'`).
    Accounts,
    /// `wallet.transactions` — keyed by [`uuid::Uuid`].
    /// Mirrors React's `TransactionsCache.Key` (`'transactions'`).
    /// Note: populate path is a no-op until `TransactionStorage` ships;
    /// the slot exists so `Change` events can patch in advance.
    Transactions,
    /// Derived: `wallet.transactions` rows with
    /// `acknowledgment_status = 'pending'` count.
    /// Mirrors React's `TransactionsCache.UnacknowledgedCountKey`.
    UnacknowledgedTransactionCount,
    /// `wallet.cashu_receive_quotes` — keyed by row UUID.
    /// Mirrors React's `CashuReceiveQuoteCache.Key`.
    /// (DB-naming: rust SDK historically calls this `mint_quote`.)
    CashuReceiveQuotes,
    /// `wallet.cashu_send_quotes` — keyed by row UUID.
    /// Mirrors React's send-quote (`UnresolvedCashuSendQuotesCache.Key`).
    /// (DB-naming: rust SDK historically calls this `melt_quote`.)
    CashuSendQuotes,
    /// `wallet.cashu_receive_swaps` — keyed by `token_hash` (the rich
    /// type's identity field).
    /// Mirrors React's `PendingCashuReceiveSwapsCache.Key`.
    CashuReceiveSwaps,
    /// `wallet.cashu_send_swaps` — keyed by row UUID.
    /// Mirrors React's `CashuSendSwapCache.Key` / `UnresolvedCashuSendSwapsCache.Key`.
    CashuSendSwaps,
    /// Derived: per-account cached balance.
    /// Sibling of S7 (proof-balance memoization) — see smells.md.
    AccountBalance,
}
