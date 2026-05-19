//! Result-record types returned by `WalletClient` methods.
//!
//! Plain serde-`Serialize`/`Deserialize` records — no enums on the
//! consumer-facing structs, so the same types round-trip cleanly across
//! UniFFI (`#[uniffi::Record]`), wasm-bindgen (`serde-wasm-bindgen`), and
//! MCP tool outputs (JSON).
//!
//! The shape borrows from the existing FFI `ReceiveResult` (see
//! `crates/agicash-ffi/src/receive.rs`) so iOS Swift code can adopt the
//! facade without changing call-site assumptions.

use agicash_domain::{Account, AccountId, AccountType, Currency, UserId};
use agicash_money::Money;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `cashuA…` / `cashuB…` encoding versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenVersion {
    V3,
    V4,
}

/// Auth state — does the wallet have a loaded session?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub user_id: Option<UserId>,
}

/// One row from `wallet.accounts` plus the computed balance for it.
///
/// Balance is denominated in the account's minor unit (`sat` for BTC,
/// `cent` for USD/USDB) — represented as a string to side-step JS float
/// rounding when the record crosses an FFI/wasm boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSummary {
    pub id: AccountId,
    pub user_id: UserId,
    pub name: String,
    pub account_type: AccountType,
    pub currency: Currency,
    pub mint_url: Option<String>,
    /// Decimal-encoded amount in the account's minor unit.
    pub balance: String,
}

impl AccountSummary {
    /// Build a summary from a raw `Account` row + a pre-computed balance
    /// in the account's minor unit.
    #[must_use]
    pub fn from_account(account: &Account, balance_minor: u64) -> Self {
        let mint_url = account
            .details
            .get("mint_url")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Self {
            id: account.id,
            user_id: account.user_id,
            name: account.name.clone(),
            account_type: account.account_type,
            currency: account.currency,
            mint_url,
            balance: balance_minor.to_string(),
        }
    }
}

/// Aggregate balance across all of a user's accounts.
///
/// `total_per_currency` is keyed by currency code (`"BTC"` / `"USD"`)
/// because some consumers (the iOS home view, Leptos PWA balance hero)
/// want both displayed; the map is empty for users with no accounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BalanceSummary {
    /// Sum of all accounts' balances, grouped by currency. Values are
    /// minor-unit-decimal strings.
    pub total_per_currency: std::collections::BTreeMap<String, String>,
    /// Per-account breakdown for callers that want to render a list.
    pub per_account: Vec<AccountSummary>,
}

/// One discovered mint, grouped from the account list.
///
/// `accounts` collects every Cashu account pointing at this mint URL,
/// one per currency the user has provisioned against it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MintSummary {
    pub mint_url: String,
    pub mint_name: String,
    pub accounts: Vec<AccountSummary>,
}

/// Dry-run preview of a Cashu token-out (`send_token`).
///
/// Mirrors `SendQuote` from `agicash-cashu`, flattened to facade types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendTokenQuote {
    pub amount_requested: Money,
    pub amount_to_send: Money,
    pub total_amount: Money,
    pub total_fee: Money,
    pub cashu_send_fee: Money,
    pub cashu_receive_fee: Money,
    pub account_id: AccountId,
}

/// Receipt for a completed Cashu token-out (`send_token`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendTokenReceipt {
    /// The encoded `cashuA…` or `cashuB…` token string.
    pub token: String,
    pub amount: Money,
    pub fee: Money,
    pub account_id: AccountId,
    pub mint_url: String,
    /// Local swap row id (UUID).
    pub swap_id: Uuid,
    /// SHA-256 of the wire-form token string.
    pub token_hash: String,
}

/// Discriminator carried on receipt records that span both happy paths and
/// idempotent re-runs. Matches the FFI's `ReceiveStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveStatus {
    /// First-time receive — proofs minted, balance credited.
    Received,
    /// Token was already redeemed by this wallet on an earlier call.
    AlreadyClaimed,
    /// Previous attempt failed terminally; no action taken.
    AlreadyFailed,
    /// Swap exists in PENDING state.
    Pending,
}

/// Receipt for a `receive_token` or `complete_receive_lightning` call.
///
/// Shared shape so iOS / PWA UI can render either flow uniformly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiveReceipt {
    pub status: ReceiveStatus,
    pub amount: Money,
    pub fee: Money,
    pub account_id: AccountId,
    pub mint_url: String,
    /// For Cashu-token receives, SHA-256 of the wire-form token.
    /// For Lightning receives, the BOLT-11 payment hash.
    pub token_hash: String,
}

/// Lightning-send preview (NUT-05 melt-quote without commit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendLightningQuote {
    pub bolt11: String,
    pub amount: Money,
    pub lightning_fee_reserve: Money,
    pub cashu_fee: Money,
    pub total_fee: Money,
    pub total_amount: Money,
    pub payment_hash: String,
    pub expires_at: DateTime<Utc>,
    pub account_id: AccountId,
}

/// Handle for an in-flight Lightning send.
///
/// The reconcile-aware send surface ([`crate::WalletClient::begin_send_lightning`]
/// / [`crate::WalletClient::poll_send_lightning`]) returns
/// [`SendLightningStatus`] carrying the `quote_id` directly (P0-1); this
/// struct is retained for the stable public type surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendLightningHandle {
    /// Wallet-side UUID of the persisted quote row.
    pub quote_id: Uuid,
    pub bolt11: String,
    pub amount: Money,
    pub total_fee: Money,
    pub account_id: AccountId,
    pub payment_hash: String,
    pub expires_at: DateTime<Utc>,
}

/// Receipt for a completed Lightning send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendLightningReceipt {
    pub quote_id: Uuid,
    pub amount: Money,
    pub lightning_fee: Money,
    pub cashu_fee: Money,
    pub total_fee: Money,
    pub amount_spent: Money,
    pub payment_preimage: String,
    pub payment_hash: String,
    pub account_id: AccountId,
}

/// Reconcile-aware outcome of [`crate::WalletClient::begin_send_lightning`]
/// and [`crate::WalletClient::poll_send_lightning`] (slice 12b-2 P0-1).
///
/// Replaces the old `complete_send_lightning` which bundled a 30s poll
/// loop and returned `Err(WalletError::Cashu("still pending"))` on
/// timeout — an ambiguous error a consumer could treat as "failed" and
/// re-quote, firing a second `post_melt` for an in-flight invoice
/// (double-pay). Every still-pending melt is the typed, **non-error**
/// [`SendLightningStatus::InFlight`]; the consumer drives
/// [`crate::WalletClient::poll_send_lightning`] on its own cadence
/// (mirrors the proven FFI `poll_melt_quote` single-shot contract +
/// the iOS `paying`/`verifying` reconcile loop).
#[derive(Debug, Clone)]
pub enum SendLightningStatus {
    /// Mint settled the melt; proofs spent + change persisted
    /// (terminal). Carries the full receipt.
    Paid(SendLightningReceipt),
    /// Lightning payment in flight. The consumer MUST poll
    /// [`crate::WalletClient::poll_send_lightning`] with this
    /// `quote_id` on its own cadence and MUST NOT re-quote or
    /// re-`begin` this invoice (doing so double-pays).
    InFlight { quote_id: Uuid },
    /// The mint authoritatively reported the melt UNPAID/FAILED and the
    /// core persisted the quote row FAILED (terminal). NOT re-quotable
    /// for the same invoice — the verdict round-tripped the mint.
    Failed { quote_id: Uuid, reason: String },
}

/// Handle for an in-flight Lightning receive (NUT-04 mint-quote).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiveLightningHandle {
    pub quote_id: Uuid,
    pub mint_quote_id: String,
    pub invoice: String,
    pub payment_hash: String,
    pub amount: Money,
    pub fee: Money,
    pub account_id: AccountId,
    pub expires_at: DateTime<Utc>,
}

/// Single status snapshot for a Lightning receive in-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveLightningState {
    Unpaid,
    Paid,
    Completed,
    Expired,
    Failed,
}

/// Snapshot of one poll round-trip against a mint quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiveLightningSnapshot {
    pub state: ReceiveLightningState,
    pub failure_reason: Option<String>,
}

/// Filter for `list_transactions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TransactionFilter {
    pub account_id: Option<AccountId>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// One page of transactions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TransactionPage {
    pub transactions: Vec<Transaction>,
    pub next_cursor: Option<String>,
}

/// Stub transaction record. Slice 12 returns
/// `WalletError::Unsupported` from `list_transactions` /
/// `get_transaction` — this shape is exposed so consumers can compile
/// against the future API; slice 11+ replaces the stub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    pub id: Uuid,
    pub account_id: AccountId,
    pub direction: TransactionDirection,
    pub amount: Money,
    pub fee: Money,
    pub created_at: DateTime<Utc>,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionDirection {
    Send,
    Receive,
}

/// Exchange-rate snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeRateSnapshot {
    pub from: Currency,
    pub to: Currency,
    pub rate: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_domain::{AccountPurpose, AccountState};
    use serde_json::json;

    fn stub_account() -> Account {
        Account {
            id: AccountId::new(),
            created_at: Utc::now(),
            user_id: UserId::new(),
            name: "Test Mint".into(),
            account_type: AccountType::Cashu,
            purpose: AccountPurpose::Transactional,
            currency: Currency::Btc,
            details: json!({ "mint_url": "https://mint.example", "keyset_counters": {} }),
            version: 0,
            state: AccountState::Active,
            expires_at: None,
        }
    }

    #[test]
    fn account_summary_from_account_extracts_mint_url() {
        let summary = AccountSummary::from_account(&stub_account(), 1234);
        assert_eq!(summary.balance, "1234");
        assert_eq!(summary.mint_url.as_deref(), Some("https://mint.example"));
        assert_eq!(summary.account_type, AccountType::Cashu);
    }

    #[test]
    fn account_summary_from_account_handles_missing_mint_url() {
        let mut a = stub_account();
        a.details = json!({});
        let summary = AccountSummary::from_account(&a, 0);
        assert!(summary.mint_url.is_none());
    }

    #[test]
    fn balance_summary_default_is_empty() {
        let b = BalanceSummary::default();
        assert!(b.total_per_currency.is_empty());
        assert!(b.per_account.is_empty());
    }

    #[test]
    fn auth_status_roundtrips_through_json() {
        let s = AuthStatus {
            logged_in: true,
            user_id: Some(UserId::from(Uuid::nil())),
        };
        let j = serde_json::to_string(&s).unwrap();
        let p: AuthStatus = serde_json::from_str(&j).unwrap();
        assert_eq!(s, p);
    }

    #[test]
    fn receive_status_roundtrips_snake_case() {
        let s = serde_json::to_string(&ReceiveStatus::AlreadyClaimed).unwrap();
        assert_eq!(s, "\"already_claimed\"");
    }

    #[test]
    fn send_lightning_status_in_flight_carries_quote_id_not_an_error() {
        let qid = uuid::Uuid::new_v4();
        let s = SendLightningStatus::InFlight { quote_id: qid };
        // The whole point of P0-1: a still-pending melt is a typed,
        // non-error outcome the consumer polls — never an Err the consumer
        // could mistake for "failed" and re-quote (double-pay).
        match s {
            SendLightningStatus::InFlight { quote_id } => assert_eq!(quote_id, qid),
            other => panic!("expected InFlight, got {other:?}"),
        }
    }

    #[test]
    fn send_lightning_status_failed_is_terminal_not_requotable_signal() {
        let qid = uuid::Uuid::new_v4();
        let s = SendLightningStatus::Failed {
            quote_id: qid,
            reason: "mint reported UNPAID after melt".into(),
        };
        match s {
            SendLightningStatus::Failed { quote_id, reason } => {
                assert_eq!(quote_id, qid);
                assert!(reason.contains("UNPAID"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
