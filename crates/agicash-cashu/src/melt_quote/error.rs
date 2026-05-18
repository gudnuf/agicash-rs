//! Melt-quote error type.
//!
//! Bundles together storage failures, mint protocol failures, and the
//! validation checks the service performs against the requested invoice +
//! account before it begins.

use super::storage::MeltQuoteStorageError;
use crate::dleq::DleqVerificationError;
use agicash_traits::CashuProviderError;

#[derive(Debug, thiserror::Error)]
pub enum MeltQuoteError {
    /// State machine asked to apply an event from a state that doesn't
    /// accept it.
    #[error("invalid state transition from {from} on event {event}")]
    InvalidTransition { from: String, event: String },

    /// Underlying storage backend failed.
    #[error("storage error: {0}")]
    Storage(MeltQuoteStorageError),

    /// An active (UNPAID/PENDING/PAID) melt quote already exists for
    /// this invoice's payment hash. Re-quoting would fire a second
    /// `post_melt` for an in-flight / already-paid invoice — a
    /// double-pay. Surfaced from the DB partial unique index
    /// `cashu_send_quotes_payment_hash_active_unique` (or the
    /// `find_active_by_payment_hash` pre-check) so FFI consumers get a
    /// clean typed error instead of a raw `Backend(...)` 23505 string.
    #[error("duplicate payment: an active melt quote already exists for this invoice")]
    DuplicatePayment,

    /// CDK / mint network or protocol failure.
    #[error("mint error: {0}")]
    Mint(#[from] CashuProviderError),

    /// BOLT-11 invoice did not parse.
    #[error("invalid bolt11 invoice: {0}")]
    InvalidInvoice(String),

    /// Invoice carried no amount (NUT-05 amountless support deferred).
    #[error("amountless invoice not supported")]
    AmountlessInvoice,

    /// Requested amount is below the mint's minimum / converted to zero.
    #[error("amount too small")]
    AmountTooSmall,

    /// Account currency disagrees with what the mint quoted.
    #[error("currency mismatch: account {account} differs from request {request}")]
    CurrencyMismatch { account: String, request: String },

    /// Account proof balance can't cover amount + fees.
    #[error("insufficient balance: need {needed}, have {have}")]
    InsufficientBalance { needed: String, have: String },

    /// Invoice expired before we could initiate the melt.
    #[error("quote expired before payment")]
    QuoteExpired,

    /// Caller asked to poll an UNPAID quote (no melt initiated yet).
    #[error("quote not yet pending")]
    QuoteNotPending,

    /// Mint reported the melt failed.
    #[error("melt failed at mint: {0}")]
    MeltFailed(String),

    /// Operational state we can't recover from automatically.
    #[error("melt unrecoverable: {0}")]
    Unrecoverable(String),

    /// NUT-12 DLEQ verification failed on a mint-returned blind
    /// signature (change blanks for NUT-08). Mint is malicious or
    /// compromised.
    #[error("DLEQ verification failed: {0}")]
    DleqVerificationFailed(#[from] DleqVerificationError),
}

impl From<MeltQuoteStorageError> for MeltQuoteError {
    /// Promote the storage-level `DuplicatePayment` to the typed
    /// top-level [`MeltQuoteError::DuplicatePayment`] so FFI consumers
    /// can pattern-match it cleanly; every other storage error keeps
    /// flowing through [`MeltQuoteError::Storage`] (preserving the
    /// behaviour the previous `#[from]` derive gave).
    fn from(e: MeltQuoteStorageError) -> Self {
        match e {
            MeltQuoteStorageError::DuplicatePayment => MeltQuoteError::DuplicatePayment,
            other => MeltQuoteError::Storage(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_transition_displays_both_sides() {
        let e = MeltQuoteError::InvalidTransition {
            from: "Paid".into(),
            event: "InitiateMelt".into(),
        };
        let s = e.to_string();
        assert!(s.contains("Paid"));
        assert!(s.contains("InitiateMelt"));
    }

    #[test]
    fn currency_mismatch_includes_both() {
        let e = MeltQuoteError::CurrencyMismatch {
            account: "BTC".into(),
            request: "USD".into(),
        };
        let s = e.to_string();
        assert!(s.contains("BTC"));
        assert!(s.contains("USD"));
    }

    #[test]
    fn insufficient_balance_includes_amounts() {
        let e = MeltQuoteError::InsufficientBalance {
            needed: "100".into(),
            have: "50".into(),
        };
        let s = e.to_string();
        assert!(s.contains("100"));
        assert!(s.contains("50"));
    }

    #[test]
    fn duplicate_payment_storage_error_promotes_to_typed_top_level_variant() {
        // The DB partial unique index surfaces a storage-level
        // DuplicatePayment; it must be promoted to the typed
        // MeltQuoteError::DuplicatePayment (NOT buried in Storage(...))
        // so FFI consumers can pattern-match it.
        let e: MeltQuoteError = MeltQuoteStorageError::DuplicatePayment.into();
        assert!(matches!(e, MeltQuoteError::DuplicatePayment));
        assert!(e.to_string().contains("duplicate payment"));
    }

    #[test]
    fn other_storage_errors_still_flow_through_storage_variant() {
        let e: MeltQuoteError = MeltQuoteStorageError::NotFound.into();
        assert!(matches!(e, MeltQuoteError::Storage(_)));
        let e: MeltQuoteError = MeltQuoteStorageError::Concurrency("x".into()).into();
        assert!(matches!(e, MeltQuoteError::Storage(_)));
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MeltQuoteError>();
    }
}
