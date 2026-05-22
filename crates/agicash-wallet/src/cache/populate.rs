//! Lazy first-read population of cache slices from storage.
//!
//! ## Why lazy
//!
//! Operator's framing: "only cache what we need, like the React app, to
//! drive the UI forward." A wallet that opens directly to the home view
//! never needs `cashu_send_swaps` populated; populating eagerly would
//! waste a network round-trip every session. Lazy population mirrors
//! React Query's behavior (first `useQuery` triggers `queryFn`).
//!
//! ## Thundering herd
//!
//! Two concurrent first-readers must not each fire a storage call. The
//! [`InFlightGuard`] uses a `tokio::sync::Mutex<HashSet<CacheKind>>` —
//! the first caller marks the key as in-flight, drops the mutex, and
//! issues the storage call; subsequent callers see the key in the set,
//! wait on the same mutex (re-checking the populated flag inside).
//! Mirrors React Query's request deduplication.
//!
//! ## Idempotence
//!
//! After a populate completes, subsequent realtime `Change` events
//! continue to mutate the same row maps. No second populate is needed
//! — the realtime stream is authoritative. The `..._populated` flag
//! stays `true` for the wallet's lifetime.

use crate::cache::keys::CacheKind;
use crate::cache::state::CacheState;
use agicash_cashu::{
    CashuMeltQuoteStorage, CashuMintQuoteStorage, CashuReceiveSwapStorage, CashuSendSwapStorage,
};
use agicash_domain::UserId;
use agicash_traits::UserStorage;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

/// Guard preventing concurrent first-population of the same cache key.
///
/// Held as a field on [`super::WalletCache`]; every populate path takes
/// the async mutex, marks its key in the set, drops, and only THEN
/// makes the storage call. Subsequent callers wait for the mutex,
/// re-check the populated flag, and either populate again (won't —
/// flag is set) or return without I/O.
#[derive(Debug, Default)]
pub(crate) struct InFlightGuard(pub(crate) AsyncMutex<HashSet<CacheKind>>);

/// Populate `accounts` from `user_storage.list_accounts(user_id)` if
/// not already populated. Idempotent.
///
/// Errors propagate the storage error converted to a string.
pub(crate) async fn populate_accounts(
    state: &Arc<RwLock<CacheState>>,
    user_storage: &dyn UserStorage,
    user_id: UserId,
    in_flight: &InFlightGuard,
) -> Result<(), String> {
    if state.read().accounts_populated {
        return Ok(());
    }
    let _g = in_flight.0.lock().await;
    if state.read().accounts_populated {
        return Ok(());
    }
    let accounts = user_storage
        .list_accounts(user_id)
        .await
        .map_err(|e| format!("list_accounts: {e}"))?;
    let mut guard = state.write();
    for a in accounts {
        let id = a.id;
        guard.accounts.insert(id, a);
    }
    guard.accounts_populated = true;
    Ok(())
}

/// Populate `cashu_receive_quotes` from storage.
pub(crate) async fn populate_cashu_receive_quotes(
    state: &Arc<RwLock<CacheState>>,
    storage: &dyn CashuMintQuoteStorage,
    user_id: UserId,
    in_flight: &InFlightGuard,
) -> Result<(), String> {
    if state.read().cashu_receive_quotes_populated {
        return Ok(());
    }
    let _g = in_flight.0.lock().await;
    if state.read().cashu_receive_quotes_populated {
        return Ok(());
    }
    let quotes = storage
        .list_pending_for_user(user_id)
        .await
        .map_err(|e| format!("list_pending_mint_quotes: {e}"))?;
    let mut guard = state.write();
    for q in quotes {
        let id = q.id;
        guard.cashu_receive_quotes.insert(id, q);
    }
    guard.cashu_receive_quotes_populated = true;
    Ok(())
}

/// Populate `cashu_send_quotes` from storage.
pub(crate) async fn populate_cashu_send_quotes(
    state: &Arc<RwLock<CacheState>>,
    storage: &dyn CashuMeltQuoteStorage,
    user_id: UserId,
    in_flight: &InFlightGuard,
) -> Result<(), String> {
    if state.read().cashu_send_quotes_populated {
        return Ok(());
    }
    let _g = in_flight.0.lock().await;
    if state.read().cashu_send_quotes_populated {
        return Ok(());
    }
    let quotes = storage
        .list_unresolved_for_user(user_id)
        .await
        .map_err(|e| format!("list_unresolved_melt_quotes: {e}"))?;
    let mut guard = state.write();
    for q in quotes {
        let id = q.id;
        guard.cashu_send_quotes.insert(id, q);
    }
    guard.cashu_send_quotes_populated = true;
    Ok(())
}

/// Populate `cashu_receive_swaps` from storage. Keyed by `token_hash`.
pub(crate) async fn populate_cashu_receive_swaps(
    state: &Arc<RwLock<CacheState>>,
    storage: &dyn CashuReceiveSwapStorage,
    user_id: UserId,
    in_flight: &InFlightGuard,
) -> Result<(), String> {
    if state.read().cashu_receive_swaps_populated {
        return Ok(());
    }
    let _g = in_flight.0.lock().await;
    if state.read().cashu_receive_swaps_populated {
        return Ok(());
    }
    let swaps = storage
        .list_pending_for_user(user_id)
        .await
        .map_err(|e| format!("list_pending_receive_swaps: {e}"))?;
    let mut guard = state.write();
    for s in swaps {
        let k = s.token_hash.clone();
        guard.cashu_receive_swaps.insert(k, s);
    }
    guard.cashu_receive_swaps_populated = true;
    Ok(())
}

/// Populate `cashu_send_swaps` from storage.
pub(crate) async fn populate_cashu_send_swaps(
    state: &Arc<RwLock<CacheState>>,
    storage: &dyn CashuSendSwapStorage,
    user_id: UserId,
    in_flight: &InFlightGuard,
) -> Result<(), String> {
    if state.read().cashu_send_swaps_populated {
        return Ok(());
    }
    let _g = in_flight.0.lock().await;
    if state.read().cashu_send_swaps_populated {
        return Ok(());
    }
    let swaps = storage
        .list_unresolved_for_user(user_id)
        .await
        .map_err(|e| format!("list_unresolved_send_swaps: {e}"))?;
    let mut guard = state.write();
    for s in swaps {
        let id = s.id;
        guard.cashu_send_swaps.insert(id, s);
    }
    guard.cashu_send_swaps_populated = true;
    Ok(())
}
