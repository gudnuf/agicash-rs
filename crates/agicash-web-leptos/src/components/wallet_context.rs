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

// `AppConfig` is part of the public `refresh_with_config` signature, so
// it must be in scope on every target (it is `cfg`-free and the native
// build constructs a dev-defaults instance).
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
    /// Idempotency latch for [`WalletData::start_visibility_refresh`].
    /// The listeners + poll are wired into the page lifetime exactly
    /// once; a Home remount (client-side nav back to `/`) must not stack
    /// duplicate `visibilitychange` / `focus` handlers or a second poll
    /// loop. `false` until the first wiring call flips it.
    reactivity_wired: RwSignal<bool>,
}

/// Foreground balance-poll interval. Mirrors the iOS / Android Tier-1
/// plan (a ~3-5s poll while the screen is visible) and stands in for the
/// web canonical model's Supabase Realtime channel until the Tier-2
/// realtime FFI seam lands. Only fires while the document is visible —
/// a backgrounded tab does no work and the `visibilitychange` handler
/// catches it up the instant it returns to the foreground.
#[cfg(target_arch = "wasm32")]
const FOREGROUND_POLL_MS: u32 = 4_000;

impl WalletData {
    /// Fresh `WalletData` in `Idle` state. The App root constructs one
    /// of these next to `AccessToken`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            user_id: RwSignal::new(None),
            accounts: RwSignal::new(LoadState::Idle),
            reactivity_wired: RwSignal::new(false),
        }
    }

    /// Kick off a refresh. Runs in the browser only — the native rlib
    /// build (used by `cargo test` on the pure pieces) treats this as a
    /// no-op so unit tests on view helpers don't need a browser.
    ///
    /// On wasm: loads the session, constructs a `SupabaseStorage`, calls
    /// `list_accounts` + (per Cashu account) `list_unspent_proofs`, and
    /// populates the signals with real balances.
    ///
    /// **Owner requirement.** This entry point reads
    /// `use_context::<AppConfig>()` synchronously, so it MUST be called
    /// from within a valid Leptos reactive owner (a component body or an
    /// `Effect`). A detached caller — a `.forget()`-leaked DOM event
    /// closure or a bare `spawn_local` future — has no owner, so
    /// `use_context` returns `None` and the load fails with the
    /// `AppConfig context missing` error. Detached callers (the `visibilitychange` /
    /// `focus` listeners and the foreground poll wired by
    /// [`WalletData::start_visibility_refresh`]) MUST instead capture the
    /// `AppConfig` once inside an owner and call
    /// [`WalletData::refresh_with_config`] with the concrete value.
    pub fn refresh(self) {
        // Read the context here, while we are still guaranteed to be
        // inside the owner that provided it (the Home mount Effect / the
        // retry handler runs under the component owner). The concrete
        // value is then threaded through to the detached future.
        #[cfg(target_arch = "wasm32")]
        let config = use_context::<AppConfig>();
        #[cfg(target_arch = "wasm32")]
        self.refresh_with_config(config);

        #[cfg(not(target_arch = "wasm32"))]
        self.refresh_with_config(None);
    }

    /// Refresh using an already-captured `AppConfig` instead of reading
    /// it from context. This is the entry point detached callers must
    /// use: the `visibilitychange` / `focus` listeners and the
    /// foreground poll installed by [`WalletData::start_visibility_refresh`]
    /// run **outside** any Leptos reactive owner, so they cannot call
    /// `use_context` themselves. [`WalletData::refresh`] captures the
    /// context from inside the owner and delegates here; the detached
    /// callers carry a clone of the value captured at wiring time.
    ///
    /// `config` is `Option` only so the native test build (which has no
    /// browser and no real config) can pass `None` and settle into the
    /// same `Ready(empty)` shape view tests expect; on wasm a `None`
    /// surfaces the `AppConfig context missing` error exactly as before.
    //
    // `config` is moved into the spawned future on wasm (the build that
    // ships); the native test build cfg's that block out, so to clippy
    // it then looks pass-by-value-but-unused. Allow it there only.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    pub fn refresh_with_config(self, config: Option<AppConfig>) {
        // Loading state visible immediately so the view can show a
        // spinner even before the async work yields.
        self.accounts.set(LoadState::Loading);

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

    /// Wire the Tier-1 reactive-refresh layer: refresh the balance when
    /// the tab regains focus / becomes visible, plus a slow foreground
    /// poll. This is the Leptos analogue of the web canonical model's
    /// `refetchOnWindowFocus:'always'` + Supabase Realtime channel
    /// (see `~/athanor/projects/agicash-rust/research/2026-05-18-balance-tracking-parity.md`,
    /// the Leptos Tier-1 section). It uses **only** the existing
    /// `fetch_account_summaries()` read path — no FFI / protocol change.
    ///
    /// Mechanism (wasm only — a no-op on the native test build):
    ///
    /// - a `visibilitychange` listener on `document`: when the document
    ///   transitions back to visible (tab refocused, OS unlock, app
    ///   foregrounded) it calls [`WalletData::refresh_with_config`],
    ///   catching up any out-of-band receive that happened while
    ///   backgrounded;
    /// - a `focus` listener on `window`: covers window-manager focus
    ///   changes that don't toggle `document.hidden` (e.g. alt-tab
    ///   between two visible windows), mirroring the web app exactly;
    /// - a ~4s foreground poll (gated on `!document.hidden()`) as the
    ///   stand-in for the not-yet-built realtime seam, so a receive
    ///   performed elsewhere shows up within a few seconds without the
    ///   user touching anything.
    ///
    /// **Owner / context capture.** Every callback above runs *outside*
    /// any Leptos reactive owner (a `.forget()`-leaked DOM closure / a
    /// detached `spawn_local` future), so none of them can read
    /// `use_context::<AppConfig>()` — that always returns `None` off the
    /// owner tree and is exactly the `AppConfig context missing`
    /// regression. The caller (the Home mount `Effect`, which *is* inside
    /// an owner) must pass the already-resolved `AppConfig` in; we clone
    /// it into each detached callback and route every refresh through
    /// [`WalletData::refresh_with_config`] so no detached path ever
    /// touches the context.
    ///
    /// Idempotent: the App root provides a single `WalletData`, but the
    /// Home page mount Effect can re-run if the user navigates away and
    /// back client-side. The `reactivity_wired` latch ensures the
    /// listeners + poll are installed exactly once for the page's
    /// lifetime (they intentionally outlive any single Home mount —
    /// balance reactivity is an app-global concern, not a per-view one,
    /// just as the web app's channel lives above the route tree).
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    pub fn start_visibility_refresh(&self, config: Option<AppConfig>) {
        // Flip the latch once. If it was already set, another mount
        // already wired everything — bail without stacking handlers.
        if self.reactivity_wired.get_untracked() {
            return;
        }
        self.reactivity_wired.set(true);

        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::closure::Closure;
            use wasm_bindgen::JsCast;

            let Some(window) = web_sys::window() else {
                return;
            };
            let Some(document) = window.document() else {
                return;
            };

            // `visibilitychange` fires on the document for both
            // hide and show transitions; only refresh on the
            // become-visible edge so a backgrounding tab does no work.
            {
                let wallet = self.clone();
                let doc_for_check = document.clone();
                // Captured at wiring time, inside the owner. The closure
                // is detached (`.forget()`), so it must NOT read context.
                let config = config.clone();
                let on_visibility = Closure::<dyn FnMut()>::new(move || {
                    if !doc_for_check.hidden() {
                        wallet.clone().refresh_with_config(config.clone());
                    }
                });
                if let Err(e) = document.add_event_listener_with_callback(
                    "visibilitychange",
                    on_visibility.as_ref().unchecked_ref(),
                ) {
                    leptos::logging::log!("visibilitychange listener attach failed: {e:?}");
                }
                // The listener lives for the page's lifetime (the SPA
                // never tears the document down). Leaking the closure
                // is the correct ownership here — `on_cleanup` would
                // detach it on the first Home unmount, which is exactly
                // the regression we must avoid.
                on_visibility.forget();
            }

            // `focus` on the window covers focus changes that don't
            // toggle `document.hidden` (alt-tab between two visible
            // windows), matching the web app's
            // `refetchOnWindowFocus:'always'`.
            {
                let wallet = self.clone();
                let config = config.clone();
                let on_focus = Closure::<dyn FnMut()>::new(move || {
                    wallet.clone().refresh_with_config(config.clone());
                });
                let target: &web_sys::EventTarget = window.as_ref();
                if let Err(e) = target
                    .add_event_listener_with_callback("focus", on_focus.as_ref().unchecked_ref())
                {
                    leptos::logging::log!("window focus listener attach failed: {e:?}");
                }
                on_focus.forget();
            }

            // Foreground poll — the Tier-1 stand-in for the realtime
            // channel. Uses the same `gloo-timers` primitive the mocked
            // redeem path already uses; a self-rescheduling timeout
            // keeps it a plain wasm future with no extra dependency.
            {
                let wallet = self.clone();
                let config = config.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    loop {
                        gloo_timers::future::TimeoutFuture::new(FOREGROUND_POLL_MS).await;
                        let still_visible = web_sys::window()
                            .and_then(|w| w.document())
                            .is_some_and(|d| !d.hidden());
                        if still_visible {
                            wallet.clone().refresh_with_config(config.clone());
                        }
                        // When hidden we skip the work but keep looping;
                        // the `visibilitychange` handler does the
                        // catch-up refresh the moment the tab returns.
                    }
                });
            }
        }
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
    let send_swap_storage = SupabaseCashuSendSwapStorage::new(Arc::clone(&storage_arc), encryption);

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
