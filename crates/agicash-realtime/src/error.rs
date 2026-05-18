//! Error types for the realtime client.
use thiserror::Error;

// `Clone` is required because `TransportError` travels through the wasm
// transport's `async-broadcast` channel (plan §Task 6 `transport_wasm.rs`),
// and `async_broadcast::Sender::<T>::try_broadcast` is bound `T: Clone`.
// Every variant is a `String`, so the derive is trivial.
#[derive(Debug, Clone, Error)]
pub enum TransportError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("send failed: {0}")]
    Send(String),
    #[error("socket closed: {0}")]
    Closed(String),
    #[error("transport: {0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum RealtimeError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("codec: {0}")]
    Codec(String),
    #[error("join rejected by server (RLS/auth): {0}")]
    JoinRejected(String),
    #[error("token provider: {0}")]
    Token(String),
}
