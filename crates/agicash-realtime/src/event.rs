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
#[derive(Debug, Clone)]
pub enum WalletRealtimeEvent {
    /// Channel (re)connected & joined. Caller MUST refetch wallet state
    /// (no replay — this is the catch-up trigger).
    Connected,
    Event(WalletEvent),
    StatusChanged(RealtimeStatus),
    Error(String),
}
