//! FFI realtime event surface.
//!
//! The Rust realtime client's only upward output is a (re)connected
//! signal plus opaque `(event_name, payload_json)` broadcasts, plus a
//! typed [`agicash_realtime::WalletChange`] payload that feeds the
//! in-process cache layer (`agicash-wallet::cache`). iOS / Android
//! register a [`WalletEventListener`]; the platform routes
//! `on_cache_change` → cache-backed re-read for the affected slice
//! (the React `useTrackWalletChanges` analog), and `on_status` /
//! `on_error` drive the banner. Leptos does NOT use this surface — it
//! consumes `agicash-realtime` + `agicash-wallet::cache` directly (it
//! links the Rust crates, not the FFI).
//!
//! `WalletEventListener` is a `UniFFI` `callback_interface`. `UniFFI`
//! marshals callbacks across the FFI boundary; the Swift/Kotlin object
//! MUST be safe to invoke from a background thread (the realtime
//! supervisor runs on a tokio task — see
//! [`crate::AgicashWallet::start_wallet_events`]). `UniFFI`'s generated
//! `Box<dyn WalletEventListener>` foreign shim is `Send + Sync`, which
//! is why the trait carries those bounds: the bridge holds the listener
//! behind an `Arc` and invokes it from the spawned supervisor + cache
//! pump tasks, not the thread that called `start_wallet_events`.
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

/// FFI-local mirror of `agicash_wallet::CacheKind` — which cache slice
/// changed. Mirror (not re-export) so the wallet crate stays free of
/// `uniffi` deps; the [`From`] impl below is exhaustive over every
/// variant so a new kind can't silently drop on the FFI floor.
///
/// Variant names match the DB-naming convention (smell S9):
/// `CashuReceiveQuotes` is the React-side "mint quotes",
/// `CashuSendQuotes` is the React-side "melt quotes". Receive / send
/// swap names match both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CacheKindFfi {
    /// `wallet.accounts` rows.
    Accounts,
    /// `wallet.transactions` rows.
    Transactions,
    /// Derived: count of `wallet.transactions` with
    /// `acknowledgment_status = 'pending'`.
    UnacknowledgedTransactionCount,
    /// `wallet.cashu_receive_quotes` rows (DB-naming for "mint quotes").
    CashuReceiveQuotes,
    /// `wallet.cashu_send_quotes` rows (DB-naming for "melt quotes").
    CashuSendQuotes,
    /// `wallet.cashu_receive_swaps` rows.
    CashuReceiveSwaps,
    /// `wallet.cashu_send_swaps` rows.
    CashuSendSwaps,
    /// Derived: per-account cached balance.
    AccountBalance,
}

impl From<agicash_wallet::CacheKind> for CacheKindFfi {
    fn from(k: agicash_wallet::CacheKind) -> Self {
        use agicash_wallet::CacheKind as K;
        match k {
            K::Accounts => Self::Accounts,
            K::Transactions => Self::Transactions,
            K::UnacknowledgedTransactionCount => Self::UnacknowledgedTransactionCount,
            K::CashuReceiveQuotes => Self::CashuReceiveQuotes,
            K::CashuSendQuotes => Self::CashuSendQuotes,
            K::CashuReceiveSwaps => Self::CashuReceiveSwaps,
            K::CashuSendSwaps => Self::CashuSendSwaps,
            K::AccountBalance => Self::AccountBalance,
        }
    }
}

/// FFI-local mirror of `agicash_wallet::RowId` — identity of one
/// cached row, stringified across the FFI boundary so iOS/Android
/// don't have to model `Uuid` / `AccountId` directly.
///
/// Mirror (not re-export) for the same reason as [`CacheKindFfi`]:
/// keeps `uniffi` out of the wallet crate.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum RowIdFfi {
    /// `wallet.accounts.id` (UUID, stringified).
    Account { id: String },
    /// Generic UUID-keyed row (transactions, quotes, send swaps), stringified.
    Uuid { id: String },
    /// `wallet.cashu_receive_swaps.token_hash` — already a string on the
    /// domain type; no UUID for receive-swap rows.
    TokenHash { hash: String },
}

impl From<&agicash_wallet::RowId> for RowIdFfi {
    fn from(r: &agicash_wallet::RowId) -> Self {
        use agicash_wallet::RowId as R;
        match r {
            R::Account(id) => Self::Account { id: id.to_string() },
            R::Uuid(id) => Self::Uuid { id: id.to_string() },
            R::TokenHash(h) => Self::TokenHash { hash: h.clone() },
        }
    }
}

/// FFI-local mirror of `agicash_wallet::CacheUpdate` — one cache
/// mutation tick. Lightweight by design: `(kind, optional row id)` lets
/// the consumer decide whether it cares without re-reading the cache,
/// and tells it which row changed if it does.
///
/// Heavy payloads do NOT cross the FFI bridge — the consumer reads the
/// new value back from the cache via the existing FFI methods
/// (e.g. `AgicashWallet::list_accounts`, which is now cache-backed).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CacheUpdateFfi {
    /// Which table slice mutated.
    pub kind: CacheKindFfi,
    /// Row identity, when the mutation was row-scoped. `None` for
    /// derived/aggregate mutations (e.g. unacknowledged-count, balance
    /// recompute without a single row id).
    pub id: Option<RowIdFfi>,
}

impl From<&agicash_wallet::CacheUpdate> for CacheUpdateFfi {
    fn from(u: &agicash_wallet::CacheUpdate) -> Self {
        Self {
            kind: u.kind.into(),
            id: u.id.as_ref().map(RowIdFfi::from),
        }
    }
}

/// Implemented on the Swift/Kotlin side; the wallet calls these on
/// realtime activity. Registered via
/// [`crate::AgicashWallet::start_wallet_events`] and dropped (with the
/// supervisor + cache pump tasks aborted) by
/// [`crate::AgicashWallet::stop_wallet_events`].
///
/// All methods are invoked from background tokio tasks — never from the
/// thread that called `start_wallet_events` — so the foreign
/// implementation must be thread-safe. UniFFI enforces `Send + Sync` on
/// the boxed foreign object; the bridge additionally never re-enters
/// the listener (each callback is a fire-and-forget notification, not
/// a request).
///
/// The cache layer (`agicash-wallet::cache`, shipped at master
/// `1b4f191b`) is the source of truth: every realtime `Change` event
/// is applied to the cache by the apply pump, and the dispatch pump
/// fires [`on_cache_change`] so the consumer can re-read the cache
/// slice. Legacy `on_event` / `on_connected` "refetch on signal" paths
/// were deleted in the FFI cache-consumer migration — the cache + the
/// Lane-D resumption driver own catch-up.
#[uniffi::export(callback_interface)]
pub trait WalletEventListener: Send + Sync {
    /// Channel (re)connected & joined. Observability only — the cache
    /// + Lane-D resumption driver handle the post-reconnect catch-up;
    /// the consumer does NOT need to refetch on this signal. (Mirrors
    /// the Leptos `Connected` arm semantics — see the Leptos cache-
    /// consumer migration design §2.1.)
    fn on_connected(&self);
    /// A DB-originated broadcast. `event` ∈ {ACCOUNT_CREATED,
    /// ACCOUNT_UPDATED, TRANSACTION_CREATED, TRANSACTION_UPDATED,
    /// CASHU_RECEIVE_QUOTE_*, ...}. `payload_json` is the raw jsonb the
    /// trigger sent (opaque to transport; parsed by the caller).
    ///
    /// Observability only — paired with [`on_cache_change`] one-for-one
    /// (the realtime supervisor fires `Event` + `Change` for every
    /// broadcast). Consumers should drive UI off `on_cache_change`; this
    /// callback is left in place so log-level taps on the FFI surface
    /// keep working.
    fn on_event(&self, event: String, payload_json: String);
    /// Status transitions for UI affordances.
    fn on_status(&self, status: RealtimeStatusFfi);
    /// Non-fatal/observability error string.
    fn on_error(&self, message: String);
    /// One cache slice mutated — re-read the corresponding cache-backed
    /// FFI method (e.g. `list_accounts` for [`CacheKindFfi::Accounts`])
    /// to get the new value. The `id` (when present) lets the consumer
    /// filter to one row if it's tracking row-level identity.
    ///
    /// Fires from a tokio task subscribed to the cache's broadcast
    /// channel (`agicash_wallet::WalletClient::cache_updates`). One tick
    /// per cache mutation; ordered with respect to mutations on a single
    /// slice; bounded — a slow listener that overruns the channel
    /// capacity sees missed ticks dropped (the cache itself stays
    /// authoritative, the next tick still triggers a re-read).
    fn on_cache_change(&self, update: CacheUpdateFfi);
}

/// Map one realtime event onto the listener. Pure dispatch — does NOT
/// touch the cache (the apply path lives in the pump in
/// [`crate::AgicashWallet::start_wallet_events`], which destructures
/// `WalletRealtimeEvent::Change(boxed)` separately and feeds it into
/// `WalletClient::apply_realtime_change` before calling this for the
/// listener-visible side of the event).
///
/// Factored out (rather than inlined in the pump) so it is hermetically
/// smoke-testable: a fake listener + a synthetic event, no socket /
/// tokio task / live stack required.
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
            // The typed-row sibling of `Event`. The pump handles
            // `Change` separately — it consumes the boxed payload and
            // feeds it into the cache's `apply_realtime_change` BEFORE
            // calling this function with the other variants. So if a
            // `Change` reaches here it means the caller bypassed the
            // pump's apply step (e.g. a test); no listener method
            // corresponds to it.
        }
    }
}

/// Dispatch one cache-update tick to the listener. Pure projection of
/// the wallet-crate [`agicash_wallet::CacheUpdate`] onto the FFI
/// [`CacheUpdateFfi`] mirror. Lives next to [`dispatch_realtime_event`]
/// for the same reason: it's the entire FFI-side bridge for the cache
/// path, factored out so a hermetic test can drive it without a
/// running cache.
pub(crate) fn dispatch_cache_update(
    listener: &dyn WalletEventListener,
    update: &agicash_wallet::CacheUpdate,
) {
    listener.on_cache_change(update.into());
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
