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
//! ## Architecture (post cache-layer migration, 2026-05-22)
//!
//! `WalletData` no longer talks to storage directly. It holds an
//! `Arc<agicash_wallet::WalletClient>` (the same composition root the
//! FFI uses) and consumes the wallet's in-process cache layer
//! (`agicash_wallet::WalletCache`) via two long-lived tasks wired up in
//! [`WalletData::start`]:
//!
//! 1. **Apply pump.** Subscribes to
//!    [`agicash_realtime::WalletRealtimeService`]'s broadcast and routes
//!    every `Change(boxed)` into `WalletClient::apply_realtime_change`,
//!    keeping the in-process cache row-current. `StatusChanged` drives
//!    the realtime banner; `Connected` / `Error` are tracing-only —
//!    there is NO refetch hedge: the resumption driver writes rows on
//!    reconnect catch-up, those writes surface as Change events, and
//!    the cache absorbs them as ordinary updates. `Event(_)` is a no-op
//!    (its old refetch role was deleted in the cache-consumer migration).
//! 2. **Dispatch pump.** Subscribes to
//!    [`agicash_wallet::WalletClient::cache_updates`] and translates each
//!    [`agicash_wallet::CacheUpdate`] tick into the corresponding signal
//!    `set()` — re-reading the cache slice that mutated. The dispatch
//!    handles `broadcast::error::RecvError::Lagged(n)` by resyncing
//!    every cache-backed signal at once (the cache is still
//!    authoritative; only the tick stream lagged).
//!
//! Initial population is lazy via the cache's `*_or_populate` methods:
//! mount calls `wallet.start(config)`, which seeds the wallet client and
//! kicks off `populate_all` (foreground accounts + four pending lists in
//! parallel). Subsequent reads are O(1) cache hits.
//!
//! Sign-out teardown drops the `Arc<WalletClient>` — its inner
//! `WalletCache` drops, the broadcast sender closes, and the dispatch
//! pump's `recv()` returns `Err(Closed)`, exiting cleanly. Same shape as
//! `realtime_service` / `driver_handle`.
//!
//! ## Empty-state correctness
//!
//! A signed-in guest with no accounts is still the steady-state
//! behaviour for a fresh account. The hero renders `$ 0` / `≈ 0 sats`
//! from these empty inputs, matching iOS `HomeView` exactly.

use leptos::prelude::*;
use uuid::Uuid;

// `AppConfig` is part of the public `start` / `refresh_with_config`
// signature, so it must be in scope on every target (it is `cfg`-free and
// the native build constructs a dev-defaults instance).
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
    /// `WalletData::start` hasn't been called yet (first paint).
    #[default]
    Idle,
    /// In-flight: an async populate is running.
    Loading,
    /// Ready with data — could be an empty `Vec`, which is the canonical
    /// "signed-in guest, no accounts" state today.
    Ready(T),
    /// Populate failed; carries a user-facing message.
    Error(String),
}

impl<T> LoadState<T> {
    /// True iff a populate is in flight.
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

#[cfg(target_arch = "wasm32")]
impl AccountSummary {
    /// Convert the facade [`agicash_wallet::AccountSummary`] to the local
    /// home-page-shaped summary. The facade's `balance` field is a
    /// decimal-encoded `String` (so the cross-FFI/JS boundary doesn't
    /// lose precision); we parse it back to `u64` since the local hero
    /// owns the numeric formatting anyway.
    ///
    /// A parse failure (shouldn't happen — the facade builds the string
    /// from a `u64`) collapses to `0` with a tracing-log breadcrumb so
    /// the page never crashes on a malformed wire-payload.
    fn from_facade(s: &agicash_wallet::AccountSummary) -> Self {
        let balance = s.balance.parse::<u64>().unwrap_or_else(|_| {
            leptos::logging::log!(
                "AccountSummary::from_facade: balance parse failed for {} → 0",
                s.balance
            );
            0
        });
        Self {
            currency: s.currency.to_string(),
            balance,
        }
    }
}

/// One in-flight money-state row, flattened to the `(id, state)` pair
/// the view layer needs to reconcile a stale "waiting…" list.
///
/// The full money/proof payload stays in the cache — a consumer that
/// wants detail re-reads by `id`. This summary only answers *which* rows
/// are still in flight and *what state* they are in, which is all the
/// realtime-reconnect catch-up needs (slice 12e Lane 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingItem {
    /// Row identity — UUID (mint/melt quote, send swap) or token hash
    /// (receive swap), stringified.
    pub id: String,
    /// Uppercase lifecycle state (`UNPAID` / `PAID` / `PENDING` / …).
    pub state: String,
}

/// The signed-in user's full in-flight money state — every pending /
/// unresolved row across the four Cashu money flows.
///
/// Sourced from the cache layer's four pending-list slices
/// (`CashuReceiveQuotes`, `CashuReceiveSwaps`, `CashuSendQuotes`,
/// `CashuSendSwaps`). The dispatch pump recomposes this on every tick
/// that mutates one of those slices.
///
/// Empty `Vec`s are the canonical "nothing in flight" state.
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
    /// until [`WalletData::start`] runs.
    pub user_id: RwSignal<Option<Uuid>>,
    /// Account list keyed by load state. `Ready(vec![])` is the
    /// canonical empty-wallet state. Updated by the dispatch pump on
    /// every `CacheKind::Accounts` / `CacheKind::AccountBalance` tick.
    pub accounts: RwSignal<LoadState<Vec<AccountSummary>>>,
    /// The user's in-flight money state (pending receives / unresolved
    /// sends). Recomposed by the dispatch pump on every tick that
    /// mutates one of the four Cashu pending-list slices.
    pub pending_state: RwSignal<LoadState<PendingStateSummary>>,
    /// Live realtime channel status. Drives the
    /// [`crate::components::RealtimeStatusBanner`] (Reconnecting / lost-
    /// connection affordance). Updated by the apply pump in
    /// [`WalletData::start`] on every `StatusChanged(_)` event;
    /// stays at `Idle` if realtime never starts (e.g. native test build,
    /// missing supabase anon key).
    pub realtime_status: RwSignal<RealtimeStatus>,
    /// Tracked handle to the running [`agicash_realtime::WalletRealtimeService`]
    /// so the sign-out / session-teardown path can call `.stop()` on it
    /// (closes the socket, drops the supervisor). `None` until the pump
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
    /// or after `.stop()` clears it.
    #[cfg(target_arch = "wasm32")]
    pub driver_handle: RwSignal<Option<std::sync::Arc<agicash_driver::DriverHandle>>, LocalStorage>,
    #[cfg(not(target_arch = "wasm32"))]
    pub driver_handle: RwSignal<Option<()>, LocalStorage>,
    /// The session-seeded [`agicash_wallet::WalletClient`] that owns the
    /// cache layer. Built once on [`WalletData::start`]; cloned into the
    /// driver, the apply pump (for `apply_realtime_change` calls), the
    /// dispatch pump (for the `cache_updates` subscription + per-slice
    /// re-reads), and the foreground populate path.
    ///
    /// Same `LocalStorage` reason as `realtime_service` /
    /// `driver_handle`: the wasm wallet client's storage stack holds
    /// non-`Send` `web_sys` handles. `Option<()>` on native to keep the
    /// `Clone + Debug` shape uniform across cfg.
    ///
    /// On sign-out [`Self::teardown_realtime`] sets this to `None`,
    /// dropping the last live `Arc` (the apply / dispatch pumps drop
    /// their clones as they exit) and naturally closing the cache's
    /// internal broadcast channel — the dispatch pump's `recv()` returns
    /// `Err(Closed)` and the loop exits cleanly.
    #[cfg(target_arch = "wasm32")]
    pub wallet_client: RwSignal<Option<std::sync::Arc<agicash_wallet::WalletClient>>, LocalStorage>,
    #[cfg(not(target_arch = "wasm32"))]
    pub wallet_client: RwSignal<Option<()>, LocalStorage>,
    /// Idempotency latch for [`WalletData::start`]. The realtime
    /// subscription + cache pumps are wired into the page lifetime
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
            // Thread-pinned: the wasm wallet client's storage stack
            // holds `!Send` `web_sys` handles. The signal carries the
            // long-lived `Arc<WalletClient>` shared with the driver,
            // apply pump, dispatch pump, and foreground populate path.
            wallet_client: RwSignal::new_local(None),
            reactivity_wired: RwSignal::new(false),
        }
    }

    /// Tear down the realtime subscription + the cache + clear local
    /// view-model state. Called from the sign-out flow (see
    /// [`crate::pages::SettingsIndexPage`]) before the auth signal is
    /// cleared. Best-effort and idempotent.
    ///
    /// Teardown order matters:
    ///
    /// 1. Stop the resumption driver — it observes the realtime
    ///    broadcast for triggers; stopping it first means any final
    ///    `Connected` / `Change` the supervisor flushes on close races a
    ///    stopped driver (those are no-op triggers anyway).
    /// 2. Stop the realtime service — closes the socket; the apply
    ///    pump's `rx.next()` returns `None` and that loop exits.
    /// 3. Drop the `Arc<WalletClient>` (clear the signal) — the cache's
    ///    internal `broadcast::Sender` drops once the last clone is
    ///    gone, the dispatch pump's `recv()` returns `Err(Closed)`, and
    ///    that loop exits cleanly.
    ///
    /// After teardown the `reactivity_wired` latch is reset so a later
    /// sign-in can wire a fresh subscription (the App root keeps a
    /// single `WalletData` instance for the page's lifetime).
    pub fn teardown_realtime(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(handle) = self.driver_handle.get_untracked() {
            handle.stop();
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(service) = self.realtime_service.get_untracked() {
            service.stop();
        }
        self.driver_handle.set(None);
        self.realtime_service.set(None);
        // Drop the wallet client signal LAST: pumps exit on their own
        // broadcast-close edges (above), but defensively clearing the
        // signal here drops our reference even if a pump task got stuck
        // shutting down.
        self.wallet_client.set(None);
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

    /// One-shot bring-up entry point. Replaces the old
    /// `refresh()` + `start_realtime()` pair the home mount Effect used
    /// to call separately.
    ///
    /// Side-effects, in order:
    ///
    /// 1. Build a session-seeded `Arc<WalletClient>` (the same
    ///    composition root the FFI uses); store it on
    ///    [`Self::wallet_client`].
    /// 2. Run an initial foreground populate against the cache
    ///    ([`Self::populate_all`]): drives `accounts` from `Idle` /
    ///    `Loading` to `Ready(…)` / `Error(_)`, plus the four pending
    ///    slices into `pending_state`.
    /// 3. Wire the `WalletRealtimeService` + `ResumptionDriver` (the
    ///    pre-existing recoverability machinery).
    /// 4. Spawn the **apply pump** — routes
    ///    `WalletRealtimeEvent::Change(boxed)` into
    ///    `wallet_client.apply_realtime_change(*boxed)`, drives the
    ///    realtime-status banner from `StatusChanged`, no other arms
    ///    refetch.
    /// 5. Spawn the **dispatch pump** — translates
    ///    `wallet_client.cache_updates()` ticks into signal `set()`s.
    ///    `Lagged(n)` triggers a full resync.
    ///
    /// Idempotent: the App root provides a single `WalletData`, but the
    /// Home page mount Effect can re-run on client-side nav back to `/`.
    /// The `reactivity_wired` latch ensures the service + socket + pumps
    /// are constructed exactly once for the page's lifetime.
    ///
    /// **Owner / context capture.** The pumps run inside detached
    /// `spawn_local` futures (no Leptos reactive owner), so they can
    /// never read `use_context::<AppConfig>()`. The caller (Home mount
    /// Effect, inside an owner) passes the already-resolved `AppConfig`;
    /// every detached path captures the value once.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    // On wasm32 the realtime handles + wallet client are `!Send`/`!Sync`
    // (`web_sys` sockets / fetch are JS-main-thread-pinned). `Arc` is
    // mandated by the agicash-realtime / agicash-wallet public APIs.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    #[allow(clippy::too_many_lines)]
    pub fn start(&self, config: Option<AppConfig>) {
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
                // No config off-owner → realtime can't authenticate and
                // the cache layer has nothing to populate from. Surface
                // a foreground error so the home page Retry button has
                // something to react to.
                leptos::logging::log!(
                    "WalletData::start: AppConfig missing — wallet bring-up skipped"
                );
                self.accounts
                    .set(LoadState::Error("AppConfig context missing".to_string()));
                return;
            };

            if config.supabase_anon_key.is_empty() {
                leptos::logging::log!(
                    "WalletData::start: supabase anon key missing — wallet bring-up skipped"
                );
                self.accounts.set(LoadState::Error(
                    "Supabase anon key missing — wallet disabled".to_string(),
                ));
                return;
            }

            // Mark accounts + pending_state as Loading for foreground
            // bring-up if there's no Ready value to preserve. This is
            // the SWR discipline today's `refresh_with_config` had —
            // kept intact so the home hero still shows a spinner only
            // when there's nothing to show.
            if !matches!(self.accounts.get_untracked(), LoadState::Ready(_)) {
                self.accounts.set(LoadState::Loading);
            }
            if !matches!(self.pending_state.get_untracked(), LoadState::Ready(_)) {
                self.pending_state.set(LoadState::Loading);
            }

            let wallet = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let uid = match load_session_user_id().await {
                    Ok(Some(uid)) => uid,
                    Ok(None) => {
                        // No session — ProtectedLayout should have
                        // redirected; nothing to subscribe to.
                        wallet.accounts.set(LoadState::Ready(Vec::new()));
                        wallet
                            .pending_state
                            .set(LoadState::Ready(PendingStateSummary::default()));
                        return;
                    }
                    Err(e) => {
                        leptos::logging::log!("WalletData::start: session load failed: {e}");
                        wallet.accounts.set(LoadState::Error(e));
                        return;
                    }
                };

                wallet.user_id.set(Some(uid));

                // 1. Build the session-seeded WalletClient. This is the
                //    single composition root: cache + driver + apply
                //    pump + dispatch pump + populate all share this
                //    `Arc<WalletClient>`. The cache lives inside.
                let wallet_client = match build_wallet_client(&config, uid).await {
                    Ok(c) => c,
                    Err(e) => {
                        leptos::logging::log!("WalletData::start: wallet client build failed: {e}");
                        wallet.accounts.set(LoadState::Error(e));
                        return;
                    }
                };
                wallet.wallet_client.set(Some(Arc::clone(&wallet_client)));

                // 2. Foreground populate. Drives signals from Loading
                //    to Ready (or Error) before any realtime tick lands.
                populate_all(&wallet, &wallet_client).await;

                // 3. Wire realtime service.
                let opensecret = match session_seeded_opensecret_client(&config).await {
                    Ok(c) => c,
                    Err(e) => {
                        leptos::logging::log!(
                            "WalletData::start: opensecret client build failed, \
                             realtime disabled (initial populate stays): {e}"
                        );
                        return;
                    }
                };
                let tokens: Arc<dyn TokenProvider> = Arc::new(
                    agicash_auth_opensecret::OpenSecretTokenProvider::new(opensecret),
                );
                let jwt = Arc::new(TokenProviderJwtSource(tokens));
                let factory = Arc::new(WasmTransportFactory);

                let service = Arc::new(WalletRealtimeService::new(
                    &config.supabase_url,
                    &config.supabase_anon_key,
                    uid.to_string(),
                    jwt,
                    factory,
                ));
                wallet.realtime_service.set(Some(Arc::clone(&service)));

                // 4. Wire the resumption driver (recoverability for
                //    rows that resolved during disconnect windows).
                //    Reuses the SAME `wallet_client` that holds the
                //    cache — driver writes go through the same
                //    composition root, so its writes also produce
                //    `Change` events that patch the cache.
                let driver_handle = {
                    use agicash_driver::{DriverConfig, ResumptionDriver, WalletClientSweeper};
                    let sweeper = Arc::new(WalletClientSweeper::new(Arc::clone(&wallet_client)));
                    let rx_driver = service.subscribe();
                    let driver = ResumptionDriver::new(sweeper, rx_driver, DriverConfig::default());
                    let handle = Arc::new(driver.start());
                    wallet.driver_handle.set(Some(Arc::clone(&handle)));
                    handle
                };

                // 5. Apply pump: realtime → cache. Subscribes to a
                //    SECOND receiver off the SAME realtime service
                //    (async-broadcast permits N independent receivers).
                //
                //    Operator's clean-cut decision: NO refetch arm.
                //    `Connected` is tracing-only — the driver + the
                //    typed `Change` stream cover catch-up. `Event(_)` is
                //    a no-op (legacy refetch path deleted). Only
                //    `StatusChanged` mutates a signal (the banner).
                {
                    let wallet_for_pump = wallet.clone();
                    let wallet_client_for_pump = Arc::clone(&wallet_client);
                    let service_for_pump = Arc::clone(&service);
                    wasm_bindgen_futures::spawn_local(async move {
                        let mut rx = service_for_pump.subscribe();
                        loop {
                            match futures_util::StreamExt::next(&mut rx).await {
                                Some(WalletRealtimeEvent::Change(boxed)) => {
                                    // Patch the cache from the typed
                                    // delta. The cache's apply path
                                    // logs + swallows conversion errors
                                    // internally; it never breaks the
                                    // pump.
                                    wallet_client_for_pump.apply_realtime_change(*boxed).await;
                                }
                                Some(WalletRealtimeEvent::StatusChanged(status)) => {
                                    wallet_for_pump.realtime_status.set(status);
                                }
                                Some(WalletRealtimeEvent::Connected) => {
                                    // Operator decision: trust the
                                    // driver + Change stream. No SWR
                                    // refetch hedge. The driver's sweep
                                    // on Connected writes rows; those
                                    // writes surface as Change events
                                    // and the apply arm above patches
                                    // the cache.
                                    leptos::logging::log!(
                                        "realtime: connected (driver sweep + Change stream \
                                         own catch-up)"
                                    );
                                }
                                Some(WalletRealtimeEvent::Event(_)) => {
                                    // Legacy refetch arm — DELETED in
                                    // the cache-consumer migration. The
                                    // typed sibling `Change(boxed)`
                                    // above is the load-bearing path.
                                }
                                Some(WalletRealtimeEvent::Error(msg)) => {
                                    // Log only — the supervisor emits a
                                    // companion `StatusChanged` so the
                                    // banner already reflects the new
                                    // state.
                                    leptos::logging::log!(
                                        "realtime: transient error (banner reflects \
                                         status): {msg}"
                                    );
                                }
                                None => break, // sender dropped — service gone.
                            }
                        }
                    });
                }

                // 6. Dispatch pump: cache → signals.
                //    Each `CacheUpdate { kind, id }` tick drives the
                //    matching cache-backed signal. The dispatch handler
                //    re-reads the cache slice (O(1) under one
                //    parking_lot lock) and `set()`s the corresponding
                //    `RwSignal<LoadState<T>>`.
                //
                //    `Lagged(n)` triggers a full resync — the cache is
                //    still authoritative, only the tick stream lagged.
                //    `Closed` means the underlying broadcast sender
                //    dropped (sign-out path cleared the `wallet_client`
                //    signal); the loop exits.
                {
                    let wallet_for_dispatch = wallet.clone();
                    let wallet_client_for_dispatch = Arc::clone(&wallet_client);
                    wasm_bindgen_futures::spawn_local(async move {
                        let mut rx = wallet_client_for_dispatch.cache_updates();
                        loop {
                            match rx.recv().await {
                                Ok(update) => {
                                    dispatch_cache_update(
                                        &wallet_for_dispatch,
                                        &wallet_client_for_dispatch,
                                        update,
                                    )
                                    .await;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    leptos::logging::log!(
                                        "cache dispatch lagged ({n}) — resyncing all signals"
                                    );
                                    resync_all_signals(
                                        &wallet_for_dispatch,
                                        &wallet_client_for_dispatch,
                                    )
                                    .await;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    leptos::logging::log!(
                                        "cache dispatch closed — wallet client dropped"
                                    );
                                    break;
                                }
                            }
                        }
                    });
                }

                // 7. DOM lifecycle: forward `visibilitychange` +
                //    `online`/`offline` to the realtime service and
                //    `notify_foreground` edges to the driver. Unchanged
                //    from the pre-migration wiring.
                wire_dom_lifecycle(&service, Some(&driver_handle));

                // 8. Drive the connect→join→serve→reconnect supervisor
                //    for the page's lifetime. `run()` borrows `&self`;
                //    the `Arc` keeps the service alive across the
                //    spawned task. The view-model `Arc` (stored above)
                //    keeps the same service addressable from outside,
                //    so `teardown_realtime` can flip the stop flag and
                //    this loop exits cleanly.
                wasm_bindgen_futures::spawn_local(async move {
                    service.run().await;
                });
            });
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            // Native: settle into the canonical empty steady state so
            // view tests render the same shape they'd see in the
            // browser steady-state.
            self.accounts.set(LoadState::Ready(Vec::new()));
            self.pending_state
                .set(LoadState::Ready(PendingStateSummary::default()));
        }
    }

    /// Foreground retry — used by the home page's "Retry" affordance
    /// when [`Self::start`] surfaced an `Error`. Re-runs
    /// [`populate_all`] against the existing `Arc<WalletClient>` (or
    /// builds one if `start` failed at the wallet-client step).
    ///
    /// `background == true` keeps the last `Ready` value on screen
    /// during the retry (SWR); `background == false` flips to `Loading`
    /// if nothing is currently `Ready`. The cache is the source of
    /// truth: a successful retry means the cache's `*_or_populate`
    /// methods completed their storage round-trip for the failed slice.
    #[cfg_attr(
        not(target_arch = "wasm32"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    pub fn refresh_with_config(self, config: Option<AppConfig>, background: bool) {
        // Foreground spinner discipline preserved from the pre-cache
        // shape: only blank the hero on a foreground refresh when there
        // is no `Ready` value to keep showing.
        let has_ready_accounts = matches!(self.accounts.get_untracked(), LoadState::Ready(_));
        if !background && !has_ready_accounts {
            self.accounts.set(LoadState::Loading);
        }
        let has_ready_pending = matches!(self.pending_state.get_untracked(), LoadState::Ready(_));
        if !background && !has_ready_pending {
            self.pending_state.set(LoadState::Loading);
        }

        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let Some(config) = config else {
                self.accounts
                    .set(LoadState::Error("AppConfig context missing".to_string()));
                return;
            };

            // If `start` already built a wallet client, re-use it. The
            // cache's `*_or_populate` methods are idempotent: a
            // successful populate is a no-op, a failed one (the case
            // we're retrying) re-attempts the storage call.
            let wallet_client = if let Some(existing) = self.wallet_client.get_untracked() {
                existing
            } else {
                // `start` failed before stashing a wallet client (e.g.
                // session load failed). Re-build now.
                let uid = match load_session_user_id().await {
                    Ok(Some(uid)) => uid,
                    Ok(None) => {
                        self.accounts.set(LoadState::Ready(Vec::new()));
                        return;
                    }
                    Err(e) => {
                        self.accounts.set(LoadState::Error(e));
                        return;
                    }
                };
                self.user_id.set(Some(uid));
                match build_wallet_client(&config, uid).await {
                    Ok(c) => {
                        self.wallet_client.set(Some(std::sync::Arc::clone(&c)));
                        c
                    }
                    Err(e) => {
                        self.accounts.set(LoadState::Error(e));
                        return;
                    }
                }
            };

            populate_all(&self, &wallet_client).await;
        });

        #[cfg(not(target_arch = "wasm32"))]
        leptos::task::spawn_local(async move {
            self.accounts.set(LoadState::Ready(Vec::new()));
            self.pending_state
                .set(LoadState::Ready(PendingStateSummary::default()));
        });
    }
}

/// Run the initial cache populate for every cache-backed signal,
/// foreground-style. Each cache method is lazy-populate-on-first-call;
/// subsequent calls (the dispatch pump, the retry button) get back-to-
/// back O(1) reads.
///
/// On error the corresponding signal flips to `LoadState::Error(_)`; the
/// other signal is unaffected so a transient pending-list failure
/// doesn't take down the balance hero (and vice versa).
#[cfg(target_arch = "wasm32")]
async fn populate_all(wallet: &WalletData, wallet_client: &agicash_wallet::WalletClient) {
    // Run accounts + pending populates concurrently. The cache layer's
    // in-flight guard serialises any duplicate first-callers across the
    // five storage calls (it's keyed by `CacheKind`), so we're safe to
    // fire them in parallel from the same task without exploding the
    // request fan-out.
    let accounts_fut = wallet_client.accounts();
    let pending_fut = read_pending_state_summary(wallet_client);
    let (accounts_res, pending_res) = futures_util::future::join(accounts_fut, pending_fut).await;

    match accounts_res {
        Ok(accounts) => {
            let summaries: Vec<AccountSummary> =
                accounts.iter().map(AccountSummary::from_facade).collect();
            wallet.accounts.set(LoadState::Ready(summaries));
        }
        Err(e) => {
            wallet.accounts.set(LoadState::Error(format!("{e}")));
        }
    }

    match pending_res {
        Ok(summary) => {
            wallet.pending_state.set(LoadState::Ready(summary));
        }
        Err(e) => {
            // Non-fatal: keep accounts on screen. Surface the pending
            // failure on its own signal.
            wallet.pending_state.set(LoadState::Error(e));
        }
    }
}

/// Read the four pending-list slices from the cache (lazy-populates on
/// first call) and recompose into [`PendingStateSummary`].
///
/// The four reads run concurrently; the cache's in-flight guard
/// serialises any duplicate storage calls across them. A single
/// storage-error from any one slice fails the whole summary (mirrors
/// the facade's `refresh_pending_state` short-circuit behaviour).
#[cfg(target_arch = "wasm32")]
async fn read_pending_state_summary(
    wallet_client: &agicash_wallet::WalletClient,
) -> Result<PendingStateSummary, String> {
    let (mq, rs, sq, ss) = futures_util::future::join4(
        wallet_client.pending_cashu_receive_quotes(),
        wallet_client.pending_cashu_receive_swaps(),
        wallet_client.unresolved_cashu_send_quotes(),
        wallet_client.unresolved_cashu_send_swaps(),
    )
    .await;

    let mint_quotes = mq.map_err(|e| format!("pending_cashu_receive_quotes: {e}"))?;
    let receive_swaps = rs.map_err(|e| format!("pending_cashu_receive_swaps: {e}"))?;
    let melt_quotes = sq.map_err(|e| format!("unresolved_cashu_send_quotes: {e}"))?;
    let send_swaps = ss.map_err(|e| format!("unresolved_cashu_send_swaps: {e}"))?;

    Ok(PendingStateSummary {
        mint_quotes: mint_quotes
            .iter()
            .map(|q| PendingItem {
                id: q.id.to_string(),
                state: mint_quote_state_label(&q.state),
            })
            .collect(),
        receive_swaps: receive_swaps
            .iter()
            .map(|sw| PendingItem {
                // Receive-swap identity is its token_hash — no row UUID.
                id: sw.token_hash.clone(),
                state: receive_swap_state_label(&sw.state),
            })
            .collect(),
        melt_quotes: melt_quotes
            .iter()
            .map(|q| PendingItem {
                id: q.id.to_string(),
                state: melt_quote_state_label(&q.state),
            })
            .collect(),
        send_swaps: send_swaps
            .iter()
            .map(|sw| PendingItem {
                id: sw.id.to_string(),
                state: send_swap_state_label(&sw.state),
            })
            .collect(),
    })
}

/// Translate one [`agicash_wallet::CacheUpdate`] tick into the matching
/// signal `set()`.
///
/// Accounts + AccountBalance ticks rebuild the local
/// `Vec<AccountSummary>` from the facade (`WalletClient::accounts()` is
/// cache-resident after the first populate so this is an in-process
/// roundtrip; the read includes a balance recompute via the cache's
/// memoized `account_balance` field).
///
/// Any of the four pending-slice ticks recompose the full
/// `PendingStateSummary` from cache. Signal-equality dedup in Leptos
/// prevents extra renders if the value didn't change.
///
/// Other ticks (`Transactions`, `UnacknowledgedTransactionCount`) are
/// out-of-scope for the current Leptos view; logged at trace and
/// ignored.
#[cfg(target_arch = "wasm32")]
async fn dispatch_cache_update(
    wallet: &WalletData,
    wallet_client: &agicash_wallet::WalletClient,
    update: agicash_wallet::CacheUpdate,
) {
    use agicash_wallet::CacheKind;

    match update.kind {
        CacheKind::Accounts | CacheKind::AccountBalance => match wallet_client.accounts().await {
            Ok(accounts) => {
                let summaries: Vec<AccountSummary> =
                    accounts.iter().map(AccountSummary::from_facade).collect();
                wallet.accounts.set(LoadState::Ready(summaries));
            }
            Err(e) => {
                leptos::logging::log!(
                    "cache dispatch: accounts re-read failed (keeping last value): {e}"
                );
            }
        },
        CacheKind::CashuReceiveQuotes
        | CacheKind::CashuReceiveSwaps
        | CacheKind::CashuSendQuotes
        | CacheKind::CashuSendSwaps => match read_pending_state_summary(wallet_client).await {
            Ok(summary) => {
                wallet.pending_state.set(LoadState::Ready(summary));
            }
            Err(e) => {
                leptos::logging::log!(
                    "cache dispatch: pending-state re-read failed (keeping last value): {e}"
                );
            }
        },
        CacheKind::Transactions | CacheKind::UnacknowledgedTransactionCount => {
            // No Leptos consumer yet — the transactions view is a
            // separate lane.
        }
    }
}

/// Full resync after a `Lagged(n)` recv error. Drops any dispatch-state
/// assumptions and rebuilds every cache-backed signal from the cache
/// (which is still authoritative — only the tick stream lagged).
///
/// Quiet on the signal level: a successful resync transitions
/// `LoadState::Ready(_)` → `LoadState::Ready(new_value)` and the dedup
/// in Leptos suppresses no-op renders.
#[cfg(target_arch = "wasm32")]
async fn resync_all_signals(wallet: &WalletData, wallet_client: &agicash_wallet::WalletClient) {
    populate_all(wallet, wallet_client).await;
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
/// therefore a clean slate with no refresh token, so the realtime JWT
/// mint fails with "No refresh token available". Every token-provider
/// construction site MUST re-seed the refresh token from
/// `BrowserSessionStorage`; this helper is that seam (still used by the
/// realtime JWT provider after the cache-consumer migration — storage
/// reads route through the `WalletClient` facade instead).
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
/// + persist. Returns the wallet regardless of whether a session was
/// found.
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
            // preview path).
        }
        Err(e) => {
            return Err(JsValue::from_str(&format!("session load failed: {e}")));
        }
    }

    Ok(wallet)
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

/// Build a session-seeded `Arc<WalletClient>`.
///
/// Single composition root for the cache-consumer migration. The
/// returned `Arc<WalletClient>` is shared by:
///
/// - The resumption driver — sweep writes (the `WalletClientSweeper`).
/// - The apply pump — `apply_realtime_change` calls from the
///   `Change(boxed)` arm of the realtime broadcast.
/// - The dispatch pump — `cache_updates()` subscription + per-slice
///   reads (`accounts()`, `pending_cashu_receive_quotes()`, etc.) on
///   every tick.
/// - The foreground populate path — initial bring-up + retry.
///
/// `from_config` does NO network I/O at construction (verified by the
/// `from_config_constructs_without_network` unit test in
/// `crates/agicash-wallet/src/builder.rs`); `set_session` is the one
/// async step (handshake + refresh exchange against the browser-
/// persisted refresh token).
#[cfg(target_arch = "wasm32")]
async fn build_wallet_client(
    config: &AppConfig,
    user_id: Uuid,
) -> Result<std::sync::Arc<agicash_wallet::WalletClient>, String> {
    use agicash_traits::SessionStorage;
    use agicash_wallet::{Session, SessionStorageChoice, WalletClient, WalletConfig};

    if config.supabase_anon_key.is_empty() {
        return Err("Supabase anon key missing — wallet client skipped".to_string());
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
    /// is the pre-first-populate shape the App root constructs.
    #[test]
    fn pending_state_load_state_idle_then_ready() {
        let idle: LoadState<PendingStateSummary> = LoadState::default();
        assert!(matches!(idle, LoadState::Idle));
        let ready = LoadState::Ready(PendingStateSummary::default());
        assert_eq!(ready.ready(), Some(&PendingStateSummary::default()));
    }

    /// Default `WalletData` matches the post-migration shape: every
    /// data signal starts `Idle`, all lifecycle handles `None`, the
    /// `reactivity_wired` latch `false`. This is the steady-state the
    /// App root provides into context and the sign-out path resets
    /// to via `clear_for_signout`.
    #[test]
    fn wallet_data_default_shape() {
        let w = WalletData::new();
        assert!(matches!(w.accounts.get_untracked(), LoadState::Idle));
        assert!(matches!(w.pending_state.get_untracked(), LoadState::Idle));
        assert_eq!(w.user_id.get_untracked(), None);
        assert_eq!(w.realtime_status.get_untracked(), RealtimeStatus::Idle);
        assert!(!w.reactivity_wired.get_untracked());
    }

    /// `clear_for_signout` returns every data signal to the
    /// `LoadState::Idle` + `user_id = None` shape that matches a fresh
    /// `WalletData`. Lifecycle handles + the wallet client get cleared
    /// by `teardown_realtime` (called inside `clear_for_signout`).
    #[test]
    fn clear_for_signout_resets_data_signals() {
        let w = WalletData::new();
        // Seed some state to verify the reset actually fires.
        w.accounts.set(LoadState::Ready(vec![AccountSummary {
            currency: "BTC".into(),
            balance: 1234,
        }]));
        w.pending_state
            .set(LoadState::Ready(PendingStateSummary::default()));
        w.user_id.set(Some(Uuid::nil()));
        w.reactivity_wired.set(true);

        w.clear_for_signout();

        assert!(matches!(w.accounts.get_untracked(), LoadState::Idle));
        assert!(matches!(w.pending_state.get_untracked(), LoadState::Idle));
        assert_eq!(w.user_id.get_untracked(), None);
        assert!(!w.reactivity_wired.get_untracked());
    }

    /// `start(None)` on native settles into the steady empty shape so
    /// view tests render the same surface as the browser steady state.
    /// (The wasm32 path is exercised by integration in the browser; the
    /// native rlib test build covers the shape.)
    #[test]
    fn start_native_settles_empty_ready() {
        let w = WalletData::new();
        w.start(None);
        assert_eq!(
            w.accounts.get_untracked().ready(),
            Some(&Vec::<AccountSummary>::new())
        );
        assert_eq!(
            w.pending_state.get_untracked().ready(),
            Some(&PendingStateSummary::default())
        );
    }
}
