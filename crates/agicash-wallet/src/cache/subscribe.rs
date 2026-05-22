//! Broadcast surface for cache observers.
//!
//! Every successful cache mutation publishes a [`CacheUpdate`] tick on a
//! `tokio::sync::broadcast` channel. Downstream consumers
//! (Leptos signals, FFI bridges, the CLI's `--watch` flag) subscribe via
//! [`crate::WalletClient::cache_updates`] and translate ticks into
//! whatever platform-native rerender they need.
//!
//! The channel is bounded — slow consumers see `Err(Lagged)` and drop
//! ticks they missed. That is intentional: a missed tick is recoverable
//! by re-reading from the cache (it's still authoritative). The pump
//! must never block on a slow observer.

use crate::cache::keys::CacheKind;
use agicash_domain::AccountId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier of one cached row, for `CacheUpdate.id`.
///
/// Most rows are `Uuid`-keyed; `Account` uses the dedicated
/// [`AccountId`] newtype; `CashuReceiveSwap` is keyed by `token_hash`
/// (its rich-type identity field; see
/// `crates/agicash-cashu/src/receive_swap/types.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowId {
    Account(AccountId),
    Uuid(Uuid),
    TokenHash(String),
}

/// One cache mutation tick.
///
/// Carries enough to let a consumer ignore irrelevant changes (`kind`)
/// without reading the cache, and to re-read a specific row if needed
/// (`id`). For derived/aggregate mutations (e.g. unack count), `id` is
/// `None`.
#[derive(Debug, Clone)]
pub struct CacheUpdate {
    /// Which table slice mutated.
    pub kind: CacheKind,
    /// Row identity, when the mutation was row-scoped. `None` for
    /// derived/aggregate mutations.
    pub id: Option<RowId>,
}
