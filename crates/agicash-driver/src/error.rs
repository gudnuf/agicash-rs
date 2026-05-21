//! Per-row error classification for the resumption driver.
//!
//! The driver's response to a row error has three branches:
//! - [`ErrorClass::Benign`] — the row resolved (or moved past where we'd
//!   act) between the `list_*` read and the act. Count it as no-op,
//!   keep going.
//! - [`ErrorClass::Transient`] — network/concurrency blip; retry with
//!   backoff up to `RetryConfig::max_attempts`.
//! - [`ErrorClass::Fatal`] — domain/auth/validation; drop the row for
//!   this sweep, the next trigger will re-evaluate.
//!
//! Classification uses [`agicash_wallet::WalletError::retry_policy`] for
//! the transient/fatal split (the §11 policy already in core), plus a
//! narrow string-prefix check on `Cashu(...)` for the
//! machine-`InvalidTransition` / `QuoteNotPaid` / `QuoteNotPending`
//! variants — those flatten through `WalletError::From<...>` into
//! `Cashu(String)`, so we recognize them by the `#[error("...")]`
//! prefixes the four error enums share. See
//! `crates/agicash-cashu/src/{send_swap, receive_swap, mint_quote,
//! melt_quote}/error.rs`.

use agicash_wallet::error::RetryPolicy;
use agicash_wallet::WalletError;

/// What kind of failure a row produced, and what the sweep should do
/// about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The row resolved or otherwise moved between read and act. Treat
    /// as a no-op success — the row is no longer stuck in the way we
    /// thought it was.
    Benign,
    /// Retry-able transient (network / concurrency). Retry per
    /// `RetryConfig`.
    Transient,
    /// Domain rejection / auth / validation / internal. No retry; drop
    /// the row from this sweep. The next trigger re-evaluates.
    Fatal,
}

/// A `WalletError` annotated with its driver classification — kept
/// for the report so a caller (or a Lane B trigger) can see exactly
/// why a row was not advanced.
#[derive(Debug, thiserror::Error)]
#[error("{class:?}: {source}")]
pub struct ClassifiedError {
    pub class: ErrorClass,
    #[source]
    pub source: WalletError,
}

impl ClassifiedError {
    /// Classify a `WalletError` produced by a row-advance facade call.
    #[must_use]
    pub fn from_wallet_error(err: WalletError) -> Self {
        let class = classify(&err);
        Self { class, source: err }
    }
}

/// Apply the §11 retry policy *plus* a benign-no-op recognition for the
/// machine-guarded state errors. The benign cases are the row already
/// resolved between read and act — see module docs.
fn classify(err: &WalletError) -> ErrorClass {
    // First: is it a benign no-op? `InvalidTransition` /
    // `QuoteNotPaid` / `QuoteNotPending` from any of the four service
    // crates flatten to `WalletError::Cashu(String)` with these exact
    // prefixes (Display impls are stable, lifted from the
    // `#[error("...")]` attributes).
    if let WalletError::Cashu(msg) = err {
        if is_benign_state_msg(msg) {
            return ErrorClass::Benign;
        }
    }
    // `Concurrency` is also a "state moved between read and write"
    // signal — but the §11 policy already classifies it as
    // `ExponentialBackoff` (retry-after-refetch). For driver purposes
    // we keep that: it's not *quite* the same as "benign no-op
    // already-resolved" — it's "transient, re-look". Stays Transient.

    match err.retry_policy() {
        RetryPolicy::ExponentialBackoff { .. } => ErrorClass::Transient,
        RetryPolicy::Never => ErrorClass::Fatal,
    }
}

/// Recognize the three machine-guarded "row already moved" prefixes the
/// service Display impls produce. Stable substring match — these strings
/// are part of the contract (every callsite has tests that pin them).
fn is_benign_state_msg(msg: &str) -> bool {
    // The four service error enums share these exact prefixes:
    //   #[error("invalid state transition from {from} on event {event}")]
    //   #[error("quote not yet paid")]
    //   #[error("quote not yet pending")]
    // `WalletError::Cashu(e.to_string())` carries them verbatim.
    msg.contains("invalid state transition")
        || msg.contains("quote not yet paid")
        || msg.contains("quote not yet pending")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_transition_is_benign() {
        let e = WalletError::Cashu(
            "invalid state transition from Pending on event swap_for_proofs_to_send".into(),
        );
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Benign
        );
    }

    #[test]
    fn quote_not_yet_paid_is_benign() {
        let e = WalletError::Cashu("quote not yet paid".into());
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Benign
        );
    }

    #[test]
    fn quote_not_yet_pending_is_benign() {
        let e = WalletError::Cashu("quote not yet pending".into());
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Benign
        );
    }

    #[test]
    fn network_is_transient() {
        let e = WalletError::Network("connection reset".into());
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Transient
        );
    }

    #[test]
    fn concurrency_is_transient() {
        let e = WalletError::Concurrency("row version stale".into());
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Transient
        );
    }

    #[test]
    fn other_cashu_is_fatal() {
        let e = WalletError::Cashu("mint returned malformed proof".into());
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Fatal
        );
    }

    #[test]
    fn unauthenticated_is_fatal() {
        let e = WalletError::Unauthenticated;
        assert_eq!(
            ClassifiedError::from_wallet_error(e).class,
            ErrorClass::Fatal
        );
    }
}
