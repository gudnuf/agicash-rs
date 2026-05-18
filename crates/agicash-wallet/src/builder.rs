//! Builder for `WalletClient`.
//!
//! Per the slice-12 plan §3 — the facade composition root is fluent and
//! takes pre-built `Arc<dyn Trait>` provider/storage handles. The
//! concrete wiring (which keyring impl, which Supabase URL) is the
//! consumer's job: CLI's `composition.rs` builds Supabase + OpenSecret
//! and hands them in; FFI's `AgicashWallet::new` does the same; the
//! Leptos PWA wires SSR proxies; the MCP server wires whatever the host
//! decides.
//!
//! Required deps:
//! - `auth` — `Arc<dyn AuthClient>` (see `crate::auth`)
//! - `user_storage` — `Arc<dyn UserStorage>` from `agicash-traits`
//! - `cashu_provider` — `Arc<dyn CashuProvider>` (CDK-backed in production)
//! - `cashu_receive_storage`, `cashu_send_storage`,
//!   `cashu_mint_quote_storage`, `cashu_melt_quote_storage` — the four
//!   storage trait handles each cashu state machine needs.
//!
//! Optional deps:
//! - `exchange_rate` — defaults to `MempoolSpaceProvider` on native
//!   targets. On wasm targets `MempoolSpaceProvider` isn't usable
//!   directly (reqwest's wasm Send-bound mismatch); callers must
//!   provide their own impl. Defaulting is gated to non-wasm to keep
//!   the facade wasm-friendly.
//! - `lightning_address_client` — defaults to the resolver crate's
//!   `Client::default()` (uses `reqwest::Client::new()`), wasm-safe.
//!
//! `build()` is fallible — if any required dep is missing, a
//! `WalletError::Validation` is returned with the missing-dep name.

use crate::auth::AuthClient;
use crate::client::WalletClient;
use crate::error::WalletError;
use agicash_cashu::{
    CashuMeltQuoteService, CashuMeltQuoteStorage, CashuMintQuoteService, CashuMintQuoteStorage,
    CashuReceiveSwapService, CashuReceiveSwapStorage, CashuSendSwapService, CashuSendSwapStorage,
};
use agicash_exchange_rate::ExchangeRateProvider;
use agicash_traits::{CashuProvider, UserStorage};
use std::sync::Arc;

/// Fluent builder. See the module doc for required vs optional fields.
#[derive(Default)]
pub struct WalletClientBuilder {
    auth: Option<Arc<dyn AuthClient>>,
    user_storage: Option<Arc<dyn UserStorage>>,
    cashu_provider: Option<Arc<dyn CashuProvider>>,
    cashu_receive_storage: Option<Arc<dyn CashuReceiveSwapStorage>>,
    cashu_send_storage: Option<Arc<dyn CashuSendSwapStorage>>,
    cashu_mint_quote_storage: Option<Arc<dyn CashuMintQuoteStorage>>,
    cashu_melt_quote_storage: Option<Arc<dyn CashuMeltQuoteStorage>>,
    exchange_rate: Option<Arc<dyn ExchangeRateProvider>>,
}

impl std::fmt::Debug for WalletClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletClientBuilder")
            .field("auth_set", &self.auth.is_some())
            .field("user_storage_set", &self.user_storage.is_some())
            .field("cashu_provider_set", &self.cashu_provider.is_some())
            .field(
                "cashu_receive_storage_set",
                &self.cashu_receive_storage.is_some(),
            )
            .field("cashu_send_storage_set", &self.cashu_send_storage.is_some())
            .field(
                "cashu_mint_quote_storage_set",
                &self.cashu_mint_quote_storage.is_some(),
            )
            .field(
                "cashu_melt_quote_storage_set",
                &self.cashu_melt_quote_storage.is_some(),
            )
            .field("exchange_rate_set", &self.exchange_rate.is_some())
            .finish()
    }
}

impl WalletClientBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn auth(mut self, auth: Arc<dyn AuthClient>) -> Self {
        self.auth = Some(auth);
        self
    }

    #[must_use]
    pub fn user_storage(mut self, storage: Arc<dyn UserStorage>) -> Self {
        self.user_storage = Some(storage);
        self
    }

    #[must_use]
    pub fn cashu_provider(mut self, provider: Arc<dyn CashuProvider>) -> Self {
        self.cashu_provider = Some(provider);
        self
    }

    #[must_use]
    pub fn cashu_receive_storage(mut self, storage: Arc<dyn CashuReceiveSwapStorage>) -> Self {
        self.cashu_receive_storage = Some(storage);
        self
    }

    #[must_use]
    pub fn cashu_send_storage(mut self, storage: Arc<dyn CashuSendSwapStorage>) -> Self {
        self.cashu_send_storage = Some(storage);
        self
    }

    #[must_use]
    pub fn cashu_mint_quote_storage(mut self, storage: Arc<dyn CashuMintQuoteStorage>) -> Self {
        self.cashu_mint_quote_storage = Some(storage);
        self
    }

    #[must_use]
    pub fn cashu_melt_quote_storage(mut self, storage: Arc<dyn CashuMeltQuoteStorage>) -> Self {
        self.cashu_melt_quote_storage = Some(storage);
        self
    }

    #[must_use]
    pub fn exchange_rate(mut self, provider: Arc<dyn ExchangeRateProvider>) -> Self {
        self.exchange_rate = Some(provider);
        self
    }

    /// Assemble the `WalletClient`. Fails with a `Validation` error if any
    /// required dep is missing — see module doc for the list.
    pub fn build(self) -> Result<Arc<WalletClient>, WalletError> {
        let auth = self
            .auth
            .ok_or_else(|| WalletError::validation("missing_dep", "auth"))?;
        let user_storage = self
            .user_storage
            .ok_or_else(|| WalletError::validation("missing_dep", "user_storage"))?;
        let cashu_provider = self
            .cashu_provider
            .ok_or_else(|| WalletError::validation("missing_dep", "cashu_provider"))?;
        let cashu_receive_storage = self
            .cashu_receive_storage
            .ok_or_else(|| WalletError::validation("missing_dep", "cashu_receive_storage"))?;
        let cashu_send_storage = self
            .cashu_send_storage
            .ok_or_else(|| WalletError::validation("missing_dep", "cashu_send_storage"))?;
        let cashu_mint_quote_storage = self
            .cashu_mint_quote_storage
            .ok_or_else(|| WalletError::validation("missing_dep", "cashu_mint_quote_storage"))?;
        let cashu_melt_quote_storage = self
            .cashu_melt_quote_storage
            .ok_or_else(|| WalletError::validation("missing_dep", "cashu_melt_quote_storage"))?;

        let receive_swap_service = Arc::new(CashuReceiveSwapService::new(
            Arc::clone(&cashu_receive_storage),
            Arc::clone(&cashu_provider),
        ));
        let send_swap_service = Arc::new(CashuSendSwapService::new(
            Arc::clone(&cashu_send_storage),
            Arc::clone(&cashu_provider),
        ));
        let mint_quote_service = Arc::new(CashuMintQuoteService::new(
            Arc::clone(&cashu_mint_quote_storage),
            Arc::clone(&cashu_provider),
        ));
        let melt_quote_service = Arc::new(CashuMeltQuoteService::new(
            Arc::clone(&cashu_melt_quote_storage),
            Arc::clone(&cashu_provider),
        ));

        Ok(Arc::new(WalletClient {
            auth,
            user_storage,
            cashu_provider,
            cashu_send_storage,
            cashu_mint_quote_storage,
            cashu_melt_quote_storage,
            receive_swap_service,
            send_swap_service,
            mint_quote_service,
            melt_quote_service,
            exchange_rate: self.exchange_rate,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_without_auth_returns_validation_error() {
        let builder = WalletClientBuilder::new();
        let err = builder.build().expect_err("missing deps");
        match err {
            WalletError::Validation { code, message } => {
                assert_eq!(code, "missing_dep");
                assert_eq!(message, "auth");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
