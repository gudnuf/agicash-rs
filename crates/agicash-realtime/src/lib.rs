//! Supabase Realtime (Phoenix channel, serializer v2.0.0) client.
//!
//! Implements exactly the protocol the agicash React app uses:
//! private broadcast-from-database on `realtime:wallet:<userId>`.
//! See research/2026-05-18-supabase-realtime-protocol.md.
#![allow(missing_docs)]

pub mod codec;
pub mod error;
pub mod event;
pub mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub mod transport_native;
#[cfg(target_arch = "wasm32")]
pub mod transport_wasm;

pub mod client;
pub mod service;

pub use error::{RealtimeError, TransportError};
// Re-exports below are restored incrementally as Tasks 5/7/Stage-2 fill the
// corresponding modules (kept off while those modules are placeholders so the
// Task-1 skeleton build is green).
pub use event::{RealtimeStatus, WalletEvent, WalletRealtimeEvent};
// pub use service::WalletRealtimeService;
pub use transport::{RealtimeTransport, WsFrame};
