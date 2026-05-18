//! FFI melt-quote (Lightning send) value types.
//!
//! Wrappers around `agicash_cashu::melt_quote::{CashuMeltQuote,
//! CashuMeltQuoteState, MeltQuotePreview, MeltOutcome}` flattened into
//! Swift-codable primitives. The symmetric counterpart of
//! [`crate::mint_quote`] (Lightning receive) — same handle / state /
//! snapshot trio, mirrored for the NUT-05 send side.
//!
//! The Rust-side domain shape carries `Money` (Decimal-typed) and a
//! per-state enum with a conditional preimage / fee payload. The Swift
//! consumer doesn't need any of the internal state machinery — it only
//! needs the visible fee breakdown (so it can render the confirmation
//! screen), the BOLT-11 + amounts, and which terminal bucket the quote
//! is in (UNPAID → PENDING → PAID / EXPIRED / FAILED). Three FFI records
//! cover the surface:
//!
//! - [`MeltQuotePreview`] — returned from `prepare_melt_quote`. Carries
//!   the fee breakdown the confirm screen displays. No persistence
//!   side effect (mirrors `agicash send lightning <bolt11> --dry-run`).
//! - [`MeltQuoteHandle`] — returned from `create_melt_quote`. Carries
//!   the persisted wallet-side quote id (for follow-up
//!   poll/execute calls), the mint-side id (informational), the
//!   BOLT-11, the fee/amount breakdown, and the expiry.
//! - [`MeltQuoteSnapshot`] — returned from `execute_melt_quote` and
//!   `poll_melt_quote`. A bare state discriminator plus an optional
//!   `failure_reason` (FAILED) and the PAID payment-preimage + final
//!   fee breakdown so the iOS receipt can render the settled amounts.

/// Pre-commit melt quote shown on the confirmation screen.
///
/// Mirrors the CLI's `QuoteOutput` JSON (the `--dry-run` branch of
/// `crates/agicash-cli/src/send_lightning.rs`). No swap row is created;
/// the iOS confirm card renders the fee breakdown then calls
/// `create_melt_quote` to persist + reserve proofs.
///
/// All `Money`-valued fields are decimal-stringified to match the
/// [`crate::receive::ReceiveResult`] / [`crate::send::SendQuotePreview`]
/// convention so Swift consumers don't thread Rust's `Decimal` through
/// the FFI boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MeltQuotePreview {
    /// The amount the receiver gets (the BOLT-11 invoice amount), in
    /// the account's minor unit. Decimal-stringified.
    pub amount: String,
    /// Mint-quoted Lightning fee reserve. The actual Lightning fee is
    /// `<= this`; any unspent reserve is refunded as change on PAID.
    /// Decimal-stringified.
    pub lightning_fee_reserve: String,
    /// Cashu input fee for the proofs the wallet will spend.
    /// Decimal-stringified.
    pub cashu_fee: String,
    /// `lightning_fee_reserve + cashu_fee` — the worst-case fee.
    /// Decimal-stringified.
    pub total_fee: String,
    /// `amount + total_fee` — the worst-case total deducted from the
    /// account (the actual debit may be lower after the reserve
    /// refund). Decimal-stringified.
    pub total_amount: String,
    /// Cashu sub-unit (`sat`, `usd`).
    pub unit: String,
    /// Wallet account currency (`BTC`, `USD`).
    pub currency: String,
    /// UUID of the account the send will debit.
    pub account_id: String,
    /// Hex-encoded BOLT-11 payment hash. Stable identifier for the
    /// receipt / debugging.
    pub payment_hash: String,
}

/// Lightning send handle. Mirrors the CLI's `QuoteIssuedOutput` JSON
/// (`crates/agicash-cli/src/send_lightning.rs`) but with the Swift-side
/// fields the carousel's Lightning-send view needs:
/// - `quote_id` for follow-up FFI calls,
/// - `invoice` for display / receipt,
/// - `amount` + fee breakdown for the in-flight card,
/// - `expires_at` for the countdown timer.
///
/// `quote_id` is the **wallet-side** UUID (Supabase `wallet.melt_quotes`
/// PK) — that's what `execute_melt_quote` and `poll_melt_quote` expect.
/// `melt_quote_id` is the mint-side string identifier returned by NUT-05
/// `POST /v1/melt/quote/bolt11`; exposed for receipt / debugging only.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MeltQuoteHandle {
    /// Wallet-side UUID of the persisted quote row. Pass this to
    /// `execute_melt_quote` and `poll_melt_quote`.
    pub quote_id: String,
    /// Mint-side NUT-05 quote id string. Informational; not used for
    /// follow-up FFI calls.
    pub melt_quote_id: String,
    /// BOLT-11 invoice the mint pays on the user's behalf.
    pub invoice: String,
    /// Hex-encoded BOLT-11 payment hash.
    pub payment_hash: String,
    /// Amount the receiver gets. Decimal-stringified (matches the
    /// `ReceiveResult.amount` convention).
    pub amount: String,
    /// Mint-quoted Lightning fee reserve. Decimal-stringified.
    pub lightning_fee_reserve: String,
    /// Cashu input fee. Decimal-stringified.
    pub cashu_fee: String,
    /// `lightning_fee_reserve + cashu_fee`. Decimal-stringified.
    pub total_fee: String,
    /// Cashu sub-unit (`sat`, `usd`).
    pub unit: String,
    /// Wallet account currency (`BTC`, `USD`).
    pub currency: String,
    /// UUID of the account the send debits.
    pub account_id: String,
    /// ISO 8601 timestamp at which the quote expires.
    pub expires_at: String,
}

/// Lifecycle state for a [`MeltQuoteHandle`]. Mirrors
/// `agicash_cashu::melt_quote::CashuMeltQuoteState` but flattens the
/// per-state payload out into [`MeltQuoteSnapshot`]'s optional fields
/// (the iOS UI never needs the change-proof machinery — the service
/// reconciles it internally).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MeltQuoteFfiState {
    /// Quote created, no melt issued — proofs reserved, awaiting the
    /// user's confirm.
    Unpaid,
    /// `post_melt` issued; the Lightning payment is in flight. iOS
    /// should poll `poll_melt_quote` until this transitions.
    Pending,
    /// Mint settled the melt; proofs spent + change credited
    /// (terminal). The snapshot carries the preimage + final fees.
    Paid,
    /// Quote expired before the melt was initiated (terminal).
    Expired,
    /// Operational failure (mint rejected, network) (terminal).
    Failed,
}

/// Snapshot returned by [`crate::wallet::AgicashWallet::execute_melt_quote`]
/// and [`crate::wallet::AgicashWallet::poll_melt_quote`].
///
/// `failure_reason` is only populated when `state == Failed`. The
/// `payment_preimage` / `lightning_fee` / `amount_spent` / `total_fee`
/// fields are only populated when `state == Paid` (the NUT-05 settled
/// receipt) — `None` for every other state.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MeltQuoteSnapshot {
    pub state: MeltQuoteFfiState,
    /// Operator-facing failure message. `Some` iff `state == Failed`.
    pub failure_reason: Option<String>,
    /// BOLT-11 payment preimage proving settlement. `Some` iff
    /// `state == Paid`.
    pub payment_preimage: Option<String>,
    /// Actual Lightning fee charged (`lightning_fee_reserve` minus the
    /// refunded change). Decimal-stringified. `Some` iff
    /// `state == Paid`.
    pub lightning_fee: Option<String>,
    /// `amount + lightning_fee` — what really left the account in
    /// network terms. Decimal-stringified. `Some` iff `state == Paid`.
    pub amount_spent: Option<String>,
    /// `lightning_fee + cashu_fee`. Decimal-stringified. `Some` iff
    /// `state == Paid`.
    pub total_fee: Option<String>,
}
