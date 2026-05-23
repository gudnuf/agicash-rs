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

use crate::auth::{AuthClient, Session};
use crate::cache::WalletCache;
use crate::client::{TokenProviderArc, WalletClient};
use crate::config::{SessionStorageChoice, WalletConfig};
use crate::error::WalletError;
use crate::opensecret_auth::OpenSecretAuthClient;
use agicash_auth_opensecret::{
    InMemorySessionStorage, OpenSecretClient, OpenSecretConfig, OpenSecretTokenProvider,
};
use agicash_cashu::{
    CashuMeltQuoteService, CashuMeltQuoteStorage, CashuMintQuoteService, CashuMintQuoteStorage,
    CashuReceiveSwapService, CashuReceiveSwapStorage, CashuSendSwapService, CashuSendSwapStorage,
    CdkCashuProvider,
};
use agicash_exchange_rate::ExchangeRateProvider;
use agicash_storage_supabase::{
    SupabaseCashuMeltQuoteStorage, SupabaseCashuMintQuoteStorage, SupabaseCashuReceiveSwapStorage,
    SupabaseCashuSendSwapStorage, SupabaseStorage, SupabaseStorageConfig,
};
use agicash_traits::{
    CashuProvider, PassthroughProofEncryption, ProofEncryption, SessionStorage, UserStorage,
};
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
    /// Proof-encryption handle used by the cache layer to decrypt
    /// `encrypted_data` on incoming realtime `Change` payloads.
    /// Defaults to `PassthroughProofEncryption` if not set — that
    /// matches what `from_config` wires for the storage impls.
    /// MUST be the same instance the storage impls were built with;
    /// otherwise the cache's `to_cashu_*` conversions yield garbage.
    encryption: Option<Arc<dyn ProofEncryption>>,
    /// Shared JWT provider stashed on the built `WalletClient` and
    /// returned by [`crate::WalletClient::token_provider`]. Production
    /// `from_config` populates this with the SAME `Arc` it hands to
    /// `SupabaseStorage` (one auth surface per wallet — closes the
    /// "parallel shell-resident `OpenSecretClient`" defect in the
    /// session-loading audit). Test harnesses may leave it unset (the
    /// builder will simply build a wallet whose `token_provider()`
    /// returns `None`).
    token_provider: Option<TokenProviderArc>,
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
            .field("encryption_set", &self.encryption.is_some())
            .field("token_provider_set", &self.token_provider.is_some())
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

    /// Provide the proof-encryption handle the cache layer should use
    /// when folding incoming realtime `Change` payloads. Must be the
    /// same instance the storage impls were built with.
    ///
    /// If unset, the cache uses
    /// [`agicash_traits::PassthroughProofEncryption`] (matches what
    /// [`crate::WalletClient::from_config`] wires for storage).
    #[must_use]
    pub fn encryption(mut self, encryption: Arc<dyn ProofEncryption>) -> Self {
        self.encryption = Some(encryption);
        self
    }

    /// Provide the shared JWT provider that
    /// [`crate::WalletClient::token_provider`] should return. Production
    /// `from_config` calls this with the SAME `Arc` it builds for
    /// `SupabaseStorage` so the facade exposes exactly one auth surface.
    /// Test harnesses can pass a stub `TokenProvider` here (the fake
    /// storage paths never reach `.get_jwt()`).
    #[must_use]
    pub fn token_provider(mut self, provider: TokenProviderArc) -> Self {
        self.token_provider = Some(provider);
        self
    }

    /// Assemble the `WalletClient`. Fails with a `Validation` error if any
    /// required dep is missing — see module doc for the list.
    ///
    /// # Harness invariant — explicit `set_session` only
    ///
    /// Unlike [`WalletClient::from_config_async`], the builder does NOT
    /// call `.load()` on any session storage. Callers (test harnesses,
    /// bespoke composition roots) MUST drive the auth client's session
    /// themselves via [`crate::WalletClient::set_session`] or one of the
    /// `auth_*` register/login methods. This keeps the builder a pure
    /// dep-injection seam — no implicit I/O — and matches what the test
    /// harness at `agicash-testing/src/harness/wallet.rs` already does
    /// (it calls `auth_guest`/`set_session` post-construction). The
    /// auto-load path is reserved for the production composition root
    /// (`from_config_async`).
    // `WalletClient` and its service handles are stored as `Arc<dyn …>` /
    // `Arc<T>` uniformly across native and wasm32. On wasm32 the inner
    // storage/provider types are `!Send`/`!Sync` (single-threaded), but the
    // `Arc` type is structural API surface — switching to `Rc` on wasm32
    // would require cfg-gating every struct field and public signature.
    // The `Arc` is deliberately uniform across cfgs; allow narrowly here.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
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

        // The cache layer holds an Arc<dyn ProofEncryption> so it can
        // call the storage `to_cashu_*` helpers on incoming realtime
        // `Change` payloads. Defaults to passthrough — matches what
        // `from_config` wires. Builder callers that use a non-trivial
        // encryption MUST call `.encryption(...)` to keep this aligned.
        let encryption: Arc<dyn ProofEncryption> = self
            .encryption
            .unwrap_or_else(|| Arc::new(PassthroughProofEncryption));
        let cache = WalletCache::new(Arc::clone(&encryption));

        Ok(Arc::new(WalletClient {
            auth,
            user_storage,
            cashu_provider,
            cashu_receive_storage,
            cashu_send_storage,
            cashu_mint_quote_storage,
            cashu_melt_quote_storage,
            receive_swap_service,
            send_swap_service,
            mint_quote_service,
            melt_quote_service,
            exchange_rate: self.exchange_rate,
            token_provider: self.token_provider,
            cache,
        }))
    }
}

/// Result of `WalletClient::from_config` / `from_config_async`. Returns
/// the composed wallet plus the `OpenSecretAuthClient` so binding shells
/// can mirror the session slot into shell-resident plumbing (spec §6 —
/// platform-layer carve-out for FFI / Leptos realtime).
type FromConfigOutput = (Arc<WalletClient>, Arc<OpenSecretAuthClient>);

impl WalletClient {
    /// **Legacy / synchronous** composition root — preserved as the
    /// drop-in API for the per-shell migration lanes that haven't yet
    /// moved to [`Self::from_config_async`].
    ///
    /// Identical to `from_config_async` minus the auto-load step: the
    /// returned wallet's session slot is empty even if `cfg.session_storage`
    /// already has a persisted session blob. Callers that need auto-load
    /// (every production shell, eventually) should call
    /// [`Self::from_config_async`] instead.
    ///
    /// Kept sync for now to preserve the existing shell compile surface
    /// while per-shell async migrations land in follow-up lanes. The doc
    /// reference is the 2026-05-22 `session-loading-full-shape.md` plan,
    /// lanes 3-5.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub fn from_config(cfg: WalletConfig) -> Result<FromConfigOutput, WalletError> {
        let composed = compose_from_config(cfg)?;
        Ok((composed.wallet, composed.auth))
    }

    /// **THE** single composition root for binding shells. Identical to
    /// `from_config` plus a [`SessionStorage::load`] + [`AuthClient::set_session`]
    /// step on the way out — so a wallet constructed with a non-empty
    /// `Keyring` / `Android` / `Browser` / `Custom` storage comes back
    /// already authenticated, with no shell-side manual two-step.
    ///
    /// # Failure policy
    ///
    /// If `.load()` returns `Some(session)` and `set_session` then fails
    /// (stale refresh token, network), this propagates the error
    /// (operator decision, 2026-05-22 — caller decides whether to clear
    /// the persisted blob + route the user to login).
    ///
    /// On `.load()` returning `None`, returns the composed wallet with
    /// an empty session — same shape as `from_config`.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub async fn from_config_async(cfg: WalletConfig) -> Result<FromConfigOutput, WalletError> {
        let composed = compose_from_config(cfg)?;

        // Auto-load: if the storage already has a persisted session,
        // route it through `set_session` so the in-memory slot AND the
        // shared `OpenSecretClient`'s tokens are seeded before the
        // wallet is returned. Errors propagate per operator decision.
        if let Some(persisted) = composed
            .session_storage
            .load()
            .await
            .map_err(WalletError::from)?
        {
            let session = Session {
                user_id: agicash_domain::UserId::from(persisted.user_id),
                refresh_token: persisted.refresh_token,
            };
            composed.auth.set_session(session).await?;
        }

        Ok((composed.wallet, composed.auth))
    }
}

/// Pieces produced by the shared composition core consumed by both
/// `from_config` and `from_config_async`. The session storage handle is
/// returned alongside the auth client so `from_config_async` can call
/// `.load()` on it without a second resolution of the
/// `SessionStorageChoice` enum.
struct ComposedFromConfig {
    wallet: Arc<WalletClient>,
    auth: Arc<OpenSecretAuthClient>,
    session_storage: Arc<dyn SessionStorage>,
}

/// Shared composition core for `from_config` + `from_config_async`. Wires
/// OpenSecret + Supabase + CDK cashu services + passthrough encryption
/// identically to the bespoke FFI `AgicashWallet::new` it replaces, then
/// routes through [`WalletClientBuilder`]. No network I/O — the
/// `from_config_async` caller layers `.load()`/`set_session` on top.
// See `build` above: `Arc` is the uniform handle type across native and
// wasm32; on wasm32 the inner types are `!Send`/`!Sync` but the `Arc` is
// structural API surface (this fn's return type is `Arc<…>`).
#[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
fn compose_from_config(cfg: WalletConfig) -> Result<ComposedFromConfig, WalletError> {
    let auth_cfg = OpenSecretConfig {
        base_url: cfg.opensecret_url,
        client_id: cfg.opensecret_client_id,
    };
    let client = OpenSecretClient::new(auth_cfg).map_err(|e| WalletError::Auth {
        code: crate::discriminator::AuthErrorCode::Internal,
        message: e.to_string(),
    })?;

    let storage_cfg = SupabaseStorageConfig {
        url: cfg.supabase_url,
        anon_key: cfg.supabase_anon_key,
    };
    // ONE shared token provider — also stashed on the WalletClient so
    // `wallet.token_provider()` returns this exact `Arc` (closes the
    // parallel-shell-client defect; see audit 2026-05-22). Cfg-gated
    // bound mirrors `SupabaseStorage::new`'s own cfg.
    let token_provider: TokenProviderArc = Arc::new(OpenSecretTokenProvider::new(client.clone()));
    let storage = Arc::new(
        SupabaseStorage::new(storage_cfg, Arc::clone(&token_provider))
            .map_err(|e| WalletError::Storage(e.to_string()))?,
    );

    let cashu_provider: Arc<dyn CashuProvider> = Arc::new(CdkCashuProvider::new());
    let encryption: Arc<dyn ProofEncryption> = Arc::new(PassthroughProofEncryption);

    let receive_storage = Arc::new(SupabaseCashuReceiveSwapStorage::new(
        Arc::clone(&storage),
        Arc::clone(&encryption),
    ));
    let send_storage = Arc::new(SupabaseCashuSendSwapStorage::new(
        Arc::clone(&storage),
        Arc::clone(&encryption),
    ));
    let mint_quote_storage = Arc::new(SupabaseCashuMintQuoteStorage::new(
        Arc::clone(&storage),
        Arc::clone(&encryption),
    ));
    let melt_quote_storage = Arc::new(SupabaseCashuMeltQuoteStorage::new(
        Arc::clone(&storage),
        Arc::clone(&encryption),
    ));

    let session_storage: Arc<dyn SessionStorage> = match cfg.session_storage {
        SessionStorageChoice::InMemory => Arc::new(InMemorySessionStorage::new()),
        SessionStorageChoice::Android { dir } => {
            #[cfg(all(feature = "android-file-storage", target_os = "android"))]
            {
                Arc::new(agicash_auth_opensecret::AndroidFileSessionStorage::new(dir))
            }
            #[cfg(not(all(feature = "android-file-storage", target_os = "android")))]
            {
                let _ = dir;
                Arc::new(InMemorySessionStorage::new())
            }
        }
        SessionStorageChoice::Browser => {
            #[cfg(target_arch = "wasm32")]
            {
                Arc::new(agicash_auth_opensecret::BrowserSessionStorage::new())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Arc::new(InMemorySessionStorage::new())
            }
        }
        #[cfg(feature = "keyring-storage")]
        SessionStorageChoice::Keyring { service } => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                Arc::new(agicash_auth_opensecret::KeyringSessionStorage::new(service))
            }
            #[cfg(target_arch = "wasm32")]
            {
                // The `keyring-storage` feature does not compile a
                // keyring backend on wasm32 — fall back to in-memory.
                // The cfg-gating in `agicash-auth-opensecret::storage`
                // means the import above wouldn't resolve otherwise.
                let _ = service;
                Arc::new(InMemorySessionStorage::new())
            }
        }
        SessionStorageChoice::Custom(storage) => storage,
    };

    let auth = Arc::new(OpenSecretAuthClient::new(
        client,
        Arc::clone(&session_storage),
    ));

    let wallet = WalletClientBuilder::new()
        .auth(Arc::clone(&auth) as Arc<dyn AuthClient>)
        .user_storage(Arc::clone(&storage) as Arc<dyn UserStorage>)
        .cashu_provider(Arc::clone(&cashu_provider))
        .cashu_receive_storage(receive_storage)
        .cashu_send_storage(send_storage)
        .cashu_mint_quote_storage(mint_quote_storage)
        .cashu_melt_quote_storage(melt_quote_storage)
        // The cache uses the same encryption handle as the storage
        // impls so its row→rich conversions decrypt identically.
        .encryption(Arc::clone(&encryption))
        // Stash the SAME token-provider Arc the storage was built with
        // so `wallet.token_provider()` is the single auth surface for
        // every shell consumer.
        .token_provider(Arc::clone(&token_provider))
        .build()?;

    Ok(ComposedFromConfig {
        wallet,
        auth,
        session_storage,
    })
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

    #[tokio::test]
    async fn from_config_constructs_without_network() {
        // Unresolvable endpoints: from_config must NOT do network I/O at
        // construction (mirrors FFI `constructor_returns_wallet_without_network`).
        let cfg = crate::config::WalletConfig {
            opensecret_url: "https://does-not-resolve-agicash.invalid".into(),
            opensecret_client_id: uuid::Uuid::nil(),
            supabase_url: "https://does-not-resolve-supabase.invalid".into(),
            supabase_anon_key: "anon-key".into(),
            session_storage: crate::config::SessionStorageChoice::InMemory,
        };
        let (wallet, _auth) = crate::WalletClient::from_config(cfg).expect("construct");
        // No session loaded → auth_status reports logged_out, no I/O.
        let status = wallet.auth_status().await.expect("auth_status");
        assert!(!status.logged_in);
        assert!(status.user_id.is_none());
    }

    #[tokio::test]
    async fn from_config_async_with_empty_storage_constructs_without_network() {
        // Empty `InMemory` `.load()` is in-process (returns `None`) — so
        // `from_config_async` against unresolvable endpoints still does
        // NO network I/O. Mirrors the sync `from_config` invariant.
        let cfg = crate::config::WalletConfig {
            opensecret_url: "https://does-not-resolve-agicash.invalid".into(),
            opensecret_client_id: uuid::Uuid::nil(),
            supabase_url: "https://does-not-resolve-supabase.invalid".into(),
            supabase_anon_key: "anon-key".into(),
            session_storage: crate::config::SessionStorageChoice::InMemory,
        };
        let (wallet, _auth) = crate::WalletClient::from_config_async(cfg)
            .await
            .expect("construct");
        let status = wallet.auth_status().await.expect("auth_status");
        assert!(!status.logged_in);
        assert!(status.user_id.is_none());
    }

    /// Test fake `SessionStorage` pre-loaded with a `PersistedSession` —
    /// proves `from_config_async` calls `.load()` and routes the result
    /// through `set_session`. Uses a stub that bypasses the network
    /// `set_session` requires by short-circuiting via a custom
    /// `AuthClient`… except we use the production
    /// `OpenSecretAuthClient`, which `set_session` calls
    /// `ensure_handshake` + `refresh_token` on the live `OpenSecretClient`
    /// — both real network. So we can't assert "comes back authenticated"
    /// without a live enclave (that's Tier 2). Instead we assert the
    /// observable: `.load()` IS being called by `from_config_async`.
    /// Storage with a sentinel error → `from_config_async` returns Err.
    #[tokio::test]
    async fn from_config_async_propagates_load_error() {
        use agicash_traits::{AuthError, PersistedSession, SessionStorage, SessionStorageError};
        use async_trait::async_trait;

        #[derive(Debug)]
        struct FailingLoadStorage;

        #[async_trait]
        impl SessionStorage for FailingLoadStorage {
            async fn store(&self, _session: &PersistedSession) -> Result<(), AuthError> {
                Ok(())
            }
            async fn load(&self) -> Result<Option<PersistedSession>, AuthError> {
                Err(AuthError::from(SessionStorageError::Io(
                    "sentinel: from_config_async must call .load()".into(),
                )))
            }
            async fn clear(&self) -> Result<(), AuthError> {
                Ok(())
            }
        }

        let cfg = crate::config::WalletConfig {
            opensecret_url: "https://does-not-resolve-agicash.invalid".into(),
            opensecret_client_id: uuid::Uuid::nil(),
            supabase_url: "https://does-not-resolve-supabase.invalid".into(),
            supabase_anon_key: "anon-key".into(),
            session_storage: crate::config::SessionStorageChoice::Custom(Arc::new(
                FailingLoadStorage,
            )),
        };
        let err = crate::WalletClient::from_config_async(cfg)
            .await
            .expect_err("auto-load failure must propagate");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("sentinel"),
            "expected propagated load error, got: {msg}"
        );
    }

    /// Empty (no-session) `Custom` storage: `.load()` returns `None`,
    /// `from_config_async` skips `set_session` and returns Ok without
    /// network — same shape as the `InMemory` happy path.
    #[tokio::test]
    async fn from_config_async_with_empty_custom_storage_is_logged_out() {
        use agicash_auth_opensecret::InMemorySessionStorage;
        use agicash_traits::SessionStorage;
        let storage: Arc<dyn SessionStorage> = Arc::new(InMemorySessionStorage::new());
        let cfg = crate::config::WalletConfig {
            opensecret_url: "https://does-not-resolve-agicash.invalid".into(),
            opensecret_client_id: uuid::Uuid::nil(),
            supabase_url: "https://does-not-resolve-supabase.invalid".into(),
            supabase_anon_key: "anon-key".into(),
            session_storage: crate::config::SessionStorageChoice::Custom(storage),
        };
        let (wallet, _auth) = crate::WalletClient::from_config_async(cfg)
            .await
            .expect("construct");
        let status = wallet.auth_status().await.expect("auth_status");
        assert!(!status.logged_in);
    }

    #[tokio::test]
    async fn from_config_populates_token_provider() {
        // `wallet.token_provider()` should return the SAME `Arc` the
        // facade installed on `SupabaseStorage` — proves the
        // single-auth-surface contract (closes the parallel
        // shell-resident OpenSecretClient defect; see audit
        // 2026-05-22). We can't `Arc::ptr_eq` against the storage's
        // private field, but we can assert the wallet exposes SOME
        // `Arc<dyn TokenProvider>` produced by `from_config`.
        let cfg = crate::config::WalletConfig {
            opensecret_url: "https://does-not-resolve-agicash.invalid".into(),
            opensecret_client_id: uuid::Uuid::nil(),
            supabase_url: "https://does-not-resolve-supabase.invalid".into(),
            supabase_anon_key: "anon-key".into(),
            session_storage: crate::config::SessionStorageChoice::InMemory,
        };
        let (wallet, _auth) = crate::WalletClient::from_config(cfg).expect("construct");
        assert!(
            wallet.token_provider().is_some(),
            "from_config must populate token_provider"
        );
    }
}
