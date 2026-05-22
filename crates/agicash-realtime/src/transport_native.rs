//! Native transport: `tokio-tungstenite` with a `rustls-platform-verifier`
//! backed TLS config. TLS chain validation goes through the platform-native
//! verifier (Security.framework on macOS/iOS, system cacerts on Android,
//! OS roots on Linux/Windows) so any user-installed root in the Sim
//! Keychain (e.g. mkcert) is honoured. Mirrors the REST path established
//! by `agicash-storage-supabase/src/client.rs:46-86`. The previous
//! `rustls-tls-native-roots` feature reads `/etc/ssl/...` on macOS but
//! NOT the iOS Simulator Keychain, which broke the realtime channel
//! silently on every iOS sim build.
//!
//! tungstenite auto-Pongs WS Pings by default — we keep that.
use crate::error::TransportError;
use crate::transport::{RealtimeTransport, WsFrame};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rustls_platform_verifier::ConfigVerifierExt;
use std::sync::{Arc, OnceLock};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async_tls_with_config, tungstenite::Message, Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error};

type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Process-wide rustls `ClientConfig` built once with the platform
/// verifier. Reused across every (re)connect so we don't pay the
/// platform-verifier init cost on each socket. `OnceLock` is enough
/// here because the config is `Send + Sync` and `Arc`-wrapped already
/// (tungstenite's `Connector::Rustls(Arc<ClientConfig>)`).
fn shared_tls_config() -> Result<Arc<rustls::ClientConfig>, TransportError> {
    // Two process-wide statics, declared up front so clippy's
    // `items_after_statements` lint is happy:
    //   * `PROVIDER_INSTALL` — ensures ring's default `CryptoProvider`
    //     is installed exactly once before the first
    //     `with_platform_verifier()` call.
    //   * `TLS_CONFIG` — caches the built `ClientConfig` so reconnect
    //     loops don't pay the platform-verifier init cost twice.
    static PROVIDER_INSTALL: OnceLock<()> = OnceLock::new();
    static TLS_CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();

    // `install_default` returns `Err` if a provider is already
    // installed (which is fine — we just need *some* provider).
    PROVIDER_INSTALL.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });

    if let Some(cfg) = TLS_CONFIG.get() {
        return Ok(Arc::clone(cfg));
    }
    let cfg = rustls::ClientConfig::with_platform_verifier()
        .map_err(|e| TransportError::Connect(format!("rustls platform verifier: {e}")))?;
    // `OnceLock::set` returns `Err` if another thread won the race —
    // in that case prefer the already-installed value (both are
    // equivalent platform-verifier configs).
    let arc = Arc::new(cfg);
    if TLS_CONFIG.set(Arc::clone(&arc)).is_err() {
        return Ok(Arc::clone(
            TLS_CONFIG.get().expect("set raced; get must be Some"),
        ));
    }
    Ok(arc)
}

#[derive(Default)]
pub struct NativeTransport {
    sock: Option<Sock>,
}

impl std::fmt::Debug for NativeTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTransport")
            .field("connected", &self.sock.is_some())
            .finish_non_exhaustive()
    }
}

impl NativeTransport {
    #[must_use]
    pub fn new() -> Self {
        Self { sock: None }
    }
}

#[async_trait]
impl RealtimeTransport for NativeTransport {
    async fn connect(&mut self, url: &str) -> Result<(), TransportError> {
        debug!(url, "native transport connect starting");
        let tls_config = shared_tls_config()?;
        let connector = Connector::Rustls(tls_config);
        // `connect_async_tls_with_config(url, None, false, Some(connector))`:
        //   - `None` → default WebSocketConfig (matches the previous
        //     `connect_async` behavior).
        //   - `false` → don't disable Nagle (same default as before).
        //   - `Some(connector)` → use our platform-verifier-backed
        //     rustls ClientConfig, not tungstenite's built-in roots.
        let (sock, _resp) = connect_async_tls_with_config(url, None, false, Some(connector))
            .await
            .map_err(|e| {
                // Log the actual tungstenite error string — this is the
                // missing breadcrumb that hid the iOS sim TLS handshake
                // failure for weeks. The supervisor wraps this into
                // `TransportError::Connect` → `RealtimeError::Transport`
                // → the "all other errors" arm of `service.rs::serve_step`
                // → infinite reconnect-loop with no diagnostic.
                error!(error = %e, url, "tungstenite connect failed");
                TransportError::Connect(e.to_string())
            })?;
        debug!(url, "native transport connect ok");
        self.sock = Some(sock);
        Ok(())
    }

    async fn send_text(&mut self, frame: String) -> Result<(), TransportError> {
        let s = self
            .sock
            .as_mut()
            .ok_or_else(|| TransportError::Send("not connected".into()))?;
        s.send(Message::Text(frame))
            .await
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    async fn recv(&mut self) -> Option<Result<WsFrame, TransportError>> {
        let s = self.sock.as_mut()?;
        loop {
            match s.next().await? {
                Ok(Message::Text(t)) => return Some(Ok(WsFrame::Text(t))),
                Ok(Message::Binary(b)) => return Some(Ok(WsFrame::Binary(b))),
                // Ping/Pong (tungstenite auto-Pongs) and raw Frame are
                // transport-internal — skip and await the next message.
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Ok(Message::Close(_)) => {
                    return Some(Err(TransportError::Closed("server close".into())))
                }
                Err(e) => return Some(Err(TransportError::Other(e.to_string()))),
            }
        }
    }

    async fn close(&mut self, _code: u16, _reason: &str) -> Result<(), TransportError> {
        if let Some(mut s) = self.sock.take() {
            let _ = s.close(None).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn connect_to_unreachable_errors_fast() {
        let mut t = NativeTransport::new();
        // TEST-NET-1 documentation block — connection can never complete.
        // Wrap in a timeout so a non-routable host (no RST, just silence)
        // can't hang the test suite; either the connect errors or the
        // timeout fires — both prove `connect` does not succeed here.
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            t.connect("ws://192.0.2.1:9/realtime/v1/websocket"),
        )
        .await;
        match r {
            Ok(inner) => assert!(inner.is_err(), "connect must not succeed"),
            Err(_elapsed) => { /* timed out → connect did not succeed */ }
        }
    }

    #[tokio::test]
    async fn shared_tls_config_returns_reused_arc() {
        // Two calls must produce the same `Arc<ClientConfig>` (pointer-eq)
        // so reconnect loops don't pay platform-verifier init twice.
        let a = shared_tls_config().expect("first build ok");
        let b = shared_tls_config().expect("second build ok");
        assert!(Arc::ptr_eq(&a, &b), "shared_tls_config must memoize");
    }
}
