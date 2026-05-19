//! [`ExchangeRateProvider`] trait + error type.
//!
//! The trait is intentionally narrow — caching, retries, and fallback live in
//! higher layers. Implementations are expected to be cheap to clone
//! (`Arc`-wrapped HTTP clients or stateless types).

use agicash_domain::Currency;
use async_trait::async_trait;
use rust_decimal::Decimal;

/// Marker bound alias — `Send + Sync` on native, empty on wasm. Lets
/// `ExchangeRateProvider` carry the right bound for each target without
/// duplicating every trait method behind `cfg`. Mirrors
/// `KeyProviderBounds` (`agicash-traits/src/key_provider.rs`) — required
/// because `agicash-wallet` (which deps this crate) must wasm-build, and
/// wasm `reqwest`'s `Response` future is not `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub trait ExchangeRateProviderBounds: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync> ExchangeRateProviderBounds for T {}

#[cfg(target_arch = "wasm32")]
pub trait ExchangeRateProviderBounds {}
#[cfg(target_arch = "wasm32")]
impl<T> ExchangeRateProviderBounds for T {}

/// Provides current exchange rates between currency pairs.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ExchangeRateProvider: ExchangeRateProviderBounds {
    /// Returns the rate as: `1 unit of `from` major-currency` =
    /// `<result> units of `to` major-currency`.
    ///
    /// E.g. `get_rate(Currency::Btc, Currency::Usd)` returns the BTC->USD
    /// price (~50000 today).
    ///
    /// Returns [`ExchangeRateError::UnsupportedPair`] if the provider does
    /// not support the requested pair.
    async fn get_rate(&self, from: Currency, to: Currency) -> Result<Decimal, ExchangeRateError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("unsupported pair: {from} -> {to}")]
    UnsupportedPair { from: Currency, to: Currency },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_variants_construct() {
        let _ = ExchangeRateError::Network("timeout".into());
        let _ = ExchangeRateError::InvalidResponse("not json".into());
        let _ = ExchangeRateError::UnsupportedPair {
            from: Currency::Btc,
            to: Currency::Usd,
        };
    }
}
