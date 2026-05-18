//! Transport abstraction: native (tokio-tungstenite) vs wasm
//! (web-sys WebSocket). Bounds-alias pattern copied verbatim from
//! `agicash-traits/src/token_provider.rs` — `Send + Sync` on native,
//! empty on wasm — so `web_sys::WebSocket`'s `!Send` is contained.
use crate::error::TransportError;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub enum WsFrame {
    Text(String),
    Binary(Vec<u8>),
}

#[cfg(not(target_arch = "wasm32"))]
pub trait TransportBounds: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync> TransportBounds for T {}

#[cfg(target_arch = "wasm32")]
pub trait TransportBounds {}
#[cfg(target_arch = "wasm32")]
impl<T> TransportBounds for T {}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RealtimeTransport: TransportBounds {
    /// Open the socket to the fully-formed URL (apikey+log_level+vsn
    /// already in the query string — the client builds it).
    async fn connect(&mut self, url: &str) -> Result<(), TransportError>;
    /// Send a Text frame. We only ever send the JSON-array form.
    async fn send_text(&mut self, frame: String) -> Result<(), TransportError>;
    /// Next inbound frame; MUST distinguish Text vs Binary.
    async fn recv(&mut self) -> Option<Result<WsFrame, TransportError>>;
    async fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ws_frame_variants_construct() {
        let t = WsFrame::Text("x".into());
        let b = WsFrame::Binary(vec![1, 2]);
        assert!(matches!(t, WsFrame::Text(_)));
        assert!(matches!(b, WsFrame::Binary(_)));
    }
}
