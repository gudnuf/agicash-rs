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
//! 2. Build an `OpenSecretTokenProvider` over a **session-seeded**
//!    `OpenSecretClient` (the SDK session manager is in-memory +
//!    per-client, so the persisted refresh token from
//!    `BrowserSessionStorage` MUST be threaded in via `set_tokens` +
//!    `refresh` — see `session_seeded_opensecret_client`), and pass it
//!    to `SupabaseStorage::new`. JWTs are minted on each call via
//!    `OpenSecretClient::generate_third_party_token` (cached
//!    server-side).
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

// `RealtimeStatus` is re-exported in `WalletData::realtime_status` so the
// status-banner component (and any future consumer) can match on it
// without depending on the realtime crate directly. On native (rlib test
// build) we stub it so the public signal still has a concrete type even
// though the wasm-only pump never runs.
#[cfg(target_arch = "wasm32")]
pub use agicash_realtime::RealtimeStatus;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RealtimeStatus {
    #[default]
    Idle,
    Connecting,
    Subscribed,
    Reconnecting,
    Error,
    Closed,
    TerminalError,
}

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

/// One in-flight money-state row, flattened to the `(id, state)` pair
/// the view layer needs to reconcile a stale "waiting…" list.
///
/// The full money/proof payload stays in the storage layer — a consumer
/// that wants detail re-fetches by `id`. This summary only answers
/// *which* rows are still in flight and *what state* they are in, which
/// is all the realtime-reconnect catch-up needs (slice 12e Lane 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingItem {
    /// Row identity — UUID (mint/melt quote, send swap) or token hash
    /// (receive swap), stringified.
    pub id: String,
    /// Uppercase lifecycle state (`UNPAID` / `PAID` / `PENDING` / …).
    pub state: String,
}

/// The signed-in user's full in-flight money state — every pending /
/// unresolved row across the four Cashu money flows, fetched in one
/// shot on a realtime (re)connect (slice 12e Lane 3, Gap-D).
///
/// The realtime channel ships no replay: a pending Lightning receive /
/// unresolved send that resolves during a disconnect window leaves a
/// stale "waiting…" row until the user navigates away. On every
/// `Connected` the pump refetches this so the view can reconcile.
/// An empty `Vec` is the canonical "nothing in flight" state. Mirrors
/// the facade `agicash_wallet::PendingStateSnapshot`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingStateSummary {
    /// UNPAID / PAID mint quotes — Lightning receives still in flight.
    pub mint_quotes: Vec<PendingItem>,
    /// PENDING receive swaps — inbound Cashu tokens still being claimed.
    pub receive_swaps: Vec<PendingItem>,
    /// UNPAID / PENDING melt quotes — Lightning sends still in flight.
    pub melt_quotes: Vec<PendingItem>,
    /// DRAFT / PENDING send swaps — outbound tokens not yet claimed.
    pub send_swaps: Vec<PendingItem>,
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
    /// The user's in-flight money state (pending receives / unresolved
    /// sends), refetched on every realtime (re)connect — the no-replay
    /// catch-up that clears stale "waiting…" rows (slice 12e Lane 3,
    /// Gap-D). `Idle` until the first `Connected`; `Ready(default)` is
    /// the canonical "nothing in flight" state. Nothing renders off this
    /// signal yet — landing it reactively is enough this lane; UI
    /// consumption is a follow-up.
    pub pending_state: RwSignal<LoadState<PendingStateSummary>>,
    /// Live realtime channel status. Drives the
    /// [`crate::components::RealtimeStatusBanner`] (Reconnecting / lost-
    /// connection affordance). Updated by the pump in
    /// [`WalletData::start_realtime`] on every `StatusChanged(_)` event;
    /// stays at `Idle` if realtime never starts (e.g. native test build,
    /// missing supabase anon key).
    pub realtime_status: RwSignal<RealtimeStatus>,
    /// Tracked handle to the running [`agicash_realtime::WalletRealtimeService`]
    /// so the sign-out / session-teardown path can call `.stop()` on it
    /// (closes the socket, drops the supervisor) — fixes the resource-leak
    /// `.forget()` pattern Lane 2b inherited. `None` until the pump
    /// finishes wiring (the async session-load + client-build can fail),
    /// or after `.stop()` clears it. Wasm-only — the native rlib build
    /// never opens a socket, so this is `Option<()>` there to keep the
    /// `Clone + Debug` shape uniform across cfg.
    ///
    /// `LocalStorage` (not the default `SyncStorage`) because the wasm
    /// `WalletRealtimeService` holds non-`Send` transport handles
    /// (`web_sys::WebSocket`); the value is pinned to the JS main thread
    /// where it was constructed, which is exactly what `LocalStorage`
    /// promises. Accessing the signal from another thread would panic,
    /// but wasm32 + leptos hydration is single-threaded by design.
    #[cfg(target_arch = "wasm32")]
    pub realtime_service:
        RwSignal<Option<std::sync::Arc<agicash_realtime::WalletRealtimeService>>, LocalStorage>,
    #[cfg(not(target_arch = "wasm32"))]
    pub realtime_service: RwSignal<Option<()>, LocalStorage>,
    /// Tracked handle to the running [`agicash_driver::ResumptionDriver`]
    /// trigger task (plan 2026-05-21 §7 Lane E). Same `LocalStorage`
    /// shape as [`Self::realtime_service`] for the same reason: the wasm
    /// driver task holds the `Arc<WalletClient>` (whose storage layer
    /// holds non-`Send` `web_sys::Headers` / `Fetch` handles), so the
    /// handle is pinned to the JS main thread where it was constructed.
    ///
    /// The handle is the React `useProcessXTasks` analog: it exposes
    /// `.stop()` (sign-out teardown) and `.notify_foreground()`
    /// (`visibilitychange` / `online` edges — see
    /// [`wire_dom_lifecycle`]). `None` until the realtime pump finishes
    /// wiring (the async session-load + wallet-client build can fail),
    /// or after `.stop()` clears it. Native rlib build is `Option<()>`
    /// to keep the `Clone + Debug` shape uniform across cfg.
    #[cfg(target_arch = "wasm32")]
    pub driver_handle: RwSignal<Option<std::sync::Arc<agicash_driver::DriverHandle>>, LocalStorage>,
    #[cfg(not(target_arch = "wasm32"))]
    pub driver_handle: RwSignal<Option<()>, LocalStorage>,
    /// Idempotency latch for [`WalletData::start_realtime`]. The
    /// realtime subscription + pump are wired into the page lifetime
    /// exactly once; a Home remount (client-side nav back to `/`) must
    /// not stack a second `WalletRealtimeService` / socket. `false`
    /// until the first wiring call flips it.
    reactivity_wired: RwSignal<bool>,
}

impl WalletData {
    /// Fresh `WalletData` in `Idle` state. The App root constructs one
    /// of these next to `AccessToken`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            user_id: RwSignal::new(None),
            accounts: RwSignal::new(LoadState::Idle),
            pending_state: RwSignal::new(LoadState::Idle),
            realtime_status: RwSignal::new(RealtimeStatus::Idle),
            // `new_local`: thread-pinned storage for the non-`Send`
            // wasm transport handles inside `WalletRealtimeService`.
            // See the struct field doc.
            realtime_service: RwSignal::new_local(None),
            // Thread-pinned for the same reason as `realtime_service`:
            // the driver handle indirectly holds the `Arc<WalletClient>`
            // whose wasm storage stack is `!Send` (`web_sys` handles).
            driver_handle: RwSignal::new_local(None),
            reactivity_wired: RwSignal::new(false),
        }
    }

    /// Tear down the realtime subscription + clear local view-model
    /// state. Called from the sign-out flow (see
    /// [`crate::pages::SettingsIndexPage`]) before the auth signal is
    /// cleared. Best-effort and idempotent: `.stop()` is a single
    /// atomic-store + 1-msg broadcast, the supervisor task observes the
    /// flag and exits at its next loop tick (closing the socket cleanly).
    /// Safe to call when realtime never started — the service handle is
    /// `None` and nothing happens.
    ///
    /// After teardown the `reactivity_wired` latch is reset so a later
    /// sign-in can wire a fresh subscription (the App root keeps a
    /// single `WalletData` instance for the page's lifetime).
    pub fn teardown_realtime(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(service) = self.realtime_service.get_untracked() {
            service.stop();
        }
        // Resumption driver (plan 2026-05-21 §7 Lane E): mirror the
        // realtime teardown — `.stop()` is idempotent (atomic-store +
        // 1-pulse wake on the trigger task's internal waker), so calling
        // it when the driver never started is a no-op. The trigger task
        // observes the flag at its next loop tick and exits; the
        // in-flight sweep (if any) runs to completion. Stop the driver
        // BEFORE clearing the realtime service so any final
        // `Connected` / `Event` the supervisor flushes on close still
        // races a stopped driver (the driver is the one observing the
        // broadcast for triggers — the realtime service close emits a
        // `StatusChanged(Closed)`, which is a no-op trigger anyway).
        #[cfg(target_arch = "wasm32")]
        if let Some(handle) = self.driver_handle.get_untracked() {
            handle.stop();
        }
        self.driver_handle.set(None);
        self.realtime_service.set(None);
        self.realtime_status.set(RealtimeStatus::Idle);
        self.reactivity_wired.set(false);
    }

    /// Reset the view-model to a freshly-signed-out shape. Companion to
    /// [`Self::teardown_realtime`] — the sign-out path runs both so the
    /// next sign-in starts from `Idle` accounts + no stale user id.
    pub fn clear_for_signout(&self) {
        self.teardown_realtime();
        self.user_id.set(None);
        self.accounts.set(LoadState::Idle);
        self.pending_state.set(LoadState::Idle);
    }

    /// Best-effort retry after a `TerminalError` (the `JoinRejected` /
    /// auth-deny cap was hit, supervisor has stopped retrying). The
    /// service's terminal latch clears on an `online: false → true`
    /// edge (see `WalletRealtimeService::set_online`), which is the
    /// minimal API that already exists — no new realtime surface
    /// required. The supervisor then resumes its connect→join cycle.
    ///
    /// No-op if realtime never started or has already been torn down.
    /// Wasm-only: the native rlib build has no service to poke.
    pub fn retry_realtime(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(service) = self.realtime_service.get_untracked() {
            // Edge-trigger the terminal latch clear. The service
            // dedupes redundant calls cheaply (atomic swap + 1 broadcast).
            service.set_online(false);
            service.set_online(true);
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
    /// `AppConfig context missing` error. The detached realtime pump
    /// wired by [`WalletData::start_realtime`] MUST instead capture the
    /// `AppConfig` once inside an owner and call
    /// [`WalletData::refresh_with_config`] with the concrete value.
    pub fn refresh(self) {
        // Read the context here, while we are still guaranteed to be
        // inside the owner that provided it (the Home mount Effect / the
        // retry handler runs under the component owner). The concrete
        // value is then threaded through to the detached future.
        #[cfg(target_arch = "wasm32")]
        let config = use_context::<AppConfig>();
        // First load / explicit user-initiated refresh: foreground, so a
        // spinner is allowed while we have no data yet.
        #[cfg(target_arch = "wasm32")]
        self.refresh_with_config(config, false);

        #[cfg(not(target_arch = "wasm32"))]
        self.refresh_with_config(None, false);
    }

    /// Refresh using an already-captured `AppConfig` instead of reading
    /// it from context. This is the entry point detached callers must
    /// use: the realtime pump spawned by [`WalletData::start_realtime`]
    /// runs **outside** any Leptos reactive owner, so it cannot call
    /// `use_context` itself. [`WalletData::refresh`] captures the
    /// context from inside the owner and delegates here; the detached
    /// callers carry a clone of the value captured at wiring time.
    ///
    /// `config` is `Option` only so the native test build (which has no
    /// browser and no real config) can pass `None` and settle into the
    /// same `Ready(empty)` shape view tests expect; on wasm a `None`
    /// surfaces the `AppConfig context missing` error exactly as before.
    ///
    /// `background` selects the load-state discipline, mirroring the iOS
    /// poll fix (`HomeView`'s foreground poll / `scenePhase` refresh call
    /// `refreshAccounts()` without ever blanking the hero):
    ///
    /// - `false` — first load or an explicit user-initiated refresh
    ///   (mount Effect, the Retry button). Allowed to flip to
    ///   `LoadState::Loading` so the view can show the full-screen
    ///   spinner *while there is no data yet*.
    /// - `true` — a silent background catch-up (a realtime
    ///   broadcast / (re)connect catch-up). Stale-while-
    ///   revalidate: the last `Ready` value stays on screen and is only
    ///   swapped when the new fetch resolves; a failure surfaces as a
    ///   quiet `Error` *only if there was nothing to keep showing*, never
    ///   as the full-screen spinner. This is the web canonical model's
    ///   `refetchOnWindowFocus` behaviour (background refetch keeps the
    ///   prior data) — the regression fixed here was the poll throwing
    ///   the balance away on every tick.
    ///
    /// Even with `background == false` the spinner only appears when
    /// there is no `Ready` data to preserve: an explicit Retry after a
    /// successful load keeps the numbers on screen rather than flashing
    /// the spinner.
    //
    // `config` is moved into the spawned future on wasm (the build that
    // ships); the native test build cfg's that block out, so to clippy
    // it then looks pass-by-value-but-unused. Allow it there only.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    pub fn refresh_with_config(self, config: Option<AppConfig>, background: bool) {
        // Stale-while-revalidate. Only blank the hero with the spinner
        // when this is a foreground refresh AND there is no `Ready`
        // value to keep showing. Background refreshes (realtime
        // broadcast / (re)connect catch-up) NEVER flip to `Loading` —
        // they keep the last balance on screen until the async fetch
        // below resolves, then swap in the new data (or surface a quiet
        // inline error). This mirrors the iOS poll discipline
        // (`refreshAccounts()` never sets a loading phase) and the web's
        // `refetchOnWindowFocus`. A realtime disconnect is non-fatal:
        // the last balance stays, and the next `Connected` refetches.
        let has_ready = matches!(self.accounts.get_untracked(), LoadState::Ready(_));
        if !background && !has_ready {
            self.accounts.set(LoadState::Loading);
        }

        // Wasm uses wasm_bindgen_futures directly because `leptos::task::
        // spawn_local` requires the leptos Executor to be installed, and
        // refresh() can fire from an Effect before hydration completes
        // that handshake. Native test build uses leptos's spawn since
        // tests run under tokio + the reactive harness.
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            // A failed *background* refresh must not blow away a balance
            // that is already on screen — a transient poll/focus error
            // should leave the last good numbers visible (stale-while-
            // revalidate), exactly as the foreground spinner is
            // suppressed above. Foreground refreshes, or background
            // refreshes with nothing to preserve, still surface the
            // error so the user isn't left staring at a stale value
            // forever with no feedback.
            let set_error = |this: &Self, msg: String| {
                let keep_stale =
                    background && matches!(this.accounts.get_untracked(), LoadState::Ready(_));
                if !keep_stale {
                    this.accounts.set(LoadState::Error(msg));
                }
            };

            let Some(config) = config else {
                set_error(&self, "AppConfig context missing".to_string());
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
                            set_error(&self, msg);
                        }
                    }
                }
                Ok(None) => {
                    // No session — ProtectedLayout should have
                    // redirected, but we don't want to spin.
                    self.accounts.set(LoadState::Ready(Vec::new()));
                }
                Err(msg) => {
                    set_error(&self, msg);
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

    /// Handle one realtime `Connected` event (the (re)connect catch-up).
    ///
    /// The realtime channel ships **no replay** (spec §5.5): everything
    /// that landed while the socket was down has to be refetched. This
    /// runs both halves of that catch-up:
    ///
    /// - [`Self::refresh_with_config`] in background/SWR mode — the
    ///   accounts + balance (the pre-slice-12e behaviour);
    /// - [`Self::refresh_pending_state`] — the in-flight money state
    ///   (slice 12e Lane 3, Gap-D), so a pending receive / unresolved
    ///   send that resolved during the disconnect window no longer
    ///   leaves a stale "waiting…" row.
    ///
    /// Factored out of the `start_realtime` pump so the catch-up is one
    /// named, self-documenting unit rather than two bare calls buried in
    /// a match arm.
    pub fn on_realtime_connected(&self, config: Option<AppConfig>) {
        self.clone().refresh_with_config(config.clone(), true);
        self.clone().refresh_pending_state(config);
    }

    /// Realtime-(re)connect catch-up: refetch the user's in-flight money
    /// state into [`Self::pending_state`] (slice 12e Lane 3, Gap-D).
    ///
    /// The realtime channel ships no replay, so on every `Connected` the
    /// pump runs this alongside the accounts [`Self::refresh_with_config`]
    /// — a pending Lightning receive / unresolved send that resolved
    /// during the disconnect window leaves a stale "waiting…" row
    /// otherwise. Pure storage reads scoped to the signed-in user; the
    /// facade `WalletClient::refresh_pending_state()` shape, against the
    /// `agicash-cashu` storage traits the Leptos crate already links.
    ///
    /// **Always background / SWR**: a refetch failure is non-fatal — the
    /// last good `pending_state` (or `Idle`) stays, the next reconnect
    /// retries. It never flips the signal to `Loading` (no spinner) and
    /// never surfaces an error screen; the accounts refresh already owns
    /// the user-visible failure path. Nothing renders off the signal yet
    /// — landing it reactively is enough this lane.
    //
    // `config` is moved into the spawned future on wasm (the build that
    // ships); the native test build cfg's that block out, so to clippy
    // it then looks pass-by-value-but-unused. Allow it there only —
    // same shape as `refresh_with_config`.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    pub fn refresh_pending_state(self, config: Option<AppConfig>) {
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let Some(config) = config else {
                // No config → realtime can't auth anyway; the
                // accounts refresh logs the same condition.
                return;
            };
            let uid = match load_session_user_id().await {
                Ok(Some(uid)) => uid,
                Ok(None) => {
                    // Signed out — nothing in flight to catch up.
                    self.pending_state
                        .set(LoadState::Ready(PendingStateSummary::default()));
                    return;
                }
                Err(e) => {
                    leptos::logging::log!(
                        "refresh_pending_state: session load failed \
                         (non-fatal, retries next reconnect): {e}"
                    );
                    return;
                }
            };
            match fetch_pending_state(&config, uid).await {
                Ok(summary) => {
                    self.pending_state.set(LoadState::Ready(summary));
                }
                Err(e) => {
                    // Non-fatal: keep the last snapshot (SWR), the
                    // next reconnect retries. The accounts refresh
                    // owns the user-visible error path.
                    leptos::logging::log!(
                        "refresh_pending_state: fetch failed (non-fatal, \
                         retries next reconnect): {e}"
                    );
                }
            }
        });

        #[cfg(not(target_arch = "wasm32"))]
        leptos::task::spawn_local(async move {
            // Native: no browser / no storage stack — settle into the
            // canonical empty state so view tests see the steady shape.
            self.pending_state
                .set(LoadState::Ready(PendingStateSummary::default()));
        });
    }

    /// Wire the realtime reactivity source: a single Supabase-Realtime
    /// subscription (the all-Rust `agicash-realtime` crate, linked
    /// directly — no FFI) drives the balance refresh. This **replaces**
    /// the deleted `start_visibility_refresh` `visibilitychange` /
    /// `focus` / 4s-poll Tier-1 hack — that apparatus caused the
    /// `AppConfig context`-missing regression and the spinner flicker;
    /// realtime is the reactivity source now. The catch-up semantics
    /// match the web canonical model's React-Query invalidation:
    ///
    /// - on every `WalletRealtimeEvent::Connected` (emitted on every
    ///   (re)join — there is **no replay**, spec §5.5) we refetch wallet
    ///   state, catching up anything that landed while disconnected;
    /// - on every `WalletRealtimeEvent::Event` (a DB `wallet:<uid>`
    ///   broadcast) we refetch.
    ///
    /// Both refetches go through [`WalletData::refresh_with_config`] in
    /// **background** mode (SWR / no-flicker discipline from Lane V):
    /// the last `Ready` balance stays on screen until the new fetch
    /// resolves; a realtime failure / disconnect is **non-fatal** — no
    /// error screen, the prior balance is kept, and the next `Connected`
    /// (after the service's internal reconnect/backoff) refetches.
    ///
    /// **Owner / context capture.** The event-pump runs inside a
    /// detached `spawn_local` future (no Leptos reactive owner), so it
    /// can never read `use_context::<AppConfig>()` — that always returns
    /// `None` off the owner tree and is exactly the `AppConfig context
    /// missing` regression. The caller (the Home mount `Effect`, which
    /// *is* inside an owner) passes the already-resolved `AppConfig`;
    /// every refetch goes through [`WalletData::refresh_with_config`]
    /// with the captured value so no detached path touches the context.
    ///
    /// Idempotent: the App root provides a single `WalletData`, but the
    /// Home page mount Effect can re-run on client-side nav back to `/`.
    /// The `reactivity_wired` latch ensures the service + socket are
    /// constructed exactly once for the page's lifetime (the realtime
    /// channel is an app-global concern that outlives any single Home
    /// mount, just as the web app's channel lives above the route tree).
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    // On wasm32 the realtime handles (`Arc<dyn JwtSource>`,
    // `Arc<WalletRealtimeService>`) are `!Send`/`!Sync` — `web_sys`
    // sockets are JS-main-thread-pinned. `Arc` is the type the
    // `agicash-realtime` public API (`WalletRealtimeService::new`) and
    // the `realtime_service` field both require; `Rc` would mean
    // changing that crate's API surface, out of scope for lint cleanup.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    // Wiring the realtime pump + the resumption driver alongside it
    // is fundamentally sequential (session-load → opensecret client →
    // realtime service → driver wallet client → driver task + pump +
    // DOM listeners). Splitting this into helpers would mean threading
    // half a dozen `Arc`s + the captured `AppConfig` through fn
    // boundaries with no real readability win — every step has its own
    // commentary block. Lint is also tripped on the native rlib build
    // (the whole body is `cfg(target_arch="wasm32")` so the
    // line-counter still sees the source span); allow unconditionally.
    #[allow(clippy::too_many_lines)]
    pub fn start_realtime(&self, config: Option<AppConfig>) {
        // Flip the latch once. If it was already set, another mount
        // already wired the subscription — bail without stacking a
        // second `WalletRealtimeService` / socket.
        if self.reactivity_wired.get_untracked() {
            return;
        }
        self.reactivity_wired.set(true);

        #[cfg(target_arch = "wasm32")]
        {
            use std::sync::Arc;

            use agicash_realtime::{
                TokenProviderJwtSource, WalletRealtimeEvent, WalletRealtimeService,
            };
            use agicash_traits::TokenProvider;

            let Some(config) = config else {
                // No config off-owner → realtime can't authenticate.
                // Non-fatal: the on-mount `refresh()` already showed the
                // balance; we just don't get live updates this session.
                leptos::logging::log!(
                    "start_realtime: AppConfig missing — realtime disabled (balance \
                     stays from the initial refresh)"
                );
                return;
            };

            if config.supabase_anon_key.is_empty() {
                leptos::logging::log!(
                    "start_realtime: supabase anon key missing — realtime disabled \
                     (balance stays from the initial refresh)"
                );
                return;
            }

            let wallet = self.clone();
            // The user id / token provider need an async context (the
            // session load is async + the OpenSecret client builds the
            // same way `fetch_account_summaries` does). Capture `config`
            // before the spawn per the documented spawn_local/use_context
            // gotcha — we never touch context inside the future.
            wasm_bindgen_futures::spawn_local(async move {
                let uid = match load_session_user_id().await {
                    Ok(Some(uid)) => uid,
                    Ok(None) => {
                        // No session — ProtectedLayout should have
                        // redirected; nothing to subscribe to.
                        return;
                    }
                    Err(e) => {
                        leptos::logging::log!(
                            "start_realtime: session load failed, realtime \
                             disabled (balance unaffected): {e}"
                        );
                        return;
                    }
                };

                // Reuse the SAME OpenSecret token source the storage
                // layer uses (mirrors `fetch_account_summaries` /
                // Lane V's rehydration): a fresh Supabase-compatible JWT
                // is minted per `get_jwt` call from the browser session's
                // refresh token. `TokenProviderJwtSource` adapts it to
                // the realtime crate's `JwtSource` (the wasm variant
                // takes `Arc<dyn TokenProvider>`, no `Send + Sync`).
                let client = match build_opensecret_client(&config).await {
                    Ok(c) => c,
                    Err(e) => {
                        leptos::logging::log!(
                            "start_realtime: opensecret client build failed, \
                             realtime disabled (balance unaffected): {e}"
                        );
                        return;
                    }
                };
                let tokens: Arc<dyn TokenProvider> = Arc::new(client);
                let jwt = Arc::new(TokenProviderJwtSource(tokens));
                let factory = Arc::new(WasmTransportFactory);

                let service = Arc::new(WalletRealtimeService::new(
                    &config.supabase_url,
                    &config.supabase_anon_key,
                    uid.to_string(),
                    jwt,
                    factory,
                ));

                // Publish the service handle so the sign-out / teardown
                // path can call `.stop()` (and the banner's retry-on-
                // TerminalError can call `.set_online(false → true)` to
                // clear the terminal latch). Replaces the
                // `Arc::clone(...).forget()`-shaped leak the old wiring
                // had: the `Arc` is now reachable from the view-model,
                // so the supervisor stays addressable for its whole
                // lifetime instead of being orphaned in a detached task.
                wallet.realtime_service.set(Some(Arc::clone(&service)));

                // Resumption driver wiring (plan 2026-05-21 §7 Lane E).
                //
                // Build a session-seeded `Arc<WalletClient>` — same
                // composition root `fetch_pending_state` uses — and
                // hand it to `WalletClientSweeper`, which the driver
                // calls into to advance every unresolved
                // send_swap/receive_swap/mint_quote/melt_quote row.
                //
                // The driver subscribes to a SECOND receiver off the
                // SAME realtime service (`async-broadcast` allows N
                // independent receivers; the existing pump above
                // already used one). On every Connected / relevant
                // Event the driver runs a coalesced sweep — the React
                // `useProcessXTasks` analog. The driver handle is
                // stored alongside the realtime service handle so the
                // sign-out path can `.stop()` both.
                //
                // A WalletClient build failure is non-fatal: the
                // realtime pump (above) and the existing background
                // `refresh_pending_state` still run; the user just
                // loses recoverability for this session. Log + carry
                // on so the page is never blocked by a driver-init
                // hiccup.
                let driver_started = match build_driver_wallet_client(&config, uid).await {
                    Ok(wallet_client) => {
                        use agicash_driver::{DriverConfig, ResumptionDriver, WalletClientSweeper};
                        let sweeper = Arc::new(WalletClientSweeper::new(wallet_client));
                        // Second receiver: independent of the pump's
                        // `subscribe()` above. Each consumer sees every
                        // event from the moment it clones; the burst-
                        // coalescing latch inside the driver handles
                        // the in-flight + dirty-bit shape.
                        let rx = service.subscribe();
                        let driver = ResumptionDriver::new(sweeper, rx, DriverConfig::default());
                        // `.start()` is idempotent — a second call on
                        // the same `ResumptionDriver` returns a fresh
                        // handle over the SAME shared state. Our
                        // `reactivity_wired` latch guards this whole
                        // block anyway, so a re-mount cannot stack a
                        // second driver task.
                        let handle = Arc::new(driver.start());
                        wallet.driver_handle.set(Some(Arc::clone(&handle)));
                        Some(handle)
                    }
                    Err(e) => {
                        leptos::logging::log!(
                            "start_realtime: resumption driver disabled — \
                             WalletClient build failed (recoverability degraded \
                             for this session, realtime still active): {e}"
                        );
                        None
                    }
                };

                // Pump: on every Connected (no replay → catch up) and
                // every broadcast Event, refetch in background/SWR mode
                // — keep the last balance, never flash the spinner, and
                // a refetch failure is itself non-fatal (the SWR path in
                // `refresh_with_config` keeps stale data).
                //
                // `StatusChanged(_)` is forwarded to
                // [`WalletData::realtime_status`] so the
                // `RealtimeStatusBanner` can render a "Reconnecting…"
                // bar (Disconnected/Reconnecting/Error/Closed) or the
                // persistent "Connection lost — Retry" affordance
                // (TerminalError). The balance is NEVER blanked on a
                // disconnect — Lane V's stale-while-revalidate
                // discipline still owns that, the banner is purely an
                // additive surface.
                //
                // `Error(_)` is logged + folded into the status banner
                // by leaving the existing status (most often
                // `Reconnecting` / `Error`) intact — the supervisor
                // emits its own `StatusChanged` on transitions, so we
                // don't need to synthesize one from a transient `Error`
                // payload (which is opaque-string anyway).
                {
                    let service_for_pump = Arc::clone(&service);
                    let wallet = wallet.clone();
                    let config = config.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let mut rx = service_for_pump.subscribe();
                        loop {
                            match futures_util::StreamExt::next(&mut rx).await {
                                Some(WalletRealtimeEvent::Connected) => {
                                    wallet.on_realtime_connected(Some(config.clone()));
                                }
                                Some(WalletRealtimeEvent::Event(_)) => {
                                    wallet
                                        .clone()
                                        .refresh_with_config(Some(config.clone()), true);
                                }
                                Some(WalletRealtimeEvent::StatusChanged(status)) => {
                                    wallet.realtime_status.set(status);
                                }
                                Some(WalletRealtimeEvent::Error(msg)) => {
                                    // Log only — the supervisor emits a
                                    // companion `StatusChanged` so the
                                    // banner already reflects the new
                                    // state. Toasting every transient
                                    // socket blip would be noise.
                                    leptos::logging::log!(
                                        "realtime: transient error (banner reflects \
                                         status): {msg}"
                                    );
                                }
                                Some(WalletRealtimeEvent::Change(_)) => {
                                    // Typed-row sibling of `Event` —
                                    // the realtime supervisor fires
                                    // both per broadcast (see
                                    // `WalletRealtimeEvent` doc). The
                                    // Leptos pump's "refetch on any
                                    // change" discipline is already
                                    // handled by the `Event(_)` arm
                                    // above; the typed payload is for
                                    // the cache layer
                                    // (`agicash-wallet`) and not for
                                    // this signal-graph pump. No
                                    // behavior change this lane.
                                }
                                None => break, // sender dropped — service gone.
                            }
                        }
                    });
                }

                // Wire DOM lifecycle hooks (Gap-E): forward
                // `visibilitychange` and `online`/`offline` to the
                // service. iOS/Android get this from native lifecycle
                // observers via the FFI; on the web the equivalents
                // are window-level events. The service handle is held
                // by the closures via `Arc` so they outlive the spawn.
                //
                // The driver handle (if it constructed) rides along the
                // SAME listeners (plan 2026-05-21 §7 Lane E): the
                // `online` + `visibilitychange → visible` edges call
                // `driver.notify_foreground()` alongside the existing
                // `service.set_online(true)` / `set_active(true)` —
                // the React `refetchOnWindowFocus` analog, on the same
                // edge the focus listener already fires. The
                // `offline` + `visibilitychange → hidden` edges do
                // NOT notify the driver (those are NOT foreground
                // signals; the driver pauses on `Unauthenticated`,
                // not on background — see `task.rs` header).
                wire_dom_lifecycle(&service, driver_started.as_ref());

                // Drive the connect→join→serve→reconnect supervisor for
                // the page's lifetime. `run()` borrows `&self`; the
                // `Arc` keeps the service alive across the spawned task.
                // The view-model `Arc` (stored above) keeps the same
                // service addressable from outside, so `teardown_realtime`
                // can flip the stop flag and this loop exits cleanly.
                wasm_bindgen_futures::spawn_local(async move {
                    service.run().await;
                });
            });
        }
    }
}

/// Wire DOM lifecycle events to the realtime supervisor (Gap-E):
/// - `document.visibilitychange` → `set_active(document.visibilityState === 'visible')`
/// - `window.online` / `window.offline` → `set_online(true/false)`
///
/// All three listeners use `.forget()` so the closures live for the
/// page's lifetime. This matches the supervisor itself (`spawn_local`'d
/// onto the page's task pool with no symmetric teardown — Lane 1 keeps
/// teardown symmetry as Gap-G's responsibility). The supervisor's
/// `set_online`/`set_active` calls are idempotent and cheap (atomic
/// swap + 1-msg broadcast), so a redundant dispatch from a no-op
/// transition is a non-issue.
#[cfg(target_arch = "wasm32")]
fn wire_dom_lifecycle(
    service: &std::sync::Arc<agicash_realtime::WalletRealtimeService>,
    driver: Option<&std::sync::Arc<agicash_driver::DriverHandle>>,
) {
    use wasm_bindgen::{closure::Closure, JsCast};

    let Some(window) = web_sys::window() else {
        leptos::logging::log!(
            "wire_dom_lifecycle: window unavailable — skipping online/active wiring"
        );
        return;
    };

    // `online` / `offline`. The `online` edge is a foreground signal
    // for the driver — the React `refetchOnWindowFocus` analog
    // (plan 2026-05-21 §7 Lane E). `offline` is NOT a foreground
    // signal; the driver only resumes on actual focus / reconnect.
    {
        let service_on = service.clone();
        let driver_on = driver.cloned();
        let online_cb = Closure::<dyn FnMut()>::new(move || {
            service_on.set_online(true);
            if let Some(d) = driver_on.as_ref() {
                d.notify_foreground();
            }
        });
        let _ =
            window.add_event_listener_with_callback("online", online_cb.as_ref().unchecked_ref());
        online_cb.forget();
    }
    {
        let service_off = service.clone();
        let offline_cb = Closure::<dyn FnMut()>::new(move || {
            service_off.set_online(false);
        });
        let _ =
            window.add_event_listener_with_callback("offline", offline_cb.as_ref().unchecked_ref());
        offline_cb.forget();
    }
    // Apply the initial reachability state once so the supervisor doesn't
    // sit on a stale `true` after the page loads offline. (Initial
    // `notify_foreground` is unnecessary: the driver's `start()`
    // already marks an `InitialStart` trigger before the spawn.)
    if let Ok(online) = web_sys::js_sys::Reflect::get(
        &window.navigator(),
        &wasm_bindgen::JsValue::from_str("onLine"),
    ) {
        if let Some(b) = online.as_bool() {
            service.set_online(b);
        }
    }

    // `visibilitychange` — drives `set_active`. The document's
    // `visibilityState` is the truth (string `"visible"` vs `"hidden"`);
    // a focus change alone doesn't fire this. Only the
    // `visible` edge is a driver foreground signal — `hidden` is the
    // background edge, not a wake-up.
    if let Some(document) = window.document() {
        let service_vis = service.clone();
        let driver_vis = driver.cloned();
        let doc_for_cb = document.clone();
        let vis_cb = Closure::<dyn FnMut()>::new(move || {
            let visible = doc_for_cb.visibility_state() == web_sys::VisibilityState::Visible;
            service_vis.set_active(visible);
            if visible {
                if let Some(d) = driver_vis.as_ref() {
                    d.notify_foreground();
                }
            }
        });
        let _ = document
            .add_event_listener_with_callback("visibilitychange", vis_cb.as_ref().unchecked_ref());
        vis_cb.forget();
        // Initial state.
        service.set_active(document.visibility_state() == web_sys::VisibilityState::Visible);
    }
}

/// Builds the wasm `WebSocket` transport per (re)connect. The realtime
/// service rebuilds a fresh transport on every reconnect (spec §2.5);
/// this factory just hands out a default `WasmTransport` each time.
#[cfg(target_arch = "wasm32")]
struct WasmTransportFactory;

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl agicash_realtime::TransportFactory for WasmTransportFactory {
    async fn make(&self) -> Box<dyn agicash_realtime::RealtimeTransport> {
        Box::new(agicash_realtime::transport_wasm::WasmTransport::new())
    }
}

/// Build an `OpenSecretClient` **with the persisted browser session
/// threaded in** — the single fix for the "No refresh token available"
/// wallet-load failure.
///
/// The `OpenSecret` SDK's `SessionManager` is purely in-memory and
/// per-client (`Arc<RwLock<Option<TokenPair>>>`, all `None` on `new()`);
/// token *persistence* lives separately in `BrowserSessionStorage`
/// (`window.localStorage`). A bare `OpenSecretClient::new(..)` is
/// therefore a clean slate with no refresh token, so
/// `generate_third_party_token` → SDK auto-refresh →
/// `Error::Authentication("No refresh token available")`. Every
/// token-provider construction site MUST re-seed the refresh token from
/// `BrowserSessionStorage` (exactly as `app.rs::rehydrate_session`
/// does); this helper is that seam, shared by both the storage path
/// (`fetch_account_summaries`) and the realtime path
/// (`build_opensecret_client`).
///
/// Steps mirror `rehydrate_session`: build client → load persisted
/// session → `set_tokens("", Some(refresh_token))` → `refresh()` once
/// (attestation handshake + `/refresh` exchange, which writes the fresh
/// access + refresh pair into this client's session manager). A missing
/// persisted session is a hard error here (callers only reach this
/// after `ProtectedLayout` gated on an authenticated session).
#[cfg(target_arch = "wasm32")]
async fn session_seeded_opensecret_client(
    config: &AppConfig,
) -> Result<agicash_auth_opensecret::OpenSecretClient, String> {
    use agicash_auth_opensecret::{
        refresh, BrowserSessionStorage, OpenSecretClient, OpenSecretConfig,
    };
    use agicash_traits::SessionStorage;

    let client = OpenSecretClient::new(OpenSecretConfig {
        base_url: config.opensecret_base_url.clone(),
        client_id: config.opensecret_client_id,
    })
    .map_err(|e| format!("build opensecret client: {e}"))?;

    let session = match BrowserSessionStorage::new().load().await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err(
                "no persisted session — refresh token unavailable (please log in again)".into(),
            )
        }
        Err(e) => return Err(format!("session load failed: {e}")),
    };

    client
        .inner()
        .set_tokens(String::new(), Some(session.refresh_token))
        .map_err(|e| format!("seed refresh token failed: {e}"))?;

    // Exchange the persisted refresh token for a fresh access token in
    // THIS client's session manager. Without this the manager still has
    // only the (now-stale-shaped) refresh token and no access token.
    refresh(&client)
        .await
        .map_err(|e| format!("session refresh failed (re-login required): {e}"))?;

    Ok(client)
}

/// Build a wasm-shell `AgicashWasmWallet` over the SAME composition core
/// the FFI uses **and** thread the persisted browser session into its
/// in-memory session slot.
///
/// `AgicashWasmWallet::new` is a parallel composition root: it
/// constructs a fresh `WalletClient` whose `OpenSecretAuthClient`
/// session slot is `None` (`InMemorySessionStorage` empty). Every
/// `require_session()`-gated facade call (`send_token`,
/// `receive_cashu_token`, `prepare_send_quote`, `check_send_token_claimed`,
/// `list_accounts`) therefore returns `WalletError::Unauthenticated`
/// from a freshly-constructed wallet — which is exactly what every
/// Send/Receive button-click did before this fix.
///
/// This helper closes the gap (the 4th instance of the FFI-shell
/// session-threading pattern): construct the wallet, load the persisted
/// session from `BrowserSessionStorage`, and if present hand it to
/// `wallet.set_session(...)` — which runs the OS handshake + refresh
/// + persist (mirrors what `session_seeded_opensecret_client` does for
/// the storage / realtime composition roots). Returns the wallet
/// regardless of whether a session was found: callers that genuinely
/// support unauthenticated operation can still receive an
/// `Unauthenticated` error on their first facade call instead of a
/// helper-level hard failure.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn seed_wasm_wallet(
    config: &AppConfig,
) -> Result<agicash_wasm::AgicashWasmWallet, wasm_bindgen::JsValue> {
    use agicash_auth_opensecret::BrowserSessionStorage;
    use agicash_traits::SessionStorage;
    use wasm_bindgen::JsValue;

    let wallet = agicash_wasm::AgicashWasmWallet::new(
        config.opensecret_base_url.clone(),
        config.opensecret_client_id.to_string(),
        config.supabase_url.clone(),
        config.supabase_anon_key.clone(),
    )?;

    match BrowserSessionStorage::new().load().await {
        Ok(Some(session)) => {
            wallet
                .set_session(session.user_id.to_string(), session.refresh_token)
                .await?;
        }
        Ok(None) => {
            // No persisted session — caller may handle the resulting
            // Unauthenticated error explicitly (e.g. a public-mint
            // preview path). The wallet is still returned so the call
            // site's `Err` arm carries a real facade error, not a
            // helper-level "no session" string.
        }
        Err(e) => {
            return Err(JsValue::from_str(&format!("session load failed: {e}")));
        }
    }

    Ok(wallet)
}

/// Build the `OpenSecret` token provider from the resolved [`AppConfig`]
/// — the same session-seeded construction `fetch_account_summaries`
/// uses for storage, so the realtime join JWT comes from the identical
/// token source. Now async + session-threaded (was a bare empty-client
/// `new`, the root cause of the "No refresh token available" failure).
#[cfg(target_arch = "wasm32")]
async fn build_opensecret_client(
    config: &AppConfig,
) -> Result<agicash_auth_opensecret::OpenSecretTokenProvider, String> {
    use agicash_auth_opensecret::OpenSecretTokenProvider;

    let client = session_seeded_opensecret_client(config).await?;
    Ok(OpenSecretTokenProvider::new(client))
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
// `SupabaseStorage` is `!Send`/`!Sync` on wasm32, but
// `SupabaseCashuSendSwapStorage::new` (the `agicash-storage-supabase`
// public API) requires `Arc<SupabaseStorage>` — the `Arc` is
// API-mandated, not a free choice; `Rc` would mean changing that
// crate's signature. Out of scope for lint cleanup. (This fn is
// already wasm32-only, so a plain allow is correct.)
#[cfg(target_arch = "wasm32")]
#[allow(clippy::arc_with_non_send_sync)]
async fn fetch_account_summaries(
    config: &AppConfig,
    user_id: Uuid,
) -> Result<Vec<AccountSummary>, String> {
    use std::sync::Arc;

    use agicash_auth_opensecret::OpenSecretTokenProvider;
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

    // OpenSecret-backed token provider over a client that has the
    // browser session's refresh token threaded in (via
    // `session_seeded_opensecret_client` → `BrowserSessionStorage` +
    // `set_tokens` + `refresh`). A bare `OpenSecretClient::new` here was
    // an empty in-memory session → `generate_third_party_token` failed
    // with "No refresh token available" and black-holed every
    // authenticated wallet load. Each `get_jwt` now mints a fresh
    // Supabase-compatible JWT from the seeded session.
    let client = session_seeded_opensecret_client(config).await?;
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

/// Map each Cashu money-state enum to the uppercase string the DB
/// `state` column + the realtime `on_event` payload use — so the
/// summary the view reconciles is consistent with the wire shape.
#[cfg(target_arch = "wasm32")]
fn mint_quote_state_label(s: &agicash_cashu::CashuMintQuoteState) -> String {
    use agicash_cashu::CashuMintQuoteState as S;
    match s {
        S::Unpaid => "UNPAID",
        S::Paid { .. } => "PAID",
        S::Completed { .. } => "COMPLETED",
        S::Expired => "EXPIRED",
        S::Failed { .. } => "FAILED",
    }
    .to_string()
}

#[cfg(target_arch = "wasm32")]
fn receive_swap_state_label(s: &agicash_cashu::CashuReceiveSwapState) -> String {
    use agicash_cashu::CashuReceiveSwapState as S;
    match s {
        S::Pending => "PENDING",
        S::Completed => "COMPLETED",
        S::Failed { .. } => "FAILED",
    }
    .to_string()
}

#[cfg(target_arch = "wasm32")]
fn melt_quote_state_label(s: &agicash_cashu::CashuMeltQuoteState) -> String {
    use agicash_cashu::CashuMeltQuoteState as S;
    match s {
        S::Unpaid => "UNPAID",
        S::Pending => "PENDING",
        S::Paid { .. } => "PAID",
        S::Expired => "EXPIRED",
        S::Failed { .. } => "FAILED",
    }
    .to_string()
}

#[cfg(target_arch = "wasm32")]
fn send_swap_state_label(s: &agicash_cashu::CashuSendSwapState) -> String {
    use agicash_cashu::CashuSendSwapState as S;
    match s {
        S::Draft => "DRAFT",
        S::Pending { .. } => "PENDING",
        S::Completed { .. } => "COMPLETED",
        S::Failed { .. } => "FAILED",
        S::Reversed => "REVERSED",
    }
    .to_string()
}

/// Build a session-seeded `Arc<WalletClient>` for the resumption driver
/// (plan 2026-05-21 §7 Lane E).
///
/// Same composition root [`fetch_pending_state`] uses
/// (`WalletClient::from_config` + `set_session` with the browser-
/// persisted refresh token), but the `Arc` is kept long-lived: it is
/// handed to `WalletClientSweeper::new`, which the driver task holds
/// for its whole lifetime so every sweep reuses the same auth /
/// storage stack instead of rebuilding it. `from_config` does NO
/// network I/O at construction (verified by the `from_config_constructs_without_network`
/// unit test in `crates/agicash-wallet/src/builder.rs`); `set_session`
/// is the one async step (handshake + refresh exchange).
///
/// Caller invariants: must be called inside the same `spawn_local`
/// future that built the realtime service — already passed
/// `AppConfig` captured before the spawn — so this fn does not need
/// to touch `use_context`. A failure here is non-fatal (logged and
/// the driver is simply skipped this session); the realtime pump and
/// the existing background `refresh_pending_state` continue to run.
#[cfg(target_arch = "wasm32")]
async fn build_driver_wallet_client(
    config: &AppConfig,
    user_id: Uuid,
) -> Result<std::sync::Arc<agicash_wallet::WalletClient>, String> {
    use agicash_traits::SessionStorage;
    use agicash_wallet::{Session, SessionStorageChoice, WalletClient, WalletConfig};

    if config.supabase_anon_key.is_empty() {
        return Err("Supabase anon key missing — driver wallet client skipped".to_string());
    }

    let (wallet, _auth) = WalletClient::from_config(WalletConfig {
        opensecret_url: config.opensecret_base_url.clone(),
        opensecret_client_id: config.opensecret_client_id,
        supabase_url: config.supabase_url.clone(),
        supabase_anon_key: config.supabase_anon_key.clone(),
        session_storage: SessionStorageChoice::InMemory,
    })
    .map_err(|e| format!("build wallet client: {e}"))?;

    let refresh_token = match agicash_auth_opensecret::BrowserSessionStorage::new()
        .load()
        .await
    {
        Ok(Some(s)) => s.refresh_token,
        Ok(None) => {
            return Err(
                "no persisted session — refresh token unavailable (please log in again)".into(),
            )
        }
        Err(e) => return Err(format!("session load failed: {e}")),
    };
    wallet
        .set_session(Session {
            user_id: agicash_domain::UserId::from(user_id),
            refresh_token,
        })
        .await
        .map_err(|e| format!("seed wallet session: {e}"))?;

    Ok(wallet)
}

/// Realtime-(re)connect catch-up: fetch the signed-in user's full
/// in-flight money state (slice 12e Lane 3, Gap-D).
///
/// Delegates to `agicash_wallet::WalletClient::refresh_pending_state` —
/// the single in-flight money-state aggregator the FFI shell also calls.
/// The `agicash-wallet` facade is wasm-composable (its deps are
/// `cfg`-gated since `c11731cf`), so the Leptos client no longer needs
/// to hand-roll a duplicate of the four `list_*` storage reads.
///
/// Construction path: `WalletClient::from_config` (the same composition
/// root the FFI and the wasm-bindgen shell use) → `set_session` with the
/// browser-persisted refresh token. `set_session` seeds the `OpenSecret`
/// client the facade's storage layer shares, exactly as the old
/// `session_seeded_opensecret_client` helper did (`ensure_handshake` +
/// `set_tokens` + `refresh`). `refresh_pending_state` then issues the
/// four reads concurrently and short-circuits on the first error.
///
/// The facade returns `PendingStateSnapshot` (raw `agicash-cashu` row
/// types); this maps it to the local `(id, state)` view summary using
/// the same state-label helpers.
#[cfg(target_arch = "wasm32")]
async fn fetch_pending_state(
    config: &AppConfig,
    user_id: Uuid,
) -> Result<PendingStateSummary, String> {
    use agicash_traits::SessionStorage;
    use agicash_wallet::{Session, SessionStorageChoice, WalletClient, WalletConfig};

    if config.supabase_anon_key.is_empty() {
        return Err("Supabase anon key missing — pending-state catch-up skipped".to_string());
    }

    // Build the facade over the SAME `from_config` composition root the
    // FFI / wasm-bindgen shell use. No network I/O at construction; the
    // browser persists the refresh token via `BrowserSessionStorage`, so
    // the facade's own session-storage choice is irrelevant here.
    let (wallet, _auth) = WalletClient::from_config(WalletConfig {
        opensecret_url: config.opensecret_base_url.clone(),
        opensecret_client_id: config.opensecret_client_id,
        supabase_url: config.supabase_url.clone(),
        supabase_anon_key: config.supabase_anon_key.clone(),
        session_storage: SessionStorageChoice::InMemory,
    })
    .map_err(|e| format!("build wallet client: {e}"))?;

    // Seed the session from the browser-persisted refresh token. This
    // threads the token into the `OpenSecret` client the facade's
    // storage layer shares — `set_session` does the same
    // `ensure_handshake` + `set_tokens` + `refresh` the old
    // `session_seeded_opensecret_client` helper did.
    let refresh_token = match agicash_auth_opensecret::BrowserSessionStorage::new()
        .load()
        .await
    {
        Ok(Some(s)) => s.refresh_token,
        Ok(None) => {
            return Err(
                "no persisted session — refresh token unavailable (please log in again)".into(),
            )
        }
        Err(e) => return Err(format!("session load failed: {e}")),
    };
    wallet
        .set_session(Session {
            user_id: agicash_domain::UserId::from(user_id),
            refresh_token,
        })
        .await
        .map_err(|e| format!("seed wallet session: {e}"))?;

    // One call — the facade issues the four reads concurrently and
    // short-circuits on the first storage error.
    let snapshot = wallet
        .refresh_pending_state()
        .await
        .map_err(|e| format!("refresh_pending_state: {e}"))?;

    Ok(PendingStateSummary {
        mint_quotes: snapshot
            .mint_quotes
            .iter()
            .map(|q| PendingItem {
                id: q.id.to_string(),
                state: mint_quote_state_label(&q.state),
            })
            .collect(),
        receive_swaps: snapshot
            .receive_swaps
            .iter()
            .map(|sw| PendingItem {
                // Receive-swap identity is its token_hash — no row UUID.
                id: sw.token_hash.clone(),
                state: receive_swap_state_label(&sw.state),
            })
            .collect(),
        melt_quotes: snapshot
            .melt_quotes
            .iter()
            .map(|q| PendingItem {
                id: q.id.to_string(),
                state: melt_quote_state_label(&q.state),
            })
            .collect(),
        send_swaps: snapshot
            .send_swaps
            .iter()
            .map(|sw| PendingItem {
                id: sw.id.to_string(),
                state: send_swap_state_label(&sw.state),
            })
            .collect(),
    })
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

    /// `PendingStateSummary::default()` is the canonical "nothing in
    /// flight" state — all four lists empty (slice 12e Lane 3).
    #[test]
    fn pending_state_summary_default_is_all_empty() {
        let p = PendingStateSummary::default();
        assert!(p.mint_quotes.is_empty());
        assert!(p.receive_swaps.is_empty());
        assert!(p.melt_quotes.is_empty());
        assert!(p.send_swaps.is_empty());
    }

    /// `pending_state` is a `LoadState<PendingStateSummary>` — `Idle`
    /// is the pre-first-`Connected` shape the App root constructs.
    #[test]
    fn pending_state_load_state_idle_then_ready() {
        let idle: LoadState<PendingStateSummary> = LoadState::default();
        assert!(matches!(idle, LoadState::Idle));
        let ready = LoadState::Ready(PendingStateSummary::default());
        assert_eq!(ready.ready(), Some(&PendingStateSummary::default()));
    }
}
