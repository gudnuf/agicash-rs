//! In-memory cache state — the row maps the cache layer owns.
//!
//! ## Shape
//!
//! One [`HashMap<id, T>`] per cacheable table, lifted under a single
//! [`parking_lot::RwLock`]. Hash maps (not vectors) because every
//! [`agicash_realtime::WalletChange`] arrives with a row id; an O(1)
//! upsert keeps the apply path tight. Derived list views walk the map
//! values and filter on read.
//!
//! ## Population flags
//!
//! Each table carries an `..._populated: bool` companion. Distinguishes
//! "the cache hasn't asked storage yet" from "the cache asked and it
//! was empty" — the populate path uses this to decide whether to take
//! the trip to storage. Mirrors the React app's TanStack Query implicit
//! `isLoading` vs `isSuccess` distinction.
//!
//! ## Version guards
//!
//! Every cacheable type carries a `version: u32` (or `i32` for
//! `Account`). Realtime broadcasts are not guaranteed in order — an
//! older `*Updated` payload could land after a newer one. Every upsert
//! checks `incoming.version > existing.version` before overwriting.
//! Mirrors React's `AccountsCache.upsert` invariant at
//! `app/features/accounts/account-hooks.ts:36-44`.

use agicash_cashu::{CashuMeltQuote, CashuMintQuote, CashuReceiveSwap, CashuSendSwap};
use agicash_domain::{Account, AccountId};
use agicash_storage_supabase::generated::tables::transactions::TransactionsRow;
use std::collections::HashMap;
use uuid::Uuid;

/// The cache's owned in-memory state.
///
/// Held behind a single [`parking_lot::RwLock`] inside
/// [`crate::cache::WalletCache`]. All fields are `pub(crate)` so the
/// sibling modules ([`super::apply`], [`super::populate`]) can mutate
/// them; external code uses the [`crate::cache::WalletCache`] surface.
//
// The `struct_excessive_bools` clippy lint fires because each table
// carries its own `..._populated` flag. They are NOT a state machine —
// each flag is independent (one table being populated says nothing
// about another). A state-machine refactor would couple them
// artificially. Keep one flag per table.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
pub(crate) struct CacheState {
    // -- Accounts ----------------------------------------------------
    /// `wallet.accounts` rows by id.
    pub(crate) accounts: HashMap<AccountId, Account>,
    /// `true` once a populate has completed (even if empty).
    pub(crate) accounts_populated: bool,

    // -- Cashu receive quotes (rust type: CashuMintQuote) ------------
    pub(crate) cashu_receive_quotes: HashMap<Uuid, CashuMintQuote>,
    pub(crate) cashu_receive_quotes_populated: bool,

    // -- Cashu send quotes (rust type: CashuMeltQuote) ---------------
    pub(crate) cashu_send_quotes: HashMap<Uuid, CashuMeltQuote>,
    pub(crate) cashu_send_quotes_populated: bool,

    // -- Cashu receive swaps -----------------------------------------
    /// Keyed by `token_hash` (the rich type's identity field, not row id).
    pub(crate) cashu_receive_swaps: HashMap<String, CashuReceiveSwap>,
    pub(crate) cashu_receive_swaps_populated: bool,

    // -- Cashu send swaps --------------------------------------------
    pub(crate) cashu_send_swaps: HashMap<Uuid, CashuSendSwap>,
    pub(crate) cashu_send_swaps_populated: bool,

    // -- Transactions (scaffolding; TransactionStorage not yet shipped) --
    /// Storage trait is stubbed (`list_transactions` returns
    /// `Unsupported` on the facade). The slot exists so realtime
    /// `Change` events that arrive while populate is unavailable still
    /// patch in; the read API returns empty + tracing-warn until
    /// `TransactionStorage` lands.
    pub(crate) transactions: HashMap<Uuid, TransactionsRow>,
    /// Reserved — set by future `populate_transactions` when
    /// `TransactionStorage` ships. See `transactions` field doc.
    #[allow(dead_code)]
    pub(crate) transactions_populated: bool,

    /// Count of `wallet.transactions` rows with
    /// `acknowledgment_status = 'pending'`. Maintained eagerly from the
    /// `previous_acknowledgment_status` delta on
    /// `TRANSACTION_UPDATED` events (the only place the wire carries an
    /// old-row fragment; see `agicash-realtime::TransactionWithPreviousAck`).
    pub(crate) unacknowledged_transaction_count: u32,

    // -- Account balance (S7 — proof-balance memoization) ------------
    /// Cached balance in the account's minor unit, keyed by account id.
    /// Populated lazily on first `WalletClient::balance()` /
    /// `WalletClient::accounts()` call. Invalidated by any
    /// `CashuSendSwap*` / `CashuReceiveSwap*` / `AccountUpdated`
    /// realtime Change (those are the events that touch unspent
    /// proofs); next read repopulates from storage.
    pub(crate) account_balance: HashMap<AccountId, u64>,
}

impl CacheState {
    /// Apply the version-guarded upsert pattern (mirrors React's
    /// `AccountsCache.upsert` invariant): only overwrite if the incoming
    /// row has a strictly greater version than the existing one.
    /// Returns `true` if the map was mutated.
    pub(crate) fn upsert_versioned<K, V, F>(
        map: &mut HashMap<K, V>,
        key: K,
        incoming: V,
        version_of: F,
    ) -> bool
    where
        K: std::hash::Hash + Eq,
        F: Fn(&V) -> i64,
    {
        match map.get(&key) {
            Some(existing) if version_of(&incoming) <= version_of(existing) => false,
            _ => {
                map.insert(key, incoming);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_versioned_inserts_when_absent() {
        let mut m: HashMap<u32, (u32, &'static str)> = HashMap::new();
        let mutated = CacheState::upsert_versioned(&mut m, 1, (5, "v5"), |(v, _)| i64::from(*v));
        assert!(mutated);
        assert_eq!(m[&1], (5, "v5"));
    }

    #[test]
    fn upsert_versioned_overwrites_higher_version() {
        let mut m: HashMap<u32, (u32, &'static str)> = HashMap::new();
        m.insert(1, (5, "v5"));
        let mutated = CacheState::upsert_versioned(&mut m, 1, (6, "v6"), |(v, _)| i64::from(*v));
        assert!(mutated);
        assert_eq!(m[&1], (6, "v6"));
    }

    #[test]
    fn upsert_versioned_rejects_equal_or_lower_version() {
        let mut m: HashMap<u32, (u32, &'static str)> = HashMap::new();
        m.insert(1, (5, "v5"));
        let equal = CacheState::upsert_versioned(&mut m, 1, (5, "stale"), |(v, _)| i64::from(*v));
        assert!(!equal);
        assert_eq!(m[&1], (5, "v5"));
        let lower = CacheState::upsert_versioned(&mut m, 1, (4, "older"), |(v, _)| i64::from(*v));
        assert!(!lower);
        assert_eq!(m[&1], (5, "v5"));
    }

    // Workspace lint `missing_debug_implementations = "warn"` requires Debug.
    #[test]
    fn cache_state_is_debug() {
        let s = CacheState::default();
        let _ = format!("{s:?}");
    }
}
