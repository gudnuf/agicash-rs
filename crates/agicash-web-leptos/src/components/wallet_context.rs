//! `WalletData` — Leptos context for the user's wallet view-model.
//!
//! The home page (and every future protected page) reads its data from
//! here rather than calling FFI / SDK code inline. The struct itself is
//! `Clone + Debug` so it slots into `provide_context` / `expect_context`
//! the same way `AccessToken` does.
//!
//! ## Why a context instead of a per-page resource?
//!
//! Several pages need the same data (home shows the balance hero,
//! accounts page shows the same list, settings shows the user). A
//! single shared `RwSignal` means one fetch on load, immediate reactive
//! updates after a Receive completes, and no cross-page refetch on
//! navigation.
//!
//! ## Where does the data come from now?
//!
//! [`WalletData::refresh`] uses the typed `agicash-storage-supabase`
//! crate — the same `SupabaseStorage` the iOS / Android / CLI binaries
//! call. The previous direct `gloo-net` REST path is gone (spec:
//! `2026-05-17-storage-supabase-wasm-port-design.md` — port shipped
//! 2026-05-17 / -18).
//!
//! The fetch path:
//!
//! 1. Read `user_id` from `BrowserSessionStorage` (already persisted by
//!    [`LoginView`] on successful auth).
//! 2. Build an `OpenSecretTokenProvider` from the [`AppConfig`] context
//!    and pass it to `SupabaseStorage::new`. JWTs are minted on
//!    each call via `OpenSecretClient::generate_third_party_token`
//!    (cached server-side).
//! 3. `storage.list_accounts(user_id).await` — typed postgrest call,
//!    same surface every other platform uses.
//! 4. Per Cashu account, `send_swap_storage.list_unspent_proofs(account.id)`
//!    and sum each proof's `.amount` (mirrors
//!    `agicash_ffi::wallet::compute_cashu_balance`). Spark accounts
//!    render `balance = 0` until their proof storage lands.
//!
//! ## Empty-state correctness
//!
//! A signed-in guest with no accounts is still the steady-state
//! behaviour for a fresh account. The hero renders `$ 0` / `≈ 0 sats`
//! from these empty inputs, matching iOS `HomeView` exactly.

use leptos::prelude::*;
use uuid::Uuid;

#[cfg(target_arch = "wasm32")]
use crate::config::AppConfig;

/// Loading state envelope. Replaces a tri-state Option pattern so the
/// view layer can distinguish "haven't asked yet" from "asked, still
/// waiting" from "asked, failed" from "asked, here's data".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LoadState<T> {
    /// `WalletData::refresh` hasn't been called yet (first paint).
    #[default]
    Idle,
    /// In-flight: an async refresh is running.
    Loading,
    /// Ready with data — could be an empty `Vec`, which is the canonical
    /// "signed-in guest, no accounts" state today.
    Ready(T),
    /// Refresh failed; carries a user-facing message.
    Error(String),
}

impl<T> LoadState<T> {
    /// True iff a refresh is in flight.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    /// Borrow the inner value if `Ready`. Returns `None` otherwise.
    pub const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// Per-account summary used by the home-page balance hero and the
/// accounts page list. Mirrors the FFI's `AccountFfi` field-for-field
/// (sans the iOS-only `id`, `name`, `mint_url`, `account_type` extras),
/// keeping the surface small so the home page never needs to grow
/// fields it doesn't render. The accounts page can lift this to a
/// richer struct later without touching the home flow.
///
/// `balance` is the raw smallest-unit total (sat for BTC, cent for
/// USD/USDB) as `u64` — matches the iOS hero's parse-the-decimal-string
/// step but skips the string round-trip since we own the data shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSummary {
    /// `"BTC"` | `"USD"` | `"USDB"`. Same labels as `AccountFfi.currency`.
    pub currency: String,
    /// Smallest-unit balance. For Cashu accounts this is the sum of the
    /// account's UNSPENT proofs (decrypted via the storage layer); for
    /// Spark accounts it's 0 until Spark's proof storage lands.
    pub balance: u64,
}

/// Cross-page wallet view-model. Provide once at the App root; consumers
/// pull it out via `expect_context::<WalletData>()`.
#[derive(Clone, Debug)]
pub struct WalletData {
    /// The signed-in user's id (from the persisted session). `None`
    /// until [`WalletData::refresh`] runs.
    pub user_id: RwSignal<Option<Uuid>>,
    /// Account list keyed by load state. `Ready(vec![])` is the
    /// canonical empty-wallet state.
    pub accounts: RwSignal<LoadState<Vec<AccountSummary>>>,
}

impl WalletData {
    /// Fresh `WalletData` in `Idle` state. The App root constructs one
    /// of these next to `AccessToken`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            user_id: RwSignal::new(None),
            accounts: RwSignal::new(LoadState::Idle),
        }
    }

    /// Kick off a refresh. Runs in the browser only — the native rlib
    /// build (used by `cargo test` on the pure pieces) treats this as a
    /// no-op so unit tests on view helpers don't need a browser.
    ///
    /// On wasm: loads the session, constructs a `SupabaseStorage`, calls
    /// `list_accounts` + (per Cashu account) `list_unspent_proofs`, and
    /// populates the signals with real balances.
    pub fn refresh(self) {
        // Loading state visible immediately so the view can show a
        // spinner even before the async work yields.
        self.accounts.set(LoadState::Loading);

        // Capture context BEFORE spawning — `spawn_local` futures run
        // outside the reactive owner that provided the context, so
        // `use_context` inside the async block always returns None.
        // Reading it sync here threads the value through to the future.
        #[cfg(target_arch = "wasm32")]
        let config = use_context::<AppConfig>();

        // Wasm uses wasm_bindgen_futures directly because `leptos::task::
        // spawn_local` requires the leptos Executor to be installed, and
        // refresh() can fire from an Effect before hydration completes
        // that handshake. Native test build uses leptos's spawn since
        // tests run under tokio + the reactive harness.
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let Some(config) = config else {
                self.accounts
                    .set(LoadState::Error("AppConfig context missing".to_string()));
                return;
            };

            match load_session_user_id().await {
                Ok(Some(uid)) => {
                    self.user_id.set(Some(uid));
                    match fetch_account_summaries(&config, uid).await {
                        Ok(accounts) => {
                            self.accounts.set(LoadState::Ready(accounts));
                        }
                        Err(msg) => {
                            self.accounts.set(LoadState::Error(msg));
                        }
                    }
                }
                Ok(None) => {
                    // No session — ProtectedLayout should have
                    // redirected, but we don't want to spin.
                    self.accounts.set(LoadState::Ready(Vec::new()));
                }
                Err(msg) => {
                    self.accounts.set(LoadState::Error(msg));
                }
            }
        });

        #[cfg(not(target_arch = "wasm32"))]
        leptos::task::spawn_local(async move {
            // Native: don't touch anything async — the test runner
            // doesn't have a browser. Just settle into Ready(empty)
            // so any view tests rendering this state get the same
            // shape they'd see in the browser steady-state.
            self.accounts.set(LoadState::Ready(Vec::new()));
        });
    }
}

impl Default for WalletData {
    fn default() -> Self {
        Self::new()
    }
}

/// Browser-only: read the `user_id` from `BrowserSessionStorage`.
/// Returns `Ok(None)` if no session is persisted (e.g. user landed on
/// `/` from a fresh tab before logging in — shouldn't happen because
/// `ProtectedLayout` redirects, but we don't want to panic if it does).
#[cfg(target_arch = "wasm32")]
async fn load_session_user_id() -> Result<Option<Uuid>, String> {
    use agicash_auth_opensecret::BrowserSessionStorage;
    use agicash_traits::SessionStorage;

    let storage = BrowserSessionStorage::new();
    match storage.load().await {
        Ok(Some(session)) => Ok(Some(session.user_id)),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("session load failed: {e}")),
    }
}

/// Build the typed `SupabaseStorage` + the send-swap storage helper,
/// fetch the user's accounts, and compute the per-account balance.
///
/// Mirrors `agicash_ffi::wallet::list_accounts` + `compute_cashu_balance`.
#[cfg(target_arch = "wasm32")]
async fn fetch_account_summaries(
    config: &AppConfig,
    user_id: Uuid,
) -> Result<Vec<AccountSummary>, String> {
    use std::sync::Arc;

    use agicash_auth_opensecret::{OpenSecretClient, OpenSecretConfig, OpenSecretTokenProvider};
    use agicash_cashu::CashuSendSwapStorage;
    use agicash_domain::{AccountType, UserId};
    use agicash_storage_supabase::{
        SupabaseCashuSendSwapStorage, SupabaseStorage, SupabaseStorageConfig,
    };
    use agicash_traits::{PassthroughProofEncryption, ProofEncryption, TokenProvider, UserStorage};

    if config.supabase_anon_key.is_empty() {
        return Err(
            "Supabase anon key missing — set <meta name=\"supabase-anon-key\"> in \
             index.html or you'll only see auth-only state."
                .to_string(),
        );
    }

    // OpenSecret-backed token provider: reuses the browser session's
    // refresh token (in `window.localStorage` via `BrowserSessionStorage`),
    // mints a fresh Supabase-compatible JWT per call.
    let client = OpenSecretClient::new(OpenSecretConfig {
        base_url: config.opensecret_base_url.clone(),
        client_id: config.opensecret_client_id,
    })
    .map_err(|e| format!("build opensecret client: {e}"))?;
    let tokens: Arc<dyn TokenProvider> = Arc::new(OpenSecretTokenProvider::new(client));

    let storage = SupabaseStorage::new(
        SupabaseStorageConfig {
            url: config.supabase_url.clone(),
            anon_key: config.supabase_anon_key.clone(),
        },
        tokens,
    )
    .map_err(|e| format!("build supabase storage: {e}"))?;

    let accounts = storage
        .list_accounts(UserId::from(user_id))
        .await
        .map_err(|e| format!("list_accounts failed: {e}"))?;

    // For per-account balance we need the send-swap storage, which
    // wraps the same `SupabaseStorage` plus a `ProofEncryption`. The
    // production stack uses `PassthroughProofEncryption` until the real
    // encryption layer ships (mirrors CLI + FFI composition root).
    let storage_arc = Arc::new(storage);
    let encryption: Arc<dyn ProofEncryption> = Arc::new(PassthroughProofEncryption);
    let send_swap_storage =
        SupabaseCashuSendSwapStorage::new(Arc::clone(&storage_arc), encryption);

    let mut summaries = Vec::with_capacity(accounts.len());
    for account in accounts {
        let balance = match account.account_type {
            AccountType::Cashu => match send_swap_storage.list_unspent_proofs(account.id).await {
                Ok(proofs) => proofs.iter().map(|p| p.proof.amount).sum::<u64>(),
                Err(e) => {
                    // Log and continue — one account's failure shouldn't
                    // black-hole the whole list. The user sees this
                    // account's balance as zero with the rest intact.
                    leptos::logging::log!(
                        "list_unspent_proofs failed for account {}: {e}",
                        account.id
                    );
                    0
                }
            },
            AccountType::Spark => {
                // Spark proof storage hasn't been wasm-ported yet (slice 9).
                // Mirrors the FFI compute_cashu_balance Spark arm.
                0
            }
        };
        summaries.push(AccountSummary {
            currency: account.currency.to_string(),
            balance,
        });
    }

    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_state_default_is_idle() {
        let ls: LoadState<Vec<u8>> = LoadState::default();
        assert!(matches!(ls, LoadState::Idle));
        assert!(!ls.is_loading());
        assert!(ls.ready().is_none());
    }

    #[test]
    fn load_state_loading_predicate() {
        let ls: LoadState<()> = LoadState::Loading;
        assert!(ls.is_loading());
        assert!(ls.ready().is_none());
    }

    #[test]
    fn load_state_ready_exposes_inner() {
        let ls = LoadState::Ready(vec![1u8, 2, 3]);
        assert_eq!(ls.ready(), Some(&vec![1u8, 2, 3]));
        assert!(!ls.is_loading());
    }

    #[test]
    fn load_state_error_carries_message() {
        let ls: LoadState<()> = LoadState::Error("boom".to_string());
        assert!(!ls.is_loading());
        assert!(ls.ready().is_none());
        match ls {
            LoadState::Error(msg) => assert_eq!(msg, "boom"),
            _ => panic!("expected Error variant"),
        }
    }
}
