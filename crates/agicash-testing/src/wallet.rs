//! `TestWallet` — an all-fakes `Arc<WalletClient>` for the Tier 1
//! hermetic suite.
//!
//! Composes the public `WalletClientBuilder` from the in-memory fakes
//! (`FakeAuthClient` + `InMemoryUserStorage` + `StubCashuProvider` + the
//! four in-memory cashu storages + `FixedExchangeRate`). No network, no
//! docker, no spawned services — wasm32-clean.
//!
//! NOTE: the plan filed this type under `src/harness/wallet.rs`. Since the
//! Tier 2 `harness` module (real `cdk-mintd`/enclave, Tasks 4-7) is out of
//! scope for this deliverable, the all-fakes Tier 1 `TestWallet` lives in
//! its own unconditional module instead of under an otherwise-absent
//! `harness/` tree. Functionally identical; re-exported unconditionally.

use crate::fakes::auth::FakeAuthClient;
use crate::fakes::cashu_storage::{
    InMemoryMeltQuoteStorage, InMemoryMintQuoteStorage, InMemoryReceiveSwapStorage,
    InMemorySendSwapStorage,
};
use crate::fakes::misc::{FixedExchangeRate, StubCashuProvider};
use crate::fakes::user_storage::InMemoryUserStorage;
use agicash_wallet::{AuthClient, WalletClient, WalletClientBuilder};
use std::sync::Arc;

/// An all-fakes wallet plus owned handles to the fakes so a test can seed
/// state (`user_storage().insert_account(...)`) and assert against them.
#[derive(Debug)]
pub struct TestWallet {
    wallet: Arc<WalletClient>,
    auth: Arc<FakeAuthClient>,
    user_storage: Arc<InMemoryUserStorage>,
    send_storage: Arc<InMemorySendSwapStorage>,
    melt_storage: Arc<InMemoryMeltQuoteStorage>,
}

impl TestWallet {
    /// Logged-out wallet. Any `require_session`-guarded facade method
    /// short-circuits to `WalletError::Unauthenticated`.
    #[must_use]
    pub fn new() -> Self {
        Self::build(Arc::new(FakeAuthClient::new()))
    }

    /// Pre-sessioned wallet. The session's `user_id` is available via
    /// [`Self::session_user_id`] for ownership-mismatch tests.
    #[must_use]
    pub fn logged_in() -> Self {
        Self::build(Arc::new(FakeAuthClient::logged_in()))
    }

    fn build(auth: Arc<FakeAuthClient>) -> Self {
        let user_storage = Arc::new(InMemoryUserStorage::new());
        let send_storage = Arc::new(InMemorySendSwapStorage::new());
        let melt_storage = Arc::new(InMemoryMeltQuoteStorage::new());
        let receive_storage = Arc::new(InMemoryReceiveSwapStorage::new());
        let mint_quote_storage = Arc::new(InMemoryMintQuoteStorage::new());

        let wallet = WalletClientBuilder::new()
            .auth(Arc::clone(&auth) as Arc<dyn AuthClient>)
            .user_storage(Arc::clone(&user_storage) as Arc<_>)
            .cashu_provider(Arc::new(StubCashuProvider) as Arc<_>)
            .cashu_receive_storage(receive_storage as Arc<_>)
            .cashu_send_storage(Arc::clone(&send_storage) as Arc<_>)
            .cashu_mint_quote_storage(mint_quote_storage as Arc<_>)
            .cashu_melt_quote_storage(Arc::clone(&melt_storage) as Arc<_>)
            .exchange_rate(Arc::new(FixedExchangeRate) as Arc<_>)
            .build()
            .expect("all required deps wired");

        Self {
            wallet,
            auth,
            user_storage,
            send_storage,
            melt_storage,
        }
    }

    /// The composed facade under test.
    #[must_use]
    pub fn wallet(&self) -> &Arc<WalletClient> {
        &self.wallet
    }

    /// The fake auth client (read its `user_id()` for ownership tests).
    #[must_use]
    pub fn auth(&self) -> &Arc<FakeAuthClient> {
        &self.auth
    }

    /// The current session's `user_id`, or `None` when logged out.
    #[must_use]
    pub fn session_user_id(&self) -> Option<agicash_domain::UserId> {
        self.auth.user_id()
    }

    /// The in-memory user/account storage (seed accounts here).
    #[must_use]
    pub fn user_storage(&self) -> &Arc<InMemoryUserStorage> {
        &self.user_storage
    }

    /// The in-memory send-swap storage (`fund_account` lives here).
    #[must_use]
    pub fn send_storage(&self) -> &Arc<InMemorySendSwapStorage> {
        &self.send_storage
    }

    /// The in-memory melt-quote storage (fault injection lives here).
    #[must_use]
    pub fn melt_storage(&self) -> &Arc<InMemoryMeltQuoteStorage> {
        &self.melt_storage
    }
}

impl Default for TestWallet {
    fn default() -> Self {
        Self::new()
    }
}
