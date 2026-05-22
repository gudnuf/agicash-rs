//! Unit tests for [`super::WalletCache`].
//!
//! ## Discipline
//!
//! Each test builds the smallest row payload that round-trips through
//! `serde_json::from_value` into the corresponding generated
//! `*Row` type, drops it inside a `WalletChange::*` variant, and feeds
//! it to [`WalletCache::apply`]. The tests assert:
//!
//! - the cache state mutates as expected,
//! - the broadcast channel ticks the right `CacheKind`,
//! - version-guard / unack-counter / unknown-event invariants hold.
//!
//! ## Encryption
//!
//! Tests use [`agicash_traits::PassthroughProofEncryption`] so the
//! `encrypted_data` JSON inside `Change` payloads is just a UTF-8 JSON
//! string the helpers can decrypt+parse directly.

use crate::cache::{CacheKind, RowId, WalletCache};
use agicash_realtime::{
    AccountWithProofs, AccountsRow, CashuProofsRow, CashuReceiveQuotesRow, CashuReceiveSwapsRow,
    CashuSendQuoteWithProofs, CashuSendQuotesRow, CashuSendSwapWithProofs, CashuSendSwapsRow,
    TransactionWithPreviousAck, TransactionsRow, WalletChange,
};
use agicash_storage_supabase::generated::enums::{
    AccountPurpose as DbAccountPurpose, AccountState as DbAccountState,
    AccountType as DbAccountType, AcknowledgmentStatus, CashuProofState, CashuReceiveQuoteState,
    CashuReceiveSwapState, CashuSendQuoteState, CashuSendSwapState, Currency as DbCurrency,
    TransactionDirection, TransactionPurpose, TransactionState, TransactionType,
};
use agicash_traits::PassthroughProofEncryption;
use base64::{engine::general_purpose, Engine};
use chrono::Utc;
use std::sync::Arc;
use uuid::Uuid;

/// Base64-encode a JSON value as a passthrough-encrypted `encrypted_data`
/// blob. Mirrors `encrypt_blob` in storage's conversions tests:
/// `serde_json::to_vec → passthrough.encrypt(=identity) → base64`.
fn encrypted(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).expect("json to_vec");
    general_purpose::STANDARD.encode(bytes)
}

fn cache() -> WalletCache {
    WalletCache::new(Arc::new(PassthroughProofEncryption))
}

fn account_row(version: i32, name: &str) -> AccountsRow {
    AccountsRow {
        id: Uuid::nil(),
        created_at: Utc::now(),
        user_id: Uuid::nil(),
        name: name.into(),
        r#type: DbAccountType::Cashu,
        purpose: DbAccountPurpose::Transactional,
        currency: DbCurrency::Btc,
        details: serde_json::json!({"mint_url": "https://mint.example"}),
        version,
        expires_at: None,
        state: DbAccountState::Active,
    }
}

fn account_with_proofs(version: i32, name: &str) -> AccountWithProofs {
    AccountWithProofs {
        row: account_row(version, name),
        proofs: Vec::new(),
    }
}

/// Base64-encoded JSON body matching `LightningReceiveData`
/// (camelCase). Passthrough encryption just hands the bytes through, so
/// the helper does `base64.decode → identity decrypt → serde parse`.
fn receive_quote_encrypted_data() -> String {
    use agicash_domain::Currency;
    use agicash_money::{Money, Unit};
    use rust_decimal::Decimal;
    let blob = serde_json::json!({
        "paymentRequest": "lnbc1...",
        "mintQuoteId": "mint-q-1",
        "amountReceived": Money::new(Decimal::from(100u64), Currency::Btc, Unit::Sat),
        "totalFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
    });
    encrypted(&blob)
}

fn receive_quote_row(id: Uuid, version: i32) -> CashuReceiveQuotesRow {
    use agicash_storage_supabase::generated::enums::ReceiveQuoteType;
    CashuReceiveQuotesRow {
        id,
        user_id: Uuid::nil(),
        account_id: Uuid::nil(),
        transaction_id: Uuid::nil(),
        keyset_id: None,
        keyset_counter: None,
        state: CashuReceiveQuoteState::Unpaid,
        version,
        created_at: Utc::now(),
        expires_at: Utc::now(),
        failure_reason: None,
        payment_hash: "deadbeef".into(),
        locking_derivation_path: String::new(),
        encrypted_data: receive_quote_encrypted_data(),
        r#type: ReceiveQuoteType::Lightning,
        quote_id_hash: "qhash".into(),
        cashu_token_melt_initiated: None,
    }
}

fn receive_swap_encrypted_data() -> String {
    use agicash_domain::Currency;
    use agicash_money::{Money, Unit};
    use rust_decimal::Decimal;
    // Matches `ReceiveData` in conversions.rs (camelCase).
    let blob = serde_json::json!({
        "tokenMintUrl": "https://mint.example",
        "tokenAmount": Money::new(Decimal::from(10u64), Currency::Btc, Unit::Sat),
        "amountReceived": Money::new(Decimal::from(10u64), Currency::Btc, Unit::Sat),
        "cashuReceiveFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        "outputAmounts": [10u64],
        "tokenProofs": [],
    });
    encrypted(&blob)
}

fn receive_swap_row(token_hash: &str, version: i32) -> CashuReceiveSwapsRow {
    CashuReceiveSwapsRow {
        token_hash: token_hash.into(),
        created_at: Utc::now(),
        account_id: Uuid::nil(),
        user_id: Uuid::nil(),
        keyset_id: "00abc".into(),
        keyset_counter: 0,
        state: CashuReceiveSwapState::Pending,
        version,
        failure_reason: None,
        transaction_id: Uuid::nil(),
        encrypted_data: receive_swap_encrypted_data(),
    }
}

fn send_quote_encrypted_data() -> String {
    use agicash_domain::Currency;
    use agicash_money::{Money, Unit};
    use rust_decimal::Decimal;
    // Matches `LightningSendData` (Unpaid; the optional Paid fields stay None).
    let blob = serde_json::json!({
        "paymentRequest": "lnbc1...",
        "meltQuoteId": "melt-q-1",
        "amountRequested": Money::new(Decimal::from(100u64), Currency::Btc, Unit::Sat),
        "amountRequestedInMsat": 100_000u64,
        "amountReceived": Money::new(Decimal::from(100u64), Currency::Btc, Unit::Sat),
        "lightningFeeReserve": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        "cashuSendFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        "amountReserved": Money::new(Decimal::from(100u64), Currency::Btc, Unit::Sat),
    });
    encrypted(&blob)
}

fn send_quote_row(id: Uuid, version: i32) -> CashuSendQuotesRow {
    CashuSendQuotesRow {
        id,
        user_id: Uuid::nil(),
        account_id: Uuid::nil(),
        transaction_id: Uuid::nil(),
        currency_requested: DbCurrency::Btc,
        keyset_id: "00abc".into(),
        keyset_counter: 0,
        number_of_change_outputs: 0,
        state: CashuSendQuoteState::Unpaid,
        version,
        created_at: Utc::now(),
        expires_at: Utc::now(),
        failure_reason: None,
        payment_hash: "deadbeef".into(),
        quote_id_hash: "qhash".into(),
        encrypted_data: send_quote_encrypted_data(),
    }
}

fn send_quote_with_proofs(id: Uuid, version: i32) -> CashuSendQuoteWithProofs {
    CashuSendQuoteWithProofs {
        row: send_quote_row(id, version),
        cashu_proofs: Vec::new(),
    }
}

fn send_swap_encrypted_data() -> String {
    use agicash_domain::Currency;
    use agicash_money::{Money, Unit};
    use rust_decimal::Decimal;
    // Matches `SendData` in conversions.rs (camelCase). OutputAmounts is
    // `OutputAmounts` from agicash_cashu — keep as None for the fixture.
    let blob = serde_json::json!({
        "tokenMintUrl": "https://mint.example",
        "amountToSend": Money::new(Decimal::from(50u64), Currency::Btc, Unit::Sat),
        "amountReceived": Money::new(Decimal::from(50u64), Currency::Btc, Unit::Sat),
        "amountReserved": Money::new(Decimal::from(50u64), Currency::Btc, Unit::Sat),
        "amountSpent": Money::new(Decimal::from(50u64), Currency::Btc, Unit::Sat),
        "cashuSendFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        "cashuReceiveFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        "totalFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
    });
    encrypted(&blob)
}

fn send_swap_row(id: Uuid, version: i32) -> CashuSendSwapsRow {
    CashuSendSwapsRow {
        id,
        user_id: Uuid::nil(),
        account_id: Uuid::nil(),
        transaction_id: Uuid::nil(),
        keyset_id: Some("00abc".into()),
        keyset_counter: Some(0),
        token_hash: None,
        state: CashuSendSwapState::Draft,
        version,
        created_at: Utc::now(),
        failure_reason: None,
        encrypted_data: send_swap_encrypted_data(),
        requires_input_proofs_swap: false,
    }
}

fn send_swap_with_proofs(id: Uuid, version: i32) -> CashuSendSwapWithProofs {
    CashuSendSwapWithProofs {
        row: send_swap_row(id, version),
        cashu_proofs: Vec::new(),
    }
}

fn transaction_row(id: Uuid, ack: Option<AcknowledgmentStatus>, version: i32) -> TransactionsRow {
    TransactionsRow {
        id,
        user_id: Uuid::nil(),
        direction: TransactionDirection::Receive,
        r#type: TransactionType::CashuLightning,
        state: TransactionState::Completed,
        account_id: Some(Uuid::nil()),
        currency: DbCurrency::Btc,
        created_at: Utc::now(),
        pending_at: None,
        completed_at: Some(Utc::now()),
        failed_at: None,
        reversed_transaction_id: None,
        reversed_at: None,
        state_sort_order: Some(0),
        encrypted_transaction_details: "ENC".into(),
        acknowledgment_status: ack,
        transaction_details: None,
        version,
        purpose: TransactionPurpose::Payment,
        account_name: "acct".into(),
        account_type: DbAccountType::Cashu,
        account_purpose: DbAccountPurpose::Transactional,
    }
}

// ===========================================================================
// 1. Account apply path
// ===========================================================================

#[tokio::test]
async fn account_created_inserts_into_cache() {
    let cache = cache();
    let mut rx = cache.subscribe_updates();
    cache
        .apply(WalletChange::AccountCreated(account_with_proofs(
            1, "Initial",
        )))
        .await;
    let snap = cache
        .account(agicash_domain::AccountId::from(Uuid::nil()))
        .expect("account present");
    assert_eq!(snap.name, "Initial");
    let tick = rx.try_recv().expect("notify tick");
    assert_eq!(tick.kind, CacheKind::Accounts);
    assert!(matches!(tick.id, Some(RowId::Account(_))));
}

#[tokio::test]
async fn account_updated_with_higher_version_overwrites() {
    let cache = cache();
    cache
        .apply(WalletChange::AccountCreated(account_with_proofs(1, "Old")))
        .await;
    cache
        .apply(WalletChange::AccountUpdated(account_with_proofs(2, "New")))
        .await;
    let snap = cache
        .account(agicash_domain::AccountId::from(Uuid::nil()))
        .expect("account present");
    assert_eq!(snap.name, "New");
}

#[tokio::test]
async fn account_updated_with_lower_version_is_ignored() {
    let cache = cache();
    cache
        .apply(WalletChange::AccountUpdated(account_with_proofs(
            5, "Newer",
        )))
        .await;
    cache
        .apply(WalletChange::AccountUpdated(account_with_proofs(
            3, "Stale",
        )))
        .await;
    let snap = cache
        .account(agicash_domain::AccountId::from(Uuid::nil()))
        .expect("account present");
    assert_eq!(snap.name, "Newer");
}

#[tokio::test]
async fn account_updated_invalidates_cached_balance() {
    let cache = cache();
    let id = agicash_domain::AccountId::from(Uuid::nil());
    cache
        .apply(WalletChange::AccountCreated(account_with_proofs(1, "A")))
        .await;
    cache.put_account_balance(id, 1234);
    assert_eq!(cache.account_balance(id), Some(1234));
    cache
        .apply(WalletChange::AccountUpdated(account_with_proofs(2, "B")))
        .await;
    assert_eq!(cache.account_balance(id), None);
}

// ===========================================================================
// 2. Cashu receive quote (mint quote) apply path
// ===========================================================================

#[tokio::test]
async fn cashu_receive_quote_created_inserts_and_ticks() {
    let cache = cache();
    let mut rx = cache.subscribe_updates();
    let id = Uuid::new_v4();
    cache
        .apply(WalletChange::CashuReceiveQuoteCreated(receive_quote_row(
            id, 1,
        )))
        .await;
    assert!(cache.cashu_receive_quote(id).is_some());
    let tick = rx.try_recv().expect("tick");
    assert_eq!(tick.kind, CacheKind::CashuReceiveQuotes);
    assert_eq!(tick.id, Some(RowId::Uuid(id)));
}

#[tokio::test]
async fn cashu_receive_quote_updated_version_guards() {
    let cache = cache();
    let id = Uuid::new_v4();
    cache
        .apply(WalletChange::CashuReceiveQuoteUpdated(receive_quote_row(
            id, 5,
        )))
        .await;
    // Stale update is dropped.
    cache
        .apply(WalletChange::CashuReceiveQuoteUpdated(receive_quote_row(
            id, 3,
        )))
        .await;
    let q = cache.cashu_receive_quote(id).expect("present");
    assert_eq!(q.version, 5);
}

// ===========================================================================
// 3. Cashu receive swap apply path (token_hash-keyed)
// ===========================================================================

#[tokio::test]
async fn cashu_receive_swap_keyed_by_token_hash() {
    let cache = cache();
    cache
        .apply(WalletChange::CashuReceiveSwapCreated(receive_swap_row(
            "tk-1", 1,
        )))
        .await;
    cache
        .apply(WalletChange::CashuReceiveSwapCreated(receive_swap_row(
            "tk-2", 1,
        )))
        .await;
    assert!(cache.cashu_receive_swap_by_token_hash("tk-1").is_some());
    assert!(cache.cashu_receive_swap_by_token_hash("tk-2").is_some());
    assert!(cache.cashu_receive_swap_by_token_hash("missing").is_none());
}

// ===========================================================================
// 4. Cashu send quote (melt quote) apply path
// ===========================================================================

#[tokio::test]
async fn cashu_send_quote_updated_inserts_and_ticks() {
    let cache = cache();
    let mut rx = cache.subscribe_updates();
    let id = Uuid::new_v4();
    cache
        .apply(WalletChange::CashuSendQuoteUpdated(send_quote_with_proofs(
            id, 1,
        )))
        .await;
    assert!(cache.cashu_send_quote(id).is_some());
    // First tick: CashuSendQuotes. Second: AccountBalance (invalidation).
    let first = rx.try_recv().expect("tick1");
    let second = rx.try_recv().expect("tick2");
    let kinds = [first.kind, second.kind];
    assert!(kinds.contains(&CacheKind::CashuSendQuotes));
    assert!(kinds.contains(&CacheKind::AccountBalance));
}

// ===========================================================================
// 5. Cashu send swap apply path
// ===========================================================================

#[tokio::test]
async fn cashu_send_swap_created_inserts() {
    let cache = cache();
    let id = Uuid::new_v4();
    cache
        .apply(WalletChange::CashuSendSwapCreated(send_swap_with_proofs(
            id, 1,
        )))
        .await;
    assert!(cache.cashu_send_swap(id).is_some());
}

#[tokio::test]
async fn cashu_send_swap_invalidates_account_balance() {
    let cache = cache();
    let id = Uuid::new_v4();
    let account_id = agicash_domain::AccountId::from(Uuid::nil());
    cache.put_account_balance(account_id, 999);
    cache
        .apply(WalletChange::CashuSendSwapCreated(send_swap_with_proofs(
            id, 1,
        )))
        .await;
    assert_eq!(cache.account_balance(account_id), None);
}

// ===========================================================================
// 6. Transaction apply path + unack count
// ===========================================================================

#[tokio::test]
async fn transaction_created_pending_increments_unack() {
    let cache = cache();
    assert_eq!(cache.unacknowledged_transaction_count(), 0);
    cache
        .apply(WalletChange::TransactionCreated(transaction_row(
            Uuid::new_v4(),
            Some(AcknowledgmentStatus::Pending),
            1,
        )))
        .await;
    assert_eq!(cache.unacknowledged_transaction_count(), 1);
}

#[tokio::test]
async fn transaction_updated_pending_to_acknowledged_decrements() {
    let cache = cache();
    let id = Uuid::new_v4();
    cache
        .apply(WalletChange::TransactionCreated(transaction_row(
            id,
            Some(AcknowledgmentStatus::Pending),
            1,
        )))
        .await;
    assert_eq!(cache.unacknowledged_transaction_count(), 1);
    cache
        .apply(WalletChange::TransactionUpdated(
            TransactionWithPreviousAck {
                row: transaction_row(id, Some(AcknowledgmentStatus::Acknowledged), 2),
                previous_acknowledgment_status: Some(AcknowledgmentStatus::Pending),
            },
        ))
        .await;
    assert_eq!(cache.unacknowledged_transaction_count(), 0);
}

#[tokio::test]
async fn transaction_updated_null_to_pending_increments_unack() {
    let cache = cache();
    let id = Uuid::new_v4();
    // First a non-pending TransactionCreated; counter stays 0.
    cache
        .apply(WalletChange::TransactionCreated(transaction_row(
            id,
            Some(AcknowledgmentStatus::Acknowledged),
            1,
        )))
        .await;
    assert_eq!(cache.unacknowledged_transaction_count(), 0);
    // Then update with previous_ack == None (SQL NULL) → Pending: increment.
    cache
        .apply(WalletChange::TransactionUpdated(
            TransactionWithPreviousAck {
                row: transaction_row(id, Some(AcknowledgmentStatus::Pending), 2),
                previous_acknowledgment_status: None,
            },
        ))
        .await;
    assert_eq!(cache.unacknowledged_transaction_count(), 1);
}

#[tokio::test]
async fn transaction_updated_no_change_to_unack_status_leaves_counter() {
    let cache = cache();
    let id = Uuid::new_v4();
    cache
        .apply(WalletChange::TransactionCreated(transaction_row(
            id,
            Some(AcknowledgmentStatus::Pending),
            1,
        )))
        .await;
    // Now bump version but keep ack == Pending. Counter must stay 1.
    cache
        .apply(WalletChange::TransactionUpdated(
            TransactionWithPreviousAck {
                row: transaction_row(id, Some(AcknowledgmentStatus::Pending), 2),
                previous_acknowledgment_status: Some(AcknowledgmentStatus::Pending),
            },
        ))
        .await;
    assert_eq!(cache.unacknowledged_transaction_count(), 1);
}

// ===========================================================================
// 7. Unknown / forward-compat
// ===========================================================================

#[tokio::test]
async fn unknown_change_does_not_panic_or_mutate() {
    let cache = cache();
    let mut rx = cache.subscribe_updates();
    cache
        .apply(WalletChange::Unknown {
            event: "FUTURE_EVENT".into(),
            payload_json: r#"{"foo":"bar"}"#.into(),
        })
        .await;
    assert!(cache.accounts_snapshot().is_none()); // never populated
    assert!(rx.try_recv().is_err()); // no tick fired
}

#[tokio::test]
async fn contact_events_are_ignored_quietly() {
    let cache = cache();
    let contact_row = agicash_realtime::ContactsRow {
        id: Uuid::nil(),
        owner_id: Uuid::nil(),
        created_at: Utc::now(),
        username: Some("alice".into()),
    };
    cache
        .apply(WalletChange::ContactCreated(contact_row.clone()))
        .await;
    cache.apply(WalletChange::ContactDeleted(contact_row)).await;
    // No assertion needed beyond not-panicking.
    assert!(cache.accounts_snapshot().is_none());
}

// ===========================================================================
// 8. Subscribe / observer surface
// ===========================================================================

#[tokio::test]
async fn subscribe_updates_receives_apply_tick() {
    let cache = cache();
    let mut rx = cache.subscribe_updates();
    cache
        .apply(WalletChange::AccountCreated(account_with_proofs(1, "Acct")))
        .await;
    let tick = rx.recv().await.expect("tick");
    assert_eq!(tick.kind, CacheKind::Accounts);
}

#[tokio::test]
async fn multiple_subscribers_each_observe() {
    let cache = cache();
    let mut rx1 = cache.subscribe_updates();
    let mut rx2 = cache.subscribe_updates();
    cache
        .apply(WalletChange::AccountCreated(account_with_proofs(1, "Acct")))
        .await;
    assert_eq!(
        rx1.recv().await.expect("rx1 tick").kind,
        CacheKind::Accounts
    );
    assert_eq!(
        rx2.recv().await.expect("rx2 tick").kind,
        CacheKind::Accounts
    );
}

// ===========================================================================
// 9. Snapshot semantics — populated flag distinguishes empty from unset
// ===========================================================================

#[tokio::test]
async fn accounts_snapshot_is_none_before_populate() {
    let cache = cache();
    assert!(cache.accounts_snapshot().is_none());
    // Even after a Change arrives, snapshot stays None because
    // populated flag is only set by populate_accounts() — Change events
    // patch rows but don't set the populated flag, mirroring React
    // Query's distinction between "applied a mutation" and "ran the
    // queryFn at least once".
    cache
        .apply(WalletChange::AccountCreated(account_with_proofs(1, "A")))
        .await;
    assert!(cache.accounts_snapshot().is_none());
    // But `account()` (per-id lookup) sees the row.
    assert!(cache
        .account(agicash_domain::AccountId::from(Uuid::nil()))
        .is_some());
}

#[tokio::test]
async fn put_account_balance_emits_tick() {
    let cache = cache();
    let mut rx = cache.subscribe_updates();
    let id = agicash_domain::AccountId::from(Uuid::nil());
    cache.put_account_balance(id, 42);
    let tick = rx.try_recv().expect("balance tick");
    assert_eq!(tick.kind, CacheKind::AccountBalance);
    assert_eq!(tick.id, Some(RowId::Account(id)));
}

// Marker to silence unused-import warnings for the proof types — they
// are imported in case follow-up tests need to populate proofs sidecars.
#[allow(dead_code)]
fn _touch_unused_imports() {
    let _ = std::marker::PhantomData::<(CashuProofsRow, CashuProofState)>;
}
