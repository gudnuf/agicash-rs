//! CLI composition root.
//!
//! Single composition root: [`WalletClient::from_config_async`] — the SAME
//! `from_config_async` the FFI/Leptos shells will consume (spec §6
//! platform-layer carve-out, session-loading-full-shape plan 2026-05-22).
//! The CLI is a real client, so it takes the real OpenSecret/Supabase/CDK
//! path; the eight bespoke `build_*` dep-bundle functions + the per-flow
//! duplicate that re-wired Supabase/OpenSecret/CDK by hand are gone.
//!
//! Two things stay CLI-shell-resident, exactly mirroring how the FFI
//! shell keeps its own `storage` handle alongside the `facade`:
//!
//! 1. **Keyring session persistence.** The CLI is a fresh process per
//!    invocation, so it owns persistence the way the iOS shell owns
//!    Keychain. The same keyring `Arc` is handed to the facade via
//!    `SessionStorageChoice::Custom(keyring)` (so `from_config_async`
//!    auto-loads any persisted session on the way out) AND retained on
//!    [`CliDeps`] so the `auth` subcommands can read/clear/persist
//!    directly.
//! 2. **A thin `UserStorage` handle.** `account list` emits the raw
//!    `Account` rows and `account default` calls `update_user_defaults`
//!    — neither is on the facade surface (the facade's
//!    `set_default_account` is `Unsupported` in slice 12). Built from
//!    the SAME `Arc<dyn TokenProvider>` the facade installed on its
//!    `SupabaseStorage` (via [`WalletClient::token_provider`]) — there
//!    is exactly one auth surface in the process, no parallel
//!    `OpenSecretClient`.

#[cfg(feature = "keyring-storage")]
use agicash_auth_opensecret::KeyringSessionStorage;
use agicash_auth_opensecret::{InMemorySessionStorage, OpenSecretConfig, DEFAULT_SERVICE};
use agicash_storage_supabase::{SupabaseStorage, SupabaseStorageConfig};
use agicash_traits::{AuthError, PersistedSession, SessionStorage, StorageError};
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
/// The facade is constructed through [`WalletClient::from_config_async`]
/// with `SessionStorageChoice::Custom(keyring)` so a persisted session
/// is auto-loaded into the in-memory slot on the way out — no separate
/// `rehydrate_session` two-step. The same keyring `Arc` is retained on
/// [`CliDeps`] for the `auth` subcommands.
///
/// **Failure policy (clear-on-stale):** if the auto-load step inside
/// `from_config_async` fails (stale refresh token, transient auth
/// backend error), this clears the keyring entry and re-constructs the
/// wallet against an empty session. Matches the pre-migration
/// `auth::rehydrate_session` "dead refresh token doesn't get retried"
/// semantic exactly. Genuine env/wiring errors (bad config, no
/// reachable `OpenSecret`) still propagate.
pub async fn build_deps() -> Result<CliDeps, CompositionError> {
    let os_cfg = OpenSecretConfig::from_env()?;
    let sb_cfg = SupabaseStorageConfig::from_env()?;
    let keyring = build_session_storage().await;

    let make_cfg = || WalletConfig {
        opensecret_url: os_cfg.base_url.clone(),
        opensecret_client_id: os_cfg.client_id,
        supabase_url: sb_cfg.url.clone(),
        supabase_anon_key: sb_cfg.anon_key.clone(),
        session_storage: SessionStorageChoice::Custom(Arc::clone(&keyring)),
    };

    let (wallet, _facade_auth) = match WalletClient::from_config_async(make_cfg()).await {
        Ok(out) => out,
        Err(e) if is_stale_session_error(&e) => {
            // Stale persisted session (refresh token rejected, auth
            // backend blip). Clear the keyring so a dead token isn't
            // retried, then re-compose with an empty session — same
            // behavior as the pre-migration `rehydrate_session` swallow
            // + clear path.
            if let Err(clear_err) = keyring.clear().await {
                eprintln!("warning: could not clear stale session: {clear_err}");
            }
            WalletClient::from_config_async(make_cfg()).await?
        }
        Err(e) => return Err(e.into()),
    };

    // Thin shell-resident UserStorage handle for the two `account`
    // operations the facade doesn't surface (raw account list +
    // update_user_defaults). Built from the SAME `Arc<dyn TokenProvider>`
    // the facade installed on its own `SupabaseStorage` — one auth surface
    // in the process, no parallel `OpenSecretClient`.
    let token_provider = wallet
        .token_provider()
        .expect("WalletClient::from_config_async always populates token_provider");
    let user_storage = Arc::new(SupabaseStorage::new(sb_cfg, token_provider)?);

    Ok(CliDeps {
        wallet,
        keyring,
        user_storage,
    })
}

/// Classify a `from_config_async` error as a stale-session failure (the
/// persisted refresh token was rejected during auto-load) vs. a genuine
/// env/wiring failure (bad config, unreachable `OpenSecret`).
///
/// Only the former should trigger keyring-clear + retry. The auth-load
/// path in `from_config_async` routes `set_session` failures through
/// `From<AuthError> for WalletError`, producing `Unauthenticated`,
/// `Auth { code, .. }`, or `Network(_)` — exactly the three this matches.
/// Other variants (e.g. `Storage`, `Internal`) are wiring problems we
/// don't try to recover from by clearing the keyring.
fn is_stale_session_error(e: &WalletError) -> bool {
    matches!(
        e,
        WalletError::Unauthenticated | WalletError::Auth { .. } | WalletError::Network(_)
    )
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

/// Persist a freshly-issued session into the shell keyring (the CLI's
/// equivalent of the iOS shell writing the refresh token to Keychain
/// after an `auth_*` call). The facade's session slot was already
/// populated by `auth_guest`/`auth_login`/`auth_signup` itself; this
/// writes through to durable storage so the NEXT process inherits the
/// session via `from_config_async`'s auto-load.
pub async fn persist_session(deps: &CliDeps, session: &Session) -> Result<(), AuthError> {
    deps.keyring
        .store(&PersistedSession {
            user_id: session.user_id.as_uuid(),
            refresh_token: session.refresh_token.clone(),
        })
        .await?;
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
