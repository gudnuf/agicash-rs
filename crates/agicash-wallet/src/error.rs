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

use crate::discriminator::{AuthErrorCode, CashuDiscriminator};
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

    /// Auth backend (OpenSecret) failure. `code` distinguishes a
    /// transient backend/internal blip (keep the signed-in UI alive)
    /// from a genuine expiry (`WalletError::Unauthenticated`) and an
    /// auth-network drop (`WalletError::Network`, retry-able). P0-2.
    #[error("auth error [{code:?}]: {message}")]
    Auth {
        code: AuthErrorCode,
        message: String,
    },

    /// Storage backend (Supabase) failure: network, RLS, missing row.
    #[error("storage error: {0}")]
    Storage(String),

    /// Cashu / mint / swap / quote failure. Covers
    /// `ReceiveSwapError`, `SendSwapError`, `MintQuoteError`,
    /// `MeltQuoteError`, `CashuProviderError`, `ReceiveFlowError`.
    #[error("cashu error: {0}")]
    Cashu(String),

    /// A cashu failure that carries a *named* discriminator the
    /// consumer must branch on (P0-3): `DUPLICATE_PAYMENT` ("resume,
    /// do NOT re-quote"), DLEQ-failed (malicious mint), etc. Distinct
    /// from the flat `Cashu(String)` so a consumer can match the
    /// signal without string-sniffing. Retry classification is
    /// unchanged vs. 12a (see `retry_policy`).
    #[error("cashu error [{discriminator:?}]: {message}")]
    CashuTyped {
        discriminator: CashuDiscriminator,
        message: String,
    },

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
            // P0-3: DuplicatePayment/QuoteExpired were Concurrency
            // (retryable) under 12a — preserve that exact policy now
            // that they carry a name. DLEQ/InsufficientBalance were
            // Cashu (Never) — preserve that too.
            Self::CashuTyped { discriminator, .. } => match discriminator {
                CashuDiscriminator::DuplicatePayment | CashuDiscriminator::QuoteExpired => {
                    RetryPolicy::ExponentialBackoff { max_attempts: 3 }
                }
                CashuDiscriminator::DleqVerificationFailed
                | CashuDiscriminator::InsufficientBalance => RetryPolicy::Never,
            },
            _ => RetryPolicy::Never,
        }
    }

    /// The named cashu signal a consumer branches on, or `None` for a
    /// flat/non-cashu error. P0-3 — restores the FFI `DUPLICATE_PAYMENT`
    /// / `DLEQ verification failed` discriminators.
    #[must_use]
    pub fn cashu_discriminator(&self) -> Option<CashuDiscriminator> {
        match self {
            Self::CashuTyped { discriminator, .. } => Some(*discriminator),
            _ => None,
        }
    }
}

impl From<AuthError> for WalletError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Unauthenticated => Self::Unauthenticated,
            // Auth-network is a transport blip — route to the 12a
            // `Network` variant so it inherits `ExponentialBackoff`
            // and stays distinct from a genuine expiry (P0-2).
            AuthError::Network(m) => Self::Network(m),
            AuthError::Backend(m) => Self::Auth {
                code: AuthErrorCode::Backend,
                message: m,
            },
            AuthError::Internal(m) => Self::Auth {
                code: AuthErrorCode::Internal,
                message: m,
            },
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

// The classification rule lives in `docs/superpowers/plans/12a-suberror-inventory.md`.
// `CashuProviderError` is the leaf transport enum; the four sub-errors that wrap
// it (`Mint`/`MintDiscovery`) delegate to its classification rather than
// string-matching foreign messages. `_ => Self::Cashu(e.to_string())` is the
// retained §11 catch-all so nothing regresses.

impl From<CashuProviderError> for WalletError {
    fn from(e: CashuProviderError) -> Self {
        match &e {
            // transport/connectivity → retry-able
            CashuProviderError::Network(_) => Self::Network(e.to_string()),
            // InvalidUrl (config), Protocol (mint domain) → conservative Never
            _ => Self::Cashu(e.to_string()),
        }
    }
}

impl From<ReceiveSwapError> for WalletError {
    fn from(e: ReceiveSwapError) -> Self {
        match e {
            // delegate to the wrapped provider's classification
            ReceiveSwapError::Mint(p) => p.into(),
            e @ ReceiveSwapError::DleqVerificationFailed(_) => Self::CashuTyped {
                discriminator: CashuDiscriminator::DleqVerificationFailed,
                message: e.to_string(),
            },
            // no own state-moved variant → no Concurrency arm
            other => Self::Cashu(other.to_string()),
        }
    }
}

impl From<SendSwapError> for WalletError {
    fn from(e: SendSwapError) -> Self {
        match e {
            SendSwapError::Mint(p) => p.into(),
            e @ SendSwapError::InsufficientBalance { .. } => Self::CashuTyped {
                discriminator: CashuDiscriminator::InsufficientBalance,
                message: e.to_string(),
            },
            e @ SendSwapError::DleqVerificationFailed(_) => Self::CashuTyped {
                discriminator: CashuDiscriminator::DleqVerificationFailed,
                message: e.to_string(),
            },
            // no own state-moved variant → no Concurrency arm
            other => Self::Cashu(other.to_string()),
        }
    }
}

impl From<MintQuoteError> for WalletError {
    fn from(e: MintQuoteError) -> Self {
        match e {
            MintQuoteError::Mint(p) => p.into(),
            // quote expired between read & write → retry after re-fetch
            e @ MintQuoteError::QuoteExpired => Self::CashuTyped {
                discriminator: CashuDiscriminator::QuoteExpired,
                message: e.to_string(),
            },
            other => Self::Cashu(other.to_string()),
        }
    }
}

impl From<MeltQuoteError> for WalletError {
    fn from(e: MeltQuoteError) -> Self {
        match e {
            MeltQuoteError::Mint(p) => p.into(),
            e @ MeltQuoteError::DuplicatePayment => Self::CashuTyped {
                discriminator: CashuDiscriminator::DuplicatePayment,
                message: e.to_string(),
            },
            e @ MeltQuoteError::QuoteExpired => Self::CashuTyped {
                discriminator: CashuDiscriminator::QuoteExpired,
                message: e.to_string(),
            },
            e @ MeltQuoteError::InsufficientBalance { .. } => Self::CashuTyped {
                discriminator: CashuDiscriminator::InsufficientBalance,
                message: e.to_string(),
            },
            e @ MeltQuoteError::DleqVerificationFailed(_) => Self::CashuTyped {
                discriminator: CashuDiscriminator::DleqVerificationFailed,
                message: e.to_string(),
            },
            other => Self::Cashu(other.to_string()),
        }
    }
}

impl From<ReceiveFlowError> for WalletError {
    fn from(e: ReceiveFlowError) -> Self {
        match e {
            // NUT-06 discovery wraps the provider — delegate
            ReceiveFlowError::MintDiscovery(p) => p.into(),
            // underlying swap carries its own classification — delegate
            ReceiveFlowError::Swap(s) => s.into(),
            // no own state-moved variant → no own Concurrency arm
            other => Self::Cashu(other.to_string()),
        }
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

    // --- Task 3: cashu sub-error reclassification (per 12a-suberror-inventory.md) ---
    // `CashuProviderError`, sub-error enums are imported via `super::*`.

    #[test]
    fn cashuprovider_network_maps_to_network() {
        let src = CashuProviderError::Network("mint unreachable".into());
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn cashuprovider_protocol_stays_cashu() {
        let src = CashuProviderError::Protocol("bad NUT-06 response".into());
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Cashu(_)), "got {w:?}");
    }

    #[test]
    fn receiveswap_mint_network_maps_to_network() {
        let src = ReceiveSwapError::Mint(CashuProviderError::Network("offline".into()));
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn receiveswap_domain_stays_cashu() {
        let src = ReceiveSwapError::AmountTooSmall;
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Cashu(_)), "got {w:?}");
    }

    #[test]
    fn sendswap_mint_network_maps_to_network() {
        let src = SendSwapError::Mint(CashuProviderError::Network("timeout".into()));
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn sendswap_domain_stays_cashu() {
        let src = SendSwapError::AmountTooSmall;
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Cashu(_)), "got {w:?}");
    }

    #[test]
    fn mintquote_mint_network_maps_to_network() {
        let src = MintQuoteError::Mint(CashuProviderError::Network("dns".into()));
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn mintquote_expired_is_cashutyped_with_12a_retry_policy() {
        // 12b-2 P0-3: MintQuoteError::QuoteExpired now routes to the
        // named CashuTyped { QuoteExpired }; 12a retry policy preserved.
        let src = MintQuoteError::QuoteExpired;
        let w: WalletError = src.into();
        assert!(
            matches!(
                w,
                WalletError::CashuTyped {
                    discriminator: CashuDiscriminator::QuoteExpired,
                    ..
                }
            ),
            "got {w:?}"
        );
        assert_eq!(
            w.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn mintquote_domain_stays_cashu() {
        let src = MintQuoteError::QuoteNotPaid;
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Cashu(_)), "got {w:?}");
    }

    #[test]
    fn meltquote_mint_network_maps_to_network() {
        let src = MeltQuoteError::Mint(CashuProviderError::Network("conn reset".into()));
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn meltquote_duplicate_payment_is_cashutyped_with_12a_retry_policy() {
        // 12b-2 P0-3: variant is now the *named* CashuTyped, but the
        // 12a externally-observable retry classification is preserved.
        let src = MeltQuoteError::DuplicatePayment;
        let w: WalletError = src.into();
        assert!(
            matches!(
                w,
                WalletError::CashuTyped {
                    discriminator: CashuDiscriminator::DuplicatePayment,
                    ..
                }
            ),
            "got {w:?}"
        );
        assert_eq!(
            w.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn meltquote_expired_is_cashutyped_with_12a_retry_policy() {
        let src = MeltQuoteError::QuoteExpired;
        let w: WalletError = src.into();
        assert!(
            matches!(
                w,
                WalletError::CashuTyped {
                    discriminator: CashuDiscriminator::QuoteExpired,
                    ..
                }
            ),
            "got {w:?}"
        );
        assert_eq!(
            w.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn meltquote_domain_stays_cashu() {
        let src = MeltQuoteError::MeltFailed("mint said no".into());
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Cashu(_)), "got {w:?}");
    }

    #[test]
    fn receiveflow_mintdiscovery_network_maps_to_network() {
        let src = ReceiveFlowError::MintDiscovery(CashuProviderError::Network("nut06".into()));
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn receiveflow_swap_network_propagates_to_network() {
        let src = ReceiveFlowError::Swap(ReceiveSwapError::Mint(CashuProviderError::Network(
            "offline".into(),
        )));
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
    }

    #[test]
    fn receiveflow_domain_stays_cashu() {
        let src = ReceiveFlowError::TokenParse("bad token".into());
        let w: WalletError = src.into();
        assert!(matches!(w, WalletError::Cashu(_)), "got {w:?}");
    }

    // --- 12b-2 Task 3: P0-2 structured auth discriminator ---

    #[test]
    fn p0_2_auth_network_routes_to_network_variant_and_is_retryable() {
        // iOS keeps the signed-in UI + in-flight payment alive on an
        // auth-network blip; it must be retry-able and NOT a session expiry.
        let w: WalletError = AuthError::Network("opensecret dns".into()).into();
        assert!(matches!(w, WalletError::Network(_)), "got {w:?}");
        assert_eq!(
            w.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn p0_2_auth_unauthenticated_stays_distinct() {
        let w: WalletError = AuthError::Unauthenticated.into();
        assert!(matches!(w, WalletError::Unauthenticated), "got {w:?}");
    }

    #[test]
    fn p0_2_auth_backend_carries_backend_code() {
        let w: WalletError = AuthError::Backend("opensecret 500".into()).into();
        match w {
            WalletError::Auth { code, ref message } => {
                assert_eq!(code, AuthErrorCode::Backend);
                assert!(message.contains("opensecret 500"));
            }
            other => panic!("expected Auth{{Backend}}, got {other:?}"),
        }
    }

    #[test]
    fn p0_2_auth_internal_carries_internal_code() {
        let w: WalletError = AuthError::Internal("bug".into()).into();
        assert!(
            matches!(
                w,
                WalletError::Auth {
                    code: AuthErrorCode::Internal,
                    ..
                }
            ),
            "got {w:?}"
        );
    }

    #[test]
    fn p0_2_auth_backend_is_never_retry() {
        // Backend is transient for the UI but not auto-retried by the
        // facade's retry policy (only Network/Concurrency are).
        let w: WalletError = AuthError::Backend("x".into()).into();
        assert_eq!(w.retry_policy(), RetryPolicy::Never);
    }

    // --- 12b-2 Task 4: P0-3 named cashu discriminator ---

    #[test]
    fn p0_3_duplicate_payment_is_named_signal() {
        // The binding P0-3 signal: "this exact invoice already has an
        // active melt quote → resume, do NOT re-quote." A named enum the
        // consumer branches on, not just "retryable".
        let w: WalletError = MeltQuoteError::DuplicatePayment.into();
        assert_eq!(
            w.cashu_discriminator(),
            Some(CashuDiscriminator::DuplicatePayment),
            "got {w:?}"
        );
    }

    #[test]
    fn p0_3_duplicate_payment_preserves_12a_retry_policy() {
        // 12a routed DuplicatePayment -> Concurrency (retryable). The
        // externally-observable retry classification MUST be unchanged;
        // only the internal variant gains a name.
        let w: WalletError = MeltQuoteError::DuplicatePayment.into();
        assert_eq!(
            w.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn p0_3_quote_expired_named_and_preserves_12a_retry_policy() {
        let w: WalletError = MeltQuoteError::QuoteExpired.into();
        assert_eq!(
            w.cashu_discriminator(),
            Some(CashuDiscriminator::QuoteExpired)
        );
        assert_eq!(
            w.retry_policy(),
            RetryPolicy::ExponentialBackoff { max_attempts: 3 }
        );
    }

    #[test]
    fn p0_3_dleq_failure_is_named_signal_and_never_retry() {
        // Build a DLEQ failure through MeltQuoteError which has a
        // dedicated DleqVerificationFailed(#[from] DleqVerificationError).
        // ProofVerificationFailed does not exist at the canonical ref;
        // mirror the codebase's own constructor (dleq.rs:418).
        use agicash_cashu::DleqVerificationError;
        let src = MeltQuoteError::DleqVerificationFailed(DleqVerificationError::CountMismatch {
            sigs: 1,
            msgs: 0,
        });
        let w: WalletError = src.into();
        assert_eq!(
            w.cashu_discriminator(),
            Some(CashuDiscriminator::DleqVerificationFailed),
            "got {w:?}"
        );
        assert_eq!(w.retry_policy(), RetryPolicy::Never);
    }

    #[test]
    fn p0_3_insufficient_balance_is_named_signal_and_never_retry() {
        let w: WalletError = MeltQuoteError::InsufficientBalance {
            needed: "100".into(),
            have: "50".into(),
        }
        .into();
        assert_eq!(
            w.cashu_discriminator(),
            Some(CashuDiscriminator::InsufficientBalance),
            "got {w:?}"
        );
        assert_eq!(w.retry_policy(), RetryPolicy::Never);
    }

    #[test]
    fn p0_3_undiscriminated_cashu_error_has_no_discriminator() {
        let w: WalletError = MeltQuoteError::MeltFailed("mint said no".into()).into();
        assert_eq!(w.cashu_discriminator(), None, "got {w:?}");
    }
}
