//! Misc Tier 1 fakes: a constant exchange-rate provider and a stub cashu
//! provider whose mint methods are intentionally unreached.
//!
//! The Tier 1 regressions (M1 wrong-owner, 12c-3 unauthenticated,
//! 12c-7 `receive_flow()` constructibility) short-circuit at the
//! auth/ownership seam before any mint round-trip, so the provider only
//! has to *exist and compose* — it never has to talk to a mint.

use agicash_domain::{Account, Currency};
use agicash_traits::{CashuProvider, CashuMintWallet, CashuProviderError};
use async_trait::async_trait;
use agicash_exchange_rate::{ExchangeRateError, ExchangeRateProvider};
use cdk::mint_url::MintUrl;
use cdk::nuts::MintInfo;
use rust_decimal::Decimal;
use std::sync::Arc;

/// Returns a constant BTC->USD rate. No network. Rate is never the thing
/// under test in Tier 1.
#[derive(Debug, Default)]
pub struct FixedExchangeRate;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ExchangeRateProvider for FixedExchangeRate {
    async fn get_rate(
        &self,
        from: Currency,
        to: Currency,
    ) -> Result<Decimal, ExchangeRateError> {
        match (from, to) {
            (Currency::Btc, Currency::Usd) => Ok(Decimal::from(50_000)),
            (Currency::Usd, Currency::Btc) => Ok(Decimal::ONE / Decimal::from(50_000)),
            (a, b) if a == b => Ok(Decimal::ONE),
            (from, to) => Err(ExchangeRateError::UnsupportedPair { from, to }),
        }
    }
}

/// A [`CashuProvider`] whose mint-touching methods always error. Wired into
/// the Tier 1 builder so the facade composes, but never actually reached:
/// M1 and 12c-3 return at the auth/ownership guard; 12c-7 only constructs
/// `ReceiveFlowService` (no I/O at construction).
#[derive(Debug, Default)]
pub struct StubCashuProvider;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CashuProvider for StubCashuProvider {
    async fn wallet_for_account(
        &self,
        _account: &Account,
    ) -> Result<Arc<CashuMintWallet>, CashuProviderError> {
        Err(CashuProviderError::Network(
            "StubCashuProvider: unused in Tier 1 (auth/ownership short-circuits first)".into(),
        ))
    }

    async fn mint_info(&self, _mint_url: &MintUrl) -> Result<MintInfo, CashuProviderError> {
        Err(CashuProviderError::Network(
            "StubCashuProvider: unused in Tier 1 (auth/ownership short-circuits first)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixed_rate_btc_usd_is_constant() {
        let r = FixedExchangeRate;
        assert_eq!(
            r.get_rate(Currency::Btc, Currency::Usd).await.unwrap(),
            Decimal::from(50_000)
        );
        assert_eq!(
            r.get_rate(Currency::Btc, Currency::Btc).await.unwrap(),
            Decimal::ONE
        );
    }

    #[tokio::test]
    async fn stub_provider_wallet_for_account_errs() {
        let p = StubCashuProvider;
        let a = crate::fakes::user_storage::cashu_account(
            agicash_domain::UserId::new(),
            "https://mint.example",
            Currency::Btc,
        );
        assert!(matches!(
            p.wallet_for_account(&a).await,
            Err(CashuProviderError::Network(_))
        ));
    }
}
