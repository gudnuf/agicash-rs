//! Main FFI wallet object.
//!
//! Holds shared `OpenSecretClient` + `SupabaseStorage` instances and a tiny
//! in-memory session slot. Persistence lives on the Swift side: after a
//! successful login the consumer reads `Session.refresh_token` and stores it
//! in iOS Keychain; on subsequent app launches it calls `set_session(...)` to
//! rehydrate the wallet before any other method.
//!
//! Auth methods mirror the CLI (`crates/agicash-cli/src/auth.rs`) but return
//! structured `Session` / `AuthStatus` values instead of printing JSON. The
//! account listing path mirrors `cmd_list` in `crates/agicash-cli/src/account.rs`.

use crate::account::AccountFfi;
use crate::error::FfiError;
use crate::exchange_rate::ExchangeRateSnapshot;
use crate::melt_quote::{
    MeltQuoteFfiState, MeltQuoteHandle, MeltQuotePreview as MeltQuotePreviewFfi, MeltQuoteSnapshot,
};
use crate::mint::MintAddResult;
use crate::mint_quote::{MintQuoteFfiState, MintQuoteHandle, MintQuoteSnapshot};
use crate::receive::{ReceiveResult, ReceiveStatus};
use crate::receive_flow::{OpenSecretSeedProvider, ReceiveFlow};
use crate::session::{AuthStatus, Session};
use crate::user::UserFfi;
use agicash_auth_opensecret::{
    auth_error_from_opensecret, OpenSecretClient, OpenSecretConfig, OpenSecretTokenProvider,
};
use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteService, CashuMeltQuoteState, CashuMeltQuoteStorage,
    CashuMintQuote, CashuMintQuoteService, CashuMintQuoteState, CashuMintQuoteStorage,
    CashuReceiveSwapService, CashuReceiveSwapState, CashuReceiveSwapStorage, CashuSeedProvider,
    CashuSendSwapService, CashuSendSwapState, CashuSendSwapStorage, CdkCashuProvider,
    CompleteMintQuoteOutcome, CompleteOutcome, MeltOutcome, MeltQuoteError, MeltQuotePreview,
    MintQuoteError, ParsedToken, ReceiveFlowService, ReceiveSwapError, TokenProof,
};
use agicash_domain::{Account, AccountId, AccountType, Currency, UserId};
use agicash_exchange_rate::{ExchangeRateError, ExchangeRateProvider, MempoolSpaceProvider};
use agicash_money::{Money, Unit};
use agicash_storage_supabase::{
    SupabaseCashuMeltQuoteStorage, SupabaseCashuMintQuoteStorage, SupabaseCashuReceiveSwapStorage,
    SupabaseCashuSendSwapStorage, SupabaseStorage, SupabaseStorageConfig,
};
use agicash_traits::{
    CashuProvider, CashuProviderError, PassthroughProofEncryption, PersistedSession,
    ProofEncryption, SessionStorage, TokenProvider, UpdateUserDefaults, UserStorage,
};
use agicash_wallet::{
    OpenSecretAuthClient, SessionStorageChoice, TokenVersion, WalletClient, WalletConfig,
};
use cdk::mint_url::MintUrl;
use cdk::nuts::nut02::Id as KeysetId;
use cdk::nuts::{CurrencyUnit, Proof, Token};
use cdk::Amount;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(uniffi::Object)]
pub struct AgicashWallet {
    client: OpenSecretClient,
    storage: Arc<SupabaseStorage>,
    /// Cashu provider (CDK-backed). Created once at construction; cheap to
    /// share across receive/send swaps.
    cashu_provider: Arc<dyn CashuProvider>,
    /// Receive-swap orchestrator. Wired against the same `SupabaseStorage`
    /// and `cashu_provider` the wallet already owns, with the slice-5
    /// `PassthroughProofEncryption` stub matching the CLI composition root.
    /// Once the encryption seam ships, this slot swaps to a real impl
    /// without the FFI surface changing.
    receive_swap_service: Arc<CashuReceiveSwapService>,
    /// Send-swap storage handle, reused here purely to call
    /// `list_unspent_proofs` from `list_accounts` so the per-account
    /// balance can be computed. Naming is awkward (the trait shape was
    /// designed around the send flow); a follow-up refactor lane should
    /// lift `list_unspent_proofs` to a balance-focused trait. Until then
    /// this is the only call-site here.
    send_swap_storage: Arc<dyn CashuSendSwapStorage>,
    /// Mint-quote (Lightning receive) orchestrator. Wired the same way as
    /// `receive_swap_service` — same `SupabaseStorage`, same provider,
    /// same passthrough encryption stub. Drives `start_mint_quote`,
    /// `poll_mint_quote`, `complete_mint_quote`.
    // TODO(12b-1 Task 12): mint-quote flow now delegates to the facade;
    // this slot is dead once the deletion pass runs.
    #[allow(dead_code)]
    mint_quote_service: Arc<CashuMintQuoteService>,
    /// Storage handle for the mint-quote rows. Kept as its own slot so
    /// `poll_mint_quote` can read the persisted quote by id without
    /// holding the service.
    // TODO(12b-1 Task 12): dead with the facade-delegated mint-quote flow.
    #[allow(dead_code)]
    mint_quote_storage: Arc<dyn CashuMintQuoteStorage>,
    /// Send-swap orchestrator. Wired against the same `send_swap_storage`
    /// + `cashu_provider` the wallet already owns. Drives
    ///   `prepare_send_quote`, `create_send_swap`, `check_send_swap_claimed`.
    ///   Mirrors `mint_quote_service`.
    send_swap_service: Arc<CashuSendSwapService>,
    /// Melt-quote (Lightning send) orchestrator. Wired the same way as
    /// `mint_quote_service` — same `SupabaseStorage`, same provider,
    /// same passthrough encryption stub. Drives `prepare_melt_quote`,
    /// `create_melt_quote`, `execute_melt_quote`, `poll_melt_quote`.
    /// The symmetric send-side counterpart of `mint_quote_service`.
    melt_quote_service: Arc<CashuMeltQuoteService>,
    /// Storage handle for the melt-quote rows. Kept as its own slot so
    /// `poll_melt_quote` / `execute_melt_quote` can read the persisted
    /// quote by id without holding the service. Mirrors
    /// `mint_quote_storage`.
    melt_quote_storage: Arc<dyn CashuMeltQuoteStorage>,
    /// In-memory session. Phase 1 leaves persistence to the Swift consumer:
    /// the iOS app stores the `refresh_token` in Keychain and rehydrates this
    /// slot via `set_session` on app launch.
    session: Arc<RwLock<Option<PersistedSession>>>,
    /// Optional persistent session storage. Populated post-construction
    /// by `set_session_storage_dir(...)`. When set, every successful
    /// auth call (`auth_guest`, `auth_login`, `auth_signup`, `set_session`)
    /// writes the resulting `PersistedSession` through to storage, and
    /// `auth_logout` clears it. On Android the impl is
    /// `AndroidFileSessionStorage` (AES-256-GCM blob in the app's private
    /// data dir); iOS keeps using its `SessionStore` keychain wrapper on
    /// the Swift side and leaves this slot empty.
    session_storage: Arc<RwLock<Option<Arc<dyn SessionStorage + Send + Sync>>>>,
    /// Realtime subscription supervisor task. Populated by
    /// `start_wallet_events`; aborted by `stop_wallet_events`. The
    /// supervisor runs the connect→join→serve→reconnect loop on a
    /// tokio task and forwards events to the registered listener.
    realtime_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// The live `WalletRealtimeService` behind `realtime_task`. Held so
    /// `stop_wallet_events` can `phx_leave` + close the socket before
    /// the task is aborted (a bare `abort()` would drop the socket
    /// without a clean leave).
    realtime_service: Arc<RwLock<Option<Arc<agicash_realtime::WalletRealtimeService>>>>,
    /// The composed facade. Built once in `new` via
    /// `WalletClient::from_config`. The delegated business methods route
    /// here; the shell-resident platform layer (the `OpenSecretClient`
    /// `client` field, session slot, session-storage backend, realtime
    /// supervisor, observability) stays on `self` per spec §6. Named
    /// `facade` (not `client`) because the pre-existing
    /// `client: OpenSecretClient` field is kept byte-for-byte for the
    /// shell-resident methods (Hard Rule 7) and the names would clash.
    // TODO(12b-1 Tasks 6-12): becomes read once the delegates are
    // rewritten; allow until then so the interim gate stays green.
    #[allow(dead_code)]
    facade: Arc<WalletClient>,
    /// The facade's auth client, retained so the shell can mirror its
    /// session slot into the existing `self.session` plumbing without
    /// changing realtime/session behavior (spec §6 carve-out).
    // TODO(12b-1 Tasks 6-12): read once auth_* delegates mirror through it.
    #[allow(dead_code)]
    facade_auth: Arc<OpenSecretAuthClient>,
}

impl std::fmt::Debug for AgicashWallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `OpenSecretClient` already redacts itself; the session is sensitive
        // material (refresh token) so we never print its contents.
        f.debug_struct("AgicashWallet")
            .field("client", &self.client)
            .field("storage", &self.storage)
            .field(
                "session_loaded",
                &self
                    .session
                    .try_read()
                    .map(|s| s.is_some())
                    .unwrap_or(false),
            )
            .finish_non_exhaustive()
    }
}

/// Generate 16 random bytes hex-encoded; the `OpenSecret` guest-registration
/// password slot accepts any string and we never need it after the first
/// login (Swift persists only the resulting refresh token).
// TODO(12b-1 Task 12): guest registration now happens inside the facade
// (`OpenSecretAuthClient::register_guest`); this shell copy is dead once
// the deletion pass runs. Allow until then so the gate stays green.
#[allow(dead_code)]
fn random_password() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("OS RNG must be available");
    hex::encode(buf)
}

#[uniffi::export(async_runtime = "tokio")]
impl AgicashWallet {
    /// Build a wallet that talks to the given `OpenSecret` and Supabase
    /// endpoints. The `client_id_uuid` must be a stringified UUID identifying
    /// this Agicash app to `OpenSecret` (matches the `OPENSECRET_CLIENT_ID`
    /// env var used by the CLI).
    //
    // UniFFI requires owned `String` arguments at the FFI boundary, so the
    // pedantic `needless_pass_by_value` lint can't be satisfied here.
    #[uniffi::constructor]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        opensecret_url: String,
        opensecret_client_id_uuid: String,
        supabase_url: String,
        supabase_anon_key: String,
    ) -> Result<Arc<Self>, FfiError> {
        // First FFI entry point on iOS — install the tracing → os_log
        // bridge so every downstream `tracing::info!` is visible via
        // `log stream --predicate 'subsystem == "app.agicash.rust"'`.
        // Idempotent; safe to call from every constructor / method.
        crate::observability::init();

        tracing::info!(
            target: "agicash_ffi::wallet",
            opensecret_url = %opensecret_url,
            supabase_url = %supabase_url,
            "AgicashWallet::new"
        );

        let client_id = Uuid::parse_str(&opensecret_client_id_uuid)
            .map_err(|e| FfiError::internal(format!("invalid opensecret_client_id_uuid: {e}")))?;
        // Capture the endpoint args for the facade `from_config` BEFORE
        // the existing wiring moves them into OpenSecretConfig /
        // SupabaseStorageConfig. `client_id` is a `Copy` Uuid, reused
        // directly. This dual-feeds the SAME values to both the kept
        // (shell-resident) wiring and the new facade root with zero
        // change to the ctor signature.
        let opensecret_url_for_facade = opensecret_url.clone();
        let supabase_url_for_facade = supabase_url.clone();
        let supabase_anon_key_for_facade = supabase_anon_key.clone();
        let auth_cfg = OpenSecretConfig {
            base_url: opensecret_url,
            client_id,
        };
        let client = OpenSecretClient::new(auth_cfg)?;

        let storage_cfg = SupabaseStorageConfig {
            url: supabase_url,
            anon_key: supabase_anon_key,
        };
        let token_provider: Arc<dyn TokenProvider + Send + Sync> =
            Arc::new(OpenSecretTokenProvider::new(client.clone()));
        let storage = Arc::new(SupabaseStorage::new(storage_cfg, token_provider)?);

        // Cashu wiring mirrors `crates/agicash-cli/src/composition.rs`
        // (`build_cashu_deps` + `build_receive_swap_deps`): one shared
        // CDK provider, plus a receive-swap service backed by the same
        // Supabase storage handle the wallet already owns. The
        // `PassthroughProofEncryption` stub matches what slice 5 ships;
        // when the real encryption layer lands the wallet just swaps
        // this constructor without the FFI shape moving.
        let cashu_provider: Arc<dyn CashuProvider> = Arc::new(CdkCashuProvider::new());
        let encryption: Arc<dyn ProofEncryption> = Arc::new(PassthroughProofEncryption);
        let receive_storage: Arc<dyn CashuReceiveSwapStorage> = Arc::new(
            SupabaseCashuReceiveSwapStorage::new(Arc::clone(&storage), Arc::clone(&encryption)),
        );
        let receive_swap_service = Arc::new(CashuReceiveSwapService::new(
            receive_storage,
            Arc::clone(&cashu_provider),
        ));
        // Reuse the same passthrough encryption seam so per-account
        // balance reads can decrypt the proofs the receive-swap service
        // wrote. Slice 5+ swaps the encryption arc without touching this
        // wiring.
        let send_swap_storage: Arc<dyn CashuSendSwapStorage> = Arc::new(
            SupabaseCashuSendSwapStorage::new(Arc::clone(&storage), Arc::clone(&encryption)),
        );

        // Mint-quote service wiring mirrors the CLI's
        // `build_mint_quote_deps` (composition.rs). Same Supabase
        // storage, same passthrough encryption stub, same CDK provider.
        let mint_quote_storage: Arc<dyn CashuMintQuoteStorage> = Arc::new(
            SupabaseCashuMintQuoteStorage::new(Arc::clone(&storage), Arc::clone(&encryption)),
        );
        let mint_quote_service = Arc::new(CashuMintQuoteService::new(
            Arc::clone(&mint_quote_storage),
            Arc::clone(&cashu_provider),
        ));

        // Send-swap service wiring mirrors the CLI's `build_send_swap_deps`
        // (composition.rs). Same `send_swap_storage` slot we already
        // own; same CDK provider.
        let send_swap_service = Arc::new(CashuSendSwapService::new(
            Arc::clone(&send_swap_storage),
            Arc::clone(&cashu_provider),
        ));

        // Melt-quote service wiring mirrors the CLI's
        // `build_melt_quote_deps` (composition.rs). Same Supabase
        // storage, same passthrough encryption stub, same CDK provider.
        // Symmetric with `mint_quote_service` above.
        let melt_quote_storage: Arc<dyn CashuMeltQuoteStorage> = Arc::new(
            SupabaseCashuMeltQuoteStorage::new(Arc::clone(&storage), Arc::clone(&encryption)),
        );
        let melt_quote_service = Arc::new(CashuMeltQuoteService::new(
            Arc::clone(&melt_quote_storage),
            Arc::clone(&cashu_provider),
        ));

        // Build the composed facade from the SAME inputs. This is the
        // single composition root the delegated methods route through.
        // `SessionStorageChoice::InMemory` matches today's construction-
        // time state (the FFI installs Android storage post-construction
        // via `set_session_storage_dir`, which stays shell-resident per
        // Hard Rule 7 — at construction `session_storage` is `None`).
        let (facade, facade_auth) = WalletClient::from_config(WalletConfig {
            opensecret_url: opensecret_url_for_facade,
            opensecret_client_id: client_id,
            supabase_url: supabase_url_for_facade,
            supabase_anon_key: supabase_anon_key_for_facade,
            session_storage: SessionStorageChoice::InMemory,
        })
        .map_err(|e| FfiError::internal(format!("from_config: {e}")))?;

        // Load-bearing no-op invariant (spec §6 / note ‡): the shell's
        // `self.session` slot IS the facade's `OpenSecretAuthClient`
        // slot (one shared `Arc`). This is the whole point of
        // `OpenSecretAuthClient::session_slot()`. With it shared:
        //  - the kept, byte-for-byte-unchanged shell-resident
        //    `set_session` / `try_restore_session` (Hard Rule 7) write
        //    `*self.session.write()` and the facade's
        //    `require_session()` immediately sees it — so the delegated
        //    business methods (list_accounts, mint_add, receive_token,
        //    …) keep working on the iOS Keychain-rehydrate launch path
        //    with ZERO behavior change;
        //  - the delegated `auth_*` populate it through the facade and
        //    the note-‡ mirror writes the same value (idempotent);
        //  - `auth_status` + realtime, which read `self.session`,
        //    observe one consistent source of truth.
        // Without sharing, `set_session` would leave the facade slot
        // empty and every delegated method would regress to
        // Unauthenticated — a catastrophic non-no-op. So we adopt the
        // facade slot as `self.session` here.
        let session = facade_auth.session_slot();

        Ok(Arc::new(Self {
            client,
            storage,
            cashu_provider,
            receive_swap_service,
            send_swap_storage,
            mint_quote_service,
            mint_quote_storage,
            send_swap_service,
            melt_quote_service,
            melt_quote_storage,
            session,
            session_storage: Arc::new(RwLock::new(None)),
            realtime_task: Arc::new(RwLock::new(None)),
            realtime_service: Arc::new(RwLock::new(None)),
            facade,
            facade_auth,
        }))
    }

    // ---- session plumbing (Swift-side Keychain hooks) ----

    /// Rehydrate an existing session into the wallet. Called by the Swift
    /// consumer on app launch after reading the refresh token from Keychain.
    /// Performs an OpenSecret token refresh so the internal client has a
    /// fresh access token. On refresh failure the in-memory session is
    /// cleared and an `Auth` error is returned so the consumer can drop the
    /// Keychain entry.
    pub async fn set_session(
        &self,
        user_id_uuid: String,
        refresh_token: String,
    ) -> Result<(), FfiError> {
        let user_id = Uuid::parse_str(&user_id_uuid)
            .map_err(|e| FfiError::internal(format!("invalid user_id_uuid: {e}")))?;

        self.client.ensure_handshake().await?;
        self.client
            .inner()
            .set_tokens(String::new(), Some(refresh_token.clone()))
            .map_err(auth_error_from_opensecret)?;

        if let Err(e) = self.client.inner().refresh_token().await {
            *self.session.write().await = None;
            return Err(auth_error_from_opensecret(e).into());
        }

        *self.session.write().await = Some(PersistedSession {
            user_id,
            refresh_token,
        });
        Ok(())
    }

    /// Return the currently-loaded session, or `None` if the wallet is
    /// logged out. Lets the Swift consumer re-sync its Keychain copy after
    /// a `auth_guest` / `auth_login` call.
    pub async fn get_persisted_session(&self) -> Option<Session> {
        self.session.read().await.clone().map(Session::from)
    }

    /// Install a filesystem-backed `SessionStorage` rooted at the given
    /// directory. On Android the caller passes
    /// `Context.getFilesDir().getAbsolutePath()` — the app's private data
    /// dir, isolated per-app by Linux UID. The directory is expected to
    /// exist (Android's `getFilesDir()` always does).
    ///
    /// Once installed, every successful auth call writes the resulting
    /// `PersistedSession` through to disk (AES-256-GCM blob + sibling
    /// random key file), and `auth_logout` removes both files. Subsequent
    /// calls to `try_restore_session` re-hydrate the in-memory slot from
    /// disk.
    ///
    /// This method is gated on `target_os = "android"` — the underlying
    /// `AndroidFileSessionStorage` type is only re-exported there.
    /// Callers on iOS / wasm / host should not invoke it; the FFI surface
    /// returns an `Internal` error if the storage backend isn't compiled
    /// in on the current target.
    // The `.await` lives in the `target_os = "android"` cfg branch; on
    // host/non-android targets that branch is compiled out, so clippy sees
    // no await. `async` is part of the UniFFI surface and must stay.
    #[allow(clippy::unused_async)]
    pub async fn set_session_storage_dir(&self, dir: String) -> Result<(), FfiError> {
        #[cfg(all(feature = "android-file-storage", target_os = "android"))]
        {
            use agicash_auth_opensecret::AndroidFileSessionStorage;
            let storage: Arc<dyn SessionStorage + Send + Sync> =
                Arc::new(AndroidFileSessionStorage::new(dir));
            *self.session_storage.write().await = Some(storage);
            tracing::info!(
                target: "agicash_ffi::wallet",
                "set_session_storage_dir: AndroidFileSessionStorage installed"
            );
            return Ok(());
        }
        #[cfg(not(all(feature = "android-file-storage", target_os = "android")))]
        {
            let _ = dir;
            Err(FfiError::internal(
                "session storage not available on this target (Android-only)",
            ))
        }
    }

    /// Attempt to rehydrate a previously-stored session from the
    /// installed `SessionStorage` backend. Returns the rehydrated
    /// `Session` on success, or `None` if no session was persisted (or
    /// no backend is installed). On a stale / unusable refresh token the
    /// stored blob is cleared and `None` is returned so the Kotlin
    /// consumer can route to the login screen without surfacing a fatal
    /// error.
    ///
    /// Internally this calls the existing `set_session(...)` plumbing
    /// once a stored blob is loaded — same OpenSecret handshake + token
    /// refresh chain, same in-memory slot rehydration.
    pub async fn try_restore_session(&self) -> Result<Option<Session>, FfiError> {
        let storage_opt = self.session_storage.read().await.clone();
        let Some(storage) = storage_opt else {
            return Ok(None);
        };

        let persisted = match storage.load().await {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(None),
            Err(e) => {
                tracing::warn!(
                    target: "agicash_ffi::wallet",
                    error = %e,
                    "try_restore_session: storage.load() failed"
                );
                return Ok(None);
            }
        };

        // Run the same handshake + refresh chain as `set_session`. On
        // failure we drop the on-disk blob so the next launch falls
        // back to the sign-in screen instead of re-trying a dead token.
        match self
            .set_session(
                persisted.user_id.to_string(),
                persisted.refresh_token.clone(),
            )
            .await
        {
            Ok(()) => {
                tracing::info!(
                    target: "agicash_ffi::wallet",
                    "try_restore_session: rehydrated session from storage"
                );
                Ok(Some(persisted.into()))
            }
            Err(e) => {
                tracing::warn!(
                    target: "agicash_ffi::wallet",
                    error = %e,
                    "try_restore_session: refresh failed, clearing stored blob"
                );
                let _ = storage.clear().await;
                Ok(None)
            }
        }
    }

    // Internal helpers `persist_session` + `clear_persisted_session` live
    // outside this `#[uniffi::export]` impl block so UniFFI's bindgen
    // doesn't try to lift `&PersistedSession` across the FFI boundary
    // (it isn't a UniFFI type). See the bare `impl AgicashWallet` block
    // below this one.

    // ---- auth surface ----

    /// Register an anonymous guest account against OpenSecret. Generates a
    /// throwaway password (the user never sees it) and returns the resulting
    /// `Session` so the Swift consumer can persist the refresh token.
    pub async fn auth_guest(&self) -> Result<Session, FfiError> {
        let s = self
            .facade
            .auth_guest()
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        // §6 carve-out (note ‡): mirror into the shell-resident session
        // slot + persistence so realtime / set_session /
        // try_restore_session keep working UNCHANGED (Hard Rule 7).
        let persisted = PersistedSession {
            user_id: s.user_id.as_uuid(),
            refresh_token: s.refresh_token.clone(),
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist_session(&persisted).await;
        Ok(crate::convert::session_from_facade(s))
    }

    /// Email + password login.
    pub async fn auth_login(&self, email: String, password: String) -> Result<Session, FfiError> {
        let s = self
            .facade
            .auth_login(&email, &password)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        // §6 carve-out (note ‡): mirror into the shell-resident slot.
        let persisted = PersistedSession {
            user_id: s.user_id.as_uuid(),
            refresh_token: s.refresh_token.clone(),
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist_session(&persisted).await;
        Ok(crate::convert::session_from_facade(s))
    }

    /// Register a new email + password user against OpenSecret. Mirrors the
    /// web app's `/signup` flow: on success the user is auto-signed-in and
    /// the resulting `Session` is returned so the Swift consumer can persist
    /// the refresh token in Keychain. The optional `name` slot maps to the
    /// OpenSecret SDK's display-name field; the iOS app does not collect it
    /// in v0 (web doesn't either) but the parameter is exposed so the
    /// surface matches the underlying SDK and future UI can populate it
    /// without another FFI churn.
    pub async fn auth_signup(
        &self,
        email: String,
        password: String,
        name: Option<String>,
    ) -> Result<Session, FfiError> {
        let s = self
            .facade
            .auth_signup(&email, &password, name.as_deref())
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        // §6 carve-out (note ‡): mirror into the shell-resident slot.
        let persisted = PersistedSession {
            user_id: s.user_id.as_uuid(),
            refresh_token: s.refresh_token.clone(),
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist_session(&persisted).await;
        Ok(crate::convert::session_from_facade(s))
    }

    /// Best-effort server logout. Always clears the in-memory session even
    /// if the server-side call fails (e.g. expired token, network error).
    /// The Swift consumer should also drop its Keychain entry on success.
    pub async fn auth_logout(&self) -> Result<(), FfiError> {
        // Facade logout = best-effort server logout + always-Ok local
        // clear (verbatim the prior FFI semantics, moved into
        // `OpenSecretAuthClient::logout`).
        self.facade
            .auth_logout()
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        // §6 carve-out (note ‡): mirror the clear into the shell-resident
        // slot + persistence so realtime / try_restore_session keep
        // working UNCHANGED. Order matters: clear in-memory first so a
        // crash mid-clear still logs the user out at the in-memory layer.
        *self.session.write().await = None;
        self.clear_persisted_session().await;
        Ok(())
    }

    /// Return whether the wallet currently holds a session.
    pub async fn auth_status(&self) -> Result<AuthStatus, FfiError> {
        let snap = self.session.read().await.clone();
        Ok(match snap {
            Some(s) => AuthStatus {
                logged_in: true,
                user_id: Some(s.user_id.to_string()),
            },
            None => AuthStatus {
                logged_in: false,
                user_id: None,
            },
        })
    }

    // ---- account surface ----

    /// List Supabase `wallet.accounts` rows for the currently-logged-in
    /// user. For each Cashu account, sums the account's UNSPENT proofs
    /// (decrypted via the storage layer's `list_unspent_proofs`) and
    /// returns the total as `balance` in the account's smallest unit
    /// (`sat` for BTC, `cent` for USD/USDB). Spark accounts always return
    /// balance `"0"` until slice 9 wires their proof storage.
    ///
    /// Per-account decryption walks the rows one-by-one; this is fine at
    /// MVP scale but could grow to N+1 latency once users hold many
    /// proofs. A grouped query is the natural follow-up.
    pub async fn list_accounts(&self) -> Result<Vec<AccountFfi>, FfiError> {
        // Shell-resident observability (spec §6) — kept verbatim. The
        // session check + balance summing now live in the facade
        // `list_accounts` (it `require_session()`s the shared slot and
        // sums via the same Supabase storage + cashu provider). The
        // post-session-loaded per-account log lines depended on the
        // now-removed inline loop; dropping them is a log-only change.
        crate::observability::init();
        tracing::info!(target: "agicash_ffi::wallet", "list_accounts: enter");
        let summaries = self
            .facade
            .list_accounts()
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        let out: Vec<AccountFfi> = summaries
            .iter()
            .map(crate::convert::account_ffi_from_summary)
            .collect();
        tracing::info!(
            target: "agicash_ffi::wallet",
            returned = out.len(),
            "list_accounts: exit"
        );
        Ok(out)
    }

    // ---- user surface ----

    /// Load the current user row from Supabase. Requires an active session.
    ///
    /// Mirrors `useUser()` on web — the resulting `UserFfi` carries the
    /// per-currency default-account slots iOS uses to render "Default"
    /// badges and to know which account is currently the default.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal("user row not found")` if the user has not
    ///   yet been upserted (e.g., guest signed in but never added a
    ///   mint). Callers can treat this the same as "no defaults set".
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn get_user(&self) -> Result<UserFfi, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);
        let row = self
            .storage
            .get_user(user_id)
            .await?
            .ok_or_else(|| FfiError::internal("user row not found"))?;
        Ok(UserFfi::from(row))
    }

    /// Set the user's default account for the account's currency. Mirrors
    /// the web `UserService.setDefaultAccount` exactly: writes the matching
    /// `default_<currency>_account_id` slot on `wallet.users`, preserving
    /// the other currency's slot. `default_currency` is NOT touched by this
    /// call — the web couples that flip to account-creation paths only, and
    /// the iOS swipe action shouldn't surprise-flip the user's primary
    /// currency.
    ///
    /// `account_id` must refer to an account that exists for the
    /// currently-logged-in user (looked up via `list_accounts`). The
    /// account's currency must be one of `BTC` or `USD` — `USDB` would have
    /// no default slot to write.
    ///
    /// Returns the resulting user row so the iOS UI can immediately reflect
    /// the change without a second round-trip.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal("invalid account_id: ...")` for non-UUID strings.
    /// - `FfiError::Internal("account not found")` if the id doesn't match
    ///   any account on this user.
    /// - `FfiError::Internal("unsupported currency for default")` if the
    ///   account's currency is `USDB`.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn set_default_account(&self, account_id: String) -> Result<UserFfi, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        let id_uuid = Uuid::parse_str(account_id.trim())
            .map_err(|e| FfiError::internal(format!("invalid account_id: {e}")))?;
        let target_id = AccountId::from(id_uuid);

        let accounts = self.storage.list_accounts(user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == target_id)
            .ok_or_else(|| FfiError::internal("account not found"))?;

        let patch = match account.currency {
            Currency::Btc => UpdateUserDefaults {
                default_btc_account_id: Some(Some(target_id)),
                ..Default::default()
            },
            Currency::Usd => UpdateUserDefaults {
                default_usd_account_id: Some(Some(target_id)),
                ..Default::default()
            },
            Currency::Usdb => {
                return Err(FfiError::internal("unsupported currency for default"));
            }
        };

        let updated = self.storage.update_user_defaults(user_id, patch).await?;
        Ok(UserFfi::from(updated))
    }

    // ---- mint surface ----

    /// Provision a new Cashu mint and create a BTC account row for it.
    ///
    /// Mirrors the `agicash mint add <url>` CLI subcommand
    /// (`crates/agicash-cli/src/mint.rs`): parse the URL, fetch NUT-06 mint
    /// info, then call `wallet.upsert_user_with_accounts` to insert the new
    /// `wallet.accounts` row. Returns the new account id + name + canonical
    /// URL so the Add Mint sheet on iOS can show a confirmation and the
    /// Accounts screen can refresh without a follow-up `list_accounts`
    /// round-trip (though it will refresh anyway).
    ///
    /// Hard-codes `currency = BTC` to match the web app's `add-mint-form.tsx`
    /// (which also hard-codes BTC). The iOS UI does not collect a currency
    /// today; if/when the web exposes USD mint creation we can add a
    /// parameter here.
    ///
    /// First-mint-add for a brand-new guest user creates a placeholder
    /// `Spark` account too — same workaround the CLI uses to satisfy the
    /// `wallet.upsert_user_with_accounts` "at least one BTC Spark"
    /// constraint. Slice 9 (Spark wiring) replaces it with a real-key-backed
    /// row.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for invalid URLs, mint unreachable, mint
    ///   protocol errors, and the post-upsert "no account matching the new
    ///   mint URL" sanity check (the underlying `MintCmdError` doesn't fit
    ///   Auth/Storage cleanly — same shape as `receive_token` funnels
    ///   `ReceiveSwapError` through Internal).
    /// - `FfiError::Storage` for raw Supabase failures (network, etc.).
    pub async fn mint_add(&self, url: String) -> Result<MintAddResult, FfiError> {
        // Hard-codes BTC (verbatim — the iOS UI + web `add-mint-form`
        // both hard-code BTC). The facade `add_mint` performs the
        // identical NUT-06 discovery + user-row-preservation + upsert +
        // account-match the bespoke body did; it `require_session()`s
        // the shared slot.
        let summary = self
            .facade
            .add_mint(url, Currency::Btc)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        Ok(crate::convert::mint_add_result_from_summary(&summary))
    }

    // ---- receive surface ----

    /// Redeem a Cashu token (V3 `cashuA…` or V4 `cashuB…`).
    ///
    /// Mirrors the `agicash receive token <token>` CLI subcommand
    /// (`crates/agicash-cli/src/receive.rs`): parse the token, pick the
    /// matching account by `(mint_url, currency)`, run
    /// `CashuReceiveSwapService::create` followed by `complete_swap`, and
    /// return a flattened receipt. Idempotent on repeat redeems of the
    /// same token (returns [`ReceiveStatus::AlreadyClaimed`]).
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for token-parse failures, missing matching
    ///   account, currency/unit mismatches, or amount-too-small after fees
    ///   (the underlying `ReceiveSwapError` doesn't fit Auth/Storage cleanly
    ///   so it is funneled through Internal — the message string carries
    ///   the discriminator the iOS UI surfaces inline).
    /// - `FfiError::Storage` for raw Supabase failures (network, etc.).
    pub async fn receive_token(&self, token: String) -> Result<ReceiveResult, FfiError> {
        // Shell-resident observability (spec §6) — kept verbatim. The
        // parse / account-pick / PENDING-create / AlreadyClaimed
        // idempotency / seed / complete_swap chain now lives in the
        // facade `receive_cashu_token` byte-for-byte; it
        // `require_session()`s the shared slot. The post-session-loaded
        // log line depended on the now-removed inline check — dropping
        // it is a log-only change.
        crate::observability::init();
        tracing::info!(
            target: "agicash_ffi::wallet",
            token_len = token.len(),
            "receive_token: enter"
        );
        let receipt = self
            .facade
            .receive_cashu_token(&token)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        Ok(crate::convert::receive_result_from_receipt(&receipt))
    }

    /// Construct a fresh [`ReceiveFlow`] handle for an interactive
    /// receive-token flow. Each call returns a new orchestrator —
    /// flows are not persisted across constructions.
    ///
    /// The returned handle exposes:
    /// - `current_state()` to snapshot the current state
    /// - `dispatch(event)` to feed UI events in and run the resulting I/O
    ///
    /// Requires an active session; returns `FfiError::Auth { UNAUTHENTICATED }`
    /// otherwise.
    pub async fn receive_flow(&self) -> Result<Arc<ReceiveFlow>, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);
        let seed_provider: Arc<dyn CashuSeedProvider> =
            Arc::new(OpenSecretSeedProvider::new(self.client.clone()));
        let service = ReceiveFlowService::new(
            user_id,
            Arc::clone(&self.storage) as Arc<dyn UserStorage>,
            Arc::clone(&self.cashu_provider),
            Arc::clone(&self.receive_swap_service),
            seed_provider,
        );
        Ok(Arc::new(ReceiveFlow::new(service)))
    }

    // ---- lightning receive (mint quote) surface ----

    /// Start a NUT-04 mint quote — request a BOLT-11 invoice from the
    /// mint backing the user's Cashu account.
    ///
    /// Mirrors the CLI's `agicash receive lightning <amount>` subcommand
    /// (`crates/agicash-cli/src/receive_lightning.rs`) but stops at the
    /// "quote issued" step. The Swift side displays the invoice and
    /// drives the poll/complete cycle itself so the polling cadence and
    /// UI feedback stay on the consumer.
    ///
    /// `amount` is the value the wallet wants to *receive* expressed in
    /// the account's minor unit (sats for BTC, cents for USD). The mint
    /// may add a small fee on top — surfaced via [`MintQuoteHandle::fee`]
    /// so the iOS UI can render a breakdown.
    ///
    /// `account_id` and `currency` together select the receiving Cashu
    /// account. `currency` is the wallet currency string (`"BTC"` /
    /// `"USD"`); when omitted defaults to `"BTC"`. `account_id` (UUID
    /// string) lets multi-mint users pick the receiving account; when
    /// omitted, the single matching Cashu+currency account is used, or
    /// an `Internal` error is returned if zero or multiple matches
    /// exist (same selector the CLI uses).
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for amount-too-small, currency mismatch,
    ///   no/ambiguous matching account, or any mint-protocol failure
    ///   (mirrors `receive_token`'s funneling pattern).
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn start_mint_quote(
        &self,
        amount: u64,
        account_id: Option<String>,
        currency: Option<String>,
    ) -> Result<MintQuoteHandle, FfiError> {
        // The facade `quote_receive_lightning` does the same
        // `require_session()` + account-pick + `create_quote` the
        // bespoke body did, on the shared session slot. Arg parsing
        // (currency default-BTC, account-id UUID, minor-unit Money)
        // routes through `convert::*`.
        let currency_enum = crate::convert::parse_currency(currency.as_deref())?;
        let account_id = crate::convert::parse_opt_account_id(account_id.as_deref())?;
        let amount_money = crate::convert::amount_to_money(amount, currency_enum);
        let handle = self
            .facade
            .quote_receive_lightning(account_id, amount_money)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        Ok(crate::convert::mint_quote_handle_from_facade(&handle))
    }

    /// Poll the mint for the current state of a previously-started
    /// quote. Single-shot: returns the snapshot of the persisted row
    /// (with one mint round-trip if still UNPAID), never loops.
    ///
    /// The iOS app owns the polling timer; this method is intended to
    /// be called every 1-3 seconds from a long-running `Task` while the
    /// LightningReceiveView is on the `invoice` step. Once the snapshot
    /// returns `Paid` (or any terminal state), the timer stops and the
    /// UI either transitions to `complete_mint_quote` (PAID) or to the
    /// failure/expiry states.
    ///
    /// `quote_id` is the wallet-side UUID returned in
    /// [`MintQuoteHandle::quote_id`] — NOT the mint-side string id.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for invalid UUID, missing quote row,
    ///   ownership mismatch, account lookup failure, or mint-protocol
    ///   failure during the single poll round-trip.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn poll_mint_quote(&self, quote_id: String) -> Result<MintQuoteSnapshot, FfiError> {
        // The facade `poll_receive_lightning` does the same
        // `require_session()` + ownership check + persisted-state
        // fast-path + zero-timeout `poll_until_paid` the bespoke body
        // did, on the shared session slot.
        let id = crate::convert::parse_quote_id(&quote_id)?;
        let snap = self
            .facade
            .poll_receive_lightning(id)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        Ok(crate::convert::mint_quote_snapshot_from_facade(&snap))
    }

    /// Drive a PAID quote to COMPLETED — mint proofs and credit the
    /// account. Returns a [`ReceiveResult`] shape identical to
    /// `receive_token`'s output so the iOS UI can render success
    /// uniformly across both flows.
    ///
    /// Idempotent on already-completed quotes (returns the existing
    /// terminal state).
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for invalid UUID, quote not yet paid,
    ///   missing account, or mint-protocol failure during the proof
    ///   minting / restore round-trip.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn complete_mint_quote(&self, quote_id: String) -> Result<ReceiveResult, FfiError> {
        // The facade `complete_receive_lightning` does the same
        // `require_session()` + ownership check + account lookup + seed
        // + `complete_receive` the bespoke body did, on the shared
        // session slot, and returns the shared `ReceiveReceipt` shape.
        let id = crate::convert::parse_quote_id(&quote_id)?;
        let receipt = self
            .facade
            .complete_receive_lightning(id)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        Ok(crate::convert::receive_result_from_receipt(&receipt))
    }

    // ---- cashu send-swap surface ----

    /// Compute the fee breakdown for a hypothetical send. Pure preview —
    /// no swap row is created. Mirrors the CLI's `agicash send <amount>
    /// --dry-run` (`crates/agicash-cli/src/send.rs`).
    ///
    /// `amount` is the value the user wants the receiver to get,
    /// expressed in the account's minor unit (sats for BTC, cents for
    /// USD). `account_id` + `currency` together pick the source Cashu
    /// account; same selector semantics as `start_mint_quote`.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session loaded.
    /// - `FfiError::Internal` for amount-too-small, currency mismatch,
    ///   no/ambiguous matching account, or mint-protocol failure.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn prepare_send_quote(
        &self,
        amount: u64,
        account_id: Option<String>,
        currency: Option<String>,
    ) -> Result<crate::send::SendQuotePreview, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        if amount == 0 {
            return Err(FfiError::internal("amount too small"));
        }

        let currency_str = currency.unwrap_or_else(|| "BTC".to_string());
        let currency_enum = Currency::from_str(&currency_str)
            .map_err(|_| FfiError::internal(format!("unsupported currency: {currency_str}")))?;
        let unit = unit_for_currency(currency_enum);
        let amount_money = Money::new(Decimal::from(amount), currency_enum, unit);

        let accounts = self.storage.list_accounts(user_id).await?;
        let account =
            pick_cashu_account_for_lightning(&accounts, account_id.as_deref(), currency_enum)?;
        let mint_url = account
            .details
            .get("mint_url")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
            .ok_or_else(|| FfiError::internal("account.details missing mint_url"))?;

        let proofs = self
            .send_swap_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| FfiError::internal(format!("list unspent proofs: {e}")))?;

        let quote = self
            .send_swap_service
            .get_quote(account, &proofs, amount_money)
            .await
            .map_err(send_swap_error_to_ffi)?;

        Ok(crate::send::SendQuotePreview {
            amount_requested: quote.amount_requested.amount().to_string(),
            amount_to_send: quote.amount_to_send.amount().to_string(),
            total_amount: quote.total_amount.amount().to_string(),
            total_fee: quote.total_fee.amount().to_string(),
            cashu_send_fee: quote.cashu_send_fee.amount().to_string(),
            cashu_receive_fee: quote.cashu_receive_fee.amount().to_string(),
            unit: quote.amount_to_send.unit().to_string(),
            currency: account.currency.to_string(),
            account_id: account.id.to_string(),
            mint_url,
        })
    }

    /// Persist a new Cashu send swap and produce a wire-form token.
    /// Mirrors the CLI's `agicash send <amount>` (without `--dry-run`).
    ///
    /// Always encodes a **V4** (`cashuB…`) token. V3 is the legacy
    /// shape; iOS v0 doesn't expose a chooser.
    ///
    /// Errors mirror `prepare_send_quote` plus token-encode failures.
    pub async fn create_send_swap(
        &self,
        amount: u64,
        account_id: Option<String>,
        currency: Option<String>,
    ) -> Result<crate::send::SendSwapHandle, FfiError> {
        // The facade `send_token` does the same `require_session()` +
        // account-pick + unspent-proof load + create + Draft→swap /
        // Pending + V4 token-encode the bespoke body did, on the shared
        // session slot. FFI always V4 (verbatim). Arg parsing routes
        // through `convert::*`.
        let currency_enum = crate::convert::parse_currency(currency.as_deref())?;
        let account_id = crate::convert::parse_opt_account_id(account_id.as_deref())?;
        let amount_money = crate::convert::amount_to_money(amount, currency_enum);
        let receipt = self
            .facade
            .send_token(account_id, amount_money, TokenVersion::V4)
            .await
            .map_err(crate::convert::wallet_error_to_ffi)?;
        Ok(crate::convert::send_swap_handle_from_facade(&receipt))
    }

    /// Check whether the receiver has claimed a previously-created
    /// send swap. Pure poll: re-loads the swap, asks the mint via
    /// NUT-07 `post_check_state` whether the swap's `proofs_to_send`
    /// are SPENT, and (if so) flips the persisted row PENDING →
    /// COMPLETED.
    ///
    /// Returns:
    /// - `Pending` while at least one proof is still UNSPENT (or in
    ///   any non-terminal state on the mint side).
    /// - `Completed` when every proof is SPENT — the receiver
    ///   redeemed. The persisted row is transitioned in the same
    ///   call; subsequent polls short-circuit on the persisted state.
    /// - `Failed` only if the swap row is already FAILED (defensive;
    ///   shouldn't happen post-PENDING). `failure_reason` is surfaced
    ///   for the iOS UI to render.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session loaded.
    /// - `FfiError::Internal` for invalid UUID, missing swap, ownership
    ///   mismatch, mint round-trip failure.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn check_send_swap_claimed(
        &self,
        swap_id: String,
    ) -> Result<crate::send::SendSwapClaimSnapshot, FfiError> {
        use cdk::nuts::{CheckStateRequest, State as CdkProofState};
        // `MintConnector` is brought into scope so the dyn-Arc
        // returned by `wallet.connector()` exposes `post_check_state`.
        #[allow(unused_imports)]
        use cdk::wallet::MintConnector;

        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        let id = Uuid::parse_str(&swap_id)
            .map_err(|e| FfiError::internal(format!("invalid swap_id: {e}")))?;
        let swap = self
            .send_swap_storage
            .get(id)
            .await
            .map_err(|e| FfiError::internal(format!("storage error: {e}")))?;
        if swap.user_id != user_id {
            return Err(FfiError::internal("swap belongs to a different user"));
        }

        // Fast path: already-terminal states short-circuit without a
        // mint round-trip.
        let proofs_to_send = match &swap.state {
            CashuSendSwapState::Completed { .. } => {
                return Ok(crate::send::SendSwapClaimSnapshot {
                    state: crate::send::SendSwapClaimState::Completed,
                    failure_reason: None,
                });
            }
            CashuSendSwapState::Failed { failure_reason } => {
                return Ok(crate::send::SendSwapClaimSnapshot {
                    state: crate::send::SendSwapClaimState::Failed,
                    failure_reason: Some(failure_reason.clone()),
                });
            }
            CashuSendSwapState::Pending { proofs_to_send, .. } => proofs_to_send.clone(),
            other => {
                return Err(FfiError::internal(format!(
                    "swap in unexpected state for claim-check: {other:?}"
                )));
            }
        };

        let accounts = self.storage.list_accounts(user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == swap.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| FfiError::internal("no matching account for swap"))?;

        let wallet = self
            .cashu_provider
            .wallet_for_account(account)
            .await
            .map_err(cashu_provider_error_to_ffi)?;

        // Hash each proof's secret to a curve point — this is the `Y`
        // identifier NUT-07 uses to look up proof state by.
        let ys: Vec<cdk::nuts::PublicKey> = proofs_to_send
            .iter()
            .map(|p| {
                let secret = cdk::secret::Secret::from_str(&p.secret)
                    .map_err(|e| FfiError::internal(format!("bad secret: {e}")))?;
                cdk::dhke::hash_to_curve(secret.as_bytes())
                    .map_err(|e| FfiError::internal(format!("hash_to_curve: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let req = CheckStateRequest { ys };
        let resp = wallet
            .connector()
            .post_check_state(req)
            .await
            .map_err(|e| FfiError::internal(format!("mint check_state: {e}")))?;

        let all_spent = !resp.states.is_empty()
            && resp
                .states
                .iter()
                .all(|s| matches!(s.state, CdkProofState::Spent));

        if all_spent {
            // PENDING → COMPLETED so subsequent polls short-circuit on
            // the persisted state.
            self.send_swap_service
                .complete(&swap)
                .await
                .map_err(send_swap_error_to_ffi)?;
            Ok(crate::send::SendSwapClaimSnapshot {
                state: crate::send::SendSwapClaimState::Completed,
                failure_reason: None,
            })
        } else {
            Ok(crate::send::SendSwapClaimSnapshot {
                state: crate::send::SendSwapClaimState::Pending,
                failure_reason: None,
            })
        }
    }

    // ---- lightning send (melt quote) surface ----

    /// Compute the fee + amount breakdown for a hypothetical Lightning
    /// send. Pure preview — no quote row is created, no proofs are
    /// reserved. Mirrors the CLI's `agicash send lightning <bolt11>
    /// --dry-run` (`crates/agicash-cli/src/send_lightning.rs`).
    ///
    /// `bolt11` is the invoice the user pasted (or that
    /// `request_lightning_invoice` produced for the LN-address path).
    /// `account_id` + `currency` together pick the source Cashu
    /// account; same selector semantics as `prepare_send_quote`. The
    /// invoice's own msat amount determines what the receiver gets —
    /// the mint quotes the Lightning fee reserve on top.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for invalid bolt11, amountless invoice,
    ///   expired invoice, currency mismatch, insufficient balance,
    ///   no/ambiguous matching account, or any mint-protocol failure
    ///   (mirrors `prepare_send_quote`'s funneling pattern).
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn prepare_melt_quote(
        &self,
        bolt11: String,
        account_id: Option<String>,
        currency: Option<String>,
    ) -> Result<MeltQuotePreviewFfi, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        let currency_str = currency.unwrap_or_else(|| "BTC".to_string());
        let currency_enum = Currency::from_str(&currency_str)
            .map_err(|_| FfiError::internal(format!("unsupported currency: {currency_str}")))?;

        let accounts = self.storage.list_accounts(user_id).await?;
        let account =
            pick_cashu_account_for_lightning(&accounts, account_id.as_deref(), currency_enum)?;

        let proofs = self
            .send_swap_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| FfiError::internal(format!("list unspent proofs: {e}")))?;

        let preview = self
            .melt_quote_service
            .get_quote(account, &proofs, &bolt11)
            .await
            .map_err(melt_quote_error_to_ffi)?;

        Ok(melt_quote_preview_from(&preview, account))
    }

    /// Request a NUT-05 melt quote, persist the UNPAID row, and reserve
    /// the proofs. Mirrors the CLI's `agicash send lightning <bolt11>`
    /// up to the "quote issued" step (without `--dry-run` /
    /// `--no-wait`): the Swift side then calls `execute_melt_quote`
    /// (which fires `post_melt`) and drives the poll cycle itself so
    /// the polling cadence + UI feedback stay on the consumer — same
    /// split as the mint-quote (Lightning receive) surface.
    ///
    /// Re-runs `get_quote` internally so the caller passes only the
    /// bolt11 (the [`MeltQuotePreview`] FFI record carries no
    /// re-constructable proof handles across the boundary). The second
    /// quote is cheap (one mint round-trip) and matches the CLI's own
    /// "preview then create" sequence.
    ///
    /// Errors mirror `prepare_melt_quote` plus quote-expired between
    /// preview and create.
    pub async fn create_melt_quote(
        &self,
        bolt11: String,
        account_id: Option<String>,
        currency: Option<String>,
    ) -> Result<MeltQuoteHandle, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        let currency_str = currency.unwrap_or_else(|| "BTC".to_string());
        let currency_enum = Currency::from_str(&currency_str)
            .map_err(|_| FfiError::internal(format!("unsupported currency: {currency_str}")))?;

        let accounts = self.storage.list_accounts(user_id).await?;
        let account =
            pick_cashu_account_for_lightning(&accounts, account_id.as_deref(), currency_enum)?
                .clone();

        let proofs = self
            .send_swap_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| FfiError::internal(format!("list unspent proofs: {e}")))?;

        let preview = self
            .melt_quote_service
            .get_quote(&account, &proofs, &bolt11)
            .await
            .map_err(melt_quote_error_to_ffi)?;

        let created = self
            .melt_quote_service
            .create_quote(user_id, &account, preview)
            .await
            .map_err(melt_quote_error_to_ffi)?;

        Ok(melt_quote_handle_from(&created.quote, &created.account))
    }

    /// Initiate the melt for a previously-created UNPAID quote: marks
    /// it PENDING, calls NUT-05 `post_melt`, then dispatches on the
    /// mint's response. Mirrors the `initiate_melt` step of the CLI's
    /// `cmd_send_lightning` (`crates/agicash-cli/src/send_lightning.rs`).
    ///
    /// Single round-trip: returns as soon as the mint replies. A PAID
    /// reply yields a terminal `Paid` snapshot (preimage + final
    /// fees); a still-in-flight reply yields `Pending` and the iOS app
    /// should then drive `poll_melt_quote` on a timer; a mint refusal
    /// yields a terminal `Failed` snapshot.
    ///
    /// `quote_id` is the wallet-side UUID from
    /// [`MeltQuoteHandle::quote_id`] — NOT the mint-side string id.
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for invalid UUID, missing quote row,
    ///   ownership mismatch, missing account, or mint-protocol failure
    ///   during the melt round-trip.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn execute_melt_quote(
        &self,
        quote_id: String,
    ) -> Result<MeltQuoteSnapshot, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        let id = Uuid::parse_str(&quote_id)
            .map_err(|e| FfiError::internal(format!("invalid quote_id: {e}")))?;
        let quote = self
            .melt_quote_storage
            .get(id)
            .await
            .map_err(|e| FfiError::internal(format!("storage error: {e}")))?;
        if quote.user_id != user_id {
            return Err(FfiError::internal("quote belongs to a different user"));
        }

        // Fast-path: terminal rows short-circuit without a mint
        // round-trip (mirrors `poll_mint_quote`'s persisted-state
        // fast-path).
        if !matches!(
            quote.state,
            CashuMeltQuoteState::Unpaid | CashuMeltQuoteState::Pending
        ) {
            return Ok(melt_quote_snapshot_from(&quote));
        }

        let accounts = self.storage.list_accounts(user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == quote.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| FfiError::internal("no matching account for quote"))?
            .clone();

        let seed = self.client.get_cashu_seed().await?;
        let outcome = self
            .melt_quote_service
            .initiate_melt(&account, quote, &seed)
            .await
            .map_err(melt_quote_error_to_ffi)?;

        Ok(melt_quote_snapshot_from_outcome(&outcome))
    }

    /// Poll the mint for the current state of a PENDING melt quote.
    /// Single-shot: reconciles change proofs + storage on PAID,
    /// flips the row FAILED on a mint UNPAID/FAILED, returns the
    /// still-pending snapshot otherwise. Never loops — the iOS app
    /// owns the polling timer (every 2-3s from a long-running `Task`),
    /// same contract as `poll_mint_quote`.
    ///
    /// Mirrors the `poll_until_complete` step of the CLI's
    /// `cmd_send_lightning` but with a zero timeout so it returns
    /// after exactly one mint status check.
    ///
    /// `quote_id` is the wallet-side UUID from
    /// [`MeltQuoteHandle::quote_id`].
    ///
    /// Errors:
    /// - `FfiError::Auth { UNAUTHENTICATED }` if no session is loaded.
    /// - `FfiError::Internal` for invalid UUID, missing quote row,
    ///   ownership mismatch, missing account, or mint-protocol failure
    ///   during the single poll round-trip.
    /// - `FfiError::Storage` for raw Supabase failures.
    pub async fn poll_melt_quote(&self, quote_id: String) -> Result<MeltQuoteSnapshot, FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = UserId::from(session.user_id);

        let id = Uuid::parse_str(&quote_id)
            .map_err(|e| FfiError::internal(format!("invalid quote_id: {e}")))?;
        let quote = self
            .melt_quote_storage
            .get(id)
            .await
            .map_err(|e| FfiError::internal(format!("storage error: {e}")))?;
        if quote.user_id != user_id {
            return Err(FfiError::internal("quote belongs to a different user"));
        }

        // Fast-path: anything that isn't PENDING is either still
        // awaiting `execute_melt_quote` (UNPAID) or already terminal —
        // return the persisted snapshot without a mint round-trip.
        if !matches!(quote.state, CashuMeltQuoteState::Pending) {
            return Ok(melt_quote_snapshot_from(&quote));
        }

        let accounts = self.storage.list_accounts(user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == quote.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| FfiError::internal("no matching account for quote"))?
            .clone();

        let seed = self.client.get_cashu_seed().await?;
        // Zero poll-interval + zero timeout → exactly one mint status
        // check, then return. Same "single status check" contract
        // `poll_mint_quote` gets from `poll_until_paid(0, 0)`.
        let outcome = self
            .melt_quote_service
            .poll_until_complete(
                &account,
                quote,
                &seed,
                std::time::Duration::from_millis(0),
                std::time::Duration::from_millis(0),
            )
            .await
            .map_err(melt_quote_error_to_ffi)?;

        Ok(melt_quote_snapshot_from_outcome(&outcome))
    }

    // ---- exchange rate (read-only price feed) surface ----

    /// Fetch the current exchange rate for one currency pair.
    ///
    /// Mirrors the CLI's `build_exchange_rate_deps` /
    /// `provider.get_rate(...)` sequence (`crates/agicash-cli/src/{composition,mint}.rs`):
    /// the slice-4 `MempoolSpaceProvider` is stateless and
    /// auth-independent (no session, no Supabase row), so it's
    /// constructed on demand here instead of being held in a wallet
    /// slot. `from` / `to` are case-insensitive currency codes
    /// (`BTC`, `USD`, `USDB`); the returned
    /// [`ExchangeRateSnapshot`] echoes them back canonically
    /// upper-cased.
    ///
    /// The returned `rate` is the price of `1` major unit of `from`
    /// denominated in major units of `to`, decimal-stringified
    /// (matching the `ReceiveResult.amount` convention). The
    /// provider supports `BTC<->USD`; any other pair surfaces as
    /// `FfiError::Internal` (`unsupported-pair`, mirroring the CLI's
    /// `classify_rate_error`).
    ///
    /// Errors (all funnel to `FfiError::Internal`, matching
    /// `prepare_melt_quote`'s pattern — no auth/storage layer is
    /// touched):
    /// - unknown currency code in `from` / `to`,
    /// - `unsupported-pair` for a pair the provider can't price,
    /// - `network-error` / `invalid-response` from the upstream
    ///   `mempool.space` fetch.
    pub async fn get_exchange_rate(
        &self,
        from: String,
        to: String,
    ) -> Result<ExchangeRateSnapshot, FfiError> {
        let from_currency = Currency::from_str(&from)
            .map_err(|_| FfiError::internal(format!("unsupported currency: {from}")))?;
        let to_currency = Currency::from_str(&to)
            .map_err(|_| FfiError::internal(format!("unsupported currency: {to}")))?;

        // Stateless provider, constructed on demand. Mirrors the CLI's
        // `build_exchange_rate_deps()` — same `MempoolSpaceProvider::new()`.
        let provider = MempoolSpaceProvider::new();
        let rate = provider
            .get_rate(from_currency, to_currency)
            .await
            .map_err(exchange_rate_error_to_ffi)?;

        Ok(exchange_rate_snapshot_from(
            &rate,
            from_currency,
            to_currency,
        ))
    }

    // ---- realtime wallet events (slice 10) ----

    /// Start the realtime wallet-event subscription for the
    /// currently-logged-in user. Joins `realtime:wallet:<userId>` and
    /// forwards every DB broadcast + (re)connect signal to `listener`.
    /// This replaces the platform Tier-1 balance pollers (the
    /// `on_connected` callback is the no-replay catch-up trigger, spec
    /// §5.5; `on_event` carries the opaque `(event, payload_json)` the
    /// caller demuxes).
    ///
    /// The supervisor runs on a tokio task; the user JWT comes from the
    /// **same** `OpenSecretTokenProvider` the wallet builds for storage
    /// (`new`, wrapped through `TokenProviderJwtSource`), so the
    /// realtime `access_token` rotates with the rest of the session. A
    /// prior subscription (if any) is replaced + aborted.
    ///
    /// Errors with `FfiError::Auth { UNAUTHENTICATED }` if no session is
    /// loaded (there is no user id to scope the channel to).
    pub async fn start_wallet_events(
        &self,
        listener: Box<dyn crate::WalletEventListener>,
    ) -> Result<(), FfiError> {
        let session = self.session.read().await.clone().ok_or(FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        })?;
        let user_id = session.user_id.to_string();

        // Reuse the wallet's OpenSecret session: the realtime channel
        // `access_token` is the same third-party JWT storage uses
        // (spec §2.2/§5.6). Rebuilt from `self.client` exactly as the
        // storage token provider is in `new`.
        let token_provider: Arc<dyn TokenProvider + Send + Sync> =
            Arc::new(OpenSecretTokenProvider::new(self.client.clone()));
        let jwt: Arc<dyn agicash_realtime::client::JwtSource> = Arc::new(
            agicash_realtime::service::TokenProviderJwtSource(token_provider),
        );
        let factory: Arc<dyn agicash_realtime::service::TransportFactory> =
            Arc::new(crate::realtime::NativeTransportFactory);
        let svc = Arc::new(agicash_realtime::WalletRealtimeService::new(
            &self.storage.supabase_base_url(),
            &self.storage.anon_key_for_realtime(),
            user_id,
            jwt,
            factory,
        ));

        let mut rx = svc.subscribe();
        // `Box<dyn WalletEventListener>` is `Send + Sync` (UniFFI's
        // foreign shim); behind an `Arc` so the spawned pump owns a
        // clone for the lifetime of the task.
        let listener: Arc<dyn crate::WalletEventListener> = Arc::from(listener);
        let svc_run = Arc::clone(&svc);
        let handle = tokio::spawn(async move {
            // Pump: drain the service's broadcast receiver and fan each
            // event out to the foreign listener via the FFI bridge.
            let pump = {
                let listener = Arc::clone(&listener);
                async move {
                    while let Ok(ev) = rx.recv().await {
                        crate::realtime::dispatch_realtime_event(listener.as_ref(), ev);
                    }
                }
            };
            // Drive the supervisor + pump together; `stop()` (via
            // `stop_wallet_events`) ends `run()`, after which the
            // broadcast sender drops and the pump's `recv()` returns
            // `Err`, ending the task cleanly even before `abort()`.
            tokio::join!(svc_run.run(), pump);
        });

        // Replace + abort any prior subscription.
        if let Some(old) = self.realtime_task.write().await.replace(handle) {
            old.abort();
        }
        if let Some(old_svc) = self
            .realtime_service
            .write()
            .await
            .replace(Arc::clone(&svc))
        {
            old_svc.stop();
        }
        Ok(())
    }

    /// Stop the realtime subscription: `phx_leave` + close the socket
    /// (clean), then abort the supervisor task. Idempotent — calling it
    /// with nothing running is a no-op (the platform calls it
    /// unconditionally on teardown / sign-out).
    pub async fn stop_wallet_events(&self) -> Result<(), FfiError> {
        if let Some(svc) = self.realtime_service.write().await.take() {
            svc.stop();
        }
        if let Some(h) = self.realtime_task.write().await.take() {
            h.abort();
        }
        Ok(())
    }
}

// Internal (non-FFI) helpers. Kept out of the `#[uniffi::export]` impl
// block so UniFFI's bindgen doesn't try to lift `&PersistedSession`
// across the FFI boundary — it isn't a UniFFI type.
impl AgicashWallet {
    /// Write the given session through to the installed
    /// [`SessionStorage`] (if any). Errors are logged but not surfaced —
    /// auth methods should succeed even if persistence fails; the
    /// session is still usable in-memory for the rest of the process
    /// lifetime, the user just won't survive a cold start.
    async fn persist_session(&self, session: &PersistedSession) {
        let storage_opt = self.session_storage.read().await.clone();
        if let Some(storage) = storage_opt {
            if let Err(e) = storage.store(session).await {
                tracing::warn!(
                    target: "agicash_ffi::wallet",
                    error = %e,
                    "persist_session: storage.store() failed (continuing in-memory)"
                );
            }
        }
    }

    /// Clear any persisted session blob. Called from `auth_logout`.
    /// Errors are logged but not surfaced — logout always returns Ok so
    /// the UI can navigate back to the sign-in screen even if the
    /// on-disk clear failed.
    async fn clear_persisted_session(&self) {
        let storage_opt = self.session_storage.read().await.clone();
        if let Some(storage) = storage_opt {
            if let Err(e) = storage.clear().await {
                tracing::warn!(
                    target: "agicash_ffi::wallet",
                    error = %e,
                    "clear_persisted_session: storage.clear() failed"
                );
            }
        }
    }
}

/// Find a `Cashu` account whose `(mint_url, currency)` pair matches the
/// supplied parsed token. Mirrors the CLI's private `pick_account`
/// (`crates/agicash-cli/src/receive.rs`) — duplicated here so the FFI
/// stays decoupled from the CLI binary.
// TODO(12b-1 Task 12): token-account pick now lives in the facade
// `receive_cashu_token`; this shell copy is dead. Allow until the
// deletion pass.
#[allow(dead_code)]
fn pick_cashu_account_for_token<'a>(
    accounts: &'a [Account],
    mint_url: &str,
    unit: &cdk::nuts::CurrencyUnit,
) -> Option<&'a Account> {
    accounts.iter().find(|a| {
        a.account_type == AccountType::Cashu
            && a.details
                .get("mint_url")
                .and_then(|v| v.as_str())
                .is_some_and(|u| mint_urls_equal(u, mint_url))
            && unit_matches_currency(unit, a.currency)
    })
}

// TODO(12b-1 Task 12): dead with the facade-delegated receive path.
#[allow(dead_code)]
fn mint_urls_equal(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

// TODO(12b-1 Task 12): dead with the facade-delegated receive path.
#[allow(dead_code)]
fn unit_matches_currency(unit: &cdk::nuts::CurrencyUnit, currency: Currency) -> bool {
    use cdk::nuts::CurrencyUnit;
    matches!(
        (unit, currency),
        (CurrencyUnit::Sat, Currency::Btc) | (CurrencyUnit::Usd, Currency::Usd)
    )
}

/// Sum the UNSPENT proofs for a single account.
///
/// Cashu accounts route through `CashuSendSwapStorage::list_unspent_proofs`
/// which internally decrypts each proof's encrypted amount. Spark accounts
/// always return 0 — slice 9 will wire their proof storage and replace this
/// branch. Storage failures are funneled through `FfiError::Internal`
/// (matching the `receive_swap_error_to_ffi` shape) since
/// `SendSwapStorageError` doesn't fit the structured Auth/Storage variants
/// cleanly.
// TODO(12b-1 Task 12): balance summing now lives in the facade
// `list_accounts`; this shell copy is dead once the deletion pass runs.
// Allow until then so the per-task gate stays green.
#[allow(dead_code)]
async fn compute_cashu_balance(
    storage: &dyn CashuSendSwapStorage,
    account: &Account,
) -> Result<u64, FfiError> {
    match account.account_type {
        AccountType::Cashu => {
            tracing::info!(
                target: "agicash_ffi::wallet",
                account_id = %account.id,
                "compute_cashu_balance: calling list_unspent_proofs"
            );
            let proofs = storage
                .list_unspent_proofs(account.id)
                .await
                .map_err(|e| FfiError::internal(format!("list unspent proofs: {e}")))?;
            let total: u64 = proofs.iter().map(|p| p.proof.amount).sum();
            tracing::info!(
                target: "agicash_ffi::wallet",
                account_id = %account.id,
                proof_count = proofs.len(),
                total_amount = total,
                "compute_cashu_balance: list_unspent_proofs returned"
            );
            Ok(total)
        }
        AccountType::Spark => Ok(0),
    }
}

/// Map the rich `ReceiveSwapError` family down to `FfiError`. The trait
/// crate already has `From<AuthError>` / `From<StorageError>` impls for
/// FFI; the cashu-specific cases (token parse, mint-mismatch,
/// amount-too-small) don't fit either family cleanly so they funnel
/// through `Internal` with a discriminator-bearing message.
// TODO(12b-1 Task 12): the facade's `WalletError` path
// (`convert::wallet_error_to_ffi`) replaces this; dead until deletion.
#[allow(dead_code)]
fn receive_swap_error_to_ffi(e: ReceiveSwapError) -> FfiError {
    match e {
        ReceiveSwapError::TokenParse(msg) => FfiError::internal(format!("invalid token: {msg}")),
        ReceiveSwapError::MintMismatch { token, account } => FfiError::internal(format!(
            "mint mismatch: token mint {token} differs from account mint {account}",
        )),
        ReceiveSwapError::CurrencyMismatch { token, account } => FfiError::internal(format!(
            "currency mismatch: token currency {token} differs from account currency {account}",
        )),
        ReceiveSwapError::AmountTooSmall => FfiError::internal("amount too small after mint fees"),
        ReceiveSwapError::InvalidTransition { from, event } => {
            FfiError::internal(format!("invalid state transition from {from} on {event}"))
        }
        // Mint-protocol failures (network, NUT errors) — surface the
        // `CashuProviderError`'s display so the UI gets something
        // meaningful without a new FFI variant.
        ReceiveSwapError::Mint(inner) => FfiError::internal(format!("mint error: {inner}")),
        // Storage is the one branch where we DO have a structured FFI
        // shape. Map the inner storage failure through the existing
        // `From<StorageError>` impl when possible; otherwise fall back
        // to Internal so the caller still sees the failure reason.
        ReceiveSwapError::Storage(s) => FfiError::internal(format!("storage error: {s}")),
        // NUT-12 DLEQ verification failed on a mint-returned blind signature
        // or peer-token proof. A mint that fails DLEQ is malicious or
        // compromised — surface with a distinct prefix so the UI can render
        // a security-flavoured error rather than a generic network blip.
        ReceiveSwapError::DleqVerificationFailed(inner) => {
            FfiError::internal(format!("DLEQ verification failed: {inner}"))
        }
    }
}

/// Map the rich `CashuProviderError` family down to `FfiError`. The mint-
/// provider failures (invalid URL, network, NUT protocol) are funneled
/// through `Internal` with a discriminator-bearing message, same shape as
/// `receive_swap_error_to_ffi` — the iOS UI parses the message prefix to
/// render an inline form-level error.
fn cashu_provider_error_to_ffi(e: CashuProviderError) -> FfiError {
    match e {
        CashuProviderError::InvalidUrl(msg) => {
            FfiError::internal(format!("invalid mint URL: {msg}"))
        }
        CashuProviderError::Network(msg) => FfiError::internal(format!("mint unreachable: {msg}")),
        CashuProviderError::Protocol(msg) => FfiError::internal(format!("mint error: {msg}")),
    }
}

// TODO(12b-1 Task 12): the facade's `ReceiveReceipt` →
// `convert::receive_result_from_receipt` replaces this; dead until
// deletion.
#[allow(dead_code)]
fn receive_result_from_outcome(
    outcome: CompleteOutcome,
    fallback_account: &Account,
    parsed: &ParsedToken,
) -> ReceiveResult {
    match outcome {
        CompleteOutcome::Completed { swap, account, .. } => ReceiveResult {
            status: ReceiveStatus::Received,
            amount: swap.amount_received.amount().to_string(),
            fee: swap.fee_amount.amount().to_string(),
            unit: swap.amount_received.unit().to_string(),
            currency: swap.amount_received.currency().to_string(),
            account_id: account.id.to_string(),
            mint_url: parsed.mint_url.clone(),
            token_hash: parsed.hash.clone(),
        },
        CompleteOutcome::AlreadyTerminal(swap) => {
            let status = match &swap.state {
                CashuReceiveSwapState::Completed => ReceiveStatus::Received,
                CashuReceiveSwapState::Failed { .. } => ReceiveStatus::AlreadyFailed,
                CashuReceiveSwapState::Pending => ReceiveStatus::Pending,
            };
            ReceiveResult {
                status,
                amount: swap.amount_received.amount().to_string(),
                fee: swap.fee_amount.amount().to_string(),
                unit: swap.amount_received.unit().to_string(),
                currency: swap.amount_received.currency().to_string(),
                account_id: fallback_account.id.to_string(),
                mint_url: parsed.mint_url.clone(),
                token_hash: parsed.hash.clone(),
            }
        }
        CompleteOutcome::Failed(swap) => ReceiveResult {
            status: ReceiveStatus::AlreadyFailed,
            amount: swap.amount_received.amount().to_string(),
            fee: swap.fee_amount.amount().to_string(),
            unit: swap.amount_received.unit().to_string(),
            currency: swap.amount_received.currency().to_string(),
            account_id: fallback_account.id.to_string(),
            mint_url: parsed.mint_url.clone(),
            token_hash: parsed.hash.clone(),
        },
    }
}

// ---- mint-quote (Lightning receive) helpers ----

/// Select the receiving Cashu account for a Lightning receive. Mirrors
/// the CLI's `pick_account` in `receive_lightning.rs`: when `requested`
/// (UUID string) is `Some`, find the matching Cashu+currency row; when
/// `None`, pick the unique Cashu+currency row or report
/// none/ambiguous.
///
/// Different from `pick_cashu_account_for_token` because Lightning
/// receives don't carry a mint URL — the user-chosen account
/// determines which mint we ask for a quote.
fn pick_cashu_account_for_lightning<'a>(
    accounts: &'a [Account],
    requested: Option<&str>,
    currency: Currency,
) -> Result<&'a Account, FfiError> {
    let cashu: Vec<&Account> = accounts
        .iter()
        .filter(|a| a.account_type == AccountType::Cashu && a.currency == currency)
        .collect();
    match requested {
        Some(id_str) => {
            let id = Uuid::parse_str(id_str)
                .map_err(|e| FfiError::internal(format!("invalid account_id: {e}")))?;
            cashu
                .into_iter()
                .find(|a| a.id == agicash_domain::AccountId::from(id))
                .ok_or_else(|| {
                    FfiError::internal(format!("no Cashu {currency} account with id {id_str}"))
                })
        }
        None => match cashu.len() {
            0 => Err(FfiError::internal(format!(
                "no Cashu {currency} account — add a mint first"
            ))),
            1 => Ok(cashu[0]),
            _ => Err(FfiError::internal(format!(
                "multiple Cashu {currency} accounts — pass account_id"
            ))),
        },
    }
}

/// Map `MintQuoteError` down to `FfiError`. Same funneling pattern as
/// `receive_swap_error_to_ffi`: storage/network/protocol failures land
/// in `Internal` with a discriminator-bearing message; validation
/// failures (amount-too-small, currency mismatch) stay as their own
/// strings so the iOS UI can pattern-match the prefix.
// TODO(12b-1 Task 12): facade `WalletError` path replaces this; only
// referenced by its own unit tests now. Allow until the deletion pass.
#[allow(dead_code)]
fn mint_quote_error_to_ffi(e: MintQuoteError) -> FfiError {
    match e {
        MintQuoteError::AmountTooSmall => FfiError::internal("amount too small"),
        MintQuoteError::CurrencyMismatch { account, request } => FfiError::internal(format!(
            "currency mismatch: account {account} differs from request {request}",
        )),
        MintQuoteError::QuoteNotPaid => FfiError::internal("quote not yet paid"),
        MintQuoteError::QuoteExpired => FfiError::internal("quote expired before payment"),
        MintQuoteError::InvalidTransition { from, event } => {
            FfiError::internal(format!("invalid state transition from {from} on {event}"))
        }
        MintQuoteError::Unrecoverable(msg) => {
            FfiError::internal(format!("mint quote unrecoverable: {msg}"))
        }
        MintQuoteError::Mint(inner) => cashu_provider_error_to_ffi(inner),
        MintQuoteError::Storage(s) => FfiError::internal(format!("storage error: {s}")),
        // NUT-12 DLEQ verification failed on the mint-returned blind
        // signature. Treat as a distinct, security-flavoured error.
        MintQuoteError::DleqVerificationFailed(inner) => {
            FfiError::internal(format!("DLEQ verification failed: {inner}"))
        }
    }
}

/// Map `SendSwapError` down to `FfiError`. Same funneling pattern as
/// `receive_swap_error_to_ffi` and `mint_quote_error_to_ffi`.
fn send_swap_error_to_ffi(e: agicash_cashu::SendSwapError) -> FfiError {
    use agicash_cashu::SendSwapError;
    match e {
        SendSwapError::AmountTooSmall => FfiError::internal("amount too small"),
        SendSwapError::CurrencyMismatch { account, request } => FfiError::internal(format!(
            "currency mismatch: account {account} differs from request {request}",
        )),
        SendSwapError::InsufficientBalance { needed, have } => {
            FfiError::internal(format!("insufficient balance: need {needed}, have {have}",))
        }
        SendSwapError::InvalidTransition { from, event } => {
            FfiError::internal(format!("invalid state transition from {from} on {event}"))
        }
        SendSwapError::Mint(inner) => cashu_provider_error_to_ffi(inner),
        SendSwapError::Storage(s) => FfiError::internal(format!("storage error: {s}")),
        SendSwapError::TokenEncode(msg) => FfiError::internal(format!("token encode error: {msg}")),
        SendSwapError::DleqVerificationFailed(inner) => {
            FfiError::internal(format!("DLEQ verification failed: {inner}"))
        }
    }
}

/// Map `Currency` -> minor `Unit` for amount-money construction. Mirrors
/// the CLI's `unit_for_currency` (`receive_lightning.rs`).
fn unit_for_currency(currency: Currency) -> Unit {
    match currency {
        Currency::Btc => Unit::Sat,
        Currency::Usd | Currency::Usdb => Unit::Cent,
    }
}

/// Build the `MintQuoteHandle` returned by `start_mint_quote`. The
/// amount + fee are decimal-stringified to match the
/// `ReceiveResult` convention.
// TODO(12b-1 Task 12): replaced by `convert::mint_quote_handle_from_facade`.
#[allow(dead_code)]
fn mint_quote_handle_from(quote: &CashuMintQuote, account: &Account) -> MintQuoteHandle {
    MintQuoteHandle {
        quote_id: quote.id.to_string(),
        mint_quote_id: quote.quote_id.clone(),
        invoice: quote.payment_request.clone(),
        payment_hash: quote.payment_hash.clone(),
        amount: quote.amount.amount().to_string(),
        fee: quote.total_fee.amount().to_string(),
        unit: quote.amount.unit().to_string(),
        currency: quote.amount.currency().to_string(),
        account_id: account.id.to_string(),
        expires_at: quote.expires_at.to_rfc3339(),
    }
}

/// Convert a persisted `CashuMintQuote` into the FFI snapshot. Maps
/// the per-state Rust enum down to the flat FFI discriminator.
// TODO(12b-1 Task 12): replaced by `convert::mint_quote_snapshot_from_facade`.
#[allow(dead_code)]
fn mint_quote_snapshot_from(quote: &CashuMintQuote) -> MintQuoteSnapshot {
    match &quote.state {
        CashuMintQuoteState::Unpaid => MintQuoteSnapshot {
            state: MintQuoteFfiState::Unpaid,
            failure_reason: None,
        },
        CashuMintQuoteState::Paid { .. } => MintQuoteSnapshot {
            state: MintQuoteFfiState::Paid,
            failure_reason: None,
        },
        CashuMintQuoteState::Completed { .. } => MintQuoteSnapshot {
            state: MintQuoteFfiState::Completed,
            failure_reason: None,
        },
        CashuMintQuoteState::Expired => MintQuoteSnapshot {
            state: MintQuoteFfiState::Expired,
            failure_reason: None,
        },
        CashuMintQuoteState::Failed { failure_reason } => MintQuoteSnapshot {
            state: MintQuoteFfiState::Failed,
            failure_reason: Some(failure_reason.clone()),
        },
    }
}

/// Build the `ReceiveResult` returned by `complete_mint_quote`. The shape
/// is identical to what `receive_token` returns so the iOS success card
/// can render uniformly across both flows. Mirrors
/// `receive_result_from_outcome` (which handles the Cashu-token swap
/// outcome) but for the `CompleteMintQuoteOutcome` enum.
// TODO(12b-1 Task 12): replaced by the facade `ReceiveReceipt` →
// `convert::receive_result_from_receipt` path.
#[allow(dead_code)]
fn receive_result_from_mint_quote_outcome(
    outcome: CompleteMintQuoteOutcome,
    fallback_account: &Account,
    fallback_quote: &CashuMintQuote,
) -> ReceiveResult {
    // Lightning quotes don't carry a token hash; we synthesize one from
    // the BOLT-11 payment hash so the receipt has a stable identifier.
    let token_hash = fallback_quote.payment_hash.clone();
    let mint_url = mint_url_from_account(fallback_account);

    match outcome {
        CompleteMintQuoteOutcome::Completed {
            quote,
            account,
            added_proofs: _,
        } => ReceiveResult {
            status: ReceiveStatus::Received,
            amount: quote.amount.amount().to_string(),
            fee: quote.total_fee.amount().to_string(),
            unit: quote.amount.unit().to_string(),
            currency: quote.amount.currency().to_string(),
            account_id: account.id.to_string(),
            mint_url: mint_url_from_account(&account),
            token_hash,
        },
        CompleteMintQuoteOutcome::AlreadyTerminal(quote) => {
            let status = match &quote.state {
                CashuMintQuoteState::Completed { .. } => ReceiveStatus::Received,
                CashuMintQuoteState::Failed { .. } | CashuMintQuoteState::Expired => {
                    ReceiveStatus::AlreadyFailed
                }
                // PAID without a follow-up complete is "still pending" from the
                // UI's perspective; treat as `Pending` so the user can retry.
                CashuMintQuoteState::Paid { .. } | CashuMintQuoteState::Unpaid => {
                    ReceiveStatus::Pending
                }
            };
            ReceiveResult {
                status,
                amount: quote.amount.amount().to_string(),
                fee: quote.total_fee.amount().to_string(),
                unit: quote.amount.unit().to_string(),
                currency: quote.amount.currency().to_string(),
                account_id: fallback_account.id.to_string(),
                mint_url,
                token_hash,
            }
        }
        CompleteMintQuoteOutcome::Failed(quote) => ReceiveResult {
            status: ReceiveStatus::AlreadyFailed,
            amount: quote.amount.amount().to_string(),
            fee: quote.total_fee.amount().to_string(),
            unit: quote.amount.unit().to_string(),
            currency: quote.amount.currency().to_string(),
            account_id: fallback_account.id.to_string(),
            mint_url,
            token_hash,
        },
    }
}

/// Pull the canonical `mint_url` string out of a Cashu account's
/// `details` blob. Defaults to empty string if the column is missing or
/// malformed — the iOS UI tolerates empty mint URLs in its success card
/// rendering (drops the line) so we don't need to error here.
// TODO(12b-1 Task 12): only the now-dead `receive_result_from_mint_quote_outcome`
// used this; dead until the deletion pass.
#[allow(dead_code)]
fn mint_url_from_account(account: &Account) -> String {
    account
        .details
        .get("mint_url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default()
}

// ---- melt-quote (Lightning send) helpers ----

/// Map `MeltQuoteError` down to `FfiError`. Same funneling pattern as
/// `mint_quote_error_to_ffi`: storage/network/protocol failures land in
/// `Internal` with a discriminator-bearing message; validation
/// failures (bad invoice, amountless, insufficient balance, currency
/// mismatch) stay as their own strings so the iOS UI can pattern-match
/// the prefix.
fn melt_quote_error_to_ffi(e: MeltQuoteError) -> FfiError {
    match e {
        MeltQuoteError::InvalidTransition { from, event } => {
            FfiError::internal(format!("invalid state transition from {from} on {event}"))
        }
        MeltQuoteError::Storage(s) => FfiError::internal(format!("storage error: {s}")),
        // An active melt quote already exists for this invoice —
        // re-quoting would double-pay. Distinct discriminator-bearing
        // message so the iOS UI can pattern-match the prefix and resume
        // the existing quote / show the paid receipt instead of
        // retrying. Same funneling shape as the DLEQ branch below.
        MeltQuoteError::DuplicatePayment => FfiError::internal(
            "DUPLICATE_PAYMENT: an active melt quote already exists for this invoice",
        ),
        MeltQuoteError::Mint(inner) => cashu_provider_error_to_ffi(inner),
        MeltQuoteError::InvalidInvoice(msg) => {
            FfiError::internal(format!("invalid bolt11 invoice: {msg}"))
        }
        MeltQuoteError::AmountlessInvoice => FfiError::internal("amountless invoice not supported"),
        MeltQuoteError::AmountTooSmall => FfiError::internal("amount too small"),
        MeltQuoteError::CurrencyMismatch { account, request } => FfiError::internal(format!(
            "currency mismatch: account {account} differs from request {request}",
        )),
        MeltQuoteError::InsufficientBalance { needed, have } => {
            FfiError::internal(format!("insufficient balance: need {needed}, have {have}",))
        }
        MeltQuoteError::QuoteExpired => FfiError::internal("quote expired before payment"),
        MeltQuoteError::QuoteNotPending => FfiError::internal("quote not yet pending"),
        MeltQuoteError::MeltFailed(msg) => {
            FfiError::internal(format!("melt failed at mint: {msg}"))
        }
        MeltQuoteError::Unrecoverable(msg) => {
            FfiError::internal(format!("melt unrecoverable: {msg}"))
        }
        // NUT-12 DLEQ verification failed on a mint-returned change
        // blank. Treat as a distinct, security-flavoured error (same
        // shape as `mint_quote_error_to_ffi`).
        MeltQuoteError::DleqVerificationFailed(inner) => {
            FfiError::internal(format!("DLEQ verification failed: {inner}"))
        }
    }
}

/// Build the `MeltQuotePreview` FFI record returned by
/// `prepare_melt_quote`. Decimal-stringifies the `Money` fields to
/// match the `ReceiveResult` / `SendQuotePreview` convention. Mirrors
/// the CLI's `print_dry_run` (`send_lightning.rs`).
fn melt_quote_preview_from(preview: &MeltQuotePreview, account: &Account) -> MeltQuotePreviewFfi {
    MeltQuotePreviewFfi {
        amount: preview.amount_received.amount().to_string(),
        lightning_fee_reserve: preview.lightning_fee_reserve.amount().to_string(),
        cashu_fee: preview.cashu_fee.amount().to_string(),
        total_fee: preview.total_fee.amount().to_string(),
        total_amount: preview.total_amount.amount().to_string(),
        unit: preview.amount_received.unit().to_string(),
        currency: account.currency.to_string(),
        account_id: account.id.to_string(),
        payment_hash: preview.payment_hash.clone(),
    }
}

/// Build the `MeltQuoteHandle` returned by `create_melt_quote`. Mirrors
/// the CLI's `print_quote_issued` (`send_lightning.rs`) — same
/// fields the receipt / in-flight card needs. The `total_fee` is the
/// worst-case `lightning_fee_reserve + cashu_fee` (the same `try_add`
/// the CLI uses, falling back to `"0"` on the impossible currency
/// mismatch).
fn melt_quote_handle_from(quote: &CashuMeltQuote, account: &Account) -> MeltQuoteHandle {
    let total_fee = quote
        .lightning_fee_reserve
        .try_add(&quote.cashu_fee)
        .map_or_else(|_| "0".to_string(), |m| m.amount().to_string());
    MeltQuoteHandle {
        quote_id: quote.id.to_string(),
        melt_quote_id: quote.quote_id.clone(),
        invoice: quote.payment_request.clone(),
        payment_hash: quote.payment_hash.clone(),
        amount: quote.amount_received.amount().to_string(),
        lightning_fee_reserve: quote.lightning_fee_reserve.amount().to_string(),
        cashu_fee: quote.cashu_fee.amount().to_string(),
        total_fee,
        unit: quote.amount_received.unit().to_string(),
        currency: account.currency.to_string(),
        account_id: account.id.to_string(),
        expires_at: quote.expires_at.to_rfc3339(),
    }
}

/// Convert a persisted `CashuMeltQuote` into the FFI snapshot. Maps the
/// per-state Rust enum down to the flat FFI discriminator and surfaces
/// the PAID receipt fields. Mirrors `mint_quote_snapshot_from`.
fn melt_quote_snapshot_from(quote: &CashuMeltQuote) -> MeltQuoteSnapshot {
    match &quote.state {
        CashuMeltQuoteState::Unpaid => MeltQuoteSnapshot {
            state: MeltQuoteFfiState::Unpaid,
            failure_reason: None,
            payment_preimage: None,
            lightning_fee: None,
            amount_spent: None,
            total_fee: None,
        },
        CashuMeltQuoteState::Pending => MeltQuoteSnapshot {
            state: MeltQuoteFfiState::Pending,
            failure_reason: None,
            payment_preimage: None,
            lightning_fee: None,
            amount_spent: None,
            total_fee: None,
        },
        CashuMeltQuoteState::Paid {
            payment_preimage,
            lightning_fee,
            amount_spent,
            total_fee,
        } => MeltQuoteSnapshot {
            state: MeltQuoteFfiState::Paid,
            failure_reason: None,
            payment_preimage: Some(payment_preimage.clone()),
            lightning_fee: Some(lightning_fee.amount().to_string()),
            amount_spent: Some(amount_spent.amount().to_string()),
            total_fee: Some(total_fee.amount().to_string()),
        },
        CashuMeltQuoteState::Expired => MeltQuoteSnapshot {
            state: MeltQuoteFfiState::Expired,
            failure_reason: None,
            payment_preimage: None,
            lightning_fee: None,
            amount_spent: None,
            total_fee: None,
        },
        CashuMeltQuoteState::Failed { failure_reason } => MeltQuoteSnapshot {
            state: MeltQuoteFfiState::Failed,
            failure_reason: Some(failure_reason.clone()),
            payment_preimage: None,
            lightning_fee: None,
            amount_spent: None,
            total_fee: None,
        },
    }
}

/// Map a `MeltOutcome` (from `initiate_melt` / `poll_until_complete`)
/// onto the FFI snapshot. The `Paid`/`Failed` variants carry the
/// persisted terminal quote so we delegate to `melt_quote_snapshot_from`
/// for the receipt fields; `Pending` carries the still-in-flight quote.
fn melt_quote_snapshot_from_outcome(outcome: &MeltOutcome) -> MeltQuoteSnapshot {
    // Every variant carries the persisted quote whose `state` already
    // encodes the terminal/in-flight bucket, so the snapshot is built
    // identically from it regardless of which variant we got.
    let quote = match outcome {
        MeltOutcome::Paid { quote, .. }
        | MeltOutcome::Pending(quote)
        | MeltOutcome::Failed(quote) => quote,
    };
    melt_quote_snapshot_from(quote)
}

// ---- exchange-rate (read-only price feed) helpers ----

/// Map `ExchangeRateError` down to `FfiError`. Same funneling pattern
/// as `melt_quote_error_to_ffi` — every variant collapses to
/// `FfiError::Internal` (the price feed touches no auth/storage
/// layer, so there's no structured `Auth`/`Storage` bucket to route
/// to). The message prefixes mirror the CLI's `classify_rate_error`
/// tags (`network-error` / `invalid-response` / `unsupported-pair`,
/// `crates/agicash-cli/src/mint.rs`) so operators can grep the same
/// strings across the CLI and the iOS error surface.
fn exchange_rate_error_to_ffi(e: ExchangeRateError) -> FfiError {
    match e {
        ExchangeRateError::Network(msg) => FfiError::internal(format!("network-error: {msg}")),
        ExchangeRateError::InvalidResponse(msg) => {
            FfiError::internal(format!("invalid-response: {msg}"))
        }
        ExchangeRateError::UnsupportedPair { from, to } => {
            FfiError::internal(format!("unsupported-pair: {from} -> {to}"))
        }
    }
}

/// Build the [`ExchangeRateSnapshot`] returned by
/// `get_exchange_rate`. Decimal-stringifies the rate to match the
/// `ReceiveResult.amount` / `MeltQuotePreview` convention, and echoes
/// the parsed pair back as canonical upper-case codes via
/// `Currency`'s `Display`.
fn exchange_rate_snapshot_from(
    rate: &Decimal,
    from: Currency,
    to: Currency,
) -> ExchangeRateSnapshot {
    ExchangeRateSnapshot {
        rate: rate.to_string(),
        from: from.to_string(),
        to: to.to_string(),
    }
}

// ---- cashu send-swap helpers ----

/// Encode a slice of `TokenProof` into a V4 (`cashuB…`) wire token.
/// Mirrors `encode_token` in `crates/agicash-cli/src/send.rs` with
/// `token_version = 4` (the default `.to_string()` path on
/// `cdk::nuts::Token`).
// TODO(12b-1 Task 12): V4 token encode now lives in the facade
// `send_token`; this shell copy is dead until the deletion pass.
#[allow(dead_code)]
fn encode_v4_token(
    mint_url: &str,
    proofs: &[TokenProof],
    currency: Currency,
) -> Result<String, String> {
    let mint = MintUrl::from_str(mint_url).map_err(|e| format!("mint url: {e}"))?;
    let cdk_proofs: Vec<Proof> = proofs
        .iter()
        .map(token_proof_to_cdk_proof)
        .collect::<Result<Vec<_>, _>>()?;
    let unit = cashu_unit_for_currency(currency);
    let token = Token::new(mint, cdk_proofs, None, unit);
    Ok(token.to_string())
}

// TODO(12b-1 Task 12): only the now-dead `encode_v4_token` used this.
#[allow(dead_code)]
fn cashu_unit_for_currency(currency: Currency) -> CurrencyUnit {
    match currency {
        Currency::Btc => CurrencyUnit::Sat,
        Currency::Usd | Currency::Usdb => CurrencyUnit::Usd,
    }
}

// TODO(12b-1 Task 12): only the now-dead `encode_v4_token` used this.
#[allow(dead_code)]
fn token_proof_to_cdk_proof(proof: &TokenProof) -> Result<Proof, String> {
    use cdk::nuts::PublicKey;
    use cdk::secret::Secret;
    let keyset_id =
        KeysetId::from_str(&proof.id).map_err(|e| format!("keyset id {}: {e}", proof.id))?;
    let secret = Secret::from_str(&proof.secret).map_err(|e| format!("secret: {e}"))?;
    let c = PublicKey::from_hex(&proof.c).map_err(|e| format!("C: {e}"))?;
    Ok(Proof {
        amount: Amount::from(proof.amount),
        keyset_id,
        secret,
        c,
        witness: None,
        dleq: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeConfig {
        opensecret_url: String,
        client_id: String,
        supabase_url: String,
        anon_key: String,
    }

    fn fake_config() -> FakeConfig {
        FakeConfig {
            opensecret_url: "https://does-not-resolve-agicash.invalid".to_string(),
            client_id: Uuid::nil().to_string(),
            supabase_url: "https://does-not-resolve-supabase.invalid".to_string(),
            anon_key: "anon-key".to_string(),
        }
    }

    #[tokio::test]
    async fn constructor_returns_wallet_without_network() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let status = wallet.auth_status().await.unwrap();
        assert!(!status.logged_in);
        assert!(status.user_id.is_none());
    }

    #[tokio::test]
    async fn list_accounts_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet.list_accounts().await.expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn mint_add_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .mint_add("https://example.invalid".into())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn get_user_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet.get_user().await.expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn set_default_account_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .set_default_account(Uuid::new_v4().to_string())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn set_default_account_rejects_bad_uuid() {
        // No session → unauth wins, so this test would pass for the wrong
        // reason if we skipped the session check first. Re-order isn't an
        // option here without a real session, so the test asserts that the
        // unauth check IS first (consistent with every other FFI method).
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .set_default_account("not-a-uuid".into())
            .await
            .expect_err("no session");
        // Auth check fires first by design — same shape as start_mint_quote /
        // poll_mint_quote. The bad-uuid branch is exercised once a session
        // is loaded; covered by manual sim verification + by the validator
        // helper test below.
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[test]
    fn parse_account_id_for_default_rejects_garbage() {
        // The bad-uuid path inside `set_default_account` is just
        // `Uuid::parse_str(...)` with a custom error prefix. Re-test the
        // raw parser so the assertion lives somewhere even when a session
        // isn't available.
        let err = Uuid::parse_str("not-a-uuid").expect_err("bad uuid");
        // Sanity: confirm the error display is non-empty so the FFI's
        // `format!("invalid account_id: {e}")` produces a useful message.
        assert!(!err.to_string().is_empty());
    }

    #[tokio::test]
    async fn constructor_rejects_bad_client_id_uuid() {
        let err = AgicashWallet::new(
            "https://example.invalid".into(),
            "not-a-uuid".into(),
            "https://supabase.invalid".into(),
            "anon".into(),
        )
        .expect_err("bad uuid");
        assert!(
            matches!(err, FfiError::Internal { ref message } if message.contains("client_id_uuid"))
        );
    }

    // ---- mint-quote (Lightning receive) FFI surface ----

    #[tokio::test]
    async fn start_mint_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .start_mint_quote(64, None, None)
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn poll_mint_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .poll_mint_quote(Uuid::new_v4().to_string())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn complete_mint_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .complete_mint_quote(Uuid::new_v4().to_string())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    // ---- cashu send-swap FFI surface ----

    #[tokio::test]
    async fn prepare_send_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .prepare_send_quote(64, None, None)
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn create_send_swap_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .create_send_swap(64, None, None)
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn check_send_swap_claimed_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .check_send_swap_claimed(Uuid::new_v4().to_string())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    // ---- helper unit tests (no FFI, no network) ----

    fn stub_account(currency: Currency) -> Account {
        use agicash_domain::{AccountId, AccountPurpose, AccountState};
        use chrono::Utc;
        use serde_json::json;
        Account {
            id: AccountId::new(),
            created_at: Utc::now(),
            user_id: UserId::new(),
            name: "Mint".into(),
            account_type: AccountType::Cashu,
            purpose: AccountPurpose::Transactional,
            currency,
            details: json!({ "mint_url": "https://m.example", "keyset_counters": {} }),
            version: 0,
            state: AccountState::Active,
            expires_at: None,
        }
    }

    #[test]
    fn pick_cashu_for_lightning_returns_only_cashu_btc() {
        let mut other = stub_account(Currency::Btc);
        other.account_type = AccountType::Spark;
        let accounts = vec![other, stub_account(Currency::Btc)];
        let picked =
            pick_cashu_account_for_lightning(&accounts, None, Currency::Btc).expect("found");
        assert_eq!(picked.account_type, AccountType::Cashu);
    }

    #[test]
    fn pick_cashu_for_lightning_errors_when_no_match() {
        let accounts = vec![stub_account(Currency::Usd)];
        let err =
            pick_cashu_account_for_lightning(&accounts, None, Currency::Btc).expect_err("none");
        assert!(matches!(err, FfiError::Internal { ref message } if message.contains("no Cashu")));
    }

    #[test]
    fn pick_cashu_for_lightning_errors_when_ambiguous() {
        let accounts = vec![stub_account(Currency::Btc), stub_account(Currency::Btc)];
        let err = pick_cashu_account_for_lightning(&accounts, None, Currency::Btc)
            .expect_err("ambiguous");
        assert!(matches!(err, FfiError::Internal { ref message } if message.contains("multiple")));
    }

    #[test]
    fn pick_cashu_for_lightning_rejects_bad_uuid() {
        let accounts = vec![stub_account(Currency::Btc)];
        let err = pick_cashu_account_for_lightning(&accounts, Some("not-a-uuid"), Currency::Btc)
            .expect_err("bad uuid");
        assert!(
            matches!(err, FfiError::Internal { ref message } if message.contains("invalid account_id"))
        );
    }

    #[test]
    fn mint_quote_error_to_ffi_maps_amount_too_small() {
        let e = mint_quote_error_to_ffi(MintQuoteError::AmountTooSmall);
        assert!(matches!(e, FfiError::Internal { ref message } if message.contains("too small")));
    }

    #[test]
    fn mint_quote_error_to_ffi_maps_quote_not_paid() {
        let e = mint_quote_error_to_ffi(MintQuoteError::QuoteNotPaid);
        assert!(
            matches!(e, FfiError::Internal { ref message } if message.contains("not yet paid"))
        );
    }

    #[test]
    fn unit_for_currency_maps_btc_to_sat() {
        assert_eq!(unit_for_currency(Currency::Btc), Unit::Sat);
        assert_eq!(unit_for_currency(Currency::Usd), Unit::Cent);
    }

    // ---- melt-quote (Lightning send) helper tests ----

    #[test]
    fn melt_quote_error_to_ffi_maps_invalid_invoice() {
        let e = melt_quote_error_to_ffi(MeltQuoteError::InvalidInvoice("parse boom".into()));
        assert!(
            matches!(e, FfiError::Internal { ref message } if message.contains("invalid bolt11"))
        );
    }

    #[test]
    fn melt_quote_error_to_ffi_maps_amountless_invoice() {
        let e = melt_quote_error_to_ffi(MeltQuoteError::AmountlessInvoice);
        assert!(matches!(e, FfiError::Internal { ref message } if message.contains("amountless")));
    }

    #[test]
    fn melt_quote_error_to_ffi_maps_insufficient_balance() {
        let e = melt_quote_error_to_ffi(MeltQuoteError::InsufficientBalance {
            needed: "100".into(),
            have: "50".into(),
        });
        assert!(matches!(
            e,
            FfiError::Internal { ref message }
                if message.contains("insufficient balance") && message.contains("100")
        ));
    }

    #[test]
    fn melt_quote_error_to_ffi_maps_quote_expired() {
        let e = melt_quote_error_to_ffi(MeltQuoteError::QuoteExpired);
        assert!(matches!(e, FfiError::Internal { ref message } if message.contains("expired")));
    }

    #[test]
    fn melt_quote_error_to_ffi_maps_quote_not_pending() {
        let e = melt_quote_error_to_ffi(MeltQuoteError::QuoteNotPending);
        assert!(
            matches!(e, FfiError::Internal { ref message } if message.contains("not yet pending"))
        );
    }

    fn stub_melt_quote(state: CashuMeltQuoteState) -> CashuMeltQuote {
        use agicash_money::Money;
        use chrono::Utc;
        use rust_decimal::Decimal;
        let money = |n: u64| Money::new(Decimal::from(n), Currency::Btc, Unit::Sat);
        CashuMeltQuote {
            id: Uuid::new_v4(),
            quote_id: "mint-qid".into(),
            user_id: UserId::new(),
            account_id: agicash_domain::AccountId::new(),
            payment_request: "lnbc640n1...".into(),
            payment_hash: "deadbeef".into(),
            amount_requested: money(64),
            amount_requested_in_msat: 64_000,
            amount_received: money(64),
            lightning_fee_reserve: money(2),
            cashu_fee: money(1),
            proofs: vec![],
            amount_reserved: money(67),
            keyset_id: "ks1".into(),
            keyset_counter: 0,
            number_of_change_outputs: 1,
            transaction_id: Uuid::new_v4(),
            created_at: Utc::now(),
            expires_at: Utc::now(),
            version: 0,
            state,
        }
    }

    #[test]
    fn melt_quote_snapshot_from_unpaid_has_no_receipt_fields() {
        let q = stub_melt_quote(CashuMeltQuoteState::Unpaid);
        let snap = melt_quote_snapshot_from(&q);
        assert_eq!(snap.state, MeltQuoteFfiState::Unpaid);
        assert!(snap.failure_reason.is_none());
        assert!(snap.payment_preimage.is_none());
        assert!(snap.amount_spent.is_none());
    }

    #[test]
    fn melt_quote_snapshot_from_pending_maps_state() {
        let q = stub_melt_quote(CashuMeltQuoteState::Pending);
        let snap = melt_quote_snapshot_from(&q);
        assert_eq!(snap.state, MeltQuoteFfiState::Pending);
        assert!(snap.payment_preimage.is_none());
    }

    #[test]
    fn melt_quote_snapshot_from_paid_carries_preimage_and_fees() {
        use agicash_money::Money;
        use rust_decimal::Decimal;
        let money = |n: u64| Money::new(Decimal::from(n), Currency::Btc, Unit::Sat);
        let q = stub_melt_quote(CashuMeltQuoteState::Paid {
            payment_preimage: "abc123".into(),
            lightning_fee: money(1),
            amount_spent: money(65),
            total_fee: money(2),
        });
        let snap = melt_quote_snapshot_from(&q);
        assert_eq!(snap.state, MeltQuoteFfiState::Paid);
        assert_eq!(snap.payment_preimage.as_deref(), Some("abc123"));
        assert_eq!(snap.lightning_fee.as_deref(), Some("1"));
        assert_eq!(snap.amount_spent.as_deref(), Some("65"));
        assert_eq!(snap.total_fee.as_deref(), Some("2"));
    }

    #[test]
    fn melt_quote_snapshot_from_failed_carries_reason() {
        let q = stub_melt_quote(CashuMeltQuoteState::Failed {
            failure_reason: "mint rejected".into(),
        });
        let snap = melt_quote_snapshot_from(&q);
        assert_eq!(snap.state, MeltQuoteFfiState::Failed);
        assert_eq!(snap.failure_reason.as_deref(), Some("mint rejected"));
    }

    #[test]
    fn melt_quote_handle_from_stringifies_money_and_total_fee() {
        let q = stub_melt_quote(CashuMeltQuoteState::Unpaid);
        let account = stub_account(Currency::Btc);
        let handle = melt_quote_handle_from(&q, &account);
        assert_eq!(handle.quote_id, q.id.to_string());
        assert_eq!(handle.melt_quote_id, "mint-qid");
        assert_eq!(handle.amount, "64");
        assert_eq!(handle.lightning_fee_reserve, "2");
        assert_eq!(handle.cashu_fee, "1");
        // total_fee = lightning_fee_reserve + cashu_fee = 2 + 1
        assert_eq!(handle.total_fee, "3");
        assert_eq!(handle.unit, "sat");
        assert_eq!(handle.currency, "BTC");
    }

    #[test]
    fn melt_quote_snapshot_from_outcome_paid_delegates() {
        use agicash_money::Money;
        use rust_decimal::Decimal;
        let money = |n: u64| Money::new(Decimal::from(n), Currency::Btc, Unit::Sat);
        let q = stub_melt_quote(CashuMeltQuoteState::Paid {
            payment_preimage: "pre".into(),
            lightning_fee: money(1),
            amount_spent: money(65),
            total_fee: money(2),
        });
        let outcome = MeltOutcome::Paid {
            quote: q,
            account: stub_account(Currency::Btc),
            change_proofs_count: 1,
        };
        let snap = melt_quote_snapshot_from_outcome(&outcome);
        assert_eq!(snap.state, MeltQuoteFfiState::Paid);
        assert_eq!(snap.payment_preimage.as_deref(), Some("pre"));
    }

    #[test]
    fn melt_quote_snapshot_from_outcome_pending_maps_state() {
        let q = stub_melt_quote(CashuMeltQuoteState::Pending);
        let outcome = MeltOutcome::Pending(q);
        let snap = melt_quote_snapshot_from_outcome(&outcome);
        assert_eq!(snap.state, MeltQuoteFfiState::Pending);
    }

    // ---- exchange-rate (read-only price feed) helper tests ----

    #[test]
    fn exchange_rate_error_to_ffi_maps_network() {
        let e = exchange_rate_error_to_ffi(ExchangeRateError::Network("timeout".into()));
        assert!(matches!(
            e,
            FfiError::Internal { ref message }
                if message.contains("network-error") && message.contains("timeout")
        ));
    }

    #[test]
    fn exchange_rate_error_to_ffi_maps_invalid_response() {
        let e = exchange_rate_error_to_ffi(ExchangeRateError::InvalidResponse("not json".into()));
        assert!(matches!(
            e,
            FfiError::Internal { ref message }
                if message.contains("invalid-response") && message.contains("not json")
        ));
    }

    #[test]
    fn exchange_rate_error_to_ffi_maps_unsupported_pair() {
        let e = exchange_rate_error_to_ffi(ExchangeRateError::UnsupportedPair {
            from: Currency::Btc,
            to: Currency::Usdb,
        });
        assert!(matches!(
            e,
            FfiError::Internal { ref message }
                if message.contains("unsupported-pair")
                    && message.contains("BTC")
                    && message.contains("USDB")
        ));
    }

    #[test]
    fn exchange_rate_snapshot_from_stringifies_rate_and_canonicalises_pair() {
        use rust_decimal::Decimal;
        let rate = Decimal::new(5_012_345, 2); // 50123.45
        let snap = exchange_rate_snapshot_from(&rate, Currency::Btc, Currency::Usd);
        assert_eq!(snap.rate, "50123.45");
        assert_eq!(snap.from, "BTC");
        assert_eq!(snap.to, "USD");
    }

    #[test]
    fn exchange_rate_snapshot_from_preserves_inverse_precision() {
        use rust_decimal::Decimal;
        // 8-dp inverse rate (USD->BTC), as the mempool provider rounds it.
        let rate = Decimal::new(1995, 8); // 0.00001995
        let snap = exchange_rate_snapshot_from(&rate, Currency::Usd, Currency::Btc);
        assert_eq!(snap.rate, "0.00001995");
        assert_eq!(snap.from, "USD");
        assert_eq!(snap.to, "BTC");
    }

    #[tokio::test]
    async fn get_exchange_rate_rejects_unknown_currency() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .get_exchange_rate("BTC".into(), "XYZ".into())
            .await
            .expect_err("unknown target currency");
        assert!(matches!(
            err,
            FfiError::Internal { ref message }
                if message.contains("unsupported currency") && message.contains("XYZ")
        ));
    }

    #[tokio::test]
    async fn get_exchange_rate_rejects_unsupported_pair_before_network() {
        // BTC->USDB is a known currency pair the provider can't price.
        // The provider short-circuits with `UnsupportedPair` before any
        // HTTP call, so this is deterministic offline.
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .get_exchange_rate("BTC".into(), "USDB".into())
            .await
            .expect_err("unsupported pair");
        assert!(matches!(
            err,
            FfiError::Internal { ref message } if message.contains("unsupported-pair")
        ));
    }

    #[tokio::test]
    async fn prepare_melt_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .prepare_melt_quote("lnbc1...".into(), None, None)
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn execute_melt_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .execute_melt_quote(Uuid::new_v4().to_string())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    #[tokio::test]
    async fn poll_melt_quote_without_session_returns_unauthenticated() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        let err = wallet
            .poll_melt_quote(Uuid::new_v4().to_string())
            .await
            .expect_err("no session");
        assert!(
            matches!(err, FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    /// `start_wallet_events` must reject (not panic) when no session is
    /// loaded — the realtime channel is `realtime:wallet:<userId>` and
    /// there is no user id to join without a session.
    #[tokio::test]
    async fn start_wallet_events_requires_session() {
        struct L;
        impl crate::WalletEventListener for L {
            fn on_connected(&self) {}
            fn on_event(&self, _: String, _: String) {}
            fn on_status(&self, _: crate::RealtimeStatusFfi) {}
            fn on_error(&self, _: String) {}
        }
        let cfg = fake_config();
        let w = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        // No session set → must error, not panic.
        let r = w.start_wallet_events(Box::new(L)).await;
        assert!(r.is_err());
        assert!(
            matches!(r, Err(FfiError::Auth { code, .. }) if code == crate::error::auth_code::UNAUTHENTICATED)
        );
    }

    /// `stop_wallet_events` with nothing running is a clean no-op (the
    /// platform calls it unconditionally on teardown).
    #[tokio::test]
    async fn stop_wallet_events_without_start_is_noop() {
        let cfg = fake_config();
        let w = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        w.stop_wallet_events().await.expect("stop is a no-op");
    }

    /// Hermetic bridge smoke test: a fake `WalletEventListener` receives
    /// the right callback for each synthetic `WalletRealtimeEvent`
    /// pushed through the FFI bridge (`dispatch_realtime_event`) — no
    /// socket, tokio task, or live Supabase stack. This proves the
    /// Stage-3 surface (`WalletRealtimeEvent` → Swift/Kotlin callbacks)
    /// without depending on Stages 1/2 runtime behavior.
    #[test]
    fn bridge_forwards_each_event_variant_to_listener() {
        use agicash_realtime::{RealtimeStatus, WalletEvent, WalletRealtimeEvent};
        use std::sync::Mutex;

        #[derive(Default)]
        struct Recorder {
            connected: Mutex<u32>,
            events: Mutex<Vec<(String, String)>>,
            statuses: Mutex<Vec<crate::RealtimeStatusFfi>>,
            errors: Mutex<Vec<String>>,
        }
        impl crate::WalletEventListener for Recorder {
            fn on_connected(&self) {
                *self.connected.lock().unwrap() += 1;
            }
            fn on_event(&self, event: String, payload_json: String) {
                self.events.lock().unwrap().push((event, payload_json));
            }
            fn on_status(&self, status: crate::RealtimeStatusFfi) {
                self.statuses.lock().unwrap().push(status);
            }
            fn on_error(&self, message: String) {
                self.errors.lock().unwrap().push(message);
            }
        }

        let rec = Recorder::default();

        crate::realtime::dispatch_realtime_event(&rec, WalletRealtimeEvent::Connected);
        crate::realtime::dispatch_realtime_event(
            &rec,
            WalletRealtimeEvent::Event(WalletEvent {
                event: "TRANSACTION_CREATED".into(),
                payload_json: r#"{"id":"abc"}"#.into(),
            }),
        );
        crate::realtime::dispatch_realtime_event(
            &rec,
            WalletRealtimeEvent::StatusChanged(RealtimeStatus::Subscribed),
        );
        crate::realtime::dispatch_realtime_event(&rec, WalletRealtimeEvent::Error("boom".into()));

        assert_eq!(*rec.connected.lock().unwrap(), 1);
        assert_eq!(
            *rec.events.lock().unwrap(),
            vec![(
                "TRANSACTION_CREATED".to_string(),
                r#"{"id":"abc"}"#.to_string()
            )]
        );
        assert_eq!(
            *rec.statuses.lock().unwrap(),
            vec![crate::RealtimeStatusFfi::Subscribed]
        );
        assert_eq!(*rec.errors.lock().unwrap(), vec!["boom".to_string()]);
    }

    #[tokio::test]
    async fn constructor_wires_facade_client() {
        let cfg = fake_config();
        let wallet = AgicashWallet::new(
            cfg.opensecret_url,
            cfg.client_id,
            cfg.supabase_url,
            cfg.anon_key,
        )
        .expect("construct");
        // The facade-backed auth_status path must work with no session
        // and no network (proves `facade` is wired + delegated).
        let status = wallet.auth_status().await.unwrap();
        assert!(!status.logged_in);
        assert!(status.user_id.is_none());
    }
}
