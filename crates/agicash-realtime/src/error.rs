//! Error types for the realtime client.
use thiserror::Error;

#[derive(Debug, Error)]
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
