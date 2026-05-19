//! Named-signal taxonomy for the facade error surface (slice 12b-2).
//!
//! Two C-like discriminator enums consumers branch on instead of
//! string-sniffing flattened error messages:
//! - [`AuthErrorCode`] — restores the FFI `auth_code` distinction iOS
//!   uses to keep the signed-in UI alive on a transient blip vs. tear
//!   it down only on genuine expiry (P0-2).
//! - [`CashuDiscriminator`] — restores the `DUPLICATE_PAYMENT` /
//!   `DLEQ verification failed` named signals consumers must branch on
//!   (P0-3).

/// Structured auth-error discriminator (P0-2). Mirrors the FFI
/// `crate::error::auth_code` distinctions iOS depends on
/// (`crates/agicash-ffi/src/error.rs` @ canonical ref). `Network` and
/// `Unauthenticated` are intentionally absent — they are already
/// first-class `WalletError::{Network, Unauthenticated}` variants; the
/// auth code only carries the two cases that otherwise collapse into a
/// stringly-typed `Auth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorCode {
    /// Auth backend failure (OpenSecret 5xx, signup/refresh rejected).
    /// Transient from the consumer's POV — keep the signed-in UI alive.
    Backend,
    /// Internal / programmer error in the auth path.
    Internal,
}

/// Named cashu signal a consumer branches on (P0-3). Restores the FFI
/// prefix discriminators (`DUPLICATE_PAYMENT:`, `DLEQ verification
/// failed:`, `insufficient balance`, `quote expired before payment`)
/// the bespoke-FFI `melt_quote_error_to_ffi` synthesized
/// (`crates/agicash-ffi/src/wallet.rs` @ canonical ref).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CashuDiscriminator {
    /// An active melt quote already exists for this invoice's payment
    /// hash. Resume the existing quote / show its receipt — do **NOT**
    /// re-quote (re-quoting fires a second `post_melt` → double-pay).
    DuplicatePayment,
    /// NUT-12 DLEQ verification failed on a mint-returned blind
    /// signature. The mint is malicious or compromised.
    DleqVerificationFailed,
    /// Proof balance can't cover amount + fees.
    InsufficientBalance,
    /// Invoice/quote expired before the payment was initiated.
    QuoteExpired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_code_variants_are_distinct() {
        assert_ne!(AuthErrorCode::Backend, AuthErrorCode::Internal);
    }

    #[test]
    fn cashu_discriminator_variants_are_distinct() {
        let all = [
            CashuDiscriminator::DuplicatePayment,
            CashuDiscriminator::DleqVerificationFailed,
            CashuDiscriminator::InsufficientBalance,
            CashuDiscriminator::QuoteExpired,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b, "{a:?} vs {b:?}");
            }
        }
    }
}
