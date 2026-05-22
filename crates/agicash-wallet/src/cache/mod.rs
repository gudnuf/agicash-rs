//! Wallet cache layer — mirrors the React app's TanStack Query surface.
//!
//! ## Why
//!
//! Before this layer every read on `WalletClient` (`list_accounts`,
//! `list_pending_mint_quotes`, …) hit Supabase. The React app already
//! solved this: `staleTime = ∞` on every query + realtime broadcasts
//! that **invalidate** the relevant key (so the next read refetches).
//!
//! Rust gets one strictly-better step: the typed
//! [`agicash_realtime::WalletRealtimeEvent::Change`] events carry the
//! actual row payload. The cache patches in-place from the delta and
//! callers read the cached row directly — no second round-trip
//! after a realtime tick.
//!
//! ## Surface
//!
//! - [`WalletCache`] — the in-`agicash-wallet` cache. One per
//!   [`crate::WalletClient`]. Public methods are READ-only;
//!   mutation is internal to the apply path.
//! - [`apply_change`] — call this for every
//!   [`agicash_realtime::WalletChange`] you receive from the realtime
//!   pump. Cache applies it (with version guards) and broadcasts a
//!   [`CacheUpdate`] tick to subscribers.
//! - [`WalletCache::subscribe_updates`] — get a
//!   `tokio::sync::broadcast::Receiver<CacheUpdate>`. One per observer
//!   (Leptos signal, FFI bridge, CLI watcher). Ticks describe WHICH
//!   slice changed; observers re-read the cache to get the new value.
//!
//! ## Construction
//!
//! [`crate::WalletClientBuilder::build`] constructs a cache by default
//! (with [`agicash_traits::PassthroughProofEncryption`] unless overridden
//! by [`crate::WalletClientBuilder::encryption`]). Existing callers
//! that don't touch the new methods see no behavioral change.
//!
//! ## Wiring
//!
//! This module does NOT subscribe to the realtime crate itself —
//! `agicash-realtime` is constructed in the FFI / Leptos shells, not
//! the wallet builder. Consumers feed `Change` events into
//! [`WalletCache::apply`] from their existing realtime pumps. Adding
//! the one-line wire-in is a follow-up lane in each shell (see the
//! design doc § 7 — "Cross-cut with consumers").

mod apply;
mod keys;
mod populate;
mod state;
mod subscribe;

#[cfg(test)]
mod tests;

pub use keys::CacheKind;
pub use subscribe::{CacheUpdate, RowId};

use crate::cache::populate::InFlightGuard;
use crate::cache::state::CacheState;
use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteStorage, CashuMintQuote, CashuMintQuoteStorage, CashuReceiveSwap,
    CashuReceiveSwapStorage, CashuSendSwap, CashuSendSwapStorage,
};
use agicash_domain::{Account, AccountId, UserId};
use agicash_realtime::WalletChange;
use agicash_traits::{ProofEncryption, UserStorage};
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Capacity for the cache-update broadcast channel.
///
/// Bounded — a slow observer that overruns sees `Err(Lagged)` on
/// `recv()` and drops missed ticks. That is intentional: missed ticks
/// are recoverable by re-reading the cache.
const UPDATE_BROADCAST_CAPACITY: usize = 256;

/// In-memory cache of the wallet's read surface.
///
/// Holds one [`std::collections::HashMap`] per cached table behind a
/// single [`parking_lot::RwLock`], plus a
/// [`tokio::sync::broadcast`] channel for change notifications.
///
/// Cheap to clone (internally `Arc`-shared); pass by value where
/// convenient.
///
/// ## WASM compat
///
/// Both sync primitives ([`parking_lot::RwLock`],
/// [`tokio::sync::broadcast`]) are wasm-compat. No `tokio::sync::Mutex`
/// inside reads, no runtime requirement for `recv()` other than a
/// `Future` executor (Leptos already runs one via
/// `wasm_bindgen_futures::spawn_local`).
#[derive(Clone)]
pub struct WalletCache {
    state: Arc<RwLock<CacheState>>,
    notify: broadcast::Sender<CacheUpdate>,
    encryption: Arc<dyn ProofEncryption>,
    in_flight: Arc<InFlightGuard>,
}

impl std::fmt::Debug for WalletCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletCache")
            .field("subscribers", &self.notify.receiver_count())
            .finish_non_exhaustive()
    }
}

impl WalletCache {
    /// Construct an empty cache.
    ///
    /// `encryption` MUST be the same proof-encryption handle the
    /// storage layer was built with — `to_cashu_*` helpers decrypt
    /// `encrypted_data` through it.
    #[must_use]
    pub fn new(encryption: Arc<dyn ProofEncryption>) -> Self {
        let (notify, _rx) = broadcast::channel(UPDATE_BROADCAST_CAPACITY);
        Self {
            state: Arc::new(RwLock::new(CacheState::default())),
            notify,
            encryption,
            in_flight: Arc::new(InFlightGuard::default()),
        }
    }

    /// Apply one typed realtime [`WalletChange`].
    ///
    /// Idempotent on stale/equal-version payloads (the upsert is
    /// version-guarded). Conversion failures are logged (`warn` /
    /// `debug` for `Unknown`) and leave state untouched. Never panics.
    ///
    /// Returns immediately for `Connected` / `StatusChanged` / `Error`
    /// flavors of [`agicash_realtime::WalletRealtimeEvent`] — those
    /// don't flow through this method; only `Change(_)` does. The FFI /
    /// Leptos pump destructures the realtime event and calls this with
    /// the inner `WalletChange` only.
    pub async fn apply(&self, change: WalletChange) {
        apply::apply_change(change, &self.state, &self.notify, self.encryption.as_ref()).await;
    }

    /// Subscribe to cache-update ticks. One receiver per consumer.
    ///
    /// Receivers handle `Err(Lagged)` by re-reading the cache (the
    /// payload they missed is still authoritative inside).
    #[must_use]
    pub fn subscribe_updates(&self) -> broadcast::Receiver<CacheUpdate> {
        self.notify.subscribe()
    }

    // -- Account reads --------------------------------------------------

    /// Snapshot of all cached accounts. Returns `None` if the cache has
    /// not been populated yet.
    ///
    /// Use [`Self::accounts_or_populate`] to populate-on-miss.
    #[must_use]
    pub fn accounts_snapshot(&self) -> Option<Vec<Account>> {
        let guard = self.state.read();
        if guard.accounts_populated {
            Some(guard.accounts.values().cloned().collect())
        } else {
            None
        }
    }

    /// Read-or-populate the account list. First call hits storage;
    /// subsequent calls return from cache.
    pub async fn accounts_or_populate(
        &self,
        user_storage: &dyn UserStorage,
        user_id: UserId,
    ) -> Result<Vec<Account>, String> {
        populate::populate_accounts(&self.state, user_storage, user_id, &self.in_flight).await?;
        Ok(self.state.read().accounts.values().cloned().collect())
    }

    /// Lookup one account by id from cache; `None` if not cached.
    /// Does NOT populate.
    #[must_use]
    pub fn account(&self, id: AccountId) -> Option<Account> {
        self.state.read().accounts.get(&id).cloned()
    }

    /// Cached balance for `account_id` (minor unit), if populated.
    /// `None` if not yet computed.
    #[must_use]
    pub fn account_balance(&self, account_id: AccountId) -> Option<u64> {
        self.state.read().account_balance.get(&account_id).copied()
    }

    /// Insert a freshly-computed balance into the cache (S7 — memoize
    /// `compute_cashu_balance` results).
    pub fn put_account_balance(&self, account_id: AccountId, balance: u64) {
        let mut guard = self.state.write();
        guard.account_balance.insert(account_id, balance);
        drop(guard);
        let _ = self.notify.send(CacheUpdate {
            kind: CacheKind::AccountBalance,
            id: Some(RowId::Account(account_id)),
        });
    }

    // -- Cashu receive quotes ------------------------------------------

    /// Read-or-populate pending mint quotes (cashu_receive_quotes).
    pub async fn pending_cashu_receive_quotes_or_populate(
        &self,
        storage: &dyn CashuMintQuoteStorage,
        user_id: UserId,
    ) -> Result<Vec<CashuMintQuote>, String> {
        populate::populate_cashu_receive_quotes(&self.state, storage, user_id, &self.in_flight)
            .await?;
        Ok(self
            .state
            .read()
            .cashu_receive_quotes
            .values()
            .cloned()
            .collect())
    }

    /// Lookup one cashu_receive_quote by row id; cache-only, no populate.
    #[must_use]
    pub fn cashu_receive_quote(&self, id: Uuid) -> Option<CashuMintQuote> {
        self.state.read().cashu_receive_quotes.get(&id).cloned()
    }

    // -- Cashu send quotes ---------------------------------------------

    pub async fn unresolved_cashu_send_quotes_or_populate(
        &self,
        storage: &dyn CashuMeltQuoteStorage,
        user_id: UserId,
    ) -> Result<Vec<CashuMeltQuote>, String> {
        populate::populate_cashu_send_quotes(&self.state, storage, user_id, &self.in_flight)
            .await?;
        Ok(self
            .state
            .read()
            .cashu_send_quotes
            .values()
            .cloned()
            .collect())
    }

    #[must_use]
    pub fn cashu_send_quote(&self, id: Uuid) -> Option<CashuMeltQuote> {
        self.state.read().cashu_send_quotes.get(&id).cloned()
    }

    // -- Cashu receive swaps -------------------------------------------

    pub async fn pending_cashu_receive_swaps_or_populate(
        &self,
        storage: &dyn CashuReceiveSwapStorage,
        user_id: UserId,
    ) -> Result<Vec<CashuReceiveSwap>, String> {
        populate::populate_cashu_receive_swaps(&self.state, storage, user_id, &self.in_flight)
            .await?;
        Ok(self
            .state
            .read()
            .cashu_receive_swaps
            .values()
            .cloned()
            .collect())
    }

    #[must_use]
    pub fn cashu_receive_swap_by_token_hash(&self, token_hash: &str) -> Option<CashuReceiveSwap> {
        self.state
            .read()
            .cashu_receive_swaps
            .get(token_hash)
            .cloned()
    }

    // -- Cashu send swaps ----------------------------------------------

    pub async fn unresolved_cashu_send_swaps_or_populate(
        &self,
        storage: &dyn CashuSendSwapStorage,
        user_id: UserId,
    ) -> Result<Vec<CashuSendSwap>, String> {
        populate::populate_cashu_send_swaps(&self.state, storage, user_id, &self.in_flight).await?;
        Ok(self
            .state
            .read()
            .cashu_send_swaps
            .values()
            .cloned()
            .collect())
    }

    #[must_use]
    pub fn cashu_send_swap(&self, id: Uuid) -> Option<CashuSendSwap> {
        self.state.read().cashu_send_swaps.get(&id).cloned()
    }

    // -- Transactions --------------------------------------------------

    /// Cached unacknowledged-transaction count.
    ///
    /// Maintained eagerly from `TransactionUpdated.previous_acknowledgment_status`
    /// deltas. Returns `0` if `TransactionStorage` has not shipped (the
    /// populate path is a no-op until then) AND no
    /// `TransactionCreated` events have arrived yet.
    #[must_use]
    pub fn unacknowledged_transaction_count(&self) -> u32 {
        self.state.read().unacknowledged_transaction_count
    }
}
