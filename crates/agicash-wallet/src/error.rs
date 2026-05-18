//! Single facade-level error enum.
//!
//! Per the slice-12 plan §5/§6.5 — collapse the per-feature error families
//! into a flat taxonomy that consumers (CLI/FFI/PWA/MCP) can pattern-match
//! without knowing about the underlying crate boundaries.
//!
//! The `Cashu` variant funnels every richer cashu sub-error
//! (`ReceiveSwapError`, `SendSwapError`, `MintQuoteError`, `MeltQuoteError`,
//! `CashuProviderError`, `ReceiveFlowError`) through a single human-readable
//! string. Discriminator-level pattern matching against the inner cause is a
//! follow-up — the current consumers (web Leptos PWA, iOS) display the
//! message inline and don't dispatch off the variant.
//!
//! `Unsupported` is the load-bearing variant for this slice: every method
//! that depends on a not-yet-shipped slice (Spark accounts, event bus,
//! `remove_mint`, transaction history) returns it with a stable string
//! prefix the caller can match on.

use agicash_cashu::{
    MeltQuoteError, MintQuoteError, ReceiveFlowError, ReceiveSwapError, SendSwapError,
};
use agicash_exchange_rate::ExchangeRateError;
use agicash_lightning_address::LightningAddressError;
use agicash_traits::{AuthError, CashuProviderError, StorageError};

/// Retry classification for a `WalletError`. Consumers branch on this
/// instead of pattern-matching error variants. Spec §11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPolicy {
    /// Never retry — domain rejection, bad input, auth, bug, unsupported.
    Never,
    /// Retry with exponential backoff (100ms/400ms/1.6s + jitter), capped.
    ExponentialBackoff { max_attempts: u8 },
}

/// Facade-level error type. Every public `WalletClient` method returns
/// `Result<_, WalletError>`.
#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    /// Caller is not authenticated — no session loaded on the auth client.
    #[error("not authenticated")]
    Unauthenticated,

    /// Auth backend (OpenSecret) failure: network, refresh, signup.
    #[error("auth error: {0}")]
    Auth(String),

    /// Storage backend (Supabase) failure: network, RLS, missing row.
    #[error("storage error: {0}")]
    Storage(String),

    /// Cashu / mint / swap / quote failure. Covers
    /// `ReceiveSwapError`, `SendSwapError`, `MintQuoteError`,
    /// `MeltQuoteError`, `CashuProviderError`, `ReceiveFlowError`.
    #[error("cashu error: {0}")]
    Cashu(String),

    /// Transport/connectivity failure (DNS, timeout, connection reset,
    /// offline mint). Retry-able with backoff. Spec §11 `Network`.
    #[error("network error: {0}")]
    Network(String),

    /// State moved between read and write (quote already spent/expired,
    /// stale version, conflict). Retry-able after re-fetch. Spec §11
    /// `Concurrency`.
    #[error("concurrency error: {0}")]
    Concurrency(String),

    /// LUD-16 Lightning Address resolution failure.
    #[error("lightning-address error: {0}")]
    LightningAddress(String),

    /// Exchange-rate provider failure.
    #[error("exchange-rate error: {0}")]
    ExchangeRate(String),

    /// Caller-side validation failure (bad amount, bad UUID, missing
    /// matching account, currency mismatch). Carries a discriminator
    /// `code` plus a human message.
    #[error("validation error [{code}]: {message}")]
    Validation { code: String, message: String },

    /// Resource not found by id.
    #[error("not found: {0}")]
    NotFound(String),

    /// Method or branch isn't implemented yet — surfaces deferred slices
    /// (Spark, event bus, `list_transactions`, `remove_mint`) without
    /// reshaping the API.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),

    /// Internal/unexpected failure. Bug, panic, invariant violation.
    #[error("internal error: {0}")]
    Internal(String),
}

impl WalletError {
    pub fn validation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Spec §11 retry policy. Default is conservative `Never`; only
    /// `Network` and `Concurrency` are retry-able.
    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        match self {
            Self::Network(_) | Self::Concurrency(_) => {
                RetryPolicy::ExponentialBackoff { max_attempts: 3 }
            }
            _ => RetryPolicy::Never,
        }
    }
}

impl From<AuthError> for WalletError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Unauthenticated => Self::Unauthenticated,
            other => Self::Auth(other.to_string()),
        }
    }
}

impl From<StorageError> for WalletError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => Self::NotFound("storage row".into()),
            other => Self::Storage(other.to_string()),
        }
    }
}

impl From<CashuProviderError> for WalletError {
    fn from(e: CashuProviderError) -> Self {
        Self::Cashu(e.to_string())
    }
}

impl From<ReceiveSwapError> for WalletError {
    fn from(e: ReceiveSwapError) -> Self {
        Self::Cashu(e.to_string())
    }
}

impl From<SendSwapError> for WalletError {
    fn from(e: SendSwapError) -> Self {
        Self::Cashu(e.to_string())
    }
}

impl From<MintQuoteError> for WalletError {
    fn from(e: MintQuoteError) -> Self {
        Self::Cashu(e.to_string())
    }
}

impl From<MeltQuoteError> for WalletError {
    fn from(e: MeltQuoteError) -> Self {
        Self::Cashu(e.to_string())
    }
}

impl From<ReceiveFlowError> for WalletError {
    fn from(e: ReceiveFlowError) -> Self {
        Self::Cashu(e.to_string())
    }
}

impl From<LightningAddressError> for WalletError {
    fn from(e: LightningAddressError) -> Self {
        Self::LightningAddress(e.to_string())
    }
}

impl From<ExchangeRateError> for WalletError {
    fn from(e: ExchangeRateError) -> Self {
        Self::ExchangeRate(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_unauthenticated_maps_through() {
        let e: WalletError = AuthError::Unauthenticated.into();
        assert!(matches!(e, WalletError::Unauthenticated));
    }

    #[test]
    fn storage_not_found_maps_to_not_found() {
        let e: WalletError = StorageError::NotFound.into();
        assert!(matches!(e, WalletError::NotFound(_)));
    }

    #[test]
    fn validation_constructor_builds_variant() {
        let e = WalletError::validation("bad_uuid", "not-a-uuid");
        match e {
            WalletError::Validation { code, message } => {
                assert_eq!(code, "bad_uuid");
                assert_eq!(message, "not-a-uuid");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn unsupported_carries_static_str() {
        let e = WalletError::Unsupported("spark accounts available in slice 9");
        let msg = e.to_string();
        assert!(msg.contains("spark"));
    }

    #[test]
    fn retry_policy_network_is_backoff_max_3() {
        let e = WalletError::Network("connection reset".into());
        assert_eq!(
            e.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn retry_policy_concurrency_is_capped_retry() {
        let e = WalletError::Concurrency("quote moved to EXPIRED".into());
        assert_eq!(
            e.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn retry_policy_never_for_domain_and_catchall() {
        for e in [
            WalletError::Cashu("token already spent".into()),
            WalletError::Unauthenticated,
            WalletError::NotFound("acct".into()),
            WalletError::validation("bad_uuid", "x"),
            WalletError::Internal("bug".into()),
            WalletError::Unsupported("slice 9"),
        ] {
            assert_eq!(e.retry_policy(), RetryPolicy::Never, "{e:?} must not retry");
        }
    }
}
