//! Domain events emitted upward by the realtime client.

/// Logical lifecycle/status of the channel (for UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeStatus {
    Idle,
    Connecting,
    Subscribed,
    Reconnecting,
    Error,
    Closed,
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
