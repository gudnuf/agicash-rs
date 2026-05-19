//! CLI composition root.
//!
//! Single composition root: [`WalletClient::from_config`] — the SAME
//! `from_config` the FFI shell consumes (spec §6 platform-layer
//! carve-out). The CLI is a real client, so it takes the real
//! OpenSecret/Supabase/CDK path; the eight bespoke `build_*` dep-bundle
//! functions + the contained per-flow duplicate that previously
//! re-wired Supabase/OpenSecret/CDK by hand are gone.
//!
//! Two things stay CLI-shell-resident, exactly mirroring how the FFI
//! shell keeps its own `storage` handle alongside the `facade`:
//!
//! 1. **Keyring session persistence.** `from_config` only installs an
//!    `InMemory` (or Android) session slot. The CLI is a fresh process
//!    per invocation, so it owns persistence the way the iOS shell owns
//!    Keychain: the keyring backend is selected here, the session is
//!    rehydrated into the facade on startup
//!    ([`rehydrate_session`]) via `WalletClient::set_session`, and the
//!    keyring is written/cleared by the `auth` subcommands.
//! 2. **A thin `UserStorage` handle.** `account list` emits the raw
//!    `Account` rows and `account default` calls
//!    `update_user_defaults` — neither is on the facade surface (the
//!    facade's `set_default_account` is `Unsupported` in slice 12).
//!    Built from the SAME endpoint config so there is exactly one
//!    wiring of `OpenSecret` + `Supabase`, no duplicate.

#[cfg(feature = "keyring-storage")]
use agicash_auth_opensecret::KeyringSessionStorage;
use agicash_auth_opensecret::{
    auth_error_from_opensecret, InMemorySessionStorage, OpenSecretClient, OpenSecretConfig,
    OpenSecretTokenProvider, DEFAULT_SERVICE,
};
use agicash_storage_supabase::{SupabaseStorage, SupabaseStorageConfig};
use agicash_traits::{AuthError, PersistedSession, SessionStorage, StorageError, TokenProvider};
use agicash_wallet::{Session, SessionStorageChoice, WalletClient, WalletConfig, WalletError};
use std::sync::Arc;

/// Everything a subcommand needs: the composed facade + the
/// CLI-shell-resident keyring (session persistence) + a thin
/// `UserStorage` handle for the two `account` ops the facade does not
/// expose. All three are wired from one endpoint config — there is no
/// second composition of OpenSecret/Supabase/CDK anywhere.
#[derive(Clone)]
pub struct CliDeps {
    pub wallet: Arc<WalletClient>,
    pub keyring: Arc<dyn SessionStorage>,
    pub user_storage: Arc<SupabaseStorage>,
    /// The shell-resident `OpenSecretClient` that backs `user_storage`'s
    /// token provider. Distinct from the facade's internal client (the
    /// facade does not expose its own — spec §6 keeps the platform layer
    /// shell-resident). Kept in session-sync with the facade by
    /// [`rehydrate_session`] running the SAME handshake + `set_tokens` +
    /// refresh the pre-migration `auth::rehydrate_session` ran, so the
    /// two facade-gap `account` ops stay authenticated.
    os_client: OpenSecretClient,
}

impl std::fmt::Debug for CliDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliDeps").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompositionError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("wallet: {0}")]
    Wallet(String),
}

impl From<WalletError> for CompositionError {
    fn from(e: WalletError) -> Self {
        Self::Wallet(e.to_string())
    }
}

/// Build the CLI deps from process env (`OPENSECRET_BASE_URL`,
/// `OPENSECRET_CLIENT_ID`, `SUPABASE_URL`/`VITE_SUPABASE_URL`,
/// `SUPABASE_ANON_KEY`/`VITE_SUPABASE_ANON_KEY`).
///
/// The facade is constructed through [`WalletClient::from_config`] with
/// `SessionStorageChoice::InMemory` — identical to the FFI shell. The
/// keyring backend (selected via the same fallback chain the CLI always
/// used) lives on the shell side and is consulted by
/// [`rehydrate_session`] + the `auth` subcommands.
pub async fn build_deps() -> Result<CliDeps, CompositionError> {
    let os_cfg = OpenSecretConfig::from_env()?;
    let sb_cfg = SupabaseStorageConfig::from_env()?;

    let (wallet, _facade_auth) = WalletClient::from_config(WalletConfig {
        opensecret_url: os_cfg.base_url.clone(),
        opensecret_client_id: os_cfg.client_id,
        supabase_url: sb_cfg.url.clone(),
        supabase_anon_key: sb_cfg.anon_key.clone(),
        session_storage: SessionStorageChoice::InMemory,
    })?;

    // Thin shell-resident UserStorage handle for the two `account`
    // operations the facade doesn't surface (raw account list +
    // update_user_defaults). Same endpoint config — no duplicate
    // composition of the network stack. The shell `OpenSecretClient` is
    // retained so `rehydrate_session` can drive its token refresh in
    // lockstep with the facade (the facade does not expose its own).
    let os_client = OpenSecretClient::new(os_cfg)?;
    let token_provider: Arc<dyn TokenProvider + Send + Sync> =
        Arc::new(OpenSecretTokenProvider::new(os_client.clone()));
    let user_storage = Arc::new(SupabaseStorage::new(sb_cfg, token_provider)?);

    let keyring = build_session_storage().await;

    Ok(CliDeps {
        wallet,
        keyring,
        user_storage,
        os_client,
    })
}

/// Resolve a [`SessionStorage`] backend via the fallback chain (verbatim
/// the pre-migration `composition::build_session_storage`):
///
/// 1. `AGICASH_SESSION_FILE` — encrypted-file backend (stubbed; warns +
///    falls through to in-memory).
/// 2. OS keyring via [`KeyringSessionStorage`] when the
///    `keyring-storage` feature is on AND reachable at runtime.
/// 3. [`InMemorySessionStorage`] — always available; warns the session
///    won't persist.
async fn build_session_storage() -> Arc<dyn SessionStorage> {
    if let Ok(path) = std::env::var("AGICASH_SESSION_FILE") {
        eprintln!(
            "note: --session-file/AGICASH_SESSION_FILE set ({path}); \
             the encrypted-file backend is not yet implemented in this build. \
             Falling back to in-memory storage; sessions will not persist."
        );
        return Arc::new(InMemorySessionStorage::new());
    }

    #[cfg(feature = "keyring-storage")]
    {
        let service = std::env::var("AGICASH_KEYRING_SERVICE")
            .unwrap_or_else(|_| DEFAULT_SERVICE.to_string());
        let keyring = KeyringSessionStorage::new(service);
        match probe_keyring(&keyring).await {
            Ok(()) => return Arc::new(keyring),
            Err(reason) => {
                eprintln!(
                    "note: secure keyring unavailable ({reason}); \
                     session will not persist across runs"
                );
            }
        }
    }
    #[cfg(not(feature = "keyring-storage"))]
    {
        let _ = DEFAULT_SERVICE;
    }

    Arc::new(InMemorySessionStorage::new())
}

/// Probe the keyring backend by attempting a `load`. `Err` only when the
/// backend is unreachable; "no entry"/`Ok(None)` is success.
#[cfg(feature = "keyring-storage")]
async fn probe_keyring(storage: &KeyringSessionStorage) -> Result<(), String> {
    match storage.load().await {
        Ok(_) => Ok(()),
        Err(AuthError::Backend(msg)) if msg.contains("session backend unavailable") => Err(msg),
        Err(e) => Err(e.to_string()),
    }
}

/// Drive the shell-resident `OpenSecretClient` to a usable access token
/// from a refresh token — VERBATIM the pre-migration
/// `auth::rehydrate_session` body (handshake → `set_tokens`(empty
/// access, refresh) → `refresh_token`). The facade owns its own client (sessioned
/// separately via `WalletClient::set_session` / `auth_*`); this keeps the
/// shell client — which backs the two facade-gap `account` ops — in
/// session-sync. `clear_on_fail` wipes the keyring entry on a dead
/// refresh token (same as the prior behavior) so it isn't retried.
async fn sync_shell_client(
    deps: &CliDeps,
    refresh_token: &str,
    clear_on_fail: bool,
) -> Result<(), AuthError> {
    deps.os_client.ensure_handshake().await?;
    deps.os_client
        .inner()
        .set_tokens(String::new(), Some(refresh_token.to_string()))
        .map_err(auth_error_from_opensecret)?;
    if let Err(e) = deps.os_client.inner().refresh_token().await {
        if clear_on_fail {
            if let Err(clear_err) = deps.keyring.clear().await {
                eprintln!("warning: could not clear stale session: {clear_err}");
            }
        }
        return Err(auth_error_from_opensecret(e));
    }
    Ok(())
}

/// Load any persisted refresh token from the shell keyring into BOTH the
/// facade and the shell `OpenSecretClient` so every subcommand inherits
/// a live session across processes.
///
/// The facade's `WalletClient::set_session` performs its own `OpenSecret`
/// handshake → `set_tokens` → `refresh_token` (byte-identical to the old
/// `deps.client` body); `sync_shell_client` runs the SAME sequence on
/// the shell client that backs `account list`/`default`. On refresh
/// failure the keyring entry is wiped so a dead token isn't retried —
/// same as the pre-migration `auth::rehydrate_session`.
///
/// Returns `true` if a session was hydrated, `false` if the keyring was
/// empty.
pub async fn rehydrate_session(deps: &CliDeps) -> Result<bool, AuthError> {
    let Some(persisted) = deps.keyring.load().await? else {
        return Ok(false);
    };

    if let Err(e) = deps
        .wallet
        .set_session(Session {
            user_id: agicash_domain::UserId::from(persisted.user_id),
            refresh_token: persisted.refresh_token.clone(),
        })
        .await
    {
        if let Err(clear_err) = deps.keyring.clear().await {
            eprintln!("warning: could not clear stale session: {clear_err}");
        }
        return Err(wallet_err_to_auth(e));
    }

    sync_shell_client(deps, &persisted.refresh_token, true).await?;

    Ok(true)
}

/// Persist a freshly-issued session into the shell keyring (the CLI's
/// equivalent of the iOS shell writing the refresh token to Keychain
/// after an `auth_*` call) AND sync the shell `OpenSecretClient` so
/// `account` ops work in the SAME process right after `auth guest/login`
/// (no second invocation needed — matches the pre-migration behavior
/// where one client served everything).
pub async fn persist_session(deps: &CliDeps, session: &Session) -> Result<(), AuthError> {
    deps.keyring
        .store(&PersistedSession {
            user_id: session.user_id.as_uuid(),
            refresh_token: session.refresh_token.clone(),
        })
        .await?;
    // Best-effort: a fresh auth_* already sessioned the facade client; a
    // refresh-failure here shouldn't fail the auth command (the keyring
    // write — the durable cross-process source of truth — succeeded).
    let _ = sync_shell_client(deps, &session.refresh_token, false).await;
    Ok(())
}

/// Map a facade `WalletError` back onto the CLI's `AuthError` channel so
/// the existing `classify_error` exit-code + error-JSON contract is
/// preserved for auth-path failures.
pub fn wallet_err_to_auth(e: WalletError) -> AuthError {
    match e {
        WalletError::Unauthenticated => AuthError::Unauthenticated,
        WalletError::Network(m) => AuthError::Network(m),
        WalletError::Auth { message, .. } => AuthError::Backend(message),
        other => AuthError::Internal(other.to_string()),
    }
}
