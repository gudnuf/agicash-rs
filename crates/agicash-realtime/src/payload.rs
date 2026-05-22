//! Typed deserialization of the wallet broadcast payloads.
//!
//! ## Why
//!
//! Until now `client::serve_step` decoded the Phoenix `broadcast` envelope
//! into `WalletEvent { event: String, payload_json: String }` and forwarded
//! the raw row JSON to consumers as an opaque string. That worked for the
//! existing "something changed, refetch the affected table" subscribers
//! (FFI bridge, Leptos pump, driver), but it leaves the typed row data the
//! server already shipped sitting unused on the floor — the next layer up
//! (the realtime-driven cache, currently in design) wants to apply the
//! row delta directly instead of issuing a refetch.
//!
//! This module is that next layer's seam: it parses each known broadcast
//! `event` name (e.g. `ACCOUNT_UPDATED`, `CASHU_SEND_SWAP_UPDATED`) into a
//! [`WalletChange`] enum carrying the typed row. The string-shaped
//! [`crate::WalletEvent`] keeps flowing in parallel so the existing
//! subscribers don't have to change behavior.
//!
//! ## Wire shape
//!
//! Triggers in `supabase/migrations/20260112150000_initial_db.sql`
//! (§REALTIME BROADCAST CONFIGURATION) publish via `realtime.send(payload,
//! event_name, topic, is_private => true)`. The Phoenix client decodes the
//! frame envelope (`codec::decode_text`) into the inner `payload` JSON;
//! that inner JSON is what [`parse_change`] takes here.
//!
//! Per-trigger payload shapes (all observed in the initial migration):
//!
//! | Event                              | Payload shape (the inner `payload`)                      |
//! |------------------------------------|----------------------------------------------------------|
//! | `ACCOUNT_CREATED` / `ACCOUNT_UPDATED`           | account row + `proofs` array (sidecar)      |
//! | `TRANSACTION_CREATED`              | transaction row                                          |
//! | `TRANSACTION_UPDATED`              | transaction row + `previous_acknowledgment_status` field |
//! | `CONTACT_CREATED` / `CONTACT_DELETED`           | contact row                                 |
//! | `CASHU_RECEIVE_QUOTE_CREATED` / `_UPDATED`      | `cashu_receive_quotes` row                    |
//! | `CASHU_RECEIVE_SWAP_CREATED` / `_UPDATED`       | `cashu_receive_swaps` row                     |
//! | `CASHU_SEND_QUOTE_CREATED` / `_UPDATED`         | `cashu_send_quotes` row + `cashu_proofs` array |
//! | `CASHU_SEND_SWAP_CREATED` / `_UPDATED`          | `cashu_send_swaps` row + `cashu_proofs` array  |
//! | `SPARK_RECEIVE_QUOTE_CREATED` / `_UPDATED`      | `spark_receive_quotes` row                    |
//! | `SPARK_SEND_QUOTE_CREATED` / `_UPDATED`         | `spark_send_quotes` row                       |
//!
//! No payload carries the `old` row except the `previous_acknowledgment_status`
//! delta on `TRANSACTION_UPDATED` — Postgres triggers were never wired to
//! emit pre-images, only the post-image (the React app's `useTrackWallet*`
//! handlers all consume single-row payloads, and the rust client mirrors
//! that contract).
//!
//! ## Naming
//!
//! Variants follow the DB / React names verbatim
//! (`CashuReceiveQuoteUpdated`, NOT `MintQuoteUpdated`); see
//! `~/athanor/projects/agicash-rust/smells.md` smell S9 (the broader
//! `mint_quote` / `melt_quote` rename is a separate lane that this work
//! deliberately does not touch).
//!
//! ## Failure mode
//!
//! [`parse_change`] is total: it never returns `Err`. An unrecognized
//! event name OR a parse failure on a known event name both collapse to
//! [`WalletChange::Unknown`], carrying the original event name + raw
//! JSON. Rationale: the existing string-shaped `WalletEvent` is the
//! load-bearing path today; the new typed sibling is an optional richer
//! channel that the cache layer subscribes to. If a server-side payload
//! shape ever drifts (e.g. a new column the codegen hasn't picked up
//! yet), we must NOT tear down the realtime pump — drop into `Unknown`
//! and let the string-shaped consumers carry on.

use agicash_storage_supabase::generated::tables;
use serde::{Deserialize, Serialize};

// Re-export the row types so callers don't need a second `use`.
pub use agicash_storage_supabase::generated::enums::AcknowledgmentStatus;
pub use tables::accounts::AccountsRow;
pub use tables::cashu_proofs::CashuProofsRow;
pub use tables::cashu_receive_quotes::CashuReceiveQuotesRow;
pub use tables::cashu_receive_swaps::CashuReceiveSwapsRow;
pub use tables::cashu_send_quotes::CashuSendQuotesRow;
pub use tables::cashu_send_swaps::CashuSendSwapsRow;
pub use tables::contacts::ContactsRow;
pub use tables::spark_receive_quotes::SparkReceiveQuotesRow;
pub use tables::spark_send_quotes::SparkSendQuotesRow;
pub use tables::transactions::TransactionsRow;

/// Accounts broadcast payload: a row from `wallet.accounts` plus the
/// proofs sidecar the trigger appends via `wallet.to_account_with_proofs`.
///
/// Matches the React app's `AgicashDbAccountWithProofs`
/// (`app/features/agicash-db/database.ts`). The `proofs` array is the
/// account's currently-unspent proofs; INSERT carries an empty array,
/// UPDATE re-emits whatever the trigger snapshotted at the time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountWithProofs {
    #[serde(flatten)]
    pub row: AccountsRow,
    /// Proofs sidecar — empty array for a fresh account; may be empty
    /// after an update too (e.g. all proofs just got spent).
    #[serde(default)]
    pub proofs: Vec<CashuProofsRow>,
}

/// `cashu_send_quotes` broadcast payload: the row plus the
/// `cashu_proofs` array the trigger gathers via
/// `where cp.spending_cashu_send_quote_id = new.id`.
///
/// Empty array means "no proofs are currently bound to this send quote"
/// (e.g. INSERT before the swap funds it, or post-COMPLETED).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CashuSendQuoteWithProofs {
    #[serde(flatten)]
    pub row: CashuSendQuotesRow,
    #[serde(default)]
    pub cashu_proofs: Vec<CashuProofsRow>,
}

/// `cashu_send_swaps` broadcast payload: the row plus the
/// `cashu_proofs` array the trigger gathers via
/// `where cp.spending_cashu_send_swap_id = new.id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CashuSendSwapWithProofs {
    #[serde(flatten)]
    pub row: CashuSendSwapsRow,
    #[serde(default)]
    pub cashu_proofs: Vec<CashuProofsRow>,
}

/// `transactions` UPDATE broadcast payload: the row plus the
/// `previous_acknowledgment_status` side-channel field the trigger sets
/// via `jsonb_set(to_jsonb(new), '{previous_acknowledgment_status}', ...)`.
///
/// This is the ONE place any trigger publishes an old-row fragment —
/// used by the React app
/// (`app/features/transactions/transaction-hooks.ts:296-311`) to invalidate
/// the unacknowledged-count badge only when the ack status actually
/// transitioned. The field is `Option<Option<...>>` so it can serialize
/// JSON `null` (the old value was explicitly null) distinctly from
/// "field absent" (no previous value coalesced through).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionWithPreviousAck {
    #[serde(flatten)]
    pub row: TransactionsRow,
    /// Old `acknowledgment_status` coalesced through the trigger's
    /// `coalesce(to_jsonb(old.acknowledgment_status), 'null'::jsonb)`.
    /// Always present on UPDATE; modeled `Option<...>` because the
    /// underlying column itself is nullable.
    #[serde(default)]
    pub previous_acknowledgment_status: Option<AcknowledgmentStatus>,
}

/// Typed deserialization of one wallet broadcast.
///
/// Variants are named after the DB tables (`CashuReceiveQuote*`, not
/// `MintQuote*`), matching the trigger event names and the React app
/// hooks (`useCashuReceiveQuoteChangeHandlers`, etc.). The broader
/// domain-name → DB-name rename is a separate lane (smell S9); this
/// crate is staying on the DB-name side of the wall.
///
/// `Unknown` is the forward-compat catch-all: a future trigger / event
/// name we don't recognize, OR a known event whose payload failed to
/// deserialize (e.g. codegen lag after a column add). The string-shaped
/// `WalletEvent` always fires in parallel, so consumers that only want
/// "something changed, refetch" don't need to handle `Unknown` at all.
#[derive(Debug, Clone, PartialEq)]
pub enum WalletChange {
    AccountCreated(AccountWithProofs),
    AccountUpdated(AccountWithProofs),
    TransactionCreated(TransactionsRow),
    TransactionUpdated(TransactionWithPreviousAck),
    ContactCreated(ContactsRow),
    ContactDeleted(ContactsRow),
    CashuReceiveQuoteCreated(CashuReceiveQuotesRow),
    CashuReceiveQuoteUpdated(CashuReceiveQuotesRow),
    CashuReceiveSwapCreated(CashuReceiveSwapsRow),
    CashuReceiveSwapUpdated(CashuReceiveSwapsRow),
    CashuSendQuoteCreated(CashuSendQuoteWithProofs),
    CashuSendQuoteUpdated(CashuSendQuoteWithProofs),
    CashuSendSwapCreated(CashuSendSwapWithProofs),
    CashuSendSwapUpdated(CashuSendSwapWithProofs),
    SparkReceiveQuoteCreated(SparkReceiveQuotesRow),
    SparkReceiveQuoteUpdated(SparkReceiveQuotesRow),
    SparkSendQuoteCreated(SparkSendQuotesRow),
    SparkSendQuoteUpdated(SparkSendQuotesRow),
    /// Unrecognized event name or a known event whose row payload
    /// failed to deserialize. Carries the original event name + raw
    /// payload JSON so a consumer that wants to log it / fall through
    /// to the string-shaped surface still has the fidelity.
    Unknown {
        event: String,
        payload_json: String,
    },
}

/// Parse one broadcast `(event_name, payload_json)` pair into a typed
/// [`WalletChange`]. Never errors: an unrecognized event name OR a
/// failed serde decode of a known event both collapse to
/// [`WalletChange::Unknown`].
#[must_use]
pub fn parse_change(event: &str, payload_json: &str) -> WalletChange {
    /// Tiny helper that either deserializes the payload into `T` and
    /// hands it to `ok`, or returns `Unknown` carrying the original
    /// event + json. Keeps `parse_change` a flat match.
    fn decode<T, F>(event: &str, payload_json: &str, ok: F) -> WalletChange
    where
        T: for<'de> Deserialize<'de>,
        F: FnOnce(T) -> WalletChange,
    {
        match serde_json::from_str::<T>(payload_json) {
            Ok(v) => ok(v),
            Err(_) => WalletChange::Unknown {
                event: event.to_string(),
                payload_json: payload_json.to_string(),
            },
        }
    }

    match event {
        "ACCOUNT_CREATED" => decode(event, payload_json, WalletChange::AccountCreated),
        "ACCOUNT_UPDATED" => decode(event, payload_json, WalletChange::AccountUpdated),
        "TRANSACTION_CREATED" => decode(event, payload_json, WalletChange::TransactionCreated),
        "TRANSACTION_UPDATED" => decode(event, payload_json, WalletChange::TransactionUpdated),
        "CONTACT_CREATED" => decode(event, payload_json, WalletChange::ContactCreated),
        "CONTACT_DELETED" => decode(event, payload_json, WalletChange::ContactDeleted),
        "CASHU_RECEIVE_QUOTE_CREATED" => {
            decode(event, payload_json, WalletChange::CashuReceiveQuoteCreated)
        }
        "CASHU_RECEIVE_QUOTE_UPDATED" => {
            decode(event, payload_json, WalletChange::CashuReceiveQuoteUpdated)
        }
        "CASHU_RECEIVE_SWAP_CREATED" => {
            decode(event, payload_json, WalletChange::CashuReceiveSwapCreated)
        }
        "CASHU_RECEIVE_SWAP_UPDATED" => {
            decode(event, payload_json, WalletChange::CashuReceiveSwapUpdated)
        }
        "CASHU_SEND_QUOTE_CREATED" => {
            decode(event, payload_json, WalletChange::CashuSendQuoteCreated)
        }
        "CASHU_SEND_QUOTE_UPDATED" => {
            decode(event, payload_json, WalletChange::CashuSendQuoteUpdated)
        }
        "CASHU_SEND_SWAP_CREATED" => {
            decode(event, payload_json, WalletChange::CashuSendSwapCreated)
        }
        "CASHU_SEND_SWAP_UPDATED" => {
            decode(event, payload_json, WalletChange::CashuSendSwapUpdated)
        }
        "SPARK_RECEIVE_QUOTE_CREATED" => {
            decode(event, payload_json, WalletChange::SparkReceiveQuoteCreated)
        }
        "SPARK_RECEIVE_QUOTE_UPDATED" => {
            decode(event, payload_json, WalletChange::SparkReceiveQuoteUpdated)
        }
        "SPARK_SEND_QUOTE_CREATED" => {
            decode(event, payload_json, WalletChange::SparkSendQuoteCreated)
        }
        "SPARK_SEND_QUOTE_UPDATED" => {
            decode(event, payload_json, WalletChange::SparkSendQuoteUpdated)
        }
        _ => WalletChange::Unknown {
            event: event.to_string(),
            payload_json: payload_json.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uuid_str() -> &'static str {
        "00000000-0000-0000-0000-000000000001"
    }

    fn ts_str() -> &'static str {
        "2026-05-21T00:00:00+00:00"
    }

    /// One INSERT broadcast: typed account + empty proofs sidecar.
    #[test]
    fn account_created_decodes_row_and_empty_proofs() {
        let payload = json!({
            "id": uuid_str(),
            "created_at": ts_str(),
            "user_id": uuid_str(),
            "name": "wire-confirm-22e",
            "type": "cashu",
            "purpose": "transactional",
            "currency": "BTC",
            "details": {"keyset_counters":{}, "mint_url":"https://testnut.cashu.space"},
            "version": 1,
            "expires_at": null,
            "state": "active",
            "proofs": []
        });
        let WalletChange::AccountCreated(p) = parse_change("ACCOUNT_CREATED", &payload.to_string())
        else {
            panic!("expected AccountCreated");
        };
        assert_eq!(p.row.name, "wire-confirm-22e");
        assert!(p.proofs.is_empty());
    }

    /// Account UPDATE carries proofs.
    #[test]
    fn account_updated_decodes_proofs_sidecar() {
        let payload = json!({
            "id": uuid_str(),
            "created_at": ts_str(),
            "user_id": uuid_str(),
            "name": "acct",
            "type": "cashu",
            "purpose": "transactional",
            "currency": "BTC",
            "details": {},
            "version": 2,
            "expires_at": null,
            "state": "active",
            "proofs": [{
                "id": uuid_str(),
                "user_id": uuid_str(),
                "account_id": uuid_str(),
                "keyset_id": "00abc",
                "amount": "8",
                "secret": "deadbeef",
                "unblinded_signature": "ff",
                "public_key_y": "ee",
                "dleq": null,
                "witness": null,
                "state": "UNSPENT",
                "version": 1,
                "created_at": ts_str(),
                "reserved_at": null,
                "spent_at": null,
                "cashu_receive_quote_id": null,
                "cashu_receive_swap_token_hash": null,
                "cashu_send_quote_id": null,
                "spending_cashu_send_quote_id": null,
                "cashu_send_swap_id": null,
                "spending_cashu_send_swap_id": null
            }]
        });
        let WalletChange::AccountUpdated(p) = parse_change("ACCOUNT_UPDATED", &payload.to_string())
        else {
            panic!("expected AccountUpdated");
        };
        assert_eq!(p.proofs.len(), 1);
        assert_eq!(p.proofs[0].amount, "8");
    }

    /// `TRANSACTION_UPDATED` carries the previous-ack side field.
    #[test]
    fn transaction_updated_carries_previous_ack() {
        let payload = json!({
            "id": uuid_str(),
            "user_id": uuid_str(),
            "direction": "RECEIVE",
            "type": "CASHU_LIGHTNING",
            "state": "COMPLETED",
            "account_id": uuid_str(),
            "currency": "BTC",
            "created_at": ts_str(),
            "pending_at": null,
            "completed_at": ts_str(),
            "failed_at": null,
            "reversed_transaction_id": null,
            "reversed_at": null,
            "state_sort_order": 0,
            "encrypted_transaction_details": "ENC",
            "acknowledgment_status": "acknowledged",
            "transaction_details": null,
            "version": 3,
            "purpose": "PAYMENT",
            "account_name": "acct",
            "account_type": "cashu",
            "account_purpose": "transactional",
            "previous_acknowledgment_status": "pending"
        });
        let WalletChange::TransactionUpdated(p) =
            parse_change("TRANSACTION_UPDATED", &payload.to_string())
        else {
            panic!("expected TransactionUpdated");
        };
        // Both the row and the side-channel field decode.
        assert_eq!(p.row.encrypted_transaction_details, "ENC");
        assert_eq!(
            p.previous_acknowledgment_status,
            Some(AcknowledgmentStatus::Pending)
        );
    }

    /// `TRANSACTION_UPDATED` previous ack can be null (the trigger's
    /// `coalesce(to_jsonb(old.acknowledgment_status), 'null'::jsonb)`).
    #[test]
    fn transaction_updated_previous_ack_null() {
        let payload = json!({
            "id": uuid_str(),
            "user_id": uuid_str(),
            "direction": "RECEIVE",
            "type": "CASHU_LIGHTNING",
            "state": "PENDING",
            "account_id": uuid_str(),
            "currency": "BTC",
            "created_at": ts_str(),
            "pending_at": null,
            "completed_at": null,
            "failed_at": null,
            "reversed_transaction_id": null,
            "reversed_at": null,
            "state_sort_order": 0,
            "encrypted_transaction_details": "ENC",
            "acknowledgment_status": null,
            "transaction_details": null,
            "version": 1,
            "purpose": "PAYMENT",
            "account_name": "acct",
            "account_type": "cashu",
            "account_purpose": "transactional",
            "previous_acknowledgment_status": null
        });
        let WalletChange::TransactionUpdated(p) =
            parse_change("TRANSACTION_UPDATED", &payload.to_string())
        else {
            panic!("expected TransactionUpdated");
        };
        assert_eq!(p.previous_acknowledgment_status, None);
    }

    /// `cashu_send_swaps` broadcast carries the proofs sidecar.
    #[test]
    fn cashu_send_swap_updated_carries_cashu_proofs() {
        let payload = json!({
            "id": uuid_str(),
            "user_id": uuid_str(),
            "account_id": uuid_str(),
            "transaction_id": uuid_str(),
            "keyset_id": "00abc",
            "keyset_counter": 0,
            "token_hash": null,
            "state": "DRAFT",
            "version": 1,
            "created_at": ts_str(),
            "failure_reason": null,
            "encrypted_data": "ENC",
            "requires_input_proofs_swap": false,
            "cashu_proofs": []
        });
        let WalletChange::CashuSendSwapUpdated(p) =
            parse_change("CASHU_SEND_SWAP_UPDATED", &payload.to_string())
        else {
            panic!("expected CashuSendSwapUpdated");
        };
        assert_eq!(p.row.encrypted_data, "ENC");
        assert!(p.cashu_proofs.is_empty());
    }

    /// Plain receive-swap row, no sidecars.
    #[test]
    fn cashu_receive_swap_updated_decodes_plain_row() {
        let payload = json!({
            "token_hash": "abcd",
            "created_at": ts_str(),
            "account_id": uuid_str(),
            "user_id": uuid_str(),
            "keyset_id": "00abc",
            "keyset_counter": 1,
            "state": "PENDING",
            "version": 2,
            "failure_reason": null,
            "transaction_id": uuid_str(),
            "encrypted_data": "ENC"
        });
        let WalletChange::CashuReceiveSwapUpdated(r) =
            parse_change("CASHU_RECEIVE_SWAP_UPDATED", &payload.to_string())
        else {
            panic!("expected CashuReceiveSwapUpdated");
        };
        assert_eq!(r.token_hash, "abcd");
    }

    /// Unknown event name → typed-`Unknown` carrying both fields verbatim.
    #[test]
    fn unknown_event_falls_through_to_unknown() {
        let got = parse_change("FUTURE_EVENT", r#"{"foo":"bar"}"#);
        assert_eq!(
            got,
            WalletChange::Unknown {
                event: "FUTURE_EVENT".into(),
                payload_json: r#"{"foo":"bar"}"#.into()
            }
        );
    }

    /// Known event with bad/incomplete payload → Unknown (NOT a panic / Err).
    /// This is the forward-compat guard: a codegen lag (a new column added
    /// upstream we haven't picked up yet) must not crash the realtime pump.
    #[test]
    fn known_event_with_invalid_payload_falls_through_to_unknown() {
        // Missing every required field on AccountsRow.
        let got = parse_change("ACCOUNT_UPDATED", r#"{"not_an_account":true}"#);
        match got {
            WalletChange::Unknown {
                event,
                payload_json,
            } => {
                assert_eq!(event, "ACCOUNT_UPDATED");
                assert_eq!(payload_json, r#"{"not_an_account":true}"#);
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// `CONTACT_DELETED` takes the old row (the only DELETE trigger in
    /// the system); decodes the same `ContactsRow` shape.
    #[test]
    fn contact_deleted_decodes_row() {
        let payload = json!({
            "id": uuid_str(),
            "created_at": ts_str(),
            "owner_id": uuid_str(),
            "username": "alice"
        });
        let WalletChange::ContactDeleted(r) = parse_change("CONTACT_DELETED", &payload.to_string())
        else {
            panic!("expected ContactDeleted");
        };
        assert_eq!(r.username.as_deref(), Some("alice"));
    }

    /// Spark variants are plain rows (no sidecars in the trigger).
    #[test]
    fn spark_send_quote_created_decodes_row() {
        let payload = json!({
            "id": uuid_str(),
            "state": "PENDING",
            "created_at": ts_str(),
            "payment_hash": "ph",
            "spark_id": null,
            "spark_transfer_id": null,
            "failure_reason": null,
            "user_id": uuid_str(),
            "account_id": uuid_str(),
            "transaction_id": uuid_str(),
            "version": 1,
            "payment_request_is_amountless": false,
            "expires_at": null,
            "encrypted_data": "ENC"
        });
        let WalletChange::SparkSendQuoteCreated(r) =
            parse_change("SPARK_SEND_QUOTE_CREATED", &payload.to_string())
        else {
            panic!("expected SparkSendQuoteCreated");
        };
        assert_eq!(r.payment_hash, "ph");
    }
}
