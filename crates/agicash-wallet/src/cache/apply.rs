//! Apply one [`agicash_realtime::WalletChange`] to [`super::state::CacheState`].
//!
//! This is the load-bearing path: every typed realtime broadcast lands
//! here, gets converted to a rich domain type via the storage `toX`
//! helpers, and is upserted into the right table map. Successful
//! mutations publish a [`super::subscribe::CacheUpdate`] tick.
//!
//! ## Discipline
//!
//! - **Total over `WalletChange`.** Every variant is handled or
//!   explicitly ignored. New variants surface as compile errors via the
//!   exhaustive match.
//! - **Conversion failures don't break the pump.** Any `toX` error is
//!   logged at warn level and counted; the cache state stays
//!   unchanged. Mirrors `parse_change`'s total-function discipline (a
//!   server-side payload-shape drift can never tear the cache down).
//! - **`Unknown` Change** logs once (debug) and is ignored. Same
//!   rationale.
//!
//! ## Version guards
//!
//! Every upsert uses [`super::state::CacheState::upsert_versioned`] so
//! an out-of-order older payload does not clobber a newer one.
//!
//! ## Unack-count delta
//!
//! `TransactionUpdated` carries `previous_acknowledgment_status`
//! (the ONE place the realtime wire emits an old-row fragment; see
//! `agicash-realtime::TransactionWithPreviousAck`). The count is patched
//! by comparing previous vs current ack status — same logic as React's
//! `transaction-hooks.ts:296-311`.

use crate::cache::keys::CacheKind;
use crate::cache::state::CacheState;
use crate::cache::subscribe::{CacheUpdate, RowId};
use agicash_realtime::WalletChange;
use agicash_storage_supabase::conversions::{
    to_account, to_cashu_melt_quote, to_cashu_mint_quote, to_cashu_receive_swap, to_cashu_send_swap,
};
use agicash_storage_supabase::generated::enums::AcknowledgmentStatus;
use agicash_storage_supabase::generated::tables::transactions::TransactionsRow;
use agicash_traits::ProofEncryption;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Apply one typed [`WalletChange`] to `state`, broadcasting a
/// [`CacheUpdate`] tick on `notify` for every successful mutation.
///
/// Never panics. Conversion failures emit a `tracing::warn!` and leave
/// state untouched. `WalletChange::Unknown` emits a `tracing::debug!`
/// and returns without touching state.
///
/// `encryption` MUST be the same proof-encryption handle the storage
/// layer uses — the `toX` helpers' `encrypted_data` decrypt would yield
/// garbage otherwise. The cache's constructor enforces this by accepting
/// the handle as an explicit ctor arg.
#[allow(clippy::too_many_lines)] // exhaustive match over WalletChange variants — flat is clearer than split.
pub(crate) async fn apply_change(
    change: WalletChange,
    state: &Arc<RwLock<CacheState>>,
    notify: &broadcast::Sender<CacheUpdate>,
    encryption: &dyn ProofEncryption,
) {
    match change {
        // -- Accounts -------------------------------------------------
        WalletChange::AccountCreated(payload) | WalletChange::AccountUpdated(payload) => {
            let account = match to_account(&payload.row) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("cache: to_account failed: {e}");
                    return;
                }
            };
            let id = account.id;
            let mutated = {
                let mut guard = state.write();
                let m = CacheState::upsert_versioned(&mut guard.accounts, id, account, |a| {
                    i64::from(a.version)
                });
                if m {
                    // Account-level row changed: invalidate the cached balance for it.
                    guard.account_balance.remove(&id);
                }
                m
            };
            if mutated {
                publish(notify, CacheKind::Accounts, Some(RowId::Account(id)));
                publish(notify, CacheKind::AccountBalance, Some(RowId::Account(id)));
            }
        }

        // -- Cashu receive quotes (rust: mint quote) -----------------
        WalletChange::CashuReceiveQuoteCreated(row)
        | WalletChange::CashuReceiveQuoteUpdated(row) => {
            let quote = match to_cashu_mint_quote(&row, encryption).await {
                Ok(q) => q,
                Err(e) => {
                    tracing::warn!("cache: to_cashu_mint_quote failed: {e}");
                    return;
                }
            };
            let id = quote.id;
            let mutated = {
                let mut guard = state.write();
                CacheState::upsert_versioned(&mut guard.cashu_receive_quotes, id, quote, |q| {
                    i64::from(q.version)
                })
            };
            if mutated {
                publish(notify, CacheKind::CashuReceiveQuotes, Some(RowId::Uuid(id)));
            }
        }

        // -- Cashu send quotes (rust: melt quote) --------------------
        WalletChange::CashuSendQuoteCreated(payload)
        | WalletChange::CashuSendQuoteUpdated(payload) => {
            // `to_cashu_melt_quote` does NOT take the proofs sidecar (the
            // rich `CashuMeltQuote.proofs` is hardcoded `Vec::new()` per
            // `conversions.rs` doc — preserves the existing storage
            // contract). The `cashu_proofs` sidecar on the wire is for
            // future use; we ignore it here.
            let quote = match to_cashu_melt_quote(&payload.row, encryption).await {
                Ok(q) => q,
                Err(e) => {
                    tracing::warn!("cache: to_cashu_melt_quote failed: {e}");
                    return;
                }
            };
            let id = quote.id;
            let send_swap_balance_invalidate = quote.account_id;
            let mutated = {
                let mut guard = state.write();
                let m =
                    CacheState::upsert_versioned(&mut guard.cashu_send_quotes, id, quote, |q| {
                        i64::from(q.version)
                    });
                if m {
                    // Lightning send touches proofs → balance changed.
                    guard.account_balance.remove(&send_swap_balance_invalidate);
                }
                m
            };
            if mutated {
                publish(notify, CacheKind::CashuSendQuotes, Some(RowId::Uuid(id)));
                publish(
                    notify,
                    CacheKind::AccountBalance,
                    Some(RowId::Account(send_swap_balance_invalidate)),
                );
            }
        }

        // -- Cashu receive swaps -------------------------------------
        WalletChange::CashuReceiveSwapCreated(row) | WalletChange::CashuReceiveSwapUpdated(row) => {
            let swap = match to_cashu_receive_swap(&row, encryption).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("cache: to_cashu_receive_swap failed: {e}");
                    return;
                }
            };
            let key = swap.token_hash.clone();
            let account_id = swap.account_id;
            let mutated = {
                let mut guard = state.write();
                let m = CacheState::upsert_versioned(
                    &mut guard.cashu_receive_swaps,
                    key.clone(),
                    swap,
                    |s| i64::from(s.version),
                );
                if m {
                    // Token claim adds proofs → balance changed.
                    guard.account_balance.remove(&account_id);
                }
                m
            };
            if mutated {
                publish(
                    notify,
                    CacheKind::CashuReceiveSwaps,
                    Some(RowId::TokenHash(key)),
                );
                publish(
                    notify,
                    CacheKind::AccountBalance,
                    Some(RowId::Account(account_id)),
                );
            }
        }

        // -- Cashu send swaps ----------------------------------------
        WalletChange::CashuSendSwapCreated(payload)
        | WalletChange::CashuSendSwapUpdated(payload) => {
            let swap =
                match to_cashu_send_swap(&payload.row, &payload.cashu_proofs, encryption).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("cache: to_cashu_send_swap failed: {e}");
                        return;
                    }
                };
            let id = swap.id;
            let account_id = swap.account_id;
            let mutated = {
                let mut guard = state.write();
                let m = CacheState::upsert_versioned(&mut guard.cashu_send_swaps, id, swap, |s| {
                    i64::from(s.version)
                });
                if m {
                    // Send swap moves proofs → balance changed.
                    guard.account_balance.remove(&account_id);
                }
                m
            };
            if mutated {
                publish(notify, CacheKind::CashuSendSwaps, Some(RowId::Uuid(id)));
                publish(
                    notify,
                    CacheKind::AccountBalance,
                    Some(RowId::Account(account_id)),
                );
            }
        }

        // -- Transactions --------------------------------------------
        WalletChange::TransactionCreated(row) => {
            apply_transaction(
                state, notify, row, /*previous_ack=*/ None, /*is_create=*/ true,
            );
        }
        WalletChange::TransactionUpdated(payload) => {
            // `payload.previous_acknowledgment_status` is
            // `Option<AcknowledgmentStatus>`: `None` = the previous
            // value was SQL NULL; `Some(s)` = previous was concretely
            // `s`. The trigger always emits this field on UPDATE (via
            // the `coalesce(..., 'null'::jsonb)` shape), so the cache
            // treats this as authoritative prior-state info.
            apply_transaction(
                state,
                notify,
                payload.row,
                payload.previous_acknowledgment_status,
                /*is_create=*/ false,
            );
        }

        // -- Contacts + Spark ----------------------------------------
        // Contacts: no rust consumer yet; placeholder for a future
        // contacts feature.
        // Spark: `agicash-spark` crate exists but no cache consumer
        // wired yet; deferred. Both fold into the same no-op arm —
        // separate `WalletChange` variants stay typed at the wire-format
        // seam, but the cache treats them identically for now.
        WalletChange::ContactCreated(_)
        | WalletChange::ContactDeleted(_)
        | WalletChange::SparkReceiveQuoteCreated(_)
        | WalletChange::SparkReceiveQuoteUpdated(_)
        | WalletChange::SparkSendQuoteCreated(_)
        | WalletChange::SparkSendQuoteUpdated(_) => {}

        // -- Unknown — server drift, never tear down the pump --------
        WalletChange::Unknown { event, .. } => {
            tracing::debug!("cache: ignored unknown realtime change event {event}");
        }
    }
}

/// Helper for the `Transaction*` arms. Upserts the row by `id` and
/// maintains the unacknowledged count using the
/// `previous_acknowledgment_status` side-channel field.
///
/// `previous_ack` (`Option<AcknowledgmentStatus>`):
/// - For `TransactionCreated`: ignored (passed as `None`); `is_create`
///   drives the count.
/// - For `TransactionUpdated`: `None` = previous was SQL NULL,
///   `Some(s)` = previous was concretely `s`. (The trigger always
///   emits the field, so "field absent" can't happen here.)
fn apply_transaction(
    state: &Arc<RwLock<CacheState>>,
    notify: &broadcast::Sender<CacheUpdate>,
    row: TransactionsRow,
    previous_ack: Option<AcknowledgmentStatus>,
    is_create: bool,
) {
    let id = row.id;
    let current_ack = row.acknowledgment_status.clone();
    let mutated = {
        let mut guard = state.write();
        // Mirror the version-guard discipline used for other tables.
        let m = CacheState::upsert_versioned(&mut guard.transactions, id, row, |r| {
            i64::from(r.version)
        });
        if m {
            adjust_unack_count(
                &mut guard.unacknowledged_transaction_count,
                previous_ack.clone(),
                current_ack.clone(),
                is_create,
            );
        }
        m
    };
    if mutated {
        publish(notify, CacheKind::Transactions, Some(RowId::Uuid(id)));
        publish(notify, CacheKind::UnacknowledgedTransactionCount, None);
    }
}

/// Adjust the unack count by comparing `previous` vs `current` ack
/// status. Mirrors React's logic at `transaction-hooks.ts:296-311`.
///
/// - On `TransactionCreated` (`is_create = true`): increment if
///   `current == Pending`. `previous` is ignored.
/// - On `TransactionUpdated`: increment when transitioning INTO
///   pending; decrement when transitioning OUT OF pending. `None`
///   previous means "was SQL NULL" (not "field absent" — the trigger
///   always emits the field).
pub(crate) fn adjust_unack_count(
    counter: &mut u32,
    previous: Option<AcknowledgmentStatus>,
    current: Option<AcknowledgmentStatus>,
    is_create: bool,
) {
    let is_pending =
        |s: &Option<AcknowledgmentStatus>| matches!(s, Some(AcknowledgmentStatus::Pending));
    if is_create {
        if is_pending(&current) {
            *counter = counter.saturating_add(1);
        }
        return;
    }
    let was_pending = is_pending(&previous);
    let now_pending = is_pending(&current);
    match (was_pending, now_pending) {
        (false, true) => *counter = counter.saturating_add(1),
        (true, false) => *counter = counter.saturating_sub(1),
        _ => {}
    }
}

/// Try-broadcast a tick on the notify channel. Errors are ignored —
/// `Err(SendError)` only happens when zero receivers exist, which is
/// the normal idle state of the cache.
fn publish(notify: &broadcast::Sender<CacheUpdate>, kind: CacheKind, id: Option<RowId>) {
    let _ = notify.send(CacheUpdate { kind, id });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_with_pending_increments_unack_count() {
        let mut c = 0;
        adjust_unack_count(
            &mut c,
            None,
            Some(AcknowledgmentStatus::Pending),
            /*is_create=*/ true,
        );
        assert_eq!(c, 1);
    }

    #[test]
    fn create_without_pending_leaves_unack_count() {
        let mut c = 0;
        adjust_unack_count(&mut c, None, Some(AcknowledgmentStatus::Acknowledged), true);
        assert_eq!(c, 0);
    }

    #[test]
    fn update_pending_to_acknowledged_decrements() {
        let mut c = 3;
        adjust_unack_count(
            &mut c,
            Some(AcknowledgmentStatus::Pending),
            Some(AcknowledgmentStatus::Acknowledged),
            false,
        );
        assert_eq!(c, 2);
    }

    #[test]
    fn update_null_to_pending_increments() {
        let mut c = 0;
        adjust_unack_count(&mut c, None, Some(AcknowledgmentStatus::Pending), false);
        assert_eq!(c, 1);
    }

    #[test]
    fn update_decrement_saturates_at_zero() {
        let mut c = 0;
        adjust_unack_count(
            &mut c,
            Some(AcknowledgmentStatus::Pending),
            Some(AcknowledgmentStatus::Acknowledged),
            false,
        );
        assert_eq!(c, 0);
    }

    #[test]
    fn update_pending_to_pending_leaves_counter() {
        let mut c = 4;
        adjust_unack_count(
            &mut c,
            Some(AcknowledgmentStatus::Pending),
            Some(AcknowledgmentStatus::Pending),
            false,
        );
        assert_eq!(c, 4);
    }
}
