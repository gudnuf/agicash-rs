//! FFI realtime event surface.
//!
//! The Rust realtime client's only upward output is a (re)connected
//! signal plus opaque `(event_name, payload_json)` broadcasts. iOS /
//! Android register a [`WalletEventListener`]; the platform routes
//! `on_connected` → balance/account refetch (the no-replay catch-up,
//! spec §5.5) and `on_event` → the same refetch (the wallet layer
//! demuxes by event name exactly as the React `useTrackWalletChanges`
//! does). Leptos does NOT use this surface — it consumes
//! `agicash-realtime` directly (it links the Rust crates, not the FFI).
//!
//! `WalletEventListener` is the first `UniFFI` `callback_interface` in
//! this crate. `UniFFI` marshals callbacks across the FFI boundary; the
//! Swift/Kotlin object MUST be safe to invoke from a background thread
//! (the realtime supervisor runs on a tokio task — see
//! [`crate::AgicashWallet::start_wallet_events`]). `UniFFI`'s generated
//! `Box<dyn WalletEventListener>` foreign shim is `Send + Sync`, which
//! is why the trait carries those bounds: the bridge holds the listener
//! behind an `Arc` and invokes it from the spawned supervisor task, not
//! the thread that called `start_wallet_events`.
use agicash_realtime::RealtimeStatus;

/// Lifecycle status forwarded for UI (spinner / "reconnecting" banner).
/// 1:1 with `agicash_realtime::RealtimeStatus` — the [`From`] impl below
/// is exhaustive over all variants so a new realtime status can't
/// silently drop on the FFI floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RealtimeStatusFfi {
    /// No subscription yet (pre-`start_wallet_events`).
    Idle,
    /// Socket opening / channel join in flight.
    Connecting,
    /// Channel joined; broadcasts flowing.
    Subscribed,
    /// Lost the channel, backing off before the next attempt.
    Reconnecting,
    /// A non-recoverable error path was hit (still followed by reconnect
    /// attempts unless `stop_wallet_events` was called).
    Error,
    /// `stop_wallet_events` left the channel and closed the socket.
    Closed,
    /// The supervisor's `JoinReplyError` (auth/RLS deny) retry cap fired
    /// — see `agicash_realtime::service::MAX_JOIN_REJECT_ATTEMPTS`. The
    /// supervisor has STOPPED retrying and will stay paused until the
    /// session resumes (e.g. `set_online(true)` after `set_online(false)`,
    /// or a fresh sign-in). Lane 1 surface stops here: clients log it as
    /// a stopgap until Lane 2 wires a UI banner.
    TerminalError,
}

impl From<RealtimeStatus> for RealtimeStatusFfi {
    fn from(s: RealtimeStatus) -> Self {
        match s {
            RealtimeStatus::Idle => Self::Idle,
            RealtimeStatus::Connecting => Self::Connecting,
            RealtimeStatus::Subscribed => Self::Subscribed,
            RealtimeStatus::Reconnecting => Self::Reconnecting,
            RealtimeStatus::Error => Self::Error,
            RealtimeStatus::Closed => Self::Closed,
            RealtimeStatus::TerminalError => Self::TerminalError,
        }
    }
}

/// Lifecycle state of one in-flight money-state row, surfaced through
/// the FFI as a stable uppercase string (matches the DB `state` column
/// and the wire shape iOS/Android already parse from `on_event`).
pub type PendingItemState = String;

/// One in-flight (pending / unresolved) money-state row, flattened to
/// the minimal `(id, state)` pair the FFI surface needs.
///
/// The full money/proof payload stays Rust-internal — a client that
/// wants the detail polls by `id` through the existing per-quote FFI
/// methods (`poll_mint_quote`, `check_send_swap_claimed`, …). This
/// record only has to tell the client *which* rows are still in flight
/// and *what state* they are in, which is all the realtime-reconnect
/// catch-up needs to refresh a stale "waiting…" list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PendingItemFfi {
    /// The row's primary-key UUID, stringified.
    pub id: String,
    /// Uppercase lifecycle state (`UNPAID` / `PAID` / `PENDING` /
    /// `DRAFT`).
    pub state: PendingItemState,
}

/// FFI projection of `agicash_wallet::PendingStateSnapshot` — every
/// in-flight money-state row the signed-in user still owns, fetched in
/// one shot on a realtime (re)connect (slice 12e Lane 3, Gap-D).
///
/// Four flat lists keyed by money-flow kind. An empty list is the
/// canonical "nothing in flight" state.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PendingStateSnapshotFfi {
    /// UNPAID / PAID mint quotes — Lightning receives still in flight.
    pub mint_quotes: Vec<PendingItemFfi>,
    /// PENDING receive swaps — inbound Cashu tokens still being claimed.
    pub receive_swaps: Vec<PendingItemFfi>,
    /// UNPAID / PENDING melt quotes — Lightning sends still in flight.
    pub melt_quotes: Vec<PendingItemFfi>,
    /// DRAFT / PENDING send swaps — outbound tokens not yet claimed.
    pub send_swaps: Vec<PendingItemFfi>,
}

/// Implemented on the Swift/Kotlin side; the wallet calls these on
/// realtime activity. Registered via
/// [`crate::AgicashWallet::start_wallet_events`] and dropped (with the
/// supervisor task aborted) by
/// [`crate::AgicashWallet::stop_wallet_events`].
///
/// All methods are invoked from the realtime supervisor's tokio
/// task — never from the calling thread — so the foreign implementation
/// must be thread-safe. UniFFI enforces `Send + Sync` on the boxed
/// foreign object; the bridge additionally never re-enters the listener
/// (each callback is a fire-and-forget notification, not a request).
#[uniffi::export(callback_interface)]
pub trait WalletEventListener: Send + Sync {
    /// Channel (re)connected & joined. The platform MUST refetch wallet
    /// + balance state — there is no replay; this is the catch-up hook
    /// that replaces the deleted Tier-1 pollers.
    fn on_connected(&self);
    /// A DB-originated broadcast. `event` ∈ {ACCOUNT_CREATED,
    /// ACCOUNT_UPDATED, TRANSACTION_CREATED, TRANSACTION_UPDATED,
    /// CASHU_RECEIVE_QUOTE_*, ...}. `payload_json` is the raw jsonb the
    /// trigger sent (opaque to transport; parsed by the caller).
    fn on_event(&self, event: String, payload_json: String);
    /// Status transitions for UI affordances.
    fn on_status(&self, status: RealtimeStatusFfi);
    /// Non-fatal/observability error string.
    fn on_error(&self, message: String);
    /// The wallet's in-flight money state, refetched after an
    /// `on_connected` (re)connect catch-up (slice 12e Lane 3, Gap-D).
    ///
    /// Fired right after `on_connected` whenever the post-reconnect
    /// `refresh_pending_state()` succeeds — the platform uses it to
    /// clear stale "waiting…" rows that resolved while the channel was
    /// down. A `refresh_pending_state()` *failure* is swallowed (logged,
    /// non-fatal): the balance refetch in `on_connected` already keeps
    /// the UI alive, and the next reconnect retries. Until iOS/Android
    /// wire pending-list UI consumption (a follow-up), a logging-only
    /// stub here is acceptable — it mirrors the pre-banner state of the
    /// `on_status` callback.
    fn on_pending_state_refreshed(&self, snapshot: PendingStateSnapshotFfi);
}

/// Map one realtime event onto the listener. This is the entire
/// FFI-side bridge: the supervisor pump in
/// [`crate::AgicashWallet::start_wallet_events`] calls this for every
/// `WalletRealtimeEvent` drained off the service's `async_broadcast`
/// receiver. Factored out (rather than inlined in the pump) so it is
/// hermetically smoke-testable: a fake listener + a synthetic event,
/// no socket / tokio task / live stack required.
pub(crate) fn dispatch_realtime_event(
    listener: &dyn WalletEventListener,
    ev: agicash_realtime::WalletRealtimeEvent,
) {
    use agicash_realtime::WalletRealtimeEvent as E;
    match ev {
        E::Connected => listener.on_connected(),
        E::Event(e) => listener.on_event(e.event, e.payload_json),
        E::StatusChanged(s) => listener.on_status(s.into()),
        E::Error(m) => listener.on_error(m),
        E::Change(_) => {
            // Typed-row sibling of `Event` — the realtime supervisor
            // fires both per broadcast (see `WalletRealtimeEvent`
            // doc). iOS / Android consume the string-shaped surface
            // through `on_event`; the typed payload is for the in-
            // process cache layer (`agicash-wallet`) and never
            // crosses the FFI boundary. No FFI behavior change.
        }
    }
}

/// Realtime-(re)connect catch-up: refetch the user's in-flight money
/// state from the facade and push it to the listener
/// (slice 12e Lane 3, Gap-D).
///
/// The realtime channel ships no replay, so on every `Connected` the
/// pump runs this right after `dispatch_realtime_event` fires
/// `on_connected`. It calls the facade's `refresh_pending_state()`
/// (a pure storage read, no mint round-trip) and, on success, projects
/// the snapshot through [`crate::convert::pending_state_snapshot_to_ffi`]
/// and dispatches it via [`WalletEventListener::on_pending_state_refreshed`].
///
/// A `refresh_pending_state()` **failure is swallowed** (logged,
/// non-fatal): the platform's balance refetch in `on_connected` already
/// keeps the UI alive, and the next reconnect retries the catch-up.
/// Returning `()` (never an error) keeps the pump loop unbreakable —
/// one failed catch-up must never tear down the realtime supervisor.
///
/// Factored out of the pump so it is unit-testable in isolation: it
/// takes the facade + listener as trait objects, no socket / tokio
/// supervisor / live stack required.
pub(crate) async fn dispatch_pending_state_refresh(
    facade: &agicash_wallet::WalletClient,
    listener: &dyn WalletEventListener,
) {
    match facade.refresh_pending_state().await {
        Ok(snapshot) => {
            listener.on_pending_state_refreshed(crate::convert::pending_state_snapshot_to_ffi(
                &snapshot,
            ));
        }
        Err(e) => {
            // Non-fatal: balance refetch in `on_connected` keeps the UI
            // alive; the next reconnect retries. Never break the pump.
            tracing::warn!(
                target: "agicash_ffi::realtime",
                "refresh_pending_state on reconnect failed (non-fatal, \
                 retries next reconnect): {e}"
            );
        }
    }
}

/// Builds the native (`tokio-tungstenite`) transport on every
/// (re)connect. The realtime service rebuilds its transport per attempt
/// (spec §2.5); this factory is the FFI-side concrete that the wallet
/// hands the service. The wasm path (Leptos) never reaches this — it
/// links `agicash-realtime` directly with its own wasm factory.
#[derive(Debug, Default)]
pub struct NativeTransportFactory;

#[async_trait::async_trait]
impl agicash_realtime::service::TransportFactory for NativeTransportFactory {
    async fn make(&self) -> Box<dyn agicash_realtime::transport::RealtimeTransport> {
        Box::new(agicash_realtime::transport_native::NativeTransport::new())
    }
}
