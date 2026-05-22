//! Domain events emitted upward by the realtime client.

/// Logical lifecycle/status of the channel (for UI).
///
/// `TerminalError` is emitted exactly once when the supervisor's
/// `JoinReplyError` (RLS/auth deny) counter exceeds the give-up cap
/// (`MAX_JOIN_REJECT_ATTEMPTS`, mirrors React's 9-attempt give-up).
/// The supervisor then pauses (does not reconnect) until the host
/// reports a session resume (`set_online(true)` after `false`, or
/// `set_active(true)` after `false`) or the caller calls `stop()`.
/// Distinguished from `Error` (recoverable / transient) so UI
/// surfaces can show a "sign out and back in" banner without
/// escalating every transient drop. This is Gap-B's terminal signal,
/// paired with the online/active lifecycle in Gap-E.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeStatus {
    Idle,
    Connecting,
    Subscribed,
    Reconnecting,
    Error,
    Closed,
    /// Retry cap exceeded on join-rejected (auth/RLS deny). Supervisor
    /// has stopped retrying — stays paused until the session resumes.
    TerminalError,
}

/// A DB-originated broadcast, demuxed by the caller per `event` name.
#[derive(Debug, Clone)]
pub struct WalletEvent {
    /// e.g. `ACCOUNT_UPDATED`, `TRANSACTION_CREATED`. (= `payload.event`)
    pub event: String,
    /// Raw jsonb the trigger sent (= `payload.payload`), opaque here.
    pub payload_json: String,
}

/// What the service streams to its consumers.
///
/// `Event` and `Change` always fire as a pair for one broadcast: `Event`
/// carries the opaque `(event_name, payload_json)` shape the FFI bridge,
/// Leptos pump, and driver consume today (string-shaped, "something
/// changed → refetch"); `Change` carries the same broadcast parsed into
/// a typed [`crate::WalletChange`] for the upcoming `agicash-wallet`
/// cache layer to apply row updates without an extra REST round-trip.
/// Existing consumers ignore `Change` (single new arm, no behavior
/// change). `Connected` / `StatusChanged` / `Error` retain their
/// previous semantics and ordering. Spec §5.4.
#[derive(Debug, Clone)]
pub enum WalletRealtimeEvent {
    /// Channel (re)connected & joined. Caller MUST refetch wallet state
    /// (no replay — this is the catch-up trigger).
    Connected,
    /// String-shaped broadcast event for refetch-style consumers.
    Event(WalletEvent),
    /// Typed broadcast payload for delta-applying consumers (the cache
    /// layer in `agicash-wallet`). Emitted in lockstep with `Event` —
    /// every `Event` is paired with a `Change` carrying the same
    /// broadcast as a typed variant. Unrecognized event names OR
    /// payloads that fail to deserialize collapse to
    /// [`crate::WalletChange::Unknown`] (the realtime pump never breaks
    /// over a payload-shape drift; see [`crate::parse_change`] doc).
    ///
    /// Boxed because `WalletChange` carries an inlined row (the largest
    /// is `TransactionWithPreviousAck` at ~256 bytes) and the rest of
    /// the enum's variants are tiny (`Connected` is a zero-byte unit);
    /// the box keeps the enum compact for the broadcast channel
    /// (`async_broadcast` clones every event on send) without hiding
    /// the type from callers — they pattern-match
    /// `WalletRealtimeEvent::Change(boxed_change)` and `*boxed_change`
    /// (or destructure via the deref pattern) the same as if it were
    /// unboxed.
    Change(Box<crate::payload::WalletChange>),
    StatusChanged(RealtimeStatus),
    Error(String),
}
