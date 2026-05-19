//! `RealWallet` — the Tier 2 real-service `Arc<WalletClient>`.
//!
//! Composed via the public [`WalletClientBuilder`] (NOT
//! `WalletClient::from_config`, which hardwires `SupabaseStorage` — the
//! docker-managed transport Tier 2 deliberately does not exercise):
//!
//! | Seam            | Tier 2 wiring                                     |
//! |-----------------|---------------------------------------------------|
//! | Auth            | **REAL** `OpenSecretAuthClient` → spawned/attached enclave |
//! | Mint            | **REAL** `CdkCashuProvider` → harness-spawned `cdk-mintd` |
//! | Storage         | **FAKE** in-memory (docker-wedge constraint)      |
//! | Exchange rate   | `FixedExchangeRate` (rate is not under test)      |
//!
//! `CdkCashuProvider` resolves the mint URL from `account.details.mint_url`
//! at call time — so pointing it at the spawned mint is just seeding an
//! account whose `mint_url` is the spawned URL (see [`Self::cashu_account`]).

use std::sync::Arc;

use agicash_auth_opensecret::{InMemorySessionStorage, OpenSecretClient, OpenSecretConfig};
use agicash_cashu::CdkCashuProvider;
use agicash_domain::{Account, Currency, UserId};
use agicash_wallet::{AuthClient, OpenSecretAuthClient, WalletClient, WalletClientBuilder};

use crate::fakes::cashu_storage::{
    InMemoryMeltQuoteStorage, InMemoryMintQuoteStorage, InMemoryReceiveSwapStorage,
    InMemorySendSwapStorage,
};
use crate::fakes::misc::FixedExchangeRate;
use crate::fakes::user_storage::{cashu_account, InMemoryUserStorage};

/// A real-service wallet (real `OpenSecret` auth + real `cdk-mintd`) over
/// in-memory storage, plus owned handles to the in-memory stores so a
/// Tier 2 test can seed accounts and fault-inject the melt storage.
#[derive(Debug)]
pub struct RealWallet {
    wallet: Arc<WalletClient>,
    auth: Arc<OpenSecretAuthClient>,
    mint_url: String,
    user_storage: Arc<InMemoryUserStorage>,
    send_storage: Arc<InMemorySendSwapStorage>,
    receive_storage: Arc<InMemoryReceiveSwapStorage>,
    mint_quote_storage: Arc<InMemoryMintQuoteStorage>,
    melt_storage: Arc<InMemoryMeltQuoteStorage>,
}

impl RealWallet {
    /// Compose the real wallet. `enclave_url` is the
    /// spawned/attached enclave (`http://127.0.0.1:3999`);
    /// `client_id` is the pre-seeded `Maple` project; `mint_url` is the
    /// harness-spawned `cdk-mintd` base URL.
    pub(super) fn compose(
        enclave_url: &str,
        client_id: &str,
        mint_url: &str,
    ) -> Result<Self, String> {
        let os_config = OpenSecretConfig {
            base_url: enclave_url.to_string(),
            client_id: client_id
                .parse()
                .map_err(|e| format!("RealWallet: bad client_id uuid {client_id}: {e}"))?,
        };
        let os_client = OpenSecretClient::new(os_config)
            .map_err(|e| format!("RealWallet: OpenSecretClient::new failed: {e}"))?;
        let session_storage = Arc::new(InMemorySessionStorage::new());
        let auth = Arc::new(OpenSecretAuthClient::new(os_client, session_storage));

        // Real CDK provider — its `wallet_for_account` reads
        // `account.details.mint_url` per call, so it talks to whatever
        // mint the seeded account points at (the spawned cdk-mintd).
        let cashu_provider: Arc<dyn agicash_traits::CashuProvider> =
            Arc::new(CdkCashuProvider::new());

        let user_storage = Arc::new(InMemoryUserStorage::new());
        let send_storage = Arc::new(InMemorySendSwapStorage::new());
        let receive_storage = Arc::new(InMemoryReceiveSwapStorage::new());
        let mint_quote_storage = Arc::new(InMemoryMintQuoteStorage::new());
        let melt_storage = Arc::new(InMemoryMeltQuoteStorage::new());

        let wallet = WalletClientBuilder::new()
            .auth(Arc::clone(&auth) as Arc<dyn AuthClient>)
            .user_storage(Arc::clone(&user_storage) as Arc<_>)
            .cashu_provider(Arc::clone(&cashu_provider))
            .cashu_receive_storage(Arc::clone(&receive_storage) as Arc<_>)
            .cashu_send_storage(Arc::clone(&send_storage) as Arc<_>)
            .cashu_mint_quote_storage(Arc::clone(&mint_quote_storage) as Arc<_>)
            .cashu_melt_quote_storage(Arc::clone(&melt_storage) as Arc<_>)
            .exchange_rate(Arc::new(FixedExchangeRate) as Arc<_>)
            .build()
            .map_err(|e| format!("RealWallet: WalletClientBuilder::build failed: {e}"))?;

        Ok(Self {
            wallet,
            auth,
            mint_url: mint_url.to_string(),
            user_storage,
            send_storage,
            receive_storage,
            mint_quote_storage,
            melt_storage,
        })
    }

    /// The composed facade under test.
    #[must_use]
    pub fn wallet(&self) -> &Arc<WalletClient> {
        &self.wallet
    }

    /// The real `OpenSecret` auth client (read its session after
    /// `auth_guest`).
    #[must_use]
    pub fn auth(&self) -> &Arc<OpenSecretAuthClient> {
        &self.auth
    }

    /// Build a Cashu `Account` whose `mint_url` is the harness-spawned
    /// mint, owned by `user_id`. Seed it with [`Self::seed_account`].
    #[must_use]
    pub fn cashu_account(&self, user_id: UserId, currency: Currency) -> Account {
        cashu_account(user_id, &self.mint_url, currency)
    }

    /// Seed an account into the in-memory user storage (so
    /// `list_accounts`/`get_account` and the cashu services see it).
    pub fn seed_account(&self, account: Account) {
        self.user_storage.insert_account(account);
    }

    /// The harness-spawned mint base URL.
    #[must_use]
    pub fn mint_url(&self) -> &str {
        &self.mint_url
    }

    #[must_use]
    pub fn user_storage(&self) -> &Arc<InMemoryUserStorage> {
        &self.user_storage
    }

    #[must_use]
    pub fn send_storage(&self) -> &Arc<InMemorySendSwapStorage> {
        &self.send_storage
    }

    #[must_use]
    pub fn receive_storage(&self) -> &Arc<InMemoryReceiveSwapStorage> {
        &self.receive_storage
    }

    #[must_use]
    pub fn mint_quote_storage(&self) -> &Arc<InMemoryMintQuoteStorage> {
        &self.mint_quote_storage
    }

    /// The in-memory melt-quote storage — fault-inject its `complete()`
    /// here for the P0 no-double-pay vector
    /// (`melt_storage().arm_complete_failure(1)`).
    #[must_use]
    pub fn melt_storage(&self) -> &Arc<InMemoryMeltQuoteStorage> {
        &self.melt_storage
    }
}
