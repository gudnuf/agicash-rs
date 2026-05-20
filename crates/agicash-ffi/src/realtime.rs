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

/// Implemented on the Swift/Kotlin side; the wallet calls these on
/// realtime activity. Registered via
/// [`crate::AgicashWallet::start_wallet_events`] and dropped (with the
/// supervisor task aborted) by
/// [`crate::AgicashWallet::stop_wallet_events`].
///
/// All four methods are invoked from the realtime supervisor's tokio
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
